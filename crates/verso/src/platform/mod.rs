//! The boundary between Verso and the device. Everything above this module is
//! the same on the Kindle and in the desktop simulator.

use std::time::{Duration, Instant};

use anyhow::Result;

use crate::geom::{Point, Rect};
use crate::gfx::Canvas;

pub mod awake;
#[cfg(feature = "desktop")]
mod desktop;
#[cfg(feature = "desktop")]
mod headless;
#[cfg(feature = "kindle")]
mod kindle;
#[cfg(feature = "kindle")]
mod mdns;
#[cfg(any(feature = "kindle", test))]
mod power;
#[cfg(any(feature = "kindle", test))]
#[cfg_attr(not(feature = "kindle"), allow(dead_code))]
mod process;
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
}

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
    /// Leave the app (window closed, or a signal on the device).
    Quit,
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
    fn set_frontlight(&mut self, _value: Frontlight) -> Result<()> {
        Ok(())
    }

    fn info(&self) -> &DeviceInfo;

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
}

/// A scripted touch, for screenshots without a window.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "kindle", allow(dead_code))] // screenshots are simulator-only
pub enum ScriptStep {
    Text(TextInput),
    Tap(Point),
    LongPress(Point),
    HoldDrag(Point, Point),
    /// Finger down, never lifted (to capture pressed states).
    Press(Point),
    Swipe(Point, Point),
}

/// A platform without a window that plays `script`, saves what's on its
/// "screen" to `path` as a PNG after every update, and then quits.
pub fn open_headless(path: &std::path::Path, script: Vec<ScriptStep>) -> Result<Box<dyn Platform>> {
    #[cfg(feature = "desktop")]
    return Ok(Box::new(headless::Headless::new(path, script)?));
    #[cfg(feature = "kindle")]
    {
        let _ = (path, script);
        anyhow::bail!("screenshots only work in the simulator")
    }
}

/// Opens the platform this binary was built for.
pub fn open() -> Result<Box<dyn Platform>> {
    #[cfg(feature = "desktop")]
    return Ok(Box::new(desktop::Desktop::open()?));
    #[cfg(feature = "kindle")]
    return Ok(Box::new(kindle::Kindle::open()?));
}
