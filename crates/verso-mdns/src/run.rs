//! The responder process: the socket, timers and lifecycle around
//! [`Responder`].
//!
//! Event-driven: it sleeps in `poll` on the socket and on a pidfd for the
//! process it serves (the SSH server), waking for packets, for probe and
//! announce timers, and every 10 s to check the address. When the watched
//! process exits, or on SIGTERM, it withdraws its records and exits.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

use crate::responder::{GROUP, Link, PORT, Responder, Send};
use crate::{NAME_FILE, PID_FILE, PROCESS_NAME};

/// How often to look for a changed address (after a Wi-Fi reconnect). A
/// netlink listener would notice at once, but this is one cheap check.
const ADDRESS_CHECK: Duration = Duration::from_secs(10);

pub struct Options {
    /// Advertise `<label>.local`; must pass [`crate::valid_label`].
    pub label: String,
    /// Exit when this process exits.
    pub watch: Option<i32>,
    /// …and only if it has this process name, to rule out a reused PID.
    pub watch_name: Option<String>,
    /// Accept mDNS on this interface in the firewall while running (Kindle).
    pub firewall: Option<String>,
}

static STOP: AtomicBool = AtomicBool::new(false);
static START: OnceLock<Instant> = OnceLock::new();

macro_rules! say {
    ($($arg:tt)*) => {{
        let elapsed = START.get().map_or(0.0, |s| s.elapsed().as_secs_f32());
        eprintln!("{} [{elapsed:7.1}s] {}", PROCESS_NAME, format_args!($($arg)*));
    }};
}

/// Runs until the watched process exits or a signal arrives. Returns the
/// process exit code.
pub fn run(options: Options) -> i32 {
    START.get_or_init(Instant::now);
    let Some(lock) = Lock::acquire() else {
        say!("another responder is running");
        return 0;
    };
    let watch = match options.watch.map(|pid| watch(pid, options.watch_name.as_deref())).transpose() {
        Ok(watch) => watch,
        Err(e) => {
            say!("not watching {:?}: {e}", options.watch);
            return 0;
        }
    };
    install_signal_handlers();
    let socket = match open_socket() {
        Ok(socket) => socket,
        Err(e) => {
            say!("can't open the mDNS socket: {e}");
            return 1;
        }
    };
    if let Some(interface) = &options.firewall {
        firewall(interface, true);
    }
    say!("started for {}.local", options.label);
    let seed = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64)
        ^ u64::from(std::process::id());
    let mut responder = Responder::new(&options.label, seed, Instant::now());
    let reason = serve(&socket, watch.as_ref(), &mut responder);
    send(&socket, responder.goodbye());
    say!("stopping: {reason}");
    if let Some(interface) = &options.firewall {
        firewall(interface, false);
    }
    let _ = fs::remove_file(NAME_FILE);
    drop(lock);
    0
}

fn serve(socket: &UdpSocket, watch: Option<&OwnedFd>, responder: &mut Responder) -> &'static str {
    let mut next_check = Instant::now();
    let mut written: Option<String> = None;
    let mut probing = responder.label().to_owned();
    let mut buf = [0u8; 9000];
    loop {
        if STOP.load(Ordering::Relaxed) {
            return "signal";
        }
        let now = Instant::now();
        if now >= next_check {
            let link = find_link();
            if link != responder.link() {
                match link {
                    Some(l) => {
                        say!("address {} on interface {}", l.addr, l.index);
                        if let Err(e) = join(socket, l.index) {
                            say!("can't join the mDNS group: {e}");
                        }
                    }
                    None => say!("no network"),
                }
                responder.set_link(link, now);
            }
            next_check = now + ADDRESS_CHECK;
        }
        send(socket, responder.on_timer(now));
        if responder.label() != probing {
            say!("{probing}.local is taken; trying {}.local", responder.label());
            probing = responder.label().to_owned();
        }
        if responder.won() != written.as_deref() {
            written = responder.won().map(str::to_owned);
            if let Some(label) = &written {
                say!("answering as {label}.local");
                if let Err(e) = write_name(label) {
                    say!("can't write {NAME_FILE}: {e}");
                }
            }
        }

        let deadline = responder.deadline().map_or(next_check, |d| d.min(next_check));
        let wait = deadline.saturating_duration_since(Instant::now());
        let timeout = wait.as_micros().div_ceil(1000).min(i32::MAX as u128) as i32;
        let mut fds = vec![libc::pollfd { fd: socket.as_raw_fd(), events: libc::POLLIN, revents: 0 }];
        if let Some(fd) = watch {
            fds.push(libc::pollfd { fd: fd.as_raw_fd(), events: libc::POLLIN, revents: 0 });
        }
        // SAFETY: `fds` is a valid array of pollfd for the length given.
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout) };
        if ready < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            say!("poll: {e}");
            return "error";
        }
        if fds.get(1).is_some_and(|fd| fd.revents != 0) {
            return "the watched process exited";
        }
        if fds[0].revents != 0 {
            loop {
                match socket.recv_from(&mut buf) {
                    Ok((n, SocketAddr::V4(src))) => send(socket, responder.on_packet(src, &buf[..n], Instant::now())),
                    Ok(_) => {}
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    Err(e) => {
                        say!("receive: {e}");
                        break;
                    }
                }
            }
        }
    }
}

fn send(socket: &UdpSocket, sends: Vec<Send>) {
    for s in sends {
        // Fails while the network drops; the address check catches up.
        let _ = socket.send_to(&s.packet, s.to);
    }
}

/// The single-instance lock: the PID file, locked for as long as we run.
struct Lock(File);

impl Lock {
    fn acquire() -> Option<Lock> {
        let mut file = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(PID_FILE).ok()?;
        // SAFETY: flock on a file descriptor we own.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return None;
        }
        file.set_len(0).ok()?;
        write!(file, "{}", std::process::id()).ok()?;
        Some(Lock(file))
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        // Empty rather than removed: removing a locked file lets a second
        // responder lock a fresh one while a third still sees the old.
        let _ = self.0.set_len(0);
    }
}

/// A pidfd that becomes readable when `pid` exits.
fn watch(pid: i32, name: Option<&str>) -> io::Result<OwnedFd> {
    // SAFETY: pidfd_open takes a PID and flags and returns a new descriptor.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) } as RawFd;
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the kernel just gave us this descriptor.
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    // The PID could have been reused before the pidfd pinned it.
    if let Some(name) = name {
        let comm = fs::read_to_string(format!("/proc/{pid}/comm"))?;
        if comm.trim() != name {
            return Err(io::Error::other(format!("process {pid} is {:?}, not {name:?}", comm.trim())));
        }
    }
    Ok(fd)
}

extern "C" fn on_signal(_: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
}

fn install_signal_handlers() {
    for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        // SAFETY: the handler only stores to an atomic, which is
        // async-signal-safe. No SA_RESTART, so poll returns EINTR at once.
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = on_signal as extern "C" fn(libc::c_int) as usize;
            libc::sigemptyset(&mut action.sa_mask);
            libc::sigaction(signal, &action, std::ptr::null_mut());
        }
    }
}

fn open_socket() -> io::Result<UdpSocket> {
    // SAFETY: plain socket calls on a descriptor we own from here on; the
    // option and address structs live across the calls that read them.
    let socket = unsafe {
        let fd = libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK, 0);
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let socket = UdpSocket::from_raw_fd(fd);
        // Shares port 5353 with any other responder on the machine (Avahi on
        // a laptop); each gets a copy of every multicast packet.
        let one: libc::c_int = 1;
        setsockopt(fd, libc::SOL_SOCKET, libc::SO_REUSEADDR, &one)?;
        let addr = libc::sockaddr_in {
            sin_family: libc::AF_INET as libc::sa_family_t,
            sin_port: PORT.to_be(),
            sin_addr: libc::in_addr { s_addr: 0 },
            sin_zero: [0; 8],
        };
        let len = std::mem::size_of_val(&addr) as libc::socklen_t;
        if libc::bind(fd, (&raw const addr).cast(), len) != 0 {
            return Err(io::Error::last_os_error());
        }
        socket
    };
    // Some resolvers drop mDNS packets that may have crossed a router (§11).
    socket.set_multicast_ttl_v4(255)?;
    socket.set_ttl(255)?;
    // Loopback stays on (the default), so resolvers on the same machine see
    // our answers too. Our own packets coming back are harmless: identical
    // records are no conflict, and an identical probe ties.
    Ok(socket)
}

/// Joins the mDNS group on the interface and sends from it. By index, so
/// the membership survives an address change; joining again is harmless.
fn join(socket: &UdpSocket, index: u32) -> io::Result<()> {
    let request = libc::ip_mreqn {
        imr_multiaddr: libc::in_addr { s_addr: u32::from_ne_bytes(GROUP.octets()) },
        imr_address: libc::in_addr { s_addr: 0 },
        imr_ifindex: index as libc::c_int,
    };
    let fd = socket.as_raw_fd();
    // SAFETY: valid socket and option struct.
    if let Err(e) = unsafe { setsockopt(fd, libc::IPPROTO_IP, libc::IP_ADD_MEMBERSHIP, &request) }
        && e.raw_os_error() != Some(libc::EADDRINUSE)
    {
        return Err(e);
    }
    // SAFETY: valid socket and option struct.
    unsafe { setsockopt(fd, libc::IPPROTO_IP, libc::IP_MULTICAST_IF, &request) }
}

/// # Safety
/// `fd` must be a socket and `T` the option's expected type.
unsafe fn setsockopt<T>(fd: RawFd, level: libc::c_int, name: libc::c_int, value: &T) -> io::Result<()> {
    let len = std::mem::size_of::<T>() as libc::socklen_t;
    // SAFETY: per the function's contract.
    if unsafe { libc::setsockopt(fd, level, name, (value as *const T).cast(), len) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// The interface other devices reach us on: the one holding the address
/// our default route uses, the same address Settings shows.
fn find_link() -> Option<Link> {
    let addr = {
        let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
        // Connecting a UDP socket sends nothing; it picks a source address.
        socket.connect("192.0.2.1:9").ok()?;
        match socket.local_addr().ok()?.ip() {
            std::net::IpAddr::V4(ip) if !ip.is_unspecified() && !ip.is_loopback() => ip,
            _ => return None,
        }
    };
    let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs fills `list`, which we walk read-only and free.
    unsafe {
        if libc::getifaddrs(&mut list) != 0 {
            return None;
        }
        let mut found = None;
        let mut entry = list;
        while !entry.is_null() {
            let e = &*entry;
            entry = e.ifa_next;
            if e.ifa_addr.is_null() || e.ifa_netmask.is_null() || i32::from((*e.ifa_addr).sa_family) != libc::AF_INET {
                continue;
            }
            let ip = |sa: *const libc::sockaddr| {
                let sin = &*sa.cast::<libc::sockaddr_in>();
                Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr))
            };
            if ip(e.ifa_addr) == addr {
                let index = libc::if_nametoindex(e.ifa_name);
                if index != 0 {
                    found = Some(Link { addr, netmask: ip(e.ifa_netmask), index });
                }
                break;
            }
        }
        libc::freeifaddrs(list);
        found
    }
}

fn write_name(label: &str) -> io::Result<()> {
    let temp = format!("{NAME_FILE}.new");
    fs::write(&temp, format!("{label}\n"))?;
    fs::rename(&temp, NAME_FILE)
}

/// Accepts (or stops accepting) mDNS queries on `interface`. Checks first,
/// so rules never pile up; removes every copy.
fn firewall(interface: &str, open: bool) {
    let port = PORT.to_string();
    let rule = ["INPUT", "-i", interface, "-p", "udp", "--dport", &port, "-j", "ACCEPT"];
    let iptables = |op: &str| {
        Command::new("iptables")
            .arg(op)
            .args(rule)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    };
    if open {
        if !iptables("-C") && !iptables("-A") {
            say!("couldn't open UDP {PORT} in the firewall");
        }
    } else {
        for _ in 0..4 {
            if !iptables("-D") {
                break;
            }
        }
    }
}
