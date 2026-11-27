//! Screens: what fills the display. One screen is active at a time (the top
//! of a stack); a screen owns its widgets and any sheet or pop-up it opens.
//! Toasts are app-wide.

pub mod books;
pub mod contents;
pub mod dictionaries;
pub mod dictionary;
pub mod dictionary_folders;
#[cfg(test)]
mod fit_tests;
pub mod folders;
pub mod footnote;
pub mod gallery;
pub mod koreader_import;
pub mod paged_text;
pub mod reader;
pub mod reader_menu;
pub mod release_notes;
pub mod setup;
pub mod styled_sheet;
pub mod system;
pub mod text_sheet;
pub mod top_drawer;
pub mod transfer;
pub mod untested;
pub mod update;

use std::time::Instant;

use crate::geom::Rect;
use crate::gfx::Icon;
use crate::platform::{Battery, KeyAction, Orientation, RefreshMode, TextInput, Update};
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
    /// at the new size as they were (see [`RelayoutState`]).
    Orientation(Orientation),
    /// Open the Top Drawer (quick settings) over this screen.
    OpenTopDrawer,
}

/// What the screens were showing before the display turned, so the screens
/// rebuilt at the new size show the same: Home or the Library folder and
/// page (saved in `Navigation`), the open book at its saved position,
/// Settings, and setup on top.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RelayoutState {
    /// The open book's id.
    pub open_book: Option<String>,
    /// Library (its saved folder and page) rather than Home.
    pub library_open: bool,
    /// Settings was open over the other screens.
    pub in_settings: bool,
    /// First-time setup was open on top, on this page.
    pub setup: Option<setup::SetupResume>,
    /// The last book generation used, so the reopened book's replies never
    /// mix with ones still on their way to the old reader.
    pub book_generation: u64,
}

/// The open book shown in the Top Drawer's header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TopDrawerBook {
    pub title: String,
}

/// Actions triggered from the Top Drawer's in-book header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TopDrawerBookAction {
    /// Close the book and return to Home/Library (saving position).
    CloseBook,
    /// Open in-book search.
    Search,
}

pub trait Screen {
    /// The screen's name in the log: the type's, or a fixed word. Never
    /// anything about a book or folder.
    fn name(&self) -> &'static str {
        let path = std::any::type_name::<Self>();
        path.rsplit("::").next().unwrap_or(path)
    }

    /// True when drawing replaces every pixel, so the app can skip its clear.
    fn fills_canvas(&self) -> bool {
        false
    }

    /// True while drawing leaves the canvas as it is (the previous screen
    /// stays up while a book opens), so the app skips drawing and diffing.
    fn keeps_canvas(&self) -> bool {
        false
    }

    /// True in the states where a swipe down from the top edge opens the Top
    /// Drawer (no sheet, menu, keyboard or selection open). The app then
    /// cancels the touch this screen saw start.
    fn allows_top_drawer(&self) -> bool {
        false
    }

    /// The open book to show in the Top Drawer's header, if a book is open.
    fn top_drawer_book(&self) -> Option<TopDrawerBook> {
        None
    }

    /// Runs an action from the Top Drawer's in-book header.
    fn on_top_drawer_book(&mut self, _action: TopDrawerBookAction, _cx: &mut EventCx) -> Action {
        Action::None
    }

    /// The Top Drawer opened over this screen: close what it replaces (the
    /// Reader Menu).
    fn top_drawer_opened(&mut self) {}

    /// The screen pushed over this one closed; this one is on top again.
    fn on_return(&mut self, _cx: &mut EventCx) {}

    /// Notes what this screen shows, before the display turns.
    fn relayout_state(&self, _state: &mut RelayoutState) {}

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

    /// A page-turn button was pressed. Only the reader turns pages.
    fn on_key(&mut self, _key: KeyAction, _cx: &mut EventCx) -> Action {
        Action::None
    }

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
    /// True once after a frame that showed a page turn (for the exit
    /// performance line).
    fn take_page_turn(&mut self) -> bool {
        false
    }
    /// The exit was refused (see `on_quit`): drop any pending exit intent, or
    /// the main loop would ask again before it reads another touch.
    fn exit_refused(&mut self) {}
    /// Save before a normal shutdown.
    fn on_quit(&mut self) -> anyhow::Result<()> {
        self.on_sleep();
        Ok(())
    }

    /// The device is going to sleep.
    fn on_sleep(&mut self) {}
    /// The device woke up.
    fn on_wake(&mut self, _now: Instant) {}
    /// Switching dark mode live from the Top Drawer.
    fn set_dark(&mut self, _dark: bool, _cx: &mut EventCx) {}
    /// True while re-rendering the current view for a new polarity.
    fn waiting_for_dark(&self) -> bool {
        false
    }
    /// Bounded cover decoding; no art means the native screensaver is used.
    fn sleep_cover(&self, _width: u32, _height: u32) -> Option<crate::library::covers::CoverImage> {
        None
    }
}

/// What a screen can use while handling an event.
pub struct EventCx<'a> {
    pub theme: &'a Theme,
    /// The whole screen.
    pub screen: Rect,
    pub now: Instant,
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
