//! Keeping the device from sleeping on its idle timeout, for long jobs such
//! as an update download. On the Kindle this is powerd's
//! `preventScreenSaver`: it stops only the idle timeout, so the power button
//! and a sleep cover still put the Kindle to sleep. `scripts/kindle-awake.sh`
//! sets the same property for development.
use anyhow::Result;

/// Whether the device is currently kept awake.
pub fn kept_awake() -> Result<bool> {
    imp::get()
}

pub fn keep_awake(on: bool) -> Result<()> {
    imp::set(on)
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
}
