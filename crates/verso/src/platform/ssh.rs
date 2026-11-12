//! Developer-only SSH server switch (Settings → Developer).
//!
//! Verso doesn't ship an SSH server. On the Kindle it starts KOReader's own
//! patched `dropbear` exactly the way KOReader's SSH plugin does: same binary,
//! working directory, keys, port and pidfile. Either app can then see and stop
//! a server the other one started, and `ssh kindle` keeps the same host key.
//! dropbear detaches, so it keeps running after Verso exits.
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
}

pub fn status() -> Status {
    imp::status()
}

/// Starts or stops the server. Quick, but can take up to ~2 s to stop.
pub fn set_running(on: bool) -> Result<()> {
    imp::set_running(on)
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
    use super::{PORT, Status, local_address};
    use anyhow::{Context, Result, bail};
    use std::path::Path;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    const KOREADER: &str = "/mnt/us/koreader";
    const BINARY: &str = "/mnt/us/koreader/dropbear";
    /// The patched dropbear reads keys relative to its working directory.
    const KEYS: &str = "/mnt/us/koreader/settings/SSH/authorized_keys";
    /// KOReader's pidfile, so both apps agree on whether it's running.
    const PIDFILE: &str = "/tmp/dropbear_koreader.pid";

    pub fn status() -> Status {
        let unavailable = if !Path::new(BINARY).exists() {
            Some("Needs KOReader’s SSH server (koreader/dropbear)")
        } else if std::fs::metadata(KEYS).map_or(true, |m| m.len() == 0) {
            Some("No SSH keys: add one to koreader/settings/SSH/authorized_keys")
        } else {
            None
        };
        Status { running: running_pid().is_some(), unavailable, address: local_address() }
    }

    pub fn set_running(on: bool) -> Result<()> {
        if on { start() } else { stop() }
    }

    /// The server's PID, if the pidfile names a live dropbear.
    fn running_pid() -> Option<i32> {
        let pid: i32 = std::fs::read_to_string(PIDFILE).ok()?.trim().parse().ok()?;
        let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
        (comm.trim() == "dropbear").then_some(pid)
    }

    fn start() -> Result<()> {
        if running_pid().is_some() {
            return Ok(());
        }
        if let Some(reason) = status().unavailable {
            bail!("{reason}");
        }
        firewall(true);
        // Same command line as KOReader with "key-only" login: no passwords.
        // Its output goes nowhere, so it never holds Verso's log file open.
        let ok = Command::new(BINARY)
            .args(["-E", "-R", &format!("-p{PORT}"), "-P", PIDFILE, "-s"])
            .current_dir(KOREADER)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .context("starting dropbear")?
            .success();
        // dropbear forks into the background and writes the pidfile from there.
        if ok && wait_until(Duration::from_secs(2), || running_pid().is_some()) {
            log::info!("SSH server started on port {PORT}");
            return Ok(());
        }
        firewall(false);
        bail!("dropbear didn't start")
    }

    fn stop() -> Result<()> {
        let Some(pid) = running_pid() else {
            let _ = std::fs::remove_file(PIDFILE);
            firewall(false);
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
        if !wait_until(Duration::from_secs(2), || !Path::new(&format!("/proc/{pid}")).exists()) {
            bail!("dropbear didn't stop");
        }
        let _ = std::fs::remove_file(PIDFILE);
        firewall(false);
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

    fn wait_until(limit: Duration, mut done: impl FnMut() -> bool) -> bool {
        let start = Instant::now();
        loop {
            if done() {
                return true;
            }
            if start.elapsed() > limit {
                return false;
            }
            std::thread::sleep(Duration::from_millis(50));
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
        Status { running: RUNNING.load(Ordering::Relaxed), unavailable: None, address: Some("192.168.1.42".into()) }
    }

    pub fn set_running(on: bool) -> Result<()> {
        RUNNING.store(on, Ordering::Relaxed);
        Ok(())
    }
}
