//! Helpers for the background servers Verso starts and leaves running: the
//! SSH server and the mDNS responder next to it.

use std::os::unix::process::CommandExt;
use std::process::Command;
use std::time::{Duration, Instant};

/// Runs `command` detached like a daemon: init adopts it (no zombie left in
/// Verso when it stops) and it has its own session, so it outlives Verso.
/// Returns once the intermediate process has exited.
pub fn spawn_detached(command: &mut Command) -> std::io::Result<bool> {
    // Fork once more and let the middle process exit.
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
    Ok(command.status()?.success())
}

/// Polls `done` every 50 ms until it holds or `limit` passes.
pub fn wait_until(limit: Duration, mut done: impl FnMut() -> bool) -> bool {
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

/// Whether `pid` is still alive.
pub fn alive(pid: i32) -> bool {
    std::path::Path::new(&format!("/proc/{pid}")).exists()
}

/// Where an update moves replaced files (`update/previous/`).
const UPDATE_DIR: &str = "/mnt/us/verso/update/";

/// Whether `pid` runs an older copy of `binary`: the file was replaced
/// since it started (deploy.sh renames a new one over it), or an update
/// moved it aside. Then it should be restarted on the new binary.
pub fn runs_replaced(pid: i32, binary: &str) -> bool {
    std::fs::read_link(format!("/proc/{pid}/exe")).is_ok_and(|exe| replaced_exe(&exe.to_string_lossy(), binary))
}

/// [`runs_replaced`] for the process's `/proc/<pid>/exe` link. Another
/// program of the same name (say, KOReader's dropbear) is never "ours".
fn replaced_exe(exe: &str, binary: &str) -> bool {
    match exe.strip_suffix(" (deleted)") {
        Some(path) => path == binary || path.starts_with(UPDATE_DIR),
        None => exe != binary && exe.starts_with(UPDATE_DIR),
    }
}

#[cfg(test)]
mod tests {
    use super::replaced_exe;

    #[test]
    fn tells_replaced_binaries() {
        let ours = "/mnt/us/verso/bin/dropbear";
        assert!(!replaced_exe(ours, ours), "current");
        assert!(replaced_exe("/mnt/us/verso/bin/dropbear (deleted)", ours), "renamed over");
        assert!(replaced_exe("/mnt/us/verso/update/previous/files/verso/bin/dropbear", ours), "moved aside");
        assert!(replaced_exe("/mnt/us/verso/update/previous/files/verso/bin/dropbear (deleted)", ours));
        assert!(!replaced_exe("/mnt/us/koreader/dropbear", ours), "KOReader's");
        assert!(!replaced_exe("/mnt/us/koreader/dropbear (deleted)", ours), "KOReader's, updated");
    }
}
