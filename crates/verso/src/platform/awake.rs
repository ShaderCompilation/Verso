//! Keeping the device from sleeping on its idle timeout, for long jobs.
//!
//! `keep_awake` sets powerd's `preventScreenSaver`, which an update download
//! uses. While it is set, powerd ignores the power button too ("Prevent
//! screen saver set. Ignoring power button", **measured** 2026-11-20), so a
//! deliberate sleep doesn't work. `scripts/kindle-awake.sh` sets the same
//! property for development.
//!
//! `postpone_sleep` instead restarts powerd's idle countdown
//! (`touchScreenSaverTimeout`), as a touch on the Kindle's own UI would.
//! Called more often than the shortest timeout, it keeps the Kindle awake
//! while the power button and a sleep cover still work, and nothing is left
//! to undo if Verso dies. Wi-Fi transfer uses it.
use anyhow::Result;

/// Whether the device is currently kept awake.
pub fn kept_awake() -> Result<bool> {
    imp::get()
}

pub fn keep_awake(on: bool) -> Result<()> {
    imp::set(on)
}

/// Restarts the idle countdown. Fails while the Kindle is asleep (powerd
/// has no such property then) and doesn't wake it.
pub fn postpone_sleep() -> Result<()> {
    imp::postpone()
}

#[cfg(feature = "kindle")]
mod imp {
    use anyhow::{Context, Result, bail, ensure};
    use std::process::Command;

    pub fn get() -> Result<bool> {
        let output = Command::new("lipc-get-prop")
            .args(["-i", "com.lab126.powerd", "preventScreenSaver"])
            .output()
            .context("reading preventScreenSaver")?;
        ensure!(output.status.success(), "couldn't read preventScreenSaver");
        match output.stdout.trim_ascii() {
            b"0" => Ok(false),
            b"1" => Ok(true),
            other => bail!("unexpected preventScreenSaver {:?}", String::from_utf8_lossy(other)),
        }
    }

    pub fn set(on: bool) -> Result<()> {
        let status = Command::new("lipc-set-prop")
            .args(["-i", "com.lab126.powerd", "preventScreenSaver", if on { "1" } else { "0" }])
            .status()
            .context("setting preventScreenSaver")?;
        ensure!(status.success(), "couldn't set preventScreenSaver");
        Ok(())
    }

    pub fn postpone() -> Result<()> {
        let status = Command::new("lipc-set-prop")
            .args(["-i", "com.lab126.powerd", "touchScreenSaverTimeout", "1"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .context("setting touchScreenSaverTimeout")?;
        ensure!(status.success(), "couldn't set touchScreenSaverTimeout");
        Ok(())
    }
}

/// The simulator has no idle sleep; the state is only remembered.
#[cfg(not(feature = "kindle"))]
mod imp {
    use anyhow::Result;
    use std::sync::atomic::{AtomicBool, Ordering};

    static AWAKE: AtomicBool = AtomicBool::new(false);

    pub fn get() -> Result<bool> {
        Ok(AWAKE.load(Ordering::Relaxed))
    }

    pub fn set(on: bool) -> Result<()> {
        log::info!("simulator: keep awake {on}");
        AWAKE.store(on, Ordering::Relaxed);
        Ok(())
    }

    pub fn postpone() -> Result<()> {
        Ok(())
    }
}
