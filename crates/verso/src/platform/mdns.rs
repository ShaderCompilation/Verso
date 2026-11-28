//! Lets computers on the Wi-Fi find the Kindle as `verso.local` while the
//! SSH server runs or Wi-Fi transfer is open, through the bundled responder
//! (crates/verso-mdns, installed as verso/bin/verso-mdns). The responder
//! watches the process it serves and exits when it does: the SSH server
//! (whoever stops it, Verso or KOReader), or Verso itself for Wi-Fi
//! transfer, so it can't outlive a crash.

use std::fs::{File, OpenOptions};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::process::{alive, runs_replaced, spawn_detached, wait_until};

const BINARY: &str = "/mnt/us/verso/bin/verso-mdns";
/// A few lines per run: start, address, the name won, why it stopped.
const LOG: &str = "/tmp/verso-mdns.log";
const INTERFACE: &str = "wlan0";

/// Starts the responder for the process `pid` called `name` (the SSH
/// server), unless it's already running the current build.
pub fn ensure(label: &str, pid: i32, name: &str) {
    if let Some(pid) = verso_mdns::running_pid() {
        if !runs_replaced(pid, BINARY) {
            return;
        }
        // deploy.sh renames the new binary into place, and an update moves
        // the old one aside; the old process keeps running the old code
        // until restarted.
        log::info!("mDNS: restarting the responder to run the updated binary");
        terminate(pid);
    }
    if !Path::new(BINARY).exists() {
        log::warn!("mDNS: {BINARY} is missing; not advertising a name");
        return;
    }
    let label = if verso_mdns::valid_label(label) { label } else { verso_mdns::DEFAULT_LABEL };
    let log = open_log().map_or_else(Stdio::null, Stdio::from);
    let mut command = Command::new(BINARY);
    command
        .args(["--watch", &pid.to_string(), "--watch-name", name, "--firewall", INTERFACE, label])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log);
    match spawn_detached(&mut command) {
        Ok(true) => log::info!("mDNS: advertising the device name"),
        Ok(false) => log::warn!("mDNS: couldn't start the responder"),
        Err(e) => log::warn!("mDNS: couldn't start the responder: {e}"),
    }
}

/// Opens the log for appending, so a respawn doesn't erase why the last
/// run stopped; starts over once it grows past a few KiB.
fn open_log() -> Option<File> {
    const MAX: u64 = 16 * 1024;
    let mut options = OpenOptions::new();
    options.create(true);
    if std::fs::metadata(LOG).is_ok_and(|m| m.len() > MAX) {
        options.write(true).truncate(true);
    } else {
        options.append(true);
    }
    options.open(LOG).ok()
}

/// Whether the responder `pid` was started to watch the process `watched`
/// (its `--watch` argument), so Verso only claims and stops its own.
fn watches(pid: i32, watched: i32) -> bool {
    let Ok(cmdline) = std::fs::read(format!("/proc/{pid}/cmdline")) else { return false };
    let watched = watched.to_string();
    let mut args = cmdline.split(|b| *b == 0);
    args.any(|a| a == b"--watch") && args.next().is_some_and(|a| a == watched.as_bytes())
}

/// For Wi-Fi transfer: starts a responder that lasts as long as Verso,
/// unless one runs already (the SSH server's). The PID of the one it
/// started, once it's running; `None` if another spawner's responder won
/// the pidfile instead.
pub fn start_for_verso(label: &str) -> Option<i32> {
    if verso_mdns::running_pid().is_some() {
        return None;
    }
    ensure(label, std::process::id() as i32, "verso");
    if !wait_until(PIDFILE_GRACE, || verso_mdns::running_pid().is_some()) {
        log::warn!("mDNS: the responder didn't start; is /tmp full? (see {LOG})");
        return None;
    }
    verso_mdns::running_pid().filter(|&pid| watches(pid, std::process::id() as i32))
}

/// Whether the responder `start_for_verso` started is still running.
pub fn running(pid: i32) -> bool {
    verso_mdns::running_pid() == Some(pid) && watches(pid, std::process::id() as i32)
}

/// Stops the responder `start_for_verso` started, if it still runs.
pub fn stop(pid: i32) {
    if running(pid) {
        terminate(pid);
    }
}

/// The name the responder won, without `.local`, once it has one.
pub fn hostname() -> Option<String> {
    verso_mdns::won_label()
}

/// After the SSH server stops, waits for the responder to follow (it
/// notices at once), so its goodbye goes out before Wi-Fi turns off.
pub fn wait_stopped() {
    if let Some(pid) = verso_mdns::running_pid()
        && !wait_until(Duration::from_secs(1), || !alive(pid))
    {
        terminate(pid);
    }
}

/// Waits up to `limit` for the responder to win a name, so Settings can
/// show it right after starting SSH. Stops early if the responder exits.
pub fn wait_for_name(limit: Duration) {
    let start = Instant::now();
    // A freshly spawned responder writes its pidfile only after it has
    // started, so until then "no pidfile" doesn't mean it has exited.
    if !wait_until(PIDFILE_GRACE.min(limit), || verso_mdns::running_pid().is_some()) {
        return;
    }
    let rest = limit.saturating_sub(start.elapsed());
    wait_until(rest, || hostname().is_some() || verso_mdns::running_pid().is_none());
}

/// How long a just-started responder gets to write its pidfile.
const PIDFILE_GRACE: Duration = Duration::from_millis(500);

fn terminate(pid: i32) {
    // SAFETY: plain kill(2) on a PID just checked to be the responder.
    unsafe { libc::kill(pid, libc::SIGTERM) };
    if !wait_until(Duration::from_secs(1), || !alive(pid)) {
        log::warn!("mDNS: the responder didn't stop");
    }
}
