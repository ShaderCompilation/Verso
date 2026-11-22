//! Battery and cable state, from sysfs or powerd. lipc-wait-event emits one
//! event name followed by optional arguments.
use super::{Battery, Event};
use std::path::{Path, PathBuf};

/// The kernel's battery and charger files (`/sys/class/power_supply`).
/// Reading them takes well under a millisecond; `lipc-get-prop` starts a
/// process, about 29 ms on the Paperwhite (measured).
#[derive(Debug, PartialEq, Eq)]
pub struct PowerFiles {
    battery: PathBuf,
    cable: Option<PathBuf>,
}
impl PowerFiles {
    /// The first supply of type `Battery` with a `capacity`, and the first
    /// `Mains` or `USB` (`USB`, `USB_DCP`, `USB_CDP`) supply with `online` as
    /// the cable. On the Paperwhite these are `bd71827_bat` and `bd71827_ac`;
    /// the `USB_WET` moisture sensor and the `Wireless` supply are not cables.
    pub fn find(root: &Path) -> Option<Self> {
        let mut entries: Vec<_> = std::fs::read_dir(root).ok()?.filter_map(|e| Some(e.ok()?.path())).collect();
        entries.sort();
        let kind = |dir: &Path| std::fs::read_to_string(dir.join("type")).map(|t| t.trim().to_owned()).ok();
        let battery =
            entries.iter().find(|d| kind(d).as_deref() == Some("Battery") && d.join("capacity").exists())?.clone();
        let cable = entries
            .iter()
            .find(|d| {
                matches!(kind(d).as_deref(), Some("Mains" | "USB" | "USB_DCP" | "USB_CDP")) && d.join("online").exists()
            })
            .cloned();
        let files = Self { battery, cable };
        files.battery().is_some().then_some(files)
    }
    pub fn battery(&self) -> Option<Battery> {
        let read = |name| std::fs::read_to_string(self.battery.join(name)).ok();
        let percent: i64 = read("capacity")?.trim().parse().ok()?;
        // "Charging", "Discharging", "Full" or "Not charging".
        let charging = read("status").is_some_and(|s| s.trim() == "Charging");
        Some(Battery { percent: percent.clamp(0, 100) as u8, charging })
    }
    /// A USB data cable is attached, charging or not. A wall charger
    /// (`USB_DCP`) sets `online = 1` while powerd reports `usb = false`.
    pub fn usb(&self) -> Option<bool> {
        let cable = self.cable.as_ref()?;
        let online = std::fs::read_to_string(cable.join("online")).ok()?;
        if online.trim() != "1" {
            return Some(false);
        }
        let kind = std::fs::read_to_string(cable.join("type")).ok();
        Some(kind.as_deref().map(str::trim) != Some("USB_DCP"))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Status {
    pub battery: Option<Battery>,
    pub usb: Option<bool>,
}
impl Status {
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
        let primary = value
            .get("battInfo")
            .and_then(|v| v.as_array())
            .and_then(|batteries| batteries.iter().find(|b| b.get("type").and_then(|v| v.as_str()) == Some("PRIMARY")));
        let battery = primary.and_then(|primary| {
            Some(Battery {
                percent: value.get("aggr_batt").or_else(|| primary.get("cap"))?.as_i64()?.clamp(0, 100) as u8,
                charging: primary.get("charging")?.as_bool()?,
            })
        });
        Some(Self { battery, usb: value.get("usb").and_then(|v| v.as_bool()) })
    }
}

pub fn parse(line: &str) -> Option<Event> {
    match line.split_whitespace().next()? {
        "goingToScreenSaver" => Some(Event::Sleep),
        "outOfScreenSaver" => Some(Event::Wake),
        "exitingScreenSaver" | "wakeupFromSuspend" => Some(Event::WakeRepaint),
        "charging" => Some(Event::Usb(true)),
        "notCharging" => Some(Event::Usb(false)),
        "battLevelChanged" => Some(Event::PowerChanged),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Paperwhite's supplies, as measured unplugged.
    fn paperwhite(root: &Path) {
        let supply = |name: &str, files: &[(&str, &str)]| {
            let dir = root.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            for (file, value) in files {
                std::fs::write(dir.join(file), format!("{value}\n")).unwrap();
            }
        };
        supply("Wireless", &[("type", "Wireless"), ("online", "0")]);
        supply("bd71827_ac", &[("type", "Mains"), ("online", "0")]);
        supply("bd71827_bat", &[("type", "Battery"), ("capacity", "38"), ("status", "Discharging"), ("online", "1")]);
        supply("max20342_moisture", &[("type", "USB_WET"), ("online", "1")]);
    }

    #[test]
    fn sysfs_gives_level_charging_and_cable() {
        let root = std::env::temp_dir().join(format!("verso-power-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        paperwhite(&root);
        let files = PowerFiles::find(&root).unwrap();
        assert_eq!(files, PowerFiles { battery: root.join("bd71827_bat"), cable: Some(root.join("bd71827_ac")) });
        assert_eq!(files.battery(), Some(Battery { percent: 38, charging: false }));
        assert_eq!(files.usb(), Some(false), "the moisture sensor isn't a cable");
        // Plugged in, as measured: Charging, and the Mains supply online.
        std::fs::write(root.join("bd71827_bat/status"), "Charging\n").unwrap();
        std::fs::write(root.join("bd71827_bat/capacity"), "41\n").unwrap();
        std::fs::write(root.join("bd71827_ac/online"), "1\n").unwrap();
        assert_eq!(files.battery(), Some(Battery { percent: 41, charging: true }));
        assert_eq!(files.usb(), Some(true));
        // Full on a cable: connected but not charging.
        std::fs::write(root.join("bd71827_bat/status"), "Full\n").unwrap();
        assert_eq!(files.battery(), Some(Battery { percent: 41, charging: false }));
        // Wall charger (USB_DCP): found by `find()`, online=1, not a USB data host.
        std::fs::write(root.join("bd71827_ac/type"), "USB_DCP\n").unwrap();
        let files = PowerFiles::find(&root).unwrap();
        assert_eq!(files.cable, Some(root.join("bd71827_ac")));
        assert_eq!(files.usb(), Some(false), "wall charger is not a USB data connection");
        // No cable supply: the cable state comes from powerd instead.
        std::fs::remove_dir_all(root.join("bd71827_ac")).unwrap();
        let files = PowerFiles::find(&root).unwrap();
        assert_eq!(files.usb(), None);
        // No battery at all: powerd it is.
        std::fs::remove_dir_all(root.join("bd71827_bat")).unwrap();
        assert_eq!(PowerFiles::find(&root), None);
        assert_eq!(PowerFiles::find(&root.join("missing")), None);
        std::fs::remove_dir_all(&root).unwrap();
    }
    #[test]
    fn a_full_usb_battery_is_connected_even_when_it_is_not_charging() {
        let full = br#"{"battInfo":[{"type":"PRIMARY","cap":100,"charging":false}],"aggr_batt":100,"usb":true}"#;
        assert_eq!(
            Status::parse(full),
            Some(Status { battery: Some(Battery { percent: 100, charging: false }), usb: Some(true) })
        );
        let wireless = br#"{"battInfo":[{"type":"PRIMARY","cap":42,"charging":true}],"usb":false}"#;
        assert_eq!(
            Status::parse(wireless),
            Some(Status { battery: Some(Battery { percent: 42, charging: true }), usb: Some(false) })
        );
        assert_eq!(Status::parse(b"{}"), Some(Status { battery: None, usb: None }));
        assert_eq!(Status::parse(b"broken"), None);
    }
    #[test]
    fn power_events_match_exact_names_and_ignore_arguments() {
        assert_eq!(parse("goingToScreenSaver 4"), Some(Event::Sleep));
        assert_eq!(parse("outOfScreenSaver 6"), Some(Event::Wake));
        assert_eq!(parse("exitingScreenSaver"), Some(Event::WakeRepaint));
        assert_eq!(parse("wakeupFromSuspend 1234"), Some(Event::WakeRepaint));
        assert_eq!(parse("charging 1"), Some(Event::Usb(true)));
        assert_eq!(parse("notCharging"), Some(Event::Usb(false)));
        assert_eq!(parse("battLevelChanged 87"), Some(Event::PowerChanged));
        for unknown in ["", "readyToSuspend 3", "unknown goingToScreenSaver"] {
            assert_eq!(parse(unknown), None);
        }
    }
}
