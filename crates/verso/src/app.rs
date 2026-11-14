//! The app: owns the canvas and the screen stack, turns gestures and timers
//! into redraws, and works out which parts of the display to refresh.

use std::time::{Duration, Instant};

use crate::geom::Rect;
use crate::gfx::{Canvas, IconRenderer, TextRenderer};
use crate::platform::{Battery, DeviceInfo, Frontlight, Orientation, RefreshMode, TextInput, Update};
use crate::screens::system::{ExitChoice, ExitPrompt};
use crate::screens::{Action, EventCx, Screen};
use crate::ui::damage::{self, FrameDiff};
use crate::ui::gesture::Gesture;
use crate::ui::theme::{PAPER, Theme};
use crate::ui::widget::DrawCx;
use crate::ui::widgets::Toast;

pub struct App {
    canvas: Canvas,
    text: TextRenderer,
    icons: IconRenderer,
    theme: Theme,
    diff: FrameDiff,
    stack: Vec<Box<dyn Screen>>,
    toast: Option<Toast>,
    /// Where the toast was drawn last.
    toast_rect: Rect,
    /// The current touch started on the toast.
    toast_touch: bool,
    /// Refreshes asked for regardless of what changed.
    forced: Vec<Update>,
    /// Areas last refreshed with `Fast` (black and white only). They get a
    /// clean `Ui` refresh once the finger lifts, even if nothing changed.
    fast_debt: Vec<Rect>,
    touch_down: bool,
    needs_redraw: bool,
    asleep: bool,
    exit: bool,
    exit_prompt: Option<ExitPrompt>,
    stop_ssh_on_exit: bool,
    pub frontlight: Option<Frontlight>,
    light_request: Option<Frontlight>,
    pub battery: Option<Battery>,
    usb_prompt: Option<Box<dyn Screen>>,
    usb_connected: bool,
    wake_cleanup: Option<Instant>,
    orientation: Orientation,
    /// A screen asked to turn the display; see `take_orientation_request`.
    orientation_request: Option<Orientation>,
}

impl App {
    pub fn new(info: &DeviceInfo, theme: Theme, screen: Box<dyn Screen>) -> Self {
        let canvas = Canvas::new(info.width, info.height);
        Self {
            diff: FrameDiff::new(&canvas),
            canvas,
            text: TextRenderer::new(),
            icons: IconRenderer::default(),
            theme,
            stack: vec![screen],
            toast: None,
            toast_rect: Rect::default(),
            toast_touch: false,
            forced: Vec::new(),
            fast_debt: Vec::new(),
            touch_down: false,
            needs_redraw: true,
            asleep: false,
            exit: false,
            exit_prompt: None,
            stop_ssh_on_exit: false,
            frontlight: None,
            light_request: None,
            battery: None,
            usb_prompt: None,
            usb_connected: false,
            wake_cleanup: None,
            orientation: Orientation::Portrait,
            orientation_request: None,
        }
    }

    /// The orientation the canvas is drawn in (set at startup or by `relayout`).
    pub fn set_orientation(&mut self, orientation: Orientation) {
        self.orientation = orientation;
    }

    pub fn take_orientation_request(&mut self) -> Option<Orientation> {
        self.orientation_request.take()
    }

    /// Saves every screen like a normal exit, before a relayout. On failure
    /// the error is shown and the screens stay as they are.
    pub fn prepare_relayout(&mut self) -> bool {
        self.on_gesture(Gesture::Cancel(crate::geom::Point::default()));
        for s in &mut self.stack {
            if let Err(e) = s.on_quit() {
                log::error!("before turning the screen: {e:#}");
                self.toast = Some(Toast::new(e.to_string(), None, Instant::now()));
                self.needs_redraw = true;
                return false;
            }
        }
        true
    }

    /// Replaces the screens with ones built for a new screen size (after
    /// `prepare_relayout`). The old screens are dropped first, so their
    /// workers stop before the new ones start.
    pub fn relayout(
        &mut self,
        info: &DeviceInfo,
        orientation: Orientation,
        build: impl FnOnce(&Theme, Rect) -> Vec<Box<dyn Screen>>,
    ) {
        self.stack.clear();
        self.orientation = orientation;
        self.canvas = Canvas::new(info.width, info.height);
        self.diff = FrameDiff::new(&self.canvas);
        self.stack = build(&self.theme, self.canvas.bounds());
        assert!(!self.stack.is_empty(), "relayout needs at least one screen");
        if self.usb_prompt.is_some() {
            self.usb_prompt = Some(Box::new(crate::screens::system::UsbPrompt::new(&self.theme, self.canvas.bounds())));
        }
        self.toast_touch = false;
        self.touch_down = false;
        self.refresh_all(RefreshMode::Full);
    }

    pub fn canvas(&self) -> &Canvas {
        &self.canvas
    }

    pub fn wants_exit(&self) -> bool {
        self.exit
    }

    /// User-requested exits ask what to do with a running SSH server. System
    /// shutdown/recovery and headless capture completion bypass this prompt.
    pub fn request_exit(&mut self) {
        if self.exit_prompt.is_some() {
            return;
        }
        if crate::platform::ssh::status().running {
            self.show_exit_prompt();
        } else {
            self.exit = true;
        }
    }

    fn show_exit_prompt(&mut self) {
        self.on_gesture(Gesture::Cancel(crate::geom::Point::default()));
        self.toast = None;
        self.toast_touch = false;
        self.exit_prompt = Some(ExitPrompt::new(&self.theme, self.canvas.bounds()));
        self.needs_redraw = true;
    }

    /// Refreshes the whole display in the next frame (first frame, wake).
    pub fn refresh_all(&mut self, mode: RefreshMode) {
        if mode == RefreshMode::Full {
            // A full refresh also satisfies queued layout refreshes and press cleanup.
            self.forced.clear();
            self.fast_debt.clear();
        }
        self.forced.push(Update { rect: self.canvas.bounds(), mode });
        self.needs_redraw = true;
    }

    /// Draws a new frame if anything changed, and returns the display updates.
    pub fn frame(&mut self) -> Vec<Update> {
        if self.asleep || !self.needs_redraw {
            return Vec::new();
        }
        self.needs_redraw = false;
        let started = Instant::now();
        if !self.stack.last().is_some_and(|s| s.fills_canvas()) {
            self.canvas.fill(PAPER);
        }
        let mut hints = Vec::new();
        {
            let mut cx = DrawCx::new(&mut self.canvas, &mut self.text, &mut self.icons, &self.theme, &mut hints);
            cx.battery = self.battery;
            if let Some(screen) = self.stack.last_mut() {
                screen.draw(&mut cx);
            }
            if let Some(prompt) = &mut self.usb_prompt {
                prompt.draw(&mut cx);
            }
            if let Some(prompt) = &self.exit_prompt {
                prompt.draw(&mut cx);
            }
            if let Some(toast) = &self.toast {
                self.toast_rect = toast.draw(&mut cx);
            }
        }
        let drawn = started.elapsed();

        let mut forced = std::mem::take(&mut self.forced);
        if !self.touch_down {
            forced.extend(self.fast_debt.drain(..).map(|rect| Update { rect, mode: RefreshMode::Ui }));
        }
        // A known full-screen refresh already covers all changed pixels. Keep
        // its strongest mode (including wake/periodic flashes) and synchronize
        // the baseline without expanding grayscale pages or comparing tiles.
        let full_mode = forced.iter().filter(|u| u.rect == self.canvas.bounds()).map(|u| u.mode).max();
        let updates = if let Some(mode) =
            full_mode.filter(|&mode| mode >= RefreshMode::Ui && hints.iter().chain(&forced).all(|u| u.mode <= mode))
        {
            self.diff.commit_full(&self.canvas);
            vec![Update { rect: self.canvas.bounds(), mode }]
        } else {
            let tiles = self.diff.changed(&self.canvas);
            let updates = damage::plan(&tiles, self.canvas.bounds(), &hints, RefreshMode::Ui, &forced);
            self.diff.commit(&self.canvas, &tiles);
            updates
        };
        for u in updates.iter().filter(|u| u.mode == RefreshMode::Fast) {
            if !self.fast_debt.iter().any(|d| d.union(&u.rect) == *d) {
                self.fast_debt.push(u.rect);
            }
        }
        log::trace!(
            "frame: drawn in {drawn:?}, diffed in {:?}, {} update(s)",
            started.elapsed() - drawn,
            updates.len()
        );
        updates
    }

    pub fn on_gesture(&mut self, g: Gesture) {
        if self.asleep {
            return;
        }
        if matches!(g, Gesture::Down(_)) {
            self.touch_down = true;
            self.toast_touch = self.toast.is_some() && self.toast_rect.contains(g.pos());
        } else if g.ends_touch() {
            self.touch_down = false;
            // Fast pressed/drag feedback needs a grayscale cleanup even when
            // the release itself changes no pixels.
            self.needs_redraw |= !self.fast_debt.is_empty();
        }
        if let Some(mut prompt) = self.exit_prompt.take() {
            let choice = prompt.on_gesture(&g);
            self.needs_redraw = true;
            match choice {
                Some(ExitChoice::Cancel) => {}
                Some(ExitChoice::KeepSsh) => self.exit = true,
                Some(ExitChoice::StopSsh) => {
                    self.exit = true;
                    self.stop_ssh_on_exit = true;
                    prompt.set_stopping(true);
                    self.exit_prompt = Some(prompt);
                }
                None => self.exit_prompt = Some(prompt),
            }
            return;
        }
        if let Some(mut prompt) = self.usb_prompt.take() {
            let mut action = Action::None;
            self.with_screen(|_, cx| {
                action = prompt.on_gesture(g, cx);
                Action::None
            });
            match action {
                Action::Exit => self.request_exit(),
                Action::Pop => self.needs_redraw = true,
                _ => self.usb_prompt = Some(prompt),
            }
            return;
        }
        if self.toast_touch {
            // Touches on the toast only dismiss it.
            if g.ends_touch() {
                self.toast_touch = false;
                if matches!(g, Gesture::Tap(_)) {
                    self.toast = None;
                    self.needs_redraw = true;
                }
            }
            return;
        }
        self.with_screen(|screen, cx| screen.on_gesture(g, cx));
    }

    pub fn on_text_input(&mut self, input: &TextInput) -> bool {
        if self.asleep {
            return false;
        }
        if self.exit_prompt.is_some() {
            if matches!(input, TextInput::Dismiss) {
                self.exit_prompt = None;
                self.needs_redraw = true;
            }
            return true;
        }
        if self.usb_prompt.is_some() {
            if matches!(input, TextInput::Dismiss) {
                self.usb_prompt = None;
                self.needs_redraw = true;
            }
            return true;
        }
        let mut consumed = false;
        self.with_screen(|screen, cx| {
            consumed = screen.on_text_input(input, cx);
            Action::None
        });
        self.needs_redraw |= consumed;
        consumed
    }

    /// The next time `on_tick` should run, if any.
    pub fn deadline(&self) -> Option<Instant> {
        if self.asleep {
            return None;
        }
        let screen = self.stack.last().and_then(|s| s.deadline());
        [screen, self.toast.as_ref().map(|t| t.until), self.wake_cleanup].into_iter().flatten().min()
    }

    pub fn on_tick(&mut self, now: Instant) {
        if self.asleep {
            return;
        }
        if self.wake_cleanup.is_some_and(|due| now >= due) {
            self.wake_cleanup = None;
            let bounds = self.canvas.bounds();
            let height = bounds.h.div_euclid(10) + 1;
            self.forced
                .push(Update { rect: Rect::new(0, bounds.bottom() - height, bounds.w, height), mode: RefreshMode::Ui });
            self.needs_redraw = true;
        }
        if self.toast.as_ref().is_some_and(|t| now >= t.until) {
            self.toast = None;
            self.needs_redraw = true;
        }
        if self.stack.last().and_then(|s| s.deadline()).is_some_and(|d| now >= d) {
            self.needs_redraw = true;
            self.with_screen(|screen, cx| screen.on_tick(cx));
        }
    }

    pub fn busy(&self) -> bool {
        self.stack.last().is_some_and(|s| s.busy())
    }

    pub fn has_pending_work(&self) -> bool {
        !self.asleep && self.stack.last().is_some_and(|s| s.has_pending_work())
    }

    pub fn run_pending_work(&mut self) {
        self.needs_redraw = true;
        self.with_screen(|screen, cx| screen.run_pending_work(cx));
    }

    pub fn on_background(&mut self) {
        if self.asleep {
            return;
        }
        self.with_screen(|screen, cx| {
            screen.on_background(cx);
            Action::None
        });
    }
    pub fn after_present(&mut self) {
        if let Some(s) = self.stack.last_mut() {
            s.after_present();
        }
    }
    /// Refuse a normal exit if durable saves failed, so the error remains
    /// visible and the reader can retry rather than silently losing its place.
    pub fn on_quit(&mut self) -> bool {
        for s in &mut self.stack {
            if let Err(e) = s.on_quit() {
                log::error!("exit: {e:#}");
                self.toast = Some(Toast::new(e.to_string(), None, Instant::now()));
                self.exit = false;
                self.stop_ssh_on_exit = false;
                if let Some(prompt) = &mut self.exit_prompt {
                    prompt.set_stopping(false);
                }
                self.needs_redraw = true;
                return false;
            }
        }
        // Save first: a failed save must keep both the app and SSH available.
        if self.stop_ssh_on_exit {
            self.stop_ssh_on_exit = false;
            if let Err(e) = crate::platform::ssh::set_running(false) {
                log::error!("stopping SSH on exit: {e:#}");
                self.exit = false;
                if let Some(prompt) = &mut self.exit_prompt {
                    prompt.set_stopping(false);
                }
                self.toast = Some(Toast::new("Couldn't stop SSH. Try again or choose Exit.", None, Instant::now()));
                self.needs_redraw = true;
                return false;
            }
        }
        true
    }
    pub fn light_request(&mut self) -> Option<Frontlight> {
        self.light_request.take()
    }

    pub fn on_sleep(&mut self) {
        // Cancel captures before dropping raw touch state, including overlays.
        self.on_gesture(Gesture::Cancel(crate::geom::Point::default()));
        for s in &mut self.stack {
            s.on_sleep();
        }
        self.asleep = true;
        self.touch_down = false;
        self.wake_cleanup = None;
    }

    pub fn draw_sleep_cover(&mut self, preference: crate::store::SleepScreen) -> bool {
        if preference == crate::store::SleepScreen::Amazon {
            return false;
        }
        // The cover stays portrait, like Amazon's sleep screen: in landscape
        // it is drawn turned, so the display's rotation stands it upright.
        let (w, h) = (self.canvas.width(), self.canvas.height());
        let (cover_w, cover_h) = match self.orientation {
            Orientation::Portrait => (w, h),
            Orientation::Landscape => (h, w),
        };
        let cover = self.stack.iter().rev().find_map(|s| s.sleep_cover(cover_w, cover_h));
        let Some(cover) = cover else { return false };
        let cover = match self.orientation {
            Orientation::Portrait => cover,
            // Panel pixel (y, w - 1 - x) shows canvas pixel (x, y).
            Orientation::Landscape => image::imageops::rotate90(&cover),
        };
        self.canvas.fill(PAPER);
        self.canvas.blit_gray(self.canvas.bounds(), cover.as_raw(), cover.width() as usize);
        true
    }

    pub fn set_battery(&mut self, battery: Option<Battery>) {
        if self.battery != battery {
            self.battery = battery;
            self.needs_redraw = true;
        }
    }

    /// Returns true when connection state changes, so the caller can discard
    /// the raw gesture that may have started before this modal appeared/closed.
    pub fn on_usb(&mut self, connected: bool) -> bool {
        if connected == self.usb_connected {
            return false;
        }
        self.usb_connected = connected;
        if !self.asleep {
            self.on_gesture(Gesture::Cancel(crate::geom::Point::default()));
        }
        self.usb_prompt = connected.then(|| {
            Box::new(crate::screens::system::UsbPrompt::new(&self.theme, self.canvas.bounds())) as Box<dyn Screen>
        });
        self.needs_redraw = true;
        true
    }

    pub fn schedule_wake_cleanup(&mut self) {
        if !self.asleep {
            self.wake_cleanup = Some(Instant::now() + Duration::from_millis(1500));
        }
    }

    pub fn on_wake(&mut self) {
        self.asleep = false;
        self.on_background();
        self.refresh_all(RefreshMode::Full);
        // Use a bounded cleanup on every wake; firmware hibernation detection is
        // inconsistent on PW6, and this avoids reading Amazon's private state.
        self.schedule_wake_cleanup();
    }

    fn with_screen(&mut self, f: impl FnOnce(&mut dyn Screen, &mut EventCx) -> Action) {
        let Some(screen) = self.stack.last_mut() else { return };
        let mut cx = EventCx {
            theme: &self.theme,
            screen: self.canvas.bounds(),
            now: Instant::now(),
            frontlight: &mut self.frontlight,
            light_request: &mut self.light_request,
            toast: &mut self.toast,
            forced: &mut self.forced,
            needs_redraw: &mut self.needs_redraw,
        };
        let action = f(screen.as_mut(), &mut cx);
        match action {
            Action::None => {}
            Action::Push(screen) => {
                self.stack.push(screen);
                self.needs_redraw = true;
            }
            Action::Pop => {
                self.needs_redraw = true;
                if self.stack.len() == 1 {
                    self.request_exit();
                } else {
                    self.stack.pop();
                    self.on_background();
                }
            }
            Action::Exit => self.request_exit(),
            Action::Orientation(o) => self.orientation_request = Some(o),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, rc::Rc};
    #[test]
    fn failed_exit_keeps_the_app_and_error_visible_for_retry() {
        struct FailsOnce(bool);
        impl Screen for FailsOnce {
            fn draw(&mut self, _: &mut DrawCx) {}
            fn on_gesture(&mut self, _: Gesture, _: &mut EventCx) -> Action {
                Action::None
            }
            fn on_quit(&mut self) -> anyhow::Result<()> {
                anyhow::ensure!(std::mem::replace(&mut self.0, true), "Couldn't save your place");
                Ok(())
            }
        }
        let info = DeviceInfo { name: "test".into(), width: 600, height: 800, dpi: 150 };
        let mut app = App::new(&info, Theme::for_dpi(150), Box::new(FailsOnce(false)));
        app.frame();
        app.exit = true;
        assert!(!app.on_quit());
        assert!(!app.wants_exit());
        assert!(app.toast.is_some());
        assert!(!app.frame().is_empty());
        assert!(app.on_quit());
    }
    struct BookScreen {
        touches: Rc<Cell<usize>>,
        backgrounds: Rc<Cell<usize>>,
        saves: Rc<Cell<usize>>,
        cover: bool,
    }
    impl Screen for BookScreen {
        fn draw(&mut self, cx: &mut DrawCx) {
            cx.canvas.fill(51);
        }
        fn on_gesture(&mut self, g: Gesture, _: &mut EventCx) -> Action {
            if matches!(g, Gesture::Tap(_)) {
                self.touches.set(self.touches.get() + 1);
            }
            Action::None
        }
        fn on_background(&mut self, _: &mut EventCx) {
            self.backgrounds.set(self.backgrounds.get() + 1);
        }
        fn after_present(&mut self) {
            self.saves.set(self.saves.get() + 1);
        }
        fn sleep_cover(&self, width: u32, height: u32) -> Option<image::GrayImage> {
            self.cover.then(|| image::GrayImage::from_pixel(width, height, image::Luma([119])))
        }
    }
    fn book_app(cover: bool) -> App {
        let info = DeviceInfo { name: "test".into(), width: 600, height: 800, dpi: 150 };
        App::new(
            &info,
            Theme::for_dpi(150),
            Box::new(BookScreen {
                touches: Rc::new(Cell::new(0)),
                backgrounds: Rc::new(Cell::new(0)),
                saves: Rc::new(Cell::new(0)),
                cover,
            }),
        )
    }
    fn tap(app: &mut App, p: crate::geom::Point) {
        app.on_gesture(Gesture::Down(p));
        app.on_gesture(Gesture::Tap(p));
    }

    #[test]
    fn ssh_exit_dialog_captures_input_and_dismissal_cancels_exit() {
        let mut app = book_app(false);
        app.frame();
        app.show_exit_prompt();
        assert!(!app.wants_exit());
        assert!(!app.frame().is_empty());
        // Background work and persistence keep running under the modal.
        app.on_background();
        app.after_present();
        assert!(app.on_text_input(&TextInput::Insert("q".into())));
        assert!(!app.wants_exit());
        // Close button, outside tap, and Escape each cancel the exit.
        let close = crate::geom::Point::new(
            600 - app.theme.margin - app.theme.mm(2.0),
            800 - app.theme.mm(63.0) + app.theme.mm(6.5),
        );
        for p in [close, crate::geom::Point::new(300, 100)] {
            tap(&mut app, p);
            assert!(app.exit_prompt.is_none());
            assert!(!app.wants_exit());
            app.show_exit_prompt();
        }
        app.on_sleep();
        app.on_wake();
        assert!(app.exit_prompt.is_some());
        assert!(app.on_text_input(&TextInput::Dismiss));
        assert!(app.exit_prompt.is_none());
        assert!(!app.wants_exit());
        app.show_exit_prompt();
        let exit = crate::geom::Point::new(300, 800 - app.theme.mm(12.0) + app.theme.button_h / 2);
        tap(&mut app, exit);
        assert!(app.wants_exit());
        assert!(!app.stop_ssh_on_exit);
        assert!(app.on_quit());
    }

    #[test]
    fn stop_ssh_exit_saves_first_and_keeps_the_dialog_on_save_failure() {
        struct SavesOnce(bool);
        impl Screen for SavesOnce {
            fn draw(&mut self, _: &mut DrawCx) {}
            fn on_gesture(&mut self, _: Gesture, _: &mut EventCx) -> Action {
                Action::None
            }
            fn on_quit(&mut self) -> anyhow::Result<()> {
                anyhow::ensure!(std::mem::replace(&mut self.0, true), "Couldn't save your place");
                Ok(())
            }
        }
        let mut app = book_app(false);
        app.stack = vec![Box::new(SavesOnce(false))];
        app.show_exit_prompt();
        let stop = crate::geom::Point::new(300, 800 - app.theme.mm(24.0) + app.theme.button_h / 2);
        tap(&mut app, stop);
        assert!(app.wants_exit() && app.stop_ssh_on_exit);
        assert!(!app.frame().is_empty(), "show Stopping SSH before the save and stop");
        assert!(!app.on_quit());
        assert!(!app.wants_exit() && !app.stop_ssh_on_exit);
        assert!(app.exit_prompt.is_some());
        // Retrying the same choice is enabled after a failed save.
        tap(&mut app, stop);
        assert!(app.wants_exit() && app.stop_ssh_on_exit);
        assert!(app.on_quit());
        assert!(!app.stop_ssh_on_exit);
    }

    #[test]
    fn full_updates_keep_flash_strength_and_do_not_swallow_stronger_local_updates() {
        let mut app = book_app(false);
        let bounds = app.canvas.bounds();
        app.refresh_all(RefreshMode::Page);
        app.forced.push(Update { rect: bounds, mode: RefreshMode::Full });
        app.forced.push(Update { rect: Rect::new(0, 0, 16, 16), mode: RefreshMode::Ui });
        assert_eq!(app.frame(), vec![Update { rect: bounds, mode: RefreshMode::Full }]);
        assert!(!app.diff.changed(&app.canvas).any());
        app.refresh_all(RefreshMode::Page);
        let local = Update { rect: Rect::new(0, 0, 16, 16), mode: RefreshMode::Full };
        app.forced.push(local);
        assert_eq!(app.frame(), vec![Update { rect: bounds, mode: RefreshMode::Page }, local]);
    }

    #[test]
    fn unchanged_gestures_and_background_wakeups_do_not_draw() {
        let mut app = book_app(false);
        app.frame();
        let p = crate::geom::Point::new(500, 400);
        for g in [Gesture::Down(p), Gesture::Move(p), Gesture::Cancel(p), Gesture::Tap(p)] {
            app.on_gesture(g);
            assert!(!app.needs_redraw);
            assert!(app.frame().is_empty());
        }
        app.on_background();
        assert!(!app.needs_redraw);
        app.with_screen(|_, cx| {
            cx.redraw();
            Action::None
        });
        assert!(app.needs_redraw);
        app.frame();
        app.with_screen(|_, cx| {
            cx.refresh(Rect::new(0, 0, 20, 20), RefreshMode::Ui);
            Action::None
        });
        assert_eq!(app.frame(), vec![Update { rect: Rect::new(0, 0, 20, 20), mode: RefreshMode::Ui }]);
    }
    #[test]
    fn touch_release_cleans_fast_debt_without_a_screen_change() {
        let mut app = book_app(false);
        app.frame();
        let p = crate::geom::Point::new(300, 400);
        let rect = Rect::new(0, 0, 20, 20);
        app.on_gesture(Gesture::Down(p));
        app.fast_debt.push(rect);
        app.on_gesture(Gesture::Move(p));
        assert!(app.frame().is_empty());
        app.on_gesture(Gesture::End(p));
        assert_eq!(app.frame(), vec![Update { rect, mode: RefreshMode::Ui }]);
        assert!(app.fast_debt.is_empty());
    }
    #[test]
    fn toasts_invalidate_on_show_dismiss_and_expiry() {
        let mut app = book_app(false);
        app.frame();
        let show = |app: &mut App| {
            app.with_screen(|_, cx| {
                cx.toast("Saved", None);
                Action::None
            });
            assert!(!app.frame().is_empty());
        };
        show(&mut app);
        let p = app.toast_rect.center();
        app.on_gesture(Gesture::Down(p));
        assert!(!app.needs_redraw);
        app.on_gesture(Gesture::Tap(p));
        assert!(app.toast.is_none());
        assert!(!app.frame().is_empty());
        show(&mut app);
        app.on_tick(app.toast.as_ref().unwrap().until);
        assert!(app.toast.is_none());
        assert!(!app.frame().is_empty());
    }
    #[test]
    fn usb_controls_keep_immediate_press_and_cancel_feedback() {
        let mut app = book_app(false);
        app.frame();
        app.on_usb(true);
        app.frame();
        let p = crate::geom::Point::new(300, 800 - app.theme.mm(12.0) + app.theme.button_h / 2);
        app.on_gesture(Gesture::Down(p));
        assert!(app.frame().iter().any(|u| u.mode == RefreshMode::Fast));
        app.on_gesture(Gesture::Cancel(p));
        assert!(app.frame().iter().any(|u| u.mode == RefreshMode::Ui));
        assert!(app.usb_prompt.is_some());
        app.on_gesture(Gesture::Down(p));
        app.frame();
        app.on_gesture(Gesture::Tap(p));
        assert!(!app.frame().is_empty());
        assert!(app.usb_prompt.is_none());
    }
    #[test]
    fn stack_changes_invalidate_without_a_screen_request() {
        let mut app = book_app(false);
        app.frame();
        app.with_screen(|_, _| {
            Action::Push(Box::new(TimedScreen {
                ticks: Rc::new(Cell::new(0)),
                backgrounds: Rc::new(Cell::new(0)),
                due: Instant::now() + Duration::from_secs(60),
            }))
        });
        assert!(app.needs_redraw);
        assert!(!app.frame().is_empty());
        app.with_screen(|_, _| Action::Pop);
        assert!(app.needs_redraw);
        assert!(!app.frame().is_empty());
    }
    #[test]
    fn covers_do_not_replace_the_awake_frame_and_native_is_a_safe_fallback() {
        let mut app = book_app(true);
        app.frame();
        app.on_sleep();
        assert!(app.draw_sleep_cover(crate::store::SleepScreen::Cover));
        assert_eq!(app.canvas.gray_data()[0], 119);
        assert!(app.frame().is_empty());
        app.on_wake();
        assert_eq!(app.frame(), vec![Update { rect: app.canvas.bounds(), mode: RefreshMode::Full }]);
        assert_eq!(app.canvas.gray_data()[0], 51);
        assert!(!app.draw_sleep_cover(crate::store::SleepScreen::Amazon));
        assert!(!book_app(false).draw_sleep_cover(crate::store::SleepScreen::Cover));
    }
    #[test]
    fn landscape_sleep_cover_is_drawn_turned_so_it_stands_upright() {
        struct Marked;
        impl Screen for Marked {
            fn draw(&mut self, _: &mut DrawCx) {}
            fn on_gesture(&mut self, _: Gesture, _: &mut EventCx) -> Action {
                Action::None
            }
            fn sleep_cover(&self, width: u32, height: u32) -> Option<image::GrayImage> {
                assert_eq!((width, height), (600, 800), "covers are made for the portrait panel");
                let mut cover = image::GrayImage::from_pixel(width, height, image::Luma([119]));
                cover.put_pixel(0, 0, image::Luma([0]));
                Some(cover)
            }
        }
        let info = DeviceInfo { name: "test".into(), width: 800, height: 600, dpi: 150 };
        let mut app = App::new(&info, Theme::for_dpi(150), Box::new(Marked));
        app.set_orientation(Orientation::Landscape);
        assert!(app.draw_sleep_cover(crate::store::SleepScreen::Cover));
        // The panel's top-left pixel shows the canvas's top-right one.
        let mut panel = vec![255u8; 600 * 800];
        app.canvas.copy_gray_rotated(app.canvas.bounds(), &mut panel, 600);
        assert_eq!(panel[0], 0);
        assert_eq!(panel[1], 119);
    }

    #[test]
    fn turning_the_screen_saves_and_rebuilds_the_screens_at_the_new_size() {
        struct Turns(Rc<Cell<usize>>);
        impl Screen for Turns {
            fn draw(&mut self, _: &mut DrawCx) {}
            fn on_gesture(&mut self, g: Gesture, _: &mut EventCx) -> Action {
                if matches!(g, Gesture::Tap(_)) { Action::Orientation(Orientation::Landscape) } else { Action::None }
            }
            fn on_quit(&mut self) -> anyhow::Result<()> {
                self.0.set(self.0.get() + 1);
                Ok(())
            }
        }
        let saves = Rc::new(Cell::new(0));
        let info = DeviceInfo { name: "test".into(), width: 600, height: 800, dpi: 150 };
        let mut app = App::new(&info, Theme::for_dpi(150), Box::new(Turns(saves.clone())));
        app.frame();
        tap(&mut app, crate::geom::Point::new(300, 400));
        assert_eq!(app.take_orientation_request(), Some(Orientation::Landscape));
        assert!(app.take_orientation_request().is_none());
        assert!(app.prepare_relayout());
        assert_eq!(saves.get(), 1, "positions are saved before the screens are rebuilt");
        let turned = DeviceInfo { width: 800, height: 600, ..info };
        app.relayout(&turned, Orientation::Landscape, |_, rect| {
            assert_eq!(rect, Rect::new(0, 0, 800, 600));
            vec![Box::new(Turns(saves.clone())), Box::new(Turns(saves.clone()))]
        });
        assert_eq!(app.stack.len(), 2);
        assert_eq!(app.canvas.bounds(), Rect::new(0, 0, 800, 600));
        assert_eq!(app.frame(), vec![Update { rect: Rect::new(0, 0, 800, 600), mode: RefreshMode::Full }]);
    }

    #[test]
    fn late_wake_cleanup_rewrites_unchanged_pixels_and_cancels_on_resleep() {
        let mut app = book_app(false);
        app.frame();
        app.on_sleep();
        app.on_wake();
        app.frame();
        let due = app.wake_cleanup.unwrap();
        app.on_tick(due);
        assert_eq!(app.frame(), vec![Update { rect: Rect::new(0, 719, 600, 81), mode: RefreshMode::Ui }]);
        assert!(app.wake_cleanup.is_none());
        app.on_sleep();
        app.on_wake();
        app.on_sleep();
        app.schedule_wake_cleanup();
        assert!(app.deadline().is_none());
        assert!(app.wake_cleanup.is_none());
    }
    #[test]
    fn usb_is_modal_but_keeps_worker_results_and_saves_running() {
        let touches = Rc::new(Cell::new(0));
        let backgrounds = Rc::new(Cell::new(0));
        let saves = Rc::new(Cell::new(0));
        let info = DeviceInfo { name: "test".into(), width: 600, height: 800, dpi: 150 };
        let mut app = App::new(
            &info,
            Theme::for_dpi(150),
            Box::new(BookScreen {
                touches: touches.clone(),
                backgrounds: backgrounds.clone(),
                saves: saves.clone(),
                cover: false,
            }),
        );
        app.on_usb(true);
        app.on_background();
        app.frame();
        app.after_present();
        assert_eq!(backgrounds.get(), 1);
        assert_eq!(saves.get(), 1);
        let p = crate::geom::Point::new(300, 400);
        app.on_gesture(Gesture::Down(p));
        app.on_gesture(Gesture::Tap(p)); // outside the sheet: dismiss, no page turn
        assert_eq!(touches.get(), 0);
        assert!(app.usb_prompt.is_none());
        app.on_usb(true); // duplicates must not reopen a dismissed prompt
        assert!(app.usb_prompt.is_none());
        app.on_usb(false);
        app.on_sleep();
        app.on_usb(true);
        assert!(app.frame().is_empty());
        app.on_wake();
        assert!(app.usb_prompt.is_some());
        app.on_usb(false);
        assert!(app.usb_prompt.is_none());
        app.on_usb(true);
        let exit = crate::geom::Point::new(300, 800 - app.theme.mm(24.0) + app.theme.button_h / 2);
        app.on_gesture(Gesture::Down(exit));
        app.on_gesture(Gesture::Tap(exit));
        assert!(app.wants_exit());
        assert_eq!(touches.get(), 0);
    }
    struct TimedScreen {
        ticks: Rc<Cell<usize>>,
        backgrounds: Rc<Cell<usize>>,
        due: Instant,
    }
    impl Screen for TimedScreen {
        fn draw(&mut self, _: &mut DrawCx) {}
        fn on_gesture(&mut self, _: Gesture, _: &mut EventCx) -> Action {
            Action::None
        }
        fn deadline(&self) -> Option<Instant> {
            Some(self.due)
        }
        fn on_tick(&mut self, _: &mut EventCx) -> Action {
            self.ticks.set(self.ticks.get() + 1);
            Action::None
        }
        fn on_background(&mut self, cx: &mut EventCx) {
            self.backgrounds.set(self.backgrounds.get() + 1);
            cx.refresh(cx.screen, RefreshMode::Full);
        }
    }
    #[test]
    fn sleep_suspends_timers_and_worker_results_until_wake() {
        let ticks = Rc::new(Cell::new(0));
        let backgrounds = Rc::new(Cell::new(0));
        let info = DeviceInfo { name: "test".into(), width: 600, height: 800, dpi: 150 };
        let screen = TimedScreen { ticks: ticks.clone(), backgrounds: backgrounds.clone(), due: Instant::now() };
        let mut app = App::new(&info, Theme::for_dpi(150), Box::new(screen));
        app.on_sleep();
        assert!(app.deadline().is_none());
        app.on_tick(Instant::now());
        app.on_background();
        assert_eq!(ticks.get(), 0);
        assert_eq!(backgrounds.get(), 0);
        app.on_wake();
        app.on_tick(Instant::now());
        assert_eq!(ticks.get(), 1);
        assert_eq!(backgrounds.get(), 1);
        let updates = app.frame();
        assert_eq!(updates.len(), 1, "wake and layout completion share one full refresh");
        assert_eq!(updates[0].mode, RefreshMode::Full);
    }
}
