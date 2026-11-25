//! The responder process: the socket, timers and lifecycle around
//! [`Responder`].
//!
//! Event-driven: it sleeps in `poll` on the socket and on a pidfd for the
//! process it serves (falling back to `/proc` polling on Linux < 5.3), waking
//! for packets, for probe and announce timers, and every 10 s to check the
//! address. When the watched process exits, or on SIGTERM, it withdraws its
//! records and exits.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::net::{SocketAddr, UdpSocket};
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
/// How often to poll `/proc/<pid>/stat` when `pidfd_open` is unavailable
/// (Linux < 5.3). Short enough to fit well inside Verso's 1 s stop wait.
const WATCH_POLL_INTERVAL: Duration = Duration::from_millis(250);
/// Return to poll after a bounded batch so signals and the watched pidfd
/// cannot be starved by a continuously readable multicast socket.
const PACKET_BATCH_LIMIT: usize = 64;

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
    let reason = serve(&socket, watch.as_ref(), options.firewall.as_deref(), &mut responder);
    send(&socket, responder.goodbye());
    say!("stopping: {reason}");
    if let Some(interface) = &options.firewall {
        firewall(interface, false);
    }
    let _ = fs::remove_file(NAME_FILE);
    drop(lock);
    0
}

fn serve(
    socket: &UdpSocket,
    watch: Option<&Watcher>,
    interface: Option<&str>,
    responder: &mut Responder,
) -> &'static str {
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
            let link = find_link(interface);
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

        let mut deadline = responder.deadline().map_or(next_check, |d| d.min(next_check));
        if let Some(interval) = watch.and_then(Watcher::poll_interval) {
            deadline = deadline.min(Instant::now() + interval);
        }
        let wait = deadline.saturating_duration_since(Instant::now());
        let timeout = wait.as_micros().div_ceil(1000).min(i32::MAX as u128) as i32;
        let mut fds = vec![libc::pollfd { fd: socket.as_raw_fd(), events: libc::POLLIN, revents: 0 }];
        if let Some(fd) = watch.and_then(Watcher::pidfd) {
            fds.push(libc::pollfd { fd, events: libc::POLLIN, revents: 0 });
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
        if fds.get(1).is_some_and(|fd| fd.revents != 0) || watch.is_some_and(|w| !w.is_alive()) {
            return "the watched process exited";
        }
        if fds[0].revents != 0 {
            for _ in 0..PACKET_BATCH_LIMIT {
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

/// Tracks the watched process either via a pidfd (Linux >= 5.3) or by polling
/// `/proc/<pid>/stat` with its start time to guard against PID reuse.
enum Watcher {
    PidFd(OwnedFd),
    Poll { pid: i32, start_ticks: u64 },
}

impl Watcher {
    fn poll(pid: i32, name: Option<&str>) -> io::Result<Self> {
        let (state, start_ticks) = read_proc_stat(pid)?;
        if !is_live_state(state) {
            return Err(io::Error::other(format!("process {pid} has exited")));
        }
        if let Some(name) = name {
            check_comm(pid, name)?;
            // Confirm the PID wasn't reused between reading `stat` and `comm`.
            let (state2, ticks2) = read_proc_stat(pid)?;
            if ticks2 != start_ticks || !is_live_state(state2) {
                return Err(io::Error::other(format!("process {pid} has exited")));
            }
        }
        Ok(Self::Poll { pid, start_ticks })
    }

    fn pidfd(&self) -> Option<RawFd> {
        match self {
            Self::PidFd(fd) => Some(fd.as_raw_fd()),
            Self::Poll { .. } => None,
        }
    }

    fn poll_interval(&self) -> Option<Duration> {
        match self {
            Self::PidFd(_) => None,
            Self::Poll { .. } => Some(WATCH_POLL_INTERVAL),
        }
    }

    fn is_alive(&self) -> bool {
        match *self {
            Self::PidFd(_) => true,
            Self::Poll { pid, start_ticks } => {
                read_proc_stat(pid).is_ok_and(|(state, ticks)| ticks == start_ticks && is_live_state(state))
            }
        }
    }
}

fn is_live_state(state: char) -> bool {
    !matches!(state, 'Z' | 'X')
}

fn check_comm(pid: i32, name: &str) -> io::Result<()> {
    let comm = fs::read_to_string(format!("/proc/{pid}/comm"))?;
    if comm.trim() != name {
        return Err(io::Error::other(format!("process {pid} is {:?}, not {name:?}", comm.trim())));
    }
    Ok(())
}

/// Parses `(state, starttime)` (fields 3 and 22) from `/proc/<pid>/stat`.
/// Field 2 `(comm)` can contain spaces and parentheses, so parsing starts
/// after the last `')'`.
fn parse_proc_stat(stat: &str) -> Option<(char, u64)> {
    let (_, rest) = stat.rsplit_once(')')?;
    let mut fields = rest.split_ascii_whitespace();
    let mut state_chars = fields.next()?.chars();
    let state = state_chars.next()?;
    if state_chars.next().is_some() {
        return None;
    }
    // Token 0 was field 3 (`state`); token 19 (18 skipped after token 0) is field 22 (`starttime`).
    let start_ticks = fields.nth(18)?.parse::<u64>().ok()?;
    Some((state, start_ticks))
}

fn read_proc_stat(pid: i32) -> io::Result<(char, u64)> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat"))?;
    parse_proc_stat(&stat).ok_or_else(|| io::Error::other(format!("can't parse /proc/{pid}/stat")))
}

/// Watches `pid`, preferring `pidfd_open` and falling back to `/proc` polling
/// on older kernels (Linux < 5.3) where `pidfd_open` returns `ENOSYS`.
fn watch(pid: i32, name: Option<&str>) -> io::Result<Watcher> {
    // SAFETY: pidfd_open takes a PID and flags and returns a new descriptor.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) } as RawFd;
    if fd < 0 {
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ENOSYS) {
            return Watcher::poll(pid, name);
        }
        return Err(err);
    }
    // SAFETY: the kernel just gave us this descriptor.
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    // The PID could have been reused before the pidfd pinned it.
    if let Some(name) = name {
        check_comm(pid, name)?;
    }
    Ok(Watcher::PidFd(fd))
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
/// our default route uses, the same address Settings shows. Without a
/// default route (a network without a gateway), `interface`'s address.
fn find_link(interface: Option<&str>) -> Option<Link> {
    let routed = (|| {
        let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
        // Connecting a UDP socket sends nothing; it picks a source address.
        socket.connect("192.0.2.1:9").ok()?;
        match socket.local_addr().ok()?.ip() {
            std::net::IpAddr::V4(ip) if !ip.is_unspecified() && !ip.is_loopback() => Some(ip),
            _ => None,
        }
    })();
    let found = match routed {
        Some(addr) => crate::interface_addresses().into_iter().map(|(_, a)| a).find(|a| a.addr == addr),
        None => crate::interface_address(interface?),
    }?;
    Some(Link { addr: found.addr, netmask: found.netmask, index: found.index })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_proc_stat_handles_spaces_and_parens() {
        // Fields after `(comm)`: 3=state(S), 4..=21 (18 fields), 22=starttime(987654321), 23=vsize
        let tricky = "42 (foo (bar) baz) S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 987654321 999\n";
        assert_eq!(parse_proc_stat(tricky), Some(('S', 987_654_321)));

        let zombie = "42 (sshd) Z 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 12345 0\n";
        let (state, ticks) = parse_proc_stat(zombie).expect("parse zombie stat");
        assert_eq!((state, ticks), ('Z', 12_345));
        assert!(!is_live_state(state));

        for bad in [
            "42 no-parens S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 123",
            "42 (short) S 1 2 3",
            "42 (bad-state) SS 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 123",
            "42 (bad-ticks) S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 nope",
        ] {
            assert_eq!(parse_proc_stat(bad), None, "{bad:?}");
        }

        let self_stat = fs::read_to_string("/proc/self/stat").expect("read /proc/self/stat");
        let (state, start_ticks) = parse_proc_stat(&self_stat).expect("parse /proc/self/stat");
        assert!(is_live_state(state), "unexpected state {state:?}");
        assert!(start_ticks > 0);
    }

    #[test]
    fn poll_watcher_tracks_child_lifecycle() {
        let self_pid = std::process::id() as i32;
        let self_comm = fs::read_to_string("/proc/self/comm").expect("read /proc/self/comm");
        assert!(Watcher::poll(self_pid, Some(self_comm.trim())).is_ok());
        assert!(Watcher::poll(self_pid, Some("definitely-not-verso-test")).is_err());

        // A stale start_ticks on a live PID represents PID reuse and must read as exited.
        let (_, real_ticks) = read_proc_stat(self_pid).expect("read self stat");
        let reused = Watcher::Poll { pid: self_pid, start_ticks: real_ticks.wrapping_add(1) };
        assert!(!reused.is_alive());

        let mut child = Command::new("sleep")
            .arg("60")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn sleep");
        let child_pid = child.id() as i32;
        let watcher = Watcher::poll(child_pid, None).expect("watch child");
        assert_eq!(watcher.pidfd(), None);
        assert_eq!(watcher.poll_interval(), Some(WATCH_POLL_INTERVAL));
        assert!(watcher.is_alive());

        let _ = child.kill();
        let _ = child.wait();
        assert!(!watcher.is_alive());
    }
}
