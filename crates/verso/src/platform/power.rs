//! Battery and cable state, from sysfs or powerd. lipc-wait-event emits one
//! event name followed by optional arguments.
//!
//! Whether a cable leads to a computer: the supply's type when it says so
//! (`USB_DCP` is a charger), else `com.lab126.hal usbConfigured` on firmware
//! whose udev sends it (the host configured the USB gadget; the Oasis 3's
//! charger and powerd both say USB on a wall charger), else powerd's `usb`.
//! On MTP firmware (PW6) `usbConfigured` comes from MTP, which needs the
//! `volumd` our launcher stops, so it never arrives while Verso runs.
use super::{Battery, Event};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The kernel's battery and charger files (`/sys/class/power_supply`).
/// Reading them takes well under a millisecond; `lipc-get-prop` starts a
/// process, about 29 ms on the Paperwhite (measured).
#[derive(Debug, PartialEq, Eq)]
pub struct PowerFiles {
    battery: PathBuf,
    cable: Option<PathBuf>,
    /// The USB device controller (`/sys/class/udc/*`), for the log only.
    udc: Option<PathBuf>,
}
impl PowerFiles {
    /// The first supply of type `Battery` with a `capacity`, and the first
    /// `Mains` or `USB` (`USB`, `USB_DCP`, `USB_CDP`) supply with `online` as
    /// the cable. On the Paperwhite these are `bd71827_bat` and `bd71827_ac`;
    /// the `USB_WET` moisture sensor and the `Wireless` supply are not cables.
    pub fn find(root: &Path, udc: &Path) -> Option<Self> {
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
        let udc = std::fs::read_dir(udc).ok().and_then(|mut e| Some(e.next()?.ok()?.path()));
        let files = Self { battery, cable, udc };
        files.battery().is_some().then_some(files)
    }
    pub fn battery(&self) -> Option<Battery> {
        let read = |name| std::fs::read_to_string(self.battery.join(name)).ok();
        let percent: i64 = read("capacity")?.trim().parse().ok()?;
        // "Charging", "Discharging", "Full" or "Not charging".
        let charging = read("status").is_some_and(|s| s.trim() == "Charging");
        Some(Battery { percent: percent.clamp(0, 100) as u8, charging })
    }
    /// What the cable is plugged into, charging or not, as far as the files
    /// tell. `None` without a cable supply.
    pub fn link(&self) -> Option<Link> {
        let cable = self.cable.as_ref()?;
        let online = std::fs::read_to_string(cable.join("online")).ok()?;
        if online.trim() != "1" {
            return Some(Link::Unplugged);
        }
        let kind = std::fs::read_to_string(cable.join("type")).ok();
        Some(match kind.as_deref().map(str::trim) {
            Some("USB_DCP") => Link::Charger,
            Some("USB" | "USB_CDP") => Link::Computer,
            _ => Link::Unsure,
        })
    }
    /// The raw files, for the log. `pluggedin` (the Oasis 3's charger) and the
    /// controller's `state` only where they exist.
    pub fn describe(&self) -> String {
        let read = |dir: &Path, name| {
            std::fs::read_to_string(dir.join(name)).map_or_else(|_| "?".to_owned(), |s| s.trim().to_owned())
        };
        let cable = self.cable.as_ref().map_or_else(
            || "no cable supply".to_owned(),
            |c| {
                let mut s = format!("cable online={} type={}", read(c, "online"), read(c, "type"));
                if c.join("pluggedin").exists() {
                    s += &format!(" pluggedin={}", read(c, "pluggedin"));
                }
                s
            },
        );
        let udc = self.udc.as_ref().map_or_else(String::new, |u| {
            let name = u.file_name().map_or_else(Default::default, |n| n.to_string_lossy());
            format!(", udc {name} state={}", read(u, "state"))
        });
        format!("battery {}% {}, {cable}{udc}", read(&self.battery, "capacity"), read(&self.battery, "status"))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Link {
    Unplugged,
    Charger,
    Computer,
    /// `Mains`: a computer on the Paperwhite 6, whose type changes with what's
    /// plugged in; on the Oasis 3 (`max77796-charger`) maybe anything.
    Unsure,
}

/// When to read again after a cable event. The files lag behind powerd:
/// `charging` arrives about 0.2 s before the battery's status leaves
/// `Discharging` (measured, PW6), so a single read keeps the old icon until
/// the next minute poll. The later reads are for slower models.
const RECHECKS: [Duration; 3] = [Duration::from_millis(500), Duration::from_millis(1500), Duration::from_secs(3)];

/// The computer connection as reported to the app, and the reads still to do.
/// Unplugging is believed at once; plugging in only when a read
/// `RECHECKS[0]` later agrees: while a cable comes out, a wall charger too
/// reads `Mains` and online for 0.1-0.2 s (measured, PW6), which would
/// open the computer prompt.
#[derive(Default)]
pub struct Cable {
    usb: Option<bool>,
    rising: Option<Instant>,
    due: VecDeque<Instant>,
    /// powerd's answer for this plug-in, until the next read that's due. The
    /// minute poll reuses it: asking takes about 29 ms on the UI thread.
    powerd: Option<bool>,
    /// Whether a host configured the gadget, on firmware that says so
    /// (`use_hal`); `None` asks powerd instead.
    configured: Option<bool>,
}
impl Cable {
    /// Schedules the reads that follow a cable event, replacing earlier ones.
    pub fn recheck(&mut self, now: Instant) {
        self.powerd = None;
        self.due = RECHECKS.iter().map(|&d| now + d).collect();
    }
    pub fn next_due(&self) -> Option<Instant> {
        self.due.front().copied()
    }
    /// Whether a read is due; several overdue ones count as one.
    pub fn take_due(&mut self, now: Instant) -> bool {
        let before = self.due.len();
        self.due.retain(|&at| at > now);
        let due = self.due.len() < before;
        if due {
            self.powerd = None;
        }
        due
    }
    /// Decide by `com.lab126.hal` events rather than powerd.
    pub fn use_hal(&mut self) {
        self.configured.get_or_insert(false);
    }
    /// Stop using them: their listener stopped.
    pub fn drop_hal(&mut self) {
        self.configured = None;
    }
    pub fn uses_hal(&self) -> bool {
        self.configured.is_some()
    }
    /// Notes a `com.lab126.hal` event; whether the answer may have changed.
    /// `usbUnconfigured` also comes when the computer suspends the bus, and
    /// then there's no transfer to offer either.
    pub fn hal(&mut self, event: Hal) -> bool {
        let Some(configured) = &mut self.configured else { return false };
        let now = match event {
            Hal::Configured => true,
            Hal::Unconfigured | Hal::PlugOut => false,
            Hal::PlugIn => return false,
        };
        std::mem::replace(configured, now) != now
    }
    /// The Kindle slept: the cable may have changed unnoticed.
    pub fn forget(&mut self) {
        self.powerd = None;
    }
    /// Whether `link` leads to a computer. `ask` is powerd's `usb`, false on
    /// a wall charger (measured, PW6) but true on the Oasis 3's (measured);
    /// without an answer, assume a computer.
    pub fn computer(&mut self, link: Link, ask: impl FnOnce() -> Option<bool>) -> bool {
        match link {
            Link::Unplugged => {
                self.powerd = None;
                if let Some(configured) = &mut self.configured {
                    *configured = false;
                }
                false
            }
            Link::Charger => false,
            Link::Computer => true,
            Link::Unsure => match self.configured {
                Some(configured) => configured,
                None => *self.powerd.get_or_insert_with(|| ask().unwrap_or(true)),
            },
        }
    }
    /// The settled state after reading `usb`.
    pub fn settle(&mut self, usb: bool, now: Instant) -> bool {
        if usb && self.usb == Some(false) {
            match self.rising {
                None => {
                    self.rising = Some(now);
                    self.recheck(now);
                    return false;
                }
                Some(since) if now.saturating_duration_since(since) < RECHECKS[0] => return false,
                Some(_) => {}
            }
        }
        self.usb = Some(usb);
        self.rising = None;
        usb
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

/// A `com.lab126.hal` USB event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hal {
    PlugIn,
    PlugOut,
    Configured,
    Unconfigured,
}
impl Hal {
    pub const EVENTS: &str = "usbPlugIn,usbPlugOut,usbConfigured,usbUnconfigured";
    pub fn name(self) -> &'static str {
        match self {
            Self::PlugIn => "usbPlugIn",
            Self::PlugOut => "usbPlugOut",
            Self::Configured => "usbConfigured",
            Self::Unconfigured => "usbUnconfigured",
        }
    }
    pub fn parse(line: &str) -> Option<Self> {
        match line.split_whitespace().next()? {
            "usbPlugIn" => Some(Self::PlugIn),
            "usbPlugOut" => Some(Self::PlugOut),
            "usbConfigured" => Some(Self::Configured),
            "usbUnconfigured" => Some(Self::Unconfigured),
            _ => None,
        }
    }
}

/// The powerd events the listener subscribes to. The suspend stages
/// (`readyToSuspend` … `resuming`) are only logged: they show when the
/// Kindle hibernated and in what order it woke.
pub const EVENTS: &str = "goingToScreenSaver,outOfScreenSaver,exitingScreenSaver,wakeupFromSuspend,charging,\
    notCharging,battLevelChanged,readyToSuspend,suspending,suspended,suspendGrace,resuming";

/// How long after `outOfScreenSaver` powerd events still go in the log.
const TRACE_AFTER_WAKE: Duration = Duration::from_secs(30);

/// Which powerd events the log shows at info: from going to sleep until
/// `TRACE_AFTER_WAKE` after waking, so a report of a bad wake says what came
/// when. Otherwise `battLevelChanged` alone would fill it.
#[derive(Default)]
pub struct Trace {
    asleep: bool,
    woke: Option<Instant>,
}
impl Trace {
    pub fn shows(&mut self, line: &str, now: Instant) -> bool {
        match line.split_whitespace().next() {
            Some("goingToScreenSaver") => (self.asleep, self.woke) = (true, None),
            Some("outOfScreenSaver") => (self.asleep, self.woke) = (false, Some(now)),
            _ => {}
        }
        self.asleep || self.woke.is_some_and(|at| now.saturating_duration_since(at) < TRACE_AFTER_WAKE)
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
        let udc = root.join("udc");
        let files = PowerFiles::find(&root, &udc).unwrap();
        assert_eq!(
            files,
            PowerFiles { battery: root.join("bd71827_bat"), cable: Some(root.join("bd71827_ac")), udc: None }
        );
        assert_eq!(files.battery(), Some(Battery { percent: 38, charging: false }));
        assert_eq!(files.link(), Some(Link::Unplugged), "the moisture sensor isn't a cable");
        // Plugged in, as measured: Charging, and the Mains supply online.
        std::fs::write(root.join("bd71827_bat/status"), "Charging\n").unwrap();
        std::fs::write(root.join("bd71827_bat/capacity"), "41\n").unwrap();
        std::fs::write(root.join("bd71827_ac/online"), "1\n").unwrap();
        assert_eq!(files.battery(), Some(Battery { percent: 41, charging: true }));
        assert_eq!(files.link(), Some(Link::Unsure));
        assert_eq!(files.describe(), "battery 41% Charging, cable online=1 type=Mains");
        // The Oasis 3's extra charger file and a controller go in the log too.
        std::fs::write(root.join("bd71827_ac/pluggedin"), "1\n").unwrap();
        std::fs::create_dir_all(udc.join("ci_hdrc.0")).unwrap();
        std::fs::write(udc.join("ci_hdrc.0/state"), "0\n").unwrap();
        let files = PowerFiles::find(&root, &udc).unwrap();
        assert_eq!(
            files.describe(),
            "battery 41% Charging, cable online=1 type=Mains pluggedin=1, udc ci_hdrc.0 state=0"
        );
        std::fs::remove_file(root.join("bd71827_ac/pluggedin")).unwrap();
        // Full on a cable: connected but not charging.
        std::fs::write(root.join("bd71827_bat/status"), "Full\n").unwrap();
        assert_eq!(files.battery(), Some(Battery { percent: 41, charging: false }));
        // Wall charger (USB_DCP): found by `find()`, online=1, not a USB data host.
        std::fs::write(root.join("bd71827_ac/type"), "USB_DCP\n").unwrap();
        let files = PowerFiles::find(&root, &udc).unwrap();
        assert_eq!(files.cable, Some(root.join("bd71827_ac")));
        assert_eq!(files.link(), Some(Link::Charger), "wall charger is not a USB data connection");
        // No cable supply: the cable state comes from powerd instead.
        std::fs::remove_dir_all(root.join("bd71827_ac")).unwrap();
        let files = PowerFiles::find(&root, &udc).unwrap();
        assert_eq!(files.link(), None);
        // No battery at all: powerd it is.
        std::fs::remove_dir_all(root.join("bd71827_bat")).unwrap();
        assert_eq!(PowerFiles::find(&root, &udc), None);
        assert_eq!(PowerFiles::find(&root.join("missing"), &udc), None);
        std::fs::remove_dir_all(&root).unwrap();
    }
    #[test]
    fn a_cable_is_read_again_and_a_computer_must_last() {
        let t0 = Instant::now();
        let at = |ms| t0 + Duration::from_millis(ms);
        let mut cable = Cable::default();
        assert!(!cable.settle(false, t0));
        assert_eq!(cable.next_due(), None, "no reads without a cable event");
        // A computer: reported at the first recheck.
        cable.recheck(at(1000));
        assert!(!cable.settle(true, at(1010)));
        assert!(!cable.take_due(at(1400)));
        assert!(cable.take_due(at(1510)));
        assert!(cable.settle(true, at(1510)));
        assert!(cable.take_due(at(9000)), "overdue reads count as one");
        assert!(!cable.take_due(at(9000)));
        assert_eq!(cable.next_due(), None);
        // Unplugged: at once.
        assert!(!cable.settle(false, at(9100)));
        // A charger coming out reads as a computer for a moment; a poll that
        // sees it must not report it, nor count towards a later plug-in.
        assert!(!cable.settle(true, at(20_000)));
        assert!(!cable.settle(false, at(20_100)));
        assert!(!cable.settle(true, at(30_000)));
        // Already on a computer at startup: no wait.
        assert!(Cable::default().settle(true, t0));
    }
    #[test]
    fn powerd_tells_a_charger_from_a_computer_when_the_type_does_not() {
        let t0 = Instant::now();
        let mut cable = Cable::default();
        let silent = || panic!("the type is enough, or powerd has answered");
        // A type that says enough never asks.
        assert!(!cable.computer(Link::Charger, silent));
        assert!(cable.computer(Link::Computer, silent));
        // A wall charger, asked about once until a read is due.
        assert!(!cable.computer(Link::Unsure, || Some(false)));
        assert!(!cable.computer(Link::Unsure, silent));
        cable.recheck(t0);
        assert!(cable.computer(Link::Unsure, || Some(true)), "powerd was late");
        assert!(cable.take_due(t0 + Duration::from_millis(600)));
        assert!(!cable.computer(Link::Unsure, || Some(false)));
        // The next plug-in, and the first look after a sleep, ask again.
        assert!(!cable.computer(Link::Unplugged, silent));
        assert!(cable.computer(Link::Unsure, || Some(true)));
        cable.forget();
        // No answer: the prompt shows rather than missing a computer.
        assert!(cable.computer(Link::Unsure, || None));
    }
    #[test]
    fn hal_tells_a_charger_from_a_computer_where_udev_reports_the_host() {
        let silent = || panic!("hal decides, not powerd");
        let mut cable = Cable::default();
        assert!(!cable.hal(Hal::Configured), "ignored unless the firmware sends it");
        cable.use_hal();
        assert!(cable.uses_hal());
        // The Oasis 3's wall charger: Mains, and nothing configures it.
        assert!(!cable.hal(Hal::PlugIn));
        assert!(!cable.computer(Link::Unsure, silent));
        // A computer configures the gadget a second or two after plug-in.
        assert!(cable.hal(Hal::Configured));
        assert!(cable.computer(Link::Unsure, silent));
        cable.forget();
        assert!(cable.computer(Link::Unsure, silent), "a sleep doesn't unconfigure it");
        assert!(cable.hal(Hal::Unconfigured));
        assert!(!cable.computer(Link::Unsure, silent));
        // Unplugging forgets it even if usbPlugOut is missed.
        cable.hal(Hal::Configured);
        assert!(!cable.computer(Link::Unplugged, silent));
        assert!(!cable.computer(Link::Unsure, silent));
        // Its listener stopped: powerd again.
        cable.drop_hal();
        assert!(!cable.uses_hal());
        assert!(cable.computer(Link::Unsure, || Some(true)));
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
    fn powerd_events_are_shown_from_sleep_until_a_while_after_waking() {
        let t0 = Instant::now();
        let at = |s| t0 + Duration::from_secs(s);
        let mut trace = Trace::default();
        assert!(!trace.shows("battLevelChanged 80", at(0)));
        for line in ["goingToScreenSaver 2", "readyToSuspend 3", "battLevelChanged 79", "wakeupFromSuspend 9"] {
            assert!(trace.shows(line, at(1)), "{line}");
        }
        assert!(trace.shows("outOfScreenSaver 1", at(2)));
        assert!(trace.shows("exitingScreenSaver", at(31)));
        assert!(!trace.shows("battLevelChanged 78", at(32)));
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
        // The listener subscribes to every event it parses.
        let names = ["goingToScreenSaver", "outOfScreenSaver", "exitingScreenSaver", "wakeupFromSuspend"];
        for name in names.into_iter().chain(["charging", "notCharging", "battLevelChanged"]) {
            assert!(parse(name).is_some() && EVENTS.split(',').any(|e| e == name), "{name}");
        }
        assert_eq!(Hal::parse("usbUnconfigured 1"), Some(Hal::Unconfigured));
        // The listener subscribes to every event it parses.
        for hal in [Hal::PlugIn, Hal::PlugOut, Hal::Configured, Hal::Unconfigured] {
            assert_eq!(Hal::parse(hal.name()), Some(hal));
            assert!(Hal::EVENTS.split(',').any(|e| e == hal.name()));
        }
        assert_eq!(Hal::parse("usbConfiguredX"), None);
    }
}
