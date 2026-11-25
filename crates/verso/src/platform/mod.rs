//! The boundary between Verso and the device. Everything above this module is
//! the same on the Kindle and in the desktop simulator.

use std::time::{Duration, Instant};

use anyhow::Result;

use crate::geom::{Point, Rect};
use crate::gfx::{Canvas, ColorMode};

pub mod awake;
#[cfg(feature = "desktop")]
mod desktop;
#[cfg(feature = "kindle")]
pub mod firewall;
#[cfg(any(feature = "kindle", test))]
#[cfg_attr(not(feature = "kindle"), allow(dead_code))]
mod flash;
#[cfg(any(feature = "kindle", test))]
#[cfg_attr(not(feature = "kindle"), allow(dead_code))]
mod framebuffer;
#[cfg(feature = "desktop")]
mod headless;
#[cfg(feature = "desktop")]
mod kaleido;
#[cfg(feature = "kindle")]
mod kindle;
#[cfg(any(feature = "kindle", test))]
#[cfg_attr(not(feature = "kindle"), allow(dead_code))]
mod kindle_profiles;
#[cfg(feature = "kindle")]
pub mod mdns;
#[cfg(any(feature = "kindle", test))]
mod power;
#[cfg(any(feature = "kindle", test))]
#[cfg_attr(not(feature = "kindle"), allow(dead_code))]
mod process;
#[cfg(feature = "desktop")]
mod sim_device;
pub mod ssh;
pub mod wifi;

/// How an area of the e-ink screen should be refreshed. The app states its
/// intent; each backend picks the matching waveform. Ordered from weakest to
/// strongest: when areas overlap, the stronger mode wins.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum RefreshMode {
    /// Fastest, black and white only, may leave slight ghosting. Button feedback.
    Fast,
    /// Clean grayscale without a flash. Menus and screen changes.
    Ui,
    /// Tuned for text. Page turns.
    Page,
    /// Full flash that clears all ghosting. Images and after wake.
    Full,
}

/// A part of the screen to update, and how.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Update {
    pub rect: Rect,
    pub mode: RefreshMode,
}

/// Which way the user holds the device. The Paperwhite's panel is portrait
/// and can't rotate in hardware, so landscape is drawn on a landscape canvas
/// and rotated in software on the way to the screen.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Orientation {
    #[default]
    Portrait,
    /// The device turned a quarter turn clockwise: its top edge on the right.
    Landscape,
}

impl Orientation {
    /// The app's screen size for a panel of `width`×`height` (its native,
    /// unrotated size).
    pub fn screen_size(self, width: u32, height: u32) -> (u32, u32) {
        match self {
            Self::Portrait => (width, height),
            Self::Landscape => (height, width),
        }
    }
}

/// Static facts about the screen, in the app's orientation.
#[derive(Clone, Debug)]
pub struct DeviceInfo {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub dpi: u32,
    /// How the app's canvas stores pixels: `Color32` only when the backend
    /// can show color.
    pub color: ColorMode,
}

/// The top step of the light controls, in powerd's own scale (`flIntensity`
/// and `currentAmberLevel`). Every Kindle so far uses 0..=24.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LightRange {
    pub brightness: u8,
    pub warmth: u8,
}

impl Default for LightRange {
    fn default() -> Self {
        Self { brightness: 24, warmth: 24 }
    }
}

/// What the app knows about the device model, beyond its screen.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeviceModel {
    /// "Kindle Paperwhite 5 (2021)"; empty when the backend doesn't say.
    pub name: String,
    /// Nobody has confirmed that Verso works on this model yet: the
    /// untested-model notice shows once.
    pub untested: bool,
    /// The model's key in `Settings::untested_seen`.
    pub key: String,
    pub light_range: LightRange,
}

/// The device has a screen or input Verso can't drive yet. The Kindle
/// backend has already said so on screen; Verso exits with
/// `EXIT_UNSUPPORTED` and the launcher saves a problem report.
#[derive(Debug)]
pub struct Unsupported(pub String);

impl std::fmt::Display for Unsupported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "this Kindle isn't supported yet: {}", self.0)
    }
}

impl std::error::Error for Unsupported {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TouchPhase {
    Down,
    Move,
    Up,
}

/// One finger's raw touch report, in screen pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Touch {
    /// Stays the same for one finger from Down to Up.
    pub id: i32,
    pub phase: TouchPhase,
    pub pos: Point,
    pub time: Instant,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(dead_code)] // System events are produced by the Kindle backend.
pub enum Event {
    Touch(Touch),
    TextInput {
        input: TextInput,
        repeat: bool,
    },
    Background,
    /// The device is going to sleep: save state, stop drawing.
    Sleep,
    /// The device woke up: the screen may have been drawn over, redraw everything.
    Wake,
    /// Late firmware drawing after suspend or screensaver exit.
    WakeRepaint,
    PowerChanged,
    Usb(bool),
    /// Asynchronous startup frontlight reading from hardware.
    Frontlight(Option<Frontlight>),
    /// A hardware page-turn button was pressed (Oasis, or PgUp/PgDn in the
    /// simulator), already turned to the way the device is held.
    Key(KeyAction),
    /// Leave the app (window closed, or a signal on the device).
    Quit,
}

/// What a hardware button asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyAction {
    PageNext,
    PagePrev,
}

impl KeyAction {
    /// The page-turn meaning of an evdev key code, held in `orientation`.
    /// Kindle codes as FBInk's input scan reads them: `KEY_PAGEUP` and
    /// `KEY_F23` go back, `KEY_PAGEDOWN` and `KEY_YEN` forward. The
    /// Oasis's buttons sit on the panel's right edge in portrait, the
    /// "back" one above; a quarter turn clockwise (`Landscape`) puts that
    /// one on the right of the bottom edge, where forward belongs, so the
    /// two swap (to verify on an Oasis 3).
    #[cfg(any(feature = "kindle", test))]
    #[cfg_attr(not(feature = "kindle"), allow(dead_code))]
    pub fn from_evdev(code: u16, orientation: Orientation) -> Option<Self> {
        const KEY_PAGEUP: u16 = 104;
        const KEY_PAGEDOWN: u16 = 109;
        const KEY_YEN: u16 = 124;
        const KEY_F23: u16 = 193;
        let portrait = match code {
            KEY_PAGEUP | KEY_F23 => Self::PagePrev,
            KEY_PAGEDOWN | KEY_YEN => Self::PageNext,
            _ => return None,
        };
        Some(match orientation {
            Orientation::Portrait => portrait,
            Orientation::Landscape => portrait.reversed(),
        })
    }

    #[cfg(any(feature = "kindle", test))]
    fn reversed(self) -> Self {
        match self {
            Self::PageNext => Self::PagePrev,
            Self::PagePrev => Self::PageNext,
        }
    }

    /// Pages to move: +1 or -1.
    pub fn step(self) -> isize {
        match self {
            Self::PageNext => 1,
            Self::PagePrev => -1,
        }
    }
}

/// Single-line entry commands shared by hardware and touch keyboards.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TextInput {
    Insert(String),
    Backspace,
    Delete,
    Left,
    Right,
    Home,
    End,
    Clear,
    Submit,
    Dismiss,
}

/// A thread-safe way to interrupt a platform's blocking event wait.
#[derive(Clone)]
pub struct Waker(std::sync::Arc<dyn Fn() + Send + Sync>);

impl Waker {
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self(std::sync::Arc::new(wake))
    }
    pub fn wake(&self) {
        (self.0)();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Frontlight {
    pub brightness: u8,
    pub warmth: Option<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Battery {
    pub percent: u8,
    pub charging: bool,
}

pub trait Platform {
    fn waker(&self) -> Waker;
    fn set_busy(&mut self, _busy: bool) {}
    fn battery(&mut self) -> Option<Battery> {
        None
    }
    fn usb_connected(&mut self) -> Option<bool> {
        None
    }
    /// Hand off input/display for the native screensaver, or retain the display
    /// for our cover. The launcher remains responsible for system UI ownership.
    fn prepare_sleep(&mut self, _cover: bool) -> Result<()> {
        Ok(())
    }
    fn resume(&mut self) -> Result<()> {
        Ok(())
    }
    /// Simulator shortcuts run only after the focused screen declines entry.
    fn unhandled_text_input(&mut self, _input: &TextInput, _repeat: bool) {}
    fn frontlight(&mut self) -> Option<Frontlight> {
        None
    }
    /// Non-blocking startup frontlight read. Immediate backends return `Some`;
    /// backends that query hardware off-thread return `None` until ready and
    /// deliver `Event::Frontlight` when the read completes.
    fn initial_frontlight(&mut self) -> Option<Option<Frontlight>> {
        Some(self.frontlight())
    }
    fn set_frontlight(&mut self, _value: Frontlight) -> Result<()> {
        Ok(())
    }

    fn info(&self) -> &DeviceInfo;

    /// The model, its light range and whether it has been tested.
    fn model(&self) -> DeviceModel {
        DeviceModel::default()
    }

    /// Finishes what opening started in the background (the Kindle's
    /// touchscreen), once the first frame is on its way. An `Unsupported`
    /// error has already been shown on screen.
    fn finish_startup(&mut self) -> Result<()> {
        Ok(())
    }

    /// Platform-specific lines included in the startup session header block.
    fn session_details(&self) -> Vec<String> {
        Vec::new()
    }

    /// Turns the screen; `info()` then reports the new size. The next frame
    /// must redraw everything.
    fn set_orientation(&mut self, orientation: Orientation) -> Result<()>;

    /// Waits for the next event. Returns `None` if `timeout` passes first;
    /// `None` as the timeout waits forever.
    fn next_event(&mut self, timeout: Option<Duration>) -> Result<Option<Event>>;

    /// Copies the given areas of `canvas` to the screen and refreshes them.
    fn present(&mut self, canvas: &Canvas, updates: &[Update]) -> Result<()>;

    /// Blocks until every refresh sent so far has finished on the panel.
    fn wait_for_complete(&mut self) -> Result<()>;

    /// Whether the framebuffer still holds `canvas`, as last presented, for
    /// the log. `None` where nothing else can draw.
    fn check_frame(&mut self, _canvas: &Canvas) -> Option<FrameCheck> {
        None
    }

    /// Display refreshes that failed this session.
    fn failed_refreshes(&self) -> u32 {
        0
    }
}

/// How much of a presented frame something else drew over: a sample of panel
/// rows, as many as `strips`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(feature = "kindle"), allow(dead_code))] // the simulator's window is its own
pub struct FrameCheck {
    pub strips: u32,
    pub changed: u32,
    /// The first and last changed panel rows, from the panel's top.
    pub rows: (i32, i32),
}

impl std::fmt::Display for FrameCheck {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        if self.changed == 0 {
            return write!(f, "framebuffer intact");
        }
        let (first, last) = self.rows;
        write!(f, "framebuffer OVERWRITTEN in {} of {} rows, panel rows {first} to {last}", self.changed, self.strips)
    }
}

/// A scripted touch, for screenshots without a window.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "kindle", allow(dead_code))] // screenshots are simulator-only
pub enum ScriptStep {
    Text(TextInput),
    /// A page-turn button press.
    Key(KeyAction),
    Tap(Point),
    LongPress(Point),
    HoldDrag(Point, Point),
    /// Finger down, never lifted (to capture pressed states).
    Press(Point),
    Swipe(Point, Point),
}

/// A platform without a window that plays `script`, saves what's on its
/// "screen" to `path` as a PNG after every update, and then quits.
/// `device`: the simulator preset from `--device`.
pub fn open_headless(
    path: &std::path::Path,
    script: Vec<ScriptStep>,
    device: Option<&str>,
) -> Result<Box<dyn Platform>> {
    #[cfg(feature = "desktop")]
    return Ok(Box::new(headless::Headless::new(path, script, sim_device::SimDevice::from_env(device)?)?));
    #[cfg(feature = "kindle")]
    {
        let _ = (path, script, device);
        anyhow::bail!("screenshots only work in the simulator")
    }
}

/// Opens the platform this binary was built for. `device`: the simulator
/// preset from `--device` (the Kindle ignores it).
pub fn open(device: Option<&str>) -> Result<Box<dyn Platform>> {
    #[cfg(feature = "desktop")]
    return Ok(Box::new(desktop::Desktop::open(sim_device::SimDevice::from_env(device)?)?));
    #[cfg(feature = "kindle")]
    {
        let _ = device;
        Ok(Box::new(kindle::Kindle::open()?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_keys_follow_the_way_the_kindle_is_held() {
        use KeyAction::*;
        for (code, portrait) in [(104, PagePrev), (193, PagePrev), (109, PageNext), (124, PageNext)] {
            assert_eq!(KeyAction::from_evdev(code, Orientation::Portrait), Some(portrait), "code {code}");
            let landscape = if portrait == PageNext { PagePrev } else { PageNext };
            assert_eq!(KeyAction::from_evdev(code, Orientation::Landscape), Some(landscape), "code {code}");
        }
        // Power, Home and anything else aren't page keys.
        for code in [116, 102, 1, 0] {
            assert_eq!(KeyAction::from_evdev(code, Orientation::Portrait), None);
        }
        assert_eq!((PageNext.step(), PagePrev.step()), (1, -1));
    }
}
