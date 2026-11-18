//! Developer-only SSH server switch (Settings → Developer).
//!
//! Verso ships its own server: upstream Dropbear, plus OpenSSH's sftp-server
//! for scp and sftp. scripts/build-ssh.sh builds them and scripts/deploy.sh
//! installs them in /mnt/us/verso/bin. Keys live in /mnt/us/verso/ssh. It uses
//! KOReader's port and pidfile, so either app sees and can stop a server the
//! other one started. The server keeps running after Verso exits. While it
//! runs, the mDNS responder (platform/mdns.rs) advertises `verso.local`.
//! See docs/device-tools.md.

use anyhow::Result;

pub const PORT: u16 = 2222;

/// What the Settings row shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Status {
    pub running: bool,
    /// Why the server can't be started, if it can't.
    pub unavailable: Option<&'static str>,
    /// This device's address on the local network, if it has one.
    pub address: Option<String>,
    /// The name it's advertised as, without `.local` (e.g. `verso`, or
    /// `verso-2` if another device had the name), once the responder has it.
    pub hostname: Option<String>,
}

pub fn status() -> Status {
    imp::status()
}

/// Starts the server and advertises it as `<hostname>.local`. Takes up to
/// ~2 s while the name is claimed.
pub fn start(hostname: &str) -> Result<()> {
    imp::start(hostname)
}

/// Stops the server and its name. Quick, but can take up to ~2 s.
pub fn stop() -> Result<()> {
    imp::stop()
}

/// Advertises a server that's already running (left on when Verso last
/// exited, or started by KOReader) under `hostname`, unless that's already
/// happening. Our server left on an older binary (after an update or a
/// deploy) is restarted on the new one, in the background. Call at startup.
pub fn advertise(hostname: &str) {
    imp::advertise(hostname)
}

/// The address other devices on the network reach us at. Connecting a UDP
/// socket sends nothing; it only asks the kernel which source address its
/// route would use. Fails when there is no network (e.g. Wi-Fi off).
#[cfg(feature = "kindle")]
fn local_address() -> Option<String> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:9").ok()?; // TEST-NET-1: any routable address works.
    let ip = socket.local_addr().ok()?.ip();
    (!ip.is_unspecified() && !ip.is_loopback()).then(|| ip.to_string())
}

#[cfg(feature = "kindle")]
mod imp {
    use super::super::mdns;
    use super::super::process::{alive, runs_replaced, spawn_detached, wait_until};
    use super::{PORT, Status, local_address};
    use anyhow::{Context, Result, bail};
    use std::fs;
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    use std::path::Path;
    use std::process::{Command, Stdio};
    use std::time::Duration;

    const BINARY: &str = "/mnt/us/verso/bin/dropbear";
    const SSH_DIR: &str = "/mnt/us/verso/ssh";
    /// The keys allowed to log in. Edited over USB.
    const KEYS: &str = "/mnt/us/verso/ssh/authorized_keys";
    /// Must match ED25519_PRIV_FILENAME in scripts/ssh/localoptions.h.
    const HOST_KEY: &str = "/mnt/us/verso/ssh/dropbear_ed25519_host_key";
    /// Taken over on the first start, so `ssh` keeps working with the same
    /// keys and host fingerprint as KOReader's server.
    const KOREADER_KEYS: &str = "/mnt/us/koreader/settings/SSH/authorized_keys";
    const KOREADER_HOST_KEY: &str = "/mnt/us/koreader/settings/SSH/dropbear_ed25519_host_key";
    /// dropbear only reads authorized_keys from a folder that nobody else can
    /// write to, and with an absolute path it checks every folder up to `/`.
    /// Everything on /mnt/us (and /tmp itself) is writable by all, so the
    /// server runs in this private folder with a copy of the keys, and is
    /// pointed at them with a relative path (`-D .`): then only this folder
    /// is checked.
    const RUN_DIR: &str = "/tmp/verso-ssh";
    /// KOReader's pidfile, so both apps agree on whether it's running.
    const PIDFILE: &str = "/tmp/dropbear_koreader.pid";

    pub fn status() -> Status {
        let running = running_pid().is_some();
        Status {
            running,
            unavailable: unavailable(),
            address: local_address(),
            hostname: if running { mdns::hostname() } else { None },
        }
    }

    pub fn advertise(hostname: &str) {
        let Some(pid) = running_pid() else { return };
        if !runs_replaced(pid, BINARY) {
            mdns::ensure(hostname, pid);
            return;
        }
        // Takes a few seconds (the name is claimed again); not on the UI thread.
        let hostname = hostname.to_owned();
        let spawned = std::thread::Builder::new().name("ssh-restart".into()).spawn(move || {
            if let Err(e) = restart(pid, &hostname) {
                log::warn!("SSH: restarting the server on its new binary: {e:#}");
            }
        });
        if let Err(e) = spawned {
            log::warn!("SSH: starting the restart thread: {e}");
        }
    }

    /// Restarts the listening server `pid` on the current binary. Open
    /// sessions are its children and keep running on the old one until
    /// they end. The responder follows the old server out and starts
    /// again with the new one.
    fn restart(pid: i32, hostname: &str) -> Result<()> {
        log::info!("SSH: restarting the server to run the updated binary");
        // SAFETY: plain kill(2) on a PID just checked to be dropbear.
        unsafe { libc::kill(pid, libc::SIGTERM) };
        if !wait_until(Duration::from_secs(2), || !alive(pid)) {
            bail!("the old server didn't stop");
        }
        let _ = fs::remove_file(PIDFILE);
        mdns::wait_stopped();
        start(hostname)
    }

    fn unavailable() -> Option<&'static str> {
        if !Path::new(BINARY).exists() {
            Some("Not installed (verso/bin/dropbear is missing)")
        } else if keys_source().is_none() {
            Some("No SSH keys: add one to verso/ssh/authorized_keys")
        } else {
            None
        }
    }

    /// Verso's keys file, or KOReader's while Verso doesn't have one yet.
    fn keys_source() -> Option<&'static str> {
        let non_empty = |path| fs::metadata(path).is_ok_and(|m| m.len() > 0);
        if Path::new(KEYS).exists() {
            non_empty(KEYS).then_some(KEYS)
        } else {
            non_empty(KOREADER_KEYS).then_some(KOREADER_KEYS)
        }
    }

    /// The server's PID, if the pidfile names a live dropbear (ours or KOReader's).
    fn running_pid() -> Option<i32> {
        let pid: i32 = fs::read_to_string(PIDFILE).ok()?.trim().parse().ok()?;
        let comm = fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
        (comm.trim() == "dropbear").then_some(pid)
    }

    pub fn start(hostname: &str) -> Result<()> {
        if running_pid().is_some() {
            advertise(hostname);
            return Ok(());
        }
        if let Some(reason) = unavailable() {
            bail!("{reason}");
        }
        prepare_keys()?;
        firewall(true);
        // -F: stay in the foreground, which keeps RUN_DIR as the working
        // folder for `-D .` (the spawn below detaches it instead). -R: create
        // a missing host key. -s: no password logins. Without -E it logs to
        // the system log, so it never holds Verso's log file open.
        let mut command = Command::new(BINARY);
        command
            .args(["-F", "-R", "-s", "-p", &PORT.to_string(), "-P", PIDFILE, "-D", "."])
            .current_dir(RUN_DIR)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let ok = spawn_detached(&mut command).context("starting dropbear")?;
        // The server writes the pidfile once it's listening.
        if ok && wait_until(Duration::from_secs(2), || running_pid().is_some()) {
            log::info!("SSH server started on port {PORT}");
            if let Some(pid) = running_pid() {
                mdns::ensure(hostname, pid);
                // Claiming the name takes about a second; without a network
                // there's nothing to wait for.
                if local_address().is_some() {
                    mdns::wait_for_name(Duration::from_secs(2));
                }
            }
            return Ok(());
        }
        firewall(false);
        bail!("dropbear didn't start")
    }

    /// Takes over KOReader's keys on the first start, then puts a private
    /// copy of the keys where dropbear accepts them (see RUN_DIR).
    fn prepare_keys() -> Result<()> {
        fs::create_dir_all(SSH_DIR).context("creating verso/ssh")?;
        if !Path::new(KEYS).exists() {
            fs::copy(KOREADER_KEYS, KEYS).context("copying KOReader's SSH keys")?;
            log::info!("SSH: copied KOReader's authorized_keys to {KEYS}");
        }
        if !Path::new(HOST_KEY).exists() && Path::new(KOREADER_HOST_KEY).exists() {
            fs::copy(KOREADER_HOST_KEY, HOST_KEY).context("copying KOReader's host key")?;
            log::info!("SSH: copied KOReader's host key");
        }
        let _ = fs::remove_dir_all(RUN_DIR);
        fs::DirBuilder::new().mode(0o700).create(RUN_DIR).context("creating the SSH run folder")?;
        let keys = Path::new(RUN_DIR).join("authorized_keys");
        fs::copy(KEYS, &keys).context("copying SSH keys")?;
        fs::set_permissions(&keys, fs::Permissions::from_mode(0o600))?;
        Ok(())
    }

    pub fn stop() -> Result<()> {
        let Some(pid) = running_pid() else {
            let _ = fs::remove_file(PIDFILE);
            let _ = fs::remove_dir_all(RUN_DIR);
            firewall(false);
            mdns::wait_stopped();
            return Ok(());
        };
        // Like KOReader: end open sessions (the server's children), then the server.
        let _ = Command::new("pkill")
            .args(["-TERM", "-P", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        // SAFETY: plain kill(2) on a PID we just checked is dropbear.
        unsafe { libc::kill(pid, libc::SIGTERM) };
        if !wait_until(Duration::from_secs(2), || !alive(pid)) {
            bail!("dropbear didn't stop");
        }
        let _ = fs::remove_file(PIDFILE);
        let _ = fs::remove_dir_all(RUN_DIR);
        firewall(false);
        mdns::wait_stopped();
        log::info!("SSH server stopped");
        Ok(())
    }

    /// Opens or closes the server's port in the Kindle's firewall, with
    /// KOReader's exact rules. Adding checks first, so rules never pile up.
    fn firewall(open: bool) {
        let rules = [
            format!("INPUT -p tcp --dport {PORT} -m conntrack --ctstate NEW,ESTABLISHED -j ACCEPT"),
            format!("OUTPUT -p tcp --sport {PORT} -m conntrack --ctstate ESTABLISHED -j ACCEPT"),
        ];
        let iptables = |op: &str, rule: &str| {
            Command::new("iptables")
                .arg(op)
                .args(rule.split_whitespace())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
        };
        for rule in &rules {
            if open {
                if !iptables("-C", rule) && !iptables("-A", rule) {
                    log::warn!("couldn't open port {PORT} in the firewall");
                }
            } else {
                // Remove every copy (another tool may have added one too).
                for _ in 0..4 {
                    if !iptables("-D", rule) {
                        break;
                    }
                }
            }
        }
    }
}

/// The simulator pretends, so the Settings row can be tried and captured.
#[cfg(not(feature = "kindle"))]
mod imp {
    use super::Status;
    use anyhow::Result;
    use std::sync::atomic::{AtomicBool, Ordering};

    static RUNNING: AtomicBool = AtomicBool::new(false);

    pub fn status() -> Status {
        let running = RUNNING.load(Ordering::Relaxed);
        let address = super::super::wifi::enabled().unwrap_or(false).then(|| "192.168.1.42".to_owned());
        let hostname = (running && address.is_some()).then(|| verso_mdns::DEFAULT_LABEL.to_owned());
        Status { running, unavailable: None, address, hostname }
    }

    pub fn start(_hostname: &str) -> Result<()> {
        RUNNING.store(true, Ordering::Relaxed);
        Ok(())
    }

    pub fn stop() -> Result<()> {
        RUNNING.store(false, Ordering::Relaxed);
        Ok(())
    }

    pub fn advertise(_hostname: &str) {}
}
