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
