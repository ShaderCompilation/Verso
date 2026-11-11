//! Screens: what fills the display. One screen is active at a time (the top
//! of a stack); a screen owns its widgets and any sheet or pop-up it opens.
//! Toasts are app-wide.

pub mod books;
pub mod contents;
pub mod gallery;
pub mod reader;
pub mod reader_menu;
pub mod text_sheet;

use std::time::Instant;

use crate::geom::Rect;
use crate::gfx::Icon;
use crate::platform::{Frontlight, RefreshMode, Update};
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
}

pub trait Screen {
    /// Draws the whole screen. Called whenever anything may have changed;
    /// only what actually changed reaches the display.
    fn draw(&mut self, cx: &mut DrawCx);

    fn on_gesture(&mut self, g: Gesture, cx: &mut EventCx) -> Action;

    /// When `on_tick` wants to be called (timers, e.g. a held stepper).
    fn deadline(&self) -> Option<Instant> {
        None
    }

    fn on_tick(&mut self, _cx: &mut EventCx) -> Action {
        Action::None
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

    /// A worker has new results. Return true if the screen needs drawing.
    fn on_background(&mut self, _cx: &mut EventCx) {}
    /// Called after pixels reach the platform, before waiting for input.
    fn after_present(&mut self) {}
    /// Save before a normal shutdown.
    fn on_quit(&mut self) {
        self.on_sleep();
    }

    /// The device is going to sleep.
    fn on_sleep(&mut self) {}
}

/// What a screen can use while handling an event.
pub struct EventCx<'a> {
    pub theme: &'a Theme,
    /// The whole screen.
    pub screen: Rect,
    pub now: Instant,
    pub frontlight: &'a mut Option<Frontlight>,
    pub light_request: &'a mut Option<Frontlight>,
    pub(crate) toast: &'a mut Option<Toast>,
    pub(crate) forced: &'a mut Vec<Update>,
}

impl EventCx<'_> {
    /// Shows a short message at the bottom of the screen.
    pub fn toast(&mut self, text: impl Into<String>, icon: Option<Icon>) {
        *self.toast = Some(Toast::new(text, icon, self.now));
    }

    /// Refreshes `rect` with `mode` in the next frame, even if nothing changed
    /// there (e.g. a full flash to clear ghosting).
    pub fn refresh(&mut self, rect: Rect, mode: RefreshMode) {
        self.forced.push(Update { rect, mode });
    }
}
