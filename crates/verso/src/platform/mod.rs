//! The boundary between Verso and the device. Everything above this module is
//! the same on the Kindle and in the desktop simulator.

use std::time::{Duration, Instant};

use anyhow::Result;

use crate::geom::{Point, Rect};
use crate::gfx::Canvas;

#[cfg(feature = "desktop")]
mod desktop;
#[cfg(feature = "desktop")]
mod headless;
#[cfg(feature = "kindle")]
mod kindle;

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

/// Static facts about the screen.
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
// The Kindle backend only produces Sleep/Wake once power events land (Phase 6).
#[cfg_attr(feature = "kindle", allow(dead_code))]
pub enum Event {
    Touch(Touch),
    Background,
    /// The device is going to sleep: save state, stop drawing.
    Sleep,
    /// The device woke up: the screen may have been drawn over, redraw everything.
    Wake,
    /// Leave the app (window closed, or a signal on the device).
    Quit,
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

pub trait Platform {
    fn waker(&self) -> Waker;
    fn set_busy(&mut self, _busy: bool) {}
    fn frontlight(&mut self) -> Option<Frontlight> {
        None
    }
    fn set_frontlight(&mut self, _value: Frontlight) -> Result<()> {
        Ok(())
    }

    fn info(&self) -> &DeviceInfo;

    /// Waits for the next event. Returns `None` if `timeout` passes first;
    /// `None` as the timeout waits forever.
    fn next_event(&mut self, timeout: Option<Duration>) -> Result<Option<Event>>;

    /// Copies the given areas of `canvas` to the screen and refreshes them.
    fn present(&mut self, canvas: &Canvas, updates: &[Update]) -> Result<()>;

    /// Blocks until every refresh sent so far has finished on the panel.
    fn wait_for_complete(&mut self) -> Result<()>;
}

/// A scripted touch, for screenshots without a window.
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "kindle", allow(dead_code))] // screenshots are simulator-only
pub enum ScriptStep {
    Tap(Point),
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
