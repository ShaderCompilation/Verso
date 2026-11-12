//! Developer-only SSH server switch (Settings → Developer).
//!
//! Verso ships its own server: upstream Dropbear, plus OpenSSH's sftp-server
//! for scp and sftp. scripts/build-ssh.sh builds them and scripts/deploy.sh
//! installs them in /mnt/us/verso/bin. Keys live in /mnt/us/verso/ssh. It uses
//! KOReader's port and pidfile, so either app sees and can stop a server the
//! other one started. The server keeps running after Verso exits.
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
    use std::fs;
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    use std::os::unix::process::CommandExt;
    use std::path::Path;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

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
        Status { running: running_pid().is_some(), unavailable: unavailable(), address: local_address() }
    }

    pub fn set_running(on: bool) -> Result<()> {
        if on { start() } else { stop() }
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

    fn start() -> Result<()> {
        if running_pid().is_some() {
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
        // Detach like a daemon: fork once more and let the middle process
        // exit, so init adopts the server (no zombie left in Verso when it
        // stops), and give it its own session, so it outlives Verso.
        // SAFETY: fork, _exit and setsid are async-signal-safe, as required
        // between fork and exec.
        unsafe {
            command.pre_exec(|| {
                match libc::fork() {
                    -1 => return Err(std::io::Error::last_os_error()),
                    0 => {}
                    _ => libc::_exit(0),
                }
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let ok = command.status().context("starting dropbear")?.success();
        // The server writes the pidfile once it's listening.
        if ok && wait_until(Duration::from_secs(2), || running_pid().is_some()) {
            log::info!("SSH server started on port {PORT}");
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

    fn stop() -> Result<()> {
        let Some(pid) = running_pid() else {
            let _ = fs::remove_file(PIDFILE);
            let _ = fs::remove_dir_all(RUN_DIR);
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
        let _ = fs::remove_file(PIDFILE);
        let _ = fs::remove_dir_all(RUN_DIR);
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
        Status {
            running: RUNNING.load(Ordering::Relaxed),
            unavailable: None,
            address: super::super::wifi::enabled().unwrap_or(false).then(|| "192.168.1.42".into()),
        }
    }

    pub fn set_running(on: bool) -> Result<()> {
        RUNNING.store(on, Ordering::Relaxed);
        Ok(())
    }
}
