//! Page-turner remotes, everything but the device files (`kindle/input.rs`):
//! which input devices count, what their keys mean, and the local port.
//!
//! Verso doesn't do Bluetooth. A press reaches it one of three ways:
//!
//! - **Virtual keyboards**, matched by name: `kindle-input-pt` (Amazon's
//!   `input_manager`, for the Kindle Click) and `kindle-button-mapper`.
//! - **The local port**: kindle-button-mapper's *Auto* actions first ask
//!   KOReader's HID Passthrough plugin on 127.0.0.1:8323 to turn the page
//!   (`GET /koreader/event/GotoViewRel/1`). Verso answers there too, so a
//!   binding made for KOReader works here.
//! - **The remote's own device** (USB or Bluetooth), unless the mapper has
//!   bindings for it: then the press already arrives as one of the above, or
//!   as a tap the mapper writes to the touchscreen.
//!
//! [`Gate`] is the second guard against one press turning two pages.
//! Key codes are never logged, only page keys and counts: a remote can be a
//! whole keyboard.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::time::{Duration, Instant};

use super::KeyAction;

/// Where the HID Passthrough plugin listens. KOReader's HTTP Inspector
/// (8080), the mapper's second choice, is left alone.
pub const PORT: u16 = 8323;
pub const MAPPER: &str = "kindle-button-mapper";
pub const MAPPER_BINARY: &str = "/mnt/us/kindle-button-mapper/kindle-button-mapper";
pub const MAPPER_CONFIG: &str = "/mnt/us/kindle-button-mapper/config.ini";
const CLICK: &str = "kindle-input-pt";

const BUS_USB: u16 = 0x03;
const BUS_BLUETOOTH: u16 = 0x05;
const KEY_PAGEUP: u16 = 104;
const KEY_PAGEDOWN: u16 = 109;
const KEY_VOLUMEDOWN: u16 = 114;
const KEY_VOLUMEUP: u16 = 115;
/// The page keys FBInk's input scan looks for on a Kindle: PAGEUP, PAGEDOWN,
/// YEN and F23 (see `KeyAction::from_evdev`).
const BUTTON_KEYS: [u16; 4] = [KEY_PAGEUP, KEY_PAGEDOWN, 124, 193];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The Kindle's own page-turn buttons; they follow the orientation.
    Buttons,
    /// Amazon's or the mapper's virtual keyboard.
    Virtual,
    /// A USB or Bluetooth device.
    Remote,
}

/// What an input device is to us. `has`: its key capabilities. Both virtual
/// keyboards claim nearly every key, so names and buses decide before keys.
pub fn classify(name: &str, bus: u16, has: impl Fn(u16) -> bool, probe_buttons: bool) -> Option<Kind> {
    if name == CLICK || name == MAPPER {
        return Some(Kind::Virtual);
    }
    // The mapper's virtual mouse, and headphones' media keys (as BlueZ names
    // them): changing the volume mustn't turn pages.
    if name.starts_with(MAPPER) || name.ends_with("(AVRCP)") {
        return None;
    }
    if matches!(bus, BUS_USB | BUS_BLUETOOTH) {
        return (0..0x300).any(|code| has(code) && page_key(code, true).is_some()).then_some(Kind::Remote);
    }
    (probe_buttons && BUTTON_KEYS.into_iter().any(has)).then_some(Kind::Buttons)
}

/// A remote key's name and meaning. Forward is forward however the Kindle is
/// held. `volume_down`: whether the device has that key. A camera shutter
/// without it has only Volume Up to turn pages with, so there it's forward.
pub fn page_key(code: u16, volume_down: bool) -> Option<(&'static str, KeyAction)> {
    use KeyAction::{PageNext, PagePrev};
    Some(match code {
        KEY_PAGEUP => ("PageUp", PagePrev),
        KEY_PAGEDOWN => ("PageDown", PageNext),
        103 => ("Up", PagePrev),
        105 => ("Left", PagePrev),
        106 => ("Right", PageNext),
        // Also the mapper's fallback page keys, with Up.
        108 => ("Down", PageNext),
        28 => ("Enter", PageNext),
        57 => ("Space", PageNext),
        KEY_VOLUMEUP if volume_down => ("VolumeUp", PagePrev),
        KEY_VOLUMEUP => ("VolumeUp", PageNext),
        KEY_VOLUMEDOWN => ("VolumeDown", PageNext),
        163 => ("NextSong", PageNext),
        165 => ("PreviousSong", PagePrev),
        304 => ("ButtonA", PageNext),
        305 => ("ButtonB", PagePrev),
        310 => ("ShoulderLeft", PagePrev),
        311 => ("ShoulderRight", PageNext),
        544 => ("PadUp", PagePrev),
        545 => ("PadDown", PageNext),
        546 => ("PadLeft", PagePrev),
        547 => ("PadRight", PageNext),
        _ => return None,
    })
}

/// Whether kindle-button-mapper's `config.ini` binds anything on this device.
/// Pairing adds an empty block for every device, which binds nothing. Devices
/// match as the mapper matches them: by Bluetooth address when the block has
/// one, else by name.
pub fn mapper_binds(config: &str, name: &str, uniq: &str) -> bool {
    #[derive(Default)]
    struct Block {
        name: String,
        uniq: String,
        bound: bool,
    }
    let address = |s: &str| s.split('/').next().unwrap_or("").trim().to_ascii_uppercase();
    let mut blocks: std::collections::HashMap<String, Block> = Default::default();
    // The device the current section belongs to, and whether it's a section
    // of bindings (`[device.ID.buttons]`) rather than the device's own.
    let mut section: Option<(String, bool)> = None;
    for line in config.lines() {
        let line = line.split(['#', ';']).next().unwrap_or("").trim();
        if let Some(header) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            let header = header.trim().to_ascii_lowercase();
            section = header.strip_prefix("device.").map(|rest| match rest.split_once('.') {
                Some((id, _)) => (id.to_owned(), true),
                None => (rest.to_owned(), false),
            });
            continue;
        }
        let (Some((id, bindings)), Some((key, value))) = (&section, line.split_once('=')) else { continue };
        let (key, value) = (key.trim(), value.trim());
        let block = blocks.entry(id.clone()).or_default();
        match key {
            _ if *bindings => block.bound |= !value.is_empty(),
            "name" => block.name = value.to_owned(),
            "uniq" => block.uniq = address(value),
            _ => {}
        }
    }
    blocks.values().any(|b| b.bound && if b.uniq.is_empty() { b.name == name } else { b.uniq == address(uniq) })
}

/// Drops the second report of one press: the same remote can show up as two
/// devices, or send two keys at once (some camera shutters send Enter and
/// Volume Up). Nobody presses two different buttons this fast on purpose;
/// the same button again always counts.
#[derive(Default)]
pub struct Gate {
    last: Option<((u32, u16), Instant)>,
}

impl Gate {
    const ECHO: Duration = Duration::from_millis(150);

    /// `button`: the source and key pressed.
    pub fn accept(&mut self, button: (u32, u16), now: Instant) -> bool {
        if let Some((last, at)) = self.last
            && last != button
            && now.saturating_duration_since(at) < Self::ECHO
        {
            return false;
        }
        self.last = Some((button, now));
        true
    }
}

pub fn listen() -> std::io::Result<TcpListener> {
    TcpListener::bind((Ipv4Addr::LOCALHOST, PORT))
}

/// Answers page turns until the process ends.
pub fn serve(listener: &TcpListener, mut on_page: impl FnMut(KeyAction)) {
    for stream in listener.incoming() {
        let answered = stream.and_then(|s| answer(s, &mut on_page));
        if let Err(e) = answered {
            log::debug!("remote port: {e}");
            std::thread::sleep(Duration::from_millis(50)); // e.g. out of file descriptors
        }
    }
}

/// Anything but a page turn is dropped without a reply: `curl` then fails and
/// the mapper's scripts move on to the Kindle's own way (brightness through
/// powerd). Any reply, even an error, would count as done.
fn answer(mut stream: TcpStream, on_page: &mut impl FnMut(KeyAction)) -> std::io::Result<()> {
    const PATIENCE: Duration = Duration::from_millis(300);
    stream.set_read_timeout(Some(PATIENCE))?;
    stream.set_write_timeout(Some(PATIENCE))?;
    let started = Instant::now();
    let (mut head, mut buf) = (Vec::new(), [0u8; 512]);
    // To the end of the headers: closing with unread bytes resets the
    // connection and can lose the reply.
    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = stream.read(&mut buf)?;
        if n == 0 || head.len() > 4096 || started.elapsed() > PATIENCE {
            return Ok(());
        }
        head.extend_from_slice(&buf[..n]);
    }
    if let Some(action) = page_request(&String::from_utf8_lossy(&head)) {
        on_page(action);
        stream.write_all(b"HTTP/1.0 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nOK")?;
    }
    Ok(())
}

fn page_request(head: &str) -> Option<KeyAction> {
    let path = head.lines().next()?.strip_prefix("GET ")?.split_whitespace().next()?;
    let step: i32 = path.strip_prefix("/koreader/event/GotoViewRel/")?.parse().ok()?;
    match step.signum() {
        1 => Some(KeyAction::PageNext),
        -1 => Some(KeyAction::PagePrev),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::KeyAction::{PageNext, PagePrev};
    use super::*;

    #[test]
    fn devices_are_told_apart_by_name_and_bus_before_keys() {
        let all = |_| true;
        let none = |_| false;
        let only = |key| move |code| code == key;
        for probe in [false, true] {
            // Claiming every key, power included, never makes these the buttons.
            assert_eq!(classify("kindle-input-pt", 0x06, all, probe), Some(Kind::Virtual));
            assert_eq!(classify("kindle-button-mapper", 0x03, all, probe), Some(Kind::Virtual));
            assert_eq!(classify("kindle-button-mapper-pointer", 0x03, all, probe), None);
            assert_eq!(classify("AB Shutter3", 0x05, only(115), probe), Some(Kind::Remote));
            assert_eq!(classify("Headphones (AVRCP)", 0x05, only(115), probe), None);
            assert_eq!(classify("A mouse", 0x05, only(0x110), probe), None);
            assert_eq!(classify("bd71828-pwrkey", 0x19, only(116), probe), None);
            assert_eq!(classify("pt_mt", 0, none, probe), None);
        }
        assert_eq!(classify("gpiokey", 0x19, only(109), true), Some(Kind::Buttons));
        assert_eq!(classify("gpiokey", 0x19, only(109), false), None, "not on touch-only models");
    }

    #[test]
    fn volume_up_is_forward_only_without_volume_down() {
        let action = |code, both| page_key(code, both).map(|(_, action)| action);
        assert_eq!((action(115, true), action(114, true)), (Some(PagePrev), Some(PageNext)));
        assert_eq!(action(115, false), Some(PageNext));
        // Power, Home, Escape and letters aren't page keys.
        for code in [116, 102, 1, 16, 30] {
            assert_eq!(action(code, true), None, "code {code}");
        }
    }

    #[test]
    fn the_mapper_keeps_the_devices_it_has_bindings_for() {
        let config = "
[settings]
debounce_ms = 0
[device.shutter]
name = AB Shutter3
uniq = aa:bb:cc:dd:ee:ff
[device.shutter.buttons]
115 = /mnt/us/kindle-button-mapper/scripts/auto.sh next_page
[device.paired_only]
name = Keyboard
uniq = 11:22:33:44:55:66
[device.paired_only.buttons]
# 28 = /mnt/us/kindle-button-mapper/scripts/auto.sh next_page
[device.by_name]
name = BLE-M3
[device.by_name.dpad]
up = x
";
        for (name, uniq, bound) in [
            ("Renamed", "AA:BB:CC:DD:EE:FF/P", true),
            ("AB Shutter3", "00:00:00:00:00:00", false), // another one of the same model
            ("Keyboard", "11:22:33:44:55:66", false),
            ("BLE-M3", "22:22:22:22:22:22", true),
            ("Unknown", "", false),
        ] {
            assert_eq!(mapper_binds(config, name, uniq), bound, "{name} {uniq}");
        }
    }

    #[test]
    fn one_press_seen_twice_counts_once() {
        let (mut gate, start) = (Gate::default(), Instant::now());
        let at = |ms| start + Duration::from_millis(ms);
        assert!(gate.accept((1, 109), at(0)));
        assert!(!gate.accept((2, 109), at(40)), "another device");
        assert!(!gate.accept((1, 115), at(60)), "another key of the same device");
        assert!(gate.accept((1, 109), at(80)), "the same button again");
        assert!(gate.accept((2, 109), at(400)), "later");
    }

    #[test]
    fn the_port_turns_pages_and_leaves_the_rest_to_the_kindle() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || serve(&listener, |action| tx.send(action).unwrap()));
        let get = |path: &str| {
            let mut s = TcpStream::connect(addr).unwrap();
            write!(s, "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").unwrap();
            let mut reply = String::new();
            let _ = s.read_to_string(&mut reply);
            reply
        };
        drop(TcpStream::connect(addr).unwrap()); // the mapper's "is it up?" probe
        assert!(get("/koreader/event/GotoViewRel/1").starts_with("HTTP/1.0 200"));
        assert!(get("/koreader/event/GotoViewRel/-1").starts_with("HTTP/1.0 200"));
        for path in ["/koreader/event/IncreaseFlIntensity/1", "/koreader/event/GotoViewRel/0", "/"] {
            assert_eq!(get(path), "", "{path}");
        }
        assert_eq!(rx.try_iter().collect::<Vec<_>>(), [PageNext, PagePrev]);
    }
}
