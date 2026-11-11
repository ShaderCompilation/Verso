//! lipc-wait-event emits one event name followed by optional arguments.
use super::{Battery, Event};

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
