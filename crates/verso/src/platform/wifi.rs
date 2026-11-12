//! On-demand Wi-Fi control. The firmware owns saved networks and reconnecting;
//! Verso only reads and changes whether Wi-Fi is enabled. See docs/device-tools.md.

use anyhow::Result;

pub fn enabled() -> Result<bool> {
    imp::enabled()
}

pub fn set_enabled(on: bool) -> Result<()> {
    imp::set_enabled(on)
}

#[cfg(feature = "kindle")]
mod imp {
    use anyhow::{Context, Result, ensure};
    use std::process::Command;

    pub fn enabled() -> Result<bool> {
        let output = Command::new("lipc-get-prop")
            .args(["-i", "com.lab126.wifid", "enable"])
            .output()
            .context("reading Wi-Fi state")?;
        ensure!(output.status.success(), "couldn't read Wi-Fi state");
        match output.stdout.trim_ascii() {
            b"0" => Ok(false),
            b"1" => Ok(true),
            _ => anyhow::bail!("unexpected Wi-Fi state"),
        }
    }

    pub fn set_enabled(on: bool) -> Result<()> {
        // Follow KOReader's kindleEnableWifi ordering: allow wireless before
        // enabling Wi-Fi, and disable Wi-Fi before disabling wireless.
        let properties = [("com.lab126.cmd", "wirelessEnable"), ("com.lab126.wifid", "enable")];
        let properties = if on { properties } else { [properties[1], properties[0]] };
        let mut result = Ok(());
        for (service, property) in properties {
            // Attempt both even if one fails. The caller reads the actual
            // state again, since firmware may have accepted only one change.
            let attempt = (|| {
                let output = Command::new("lipc-set-prop")
                    .args(["-i", service, property, if on { "1" } else { "0" }])
                    .output()
                    .with_context(|| format!("setting {service} {property}"))?;
                ensure!(output.status.success(), "couldn't set {service} {property}");
                Ok(())
            })();
            if result.is_ok() {
                result = attempt;
            }
        }
        result
    }
}

/// Simulated state lasts for the process, including reopening Settings.
#[cfg(not(feature = "kindle"))]
mod imp {
    use anyhow::Result;
    use std::sync::atomic::{AtomicBool, Ordering};

    static ENABLED: AtomicBool = AtomicBool::new(true);

    pub fn enabled() -> Result<bool> {
        Ok(ENABLED.load(Ordering::Relaxed))
    }

    pub fn set_enabled(on: bool) -> Result<()> {
        ENABLED.store(on, Ordering::Relaxed);
        Ok(())
    }
}
