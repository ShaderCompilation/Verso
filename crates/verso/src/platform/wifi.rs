//! On-demand Wi-Fi control. The firmware owns saved networks and reconnecting;
//! Verso only reads and changes whether Wi-Fi is enabled. See docs/device-tools.md.

use anyhow::Result;

pub fn enabled() -> Result<bool> {
    imp::enabled()
}

pub fn set_enabled(on: bool) -> Result<()> {
    imp::set_enabled(on)
}

/// Whether Wi-Fi can be used right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Link {
    Off,
    /// Enabled, but there's no route to the internet yet (still joining a
    /// network, or none in range).
    Connecting,
    Connected,
}

/// Reads the link without changing anything. One `lipc` call and a route
/// lookup on the Kindle, so call it off the UI thread.
pub fn link() -> Result<Link> {
    if !enabled()? {
        return Ok(Link::Off);
    }
    Ok(if imp::has_route() { Link::Connected } else { Link::Connecting })
}

#[cfg(feature = "kindle")]
mod imp {
    use anyhow::{Context, Result, ensure};
    use std::process::Command;
    use std::time::{Duration, Instant};

    /// How long to wait for wifid to report a change it has accepted.
    const SETTLE_TIMEOUT: Duration = Duration::from_secs(2);
    const SETTLE_POLL: Duration = Duration::from_millis(100);

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

    /// Connecting a UDP socket sends nothing; it only asks the kernel for a
    /// route, which exists once Wi-Fi has joined a network.
    pub fn has_route() -> bool {
        let route = || -> Option<()> {
            let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
            socket.connect("192.0.2.1:9").ok()?; // TEST-NET-1: any routable address works.
            let ip = socket.local_addr().ok()?.ip();
            (!ip.is_unspecified() && !ip.is_loopback()).then_some(())
        };
        route().is_some()
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
        if result.is_ok() {
            // wifid applies the change asynchronously: the first read right
            // after turning off can still return 1 (measured). Wait until it
            // reports the requested state so the caller's read is current.
            let deadline = Instant::now() + SETTLE_TIMEOUT;
            while enabled().ok() != Some(on) && Instant::now() < deadline {
                std::thread::sleep(SETTLE_POLL);
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

    /// The simulator's computer is assumed to be online.
    pub fn has_route() -> bool {
        true
    }
}
