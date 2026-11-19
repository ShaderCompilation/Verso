//! Screens: what fills the display. One screen is active at a time (the top
//! of a stack); a screen owns its widgets and any sheet or pop-up it opens.
//! Toasts are app-wide.

pub mod books;
pub mod contents;
pub mod dictionaries;
pub mod dictionary;
pub mod dictionary_folders;
pub mod folders;
pub mod gallery;
pub mod reader;
pub mod reader_menu;
pub mod system;
pub mod text_sheet;
pub mod update;

use std::time::Instant;

use crate::geom::Rect;
use crate::gfx::Icon;
use crate::platform::{Battery, Frontlight, Orientation, RefreshMode, TextInput, Update};
use crate::ui::gesture::Gesture;
use crate::ui::theme::Theme;
use crate::ui::widget::DrawCx;
use crate::ui::widgets::Toast;

/// What a screen wants to happen next.
pub enum Action {
    None,
    /// Open another screen on top of this one.
    Push(Box<dyn Screen>),
    /// Close this screen and go back to the one below.
    Pop,
    /// Leave Verso.
    Exit,
    /// Leave Verso at once with this exit code, for the launcher: install
    /// the staged update or go back (`verso_update::launcher`). Saves like
    /// any exit, and leaves a running SSH server running.
    ExitWith(i32),
    /// Turn the screen. The app saves like on exit and rebuilds its screens
    /// at the new size, reopening Settings on top of the library.
    Orientation(Orientation),
}

pub trait Screen {
    /// True when drawing replaces every pixel, so the app can skip its clear.
    fn fills_canvas(&self) -> bool {
        false
    }

    /// Draws the whole screen. Called whenever anything may have changed;
    /// only what actually changed reaches the display.
    fn draw(&mut self, cx: &mut DrawCx);

    /// Request `cx.redraw()` for visual changes, including pressed controls.
    /// Stack actions, toasts and explicit refreshes invalidate automatically.
    fn on_gesture(&mut self, g: Gesture, cx: &mut EventCx) -> Action;

    /// Called before draining a batch of already-queued events.
    fn begin_batch(&mut self) {}
    /// Called after draining a batch of already-queued events, before drawing.
    fn finish_batch(&mut self, _cx: &mut EventCx) {}

    /// True if a focused field or its text-entry overlay consumed the input.
    fn on_text_input(&mut self, _input: &TextInput, _cx: &mut EventCx) -> bool {
        false
    }

    /// When `on_tick` wants to be called (timers, e.g. a held stepper).
    fn deadline(&self) -> Option<Instant> {
        None
    }

    fn on_tick(&mut self, _cx: &mut EventCx) -> Action {
        Action::None
    }

    /// Bounded layout work after presentation, interleaved with input. Returns
    /// whether visible state changed; cache-only progress needs no redraw.
    fn has_idle_work(&self) -> bool {
        false
    }
    fn on_idle(&mut self, _text: &mut crate::gfx::TextRenderer) -> bool {
        false
    }

    /// Used by scripted screenshots to wait for asynchronous screen changes.
    fn busy(&self) -> bool {
        false
    }

    /// Slow work to do once the current frame is on screen (so a "please
    /// wait" message shows first).
    fn has_pending_work(&self) -> bool {
        false
    }

    fn run_pending_work(&mut self, _cx: &mut EventCx) -> Action {
        Action::None
    }

    /// A worker has new results. Request `cx.redraw()` only for visual changes;
    /// consuming cache-only or stale results does not invalidate the screen.
    fn on_background(&mut self, _cx: &mut EventCx) {}
    /// Called after pixels reach the platform, before waiting for input.
    fn after_present(&mut self) {}
    /// Save before a normal shutdown.
    fn on_quit(&mut self) -> anyhow::Result<()> {
        self.on_sleep();
        Ok(())
    }

    /// The device is going to sleep.
    fn on_sleep(&mut self) {}
    /// The device woke up.
    fn on_wake(&mut self, _now: Instant) {}
    /// Bounded cover decoding; no art means the native screensaver is used.
    fn sleep_cover(&self, _width: u32, _height: u32) -> Option<image::GrayImage> {
        None
    }
}

/// What a screen can use while handling an event.
pub struct EventCx<'a> {
    pub theme: &'a Theme,
    /// The whole screen.
    pub screen: Rect,
    pub now: Instant,
    pub frontlight: &'a mut Option<Frontlight>,
    pub light_request: &'a mut Option<Frontlight>,
    /// The last battery reading (None: no battery, or unknown).
    pub battery: Option<Battery>,
    pub(crate) toast: &'a mut Option<Toast>,
    pub(crate) forced: &'a mut Vec<Update>,
    pub(crate) needs_redraw: &'a mut bool,
}

impl EventCx<'_> {
    /// Invalidate the screen after changing its visible state.
    pub fn redraw(&mut self) {
        *self.needs_redraw = true;
    }

    /// Shows a short message at the bottom of the screen.
    pub fn toast(&mut self, text: impl Into<String>, icon: Option<Icon>) {
        *self.toast = Some(Toast::new(text, icon, self.now));
        self.redraw();
    }

    /// Refreshes `rect` with `mode` in the next frame, even if nothing changed
    /// there (e.g. a full flash to clear ghosting).
    pub fn refresh(&mut self, rect: Rect, mode: RefreshMode) {
        self.forced.push(Update { rect, mode });
        self.redraw();
    }
}
