//! Lets computers on the Wi-Fi find the Kindle as `verso.local` while the
//! SSH server runs or Wi-Fi transfer is open, through the bundled responder
//! (crates/verso-mdns, installed as verso/bin/verso-mdns). The responder
//! watches the process it serves and exits when it does: the SSH server
//! (whoever stops it, Verso or KOReader), or Verso itself for Wi-Fi
//! transfer, so it can't outlive a crash. See docs/device-tools.md and
//! docs/web-app.md.

use std::fs::File;
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
    let log = File::create(LOG).map_or_else(|_| Stdio::null(), Stdio::from);
    let mut command = Command::new(BINARY);
    command
        .args(["--watch", &pid.to_string(), "--watch-name", name, "--firewall", INTERFACE, label])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log);
    match spawn_detached(&mut command) {
        Ok(true) => log::info!("mDNS: advertising {label}.local"),
        Ok(false) => log::warn!("mDNS: couldn't start the responder"),
        Err(e) => log::warn!("mDNS: couldn't start the responder: {e}"),
    }
}

/// For Wi-Fi transfer: starts a responder that lasts as long as Verso,
/// unless one runs already (the SSH server's). The PID of the one it
/// started, once it's running.
pub fn start_for_verso(label: &str) -> Option<i32> {
    if verso_mdns::running_pid().is_some() {
        return None;
    }
    ensure(label, std::process::id() as i32, "verso");
    wait_until(PIDFILE_GRACE, || verso_mdns::running_pid().is_some());
    verso_mdns::running_pid()
}

/// Stops the responder `start_for_verso` started, if it still runs.
pub fn stop(pid: i32) {
    if verso_mdns::running_pid() == Some(pid) {
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
