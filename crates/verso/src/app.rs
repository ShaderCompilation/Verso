//! The app: owns the canvas and the screen stack, turns gestures and timers
//! into redraws, and works out which parts of the display to refresh.

use std::time::Instant;

use crate::geom::Rect;
use crate::gfx::{Canvas, IconRenderer, TextRenderer};
use crate::platform::{DeviceInfo, Frontlight, RefreshMode, TextInput, Update};
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
    pub frontlight: Option<Frontlight>,
    light_request: Option<Frontlight>,
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
            frontlight: None,
            light_request: None,
        }
    }

    pub fn canvas(&self) -> &Canvas {
        &self.canvas
    }

    pub fn wants_exit(&self) -> bool {
        self.exit
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
        self.canvas.fill(PAPER);
        let mut hints = Vec::new();
        {
            let mut cx = DrawCx::new(&mut self.canvas, &mut self.text, &mut self.icons, &self.theme, &mut hints);
            if let Some(screen) = self.stack.last_mut() {
                screen.draw(&mut cx);
            }
            if let Some(toast) = &self.toast {
                self.toast_rect = toast.draw(&mut cx);
            }
        }
        let drawn = started.elapsed();

        let tiles = self.diff.changed(&self.canvas);
        let mut forced = std::mem::take(&mut self.forced);
        if !self.touch_down {
            forced.extend(self.fast_debt.drain(..).map(|rect| Update { rect, mode: RefreshMode::Ui }));
        }
        let updates = damage::plan(&tiles, self.canvas.bounds(), &hints, RefreshMode::Ui, &forced);
        self.diff.commit(&self.canvas, &tiles);
        for u in updates.iter().filter(|u| u.mode == RefreshMode::Fast) {
            if !self.fast_debt.iter().any(|d| d.union(&u.rect) == *d) {
                self.fast_debt.push(u.rect);
            }
        }
        log::trace!("frame: drawn in {drawn:?}, diffed in {:?}", started.elapsed() - drawn);
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
        }
        self.needs_redraw = true;
        if self.toast_touch {
            // Touches on the toast only dismiss it.
            if g.ends_touch() {
                self.toast_touch = false;
                if matches!(g, Gesture::Tap(_)) {
                    self.toast = None;
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
        [screen, self.toast.as_ref().map(|t| t.until)].into_iter().flatten().min()
    }

    pub fn on_tick(&mut self, now: Instant) {
        if self.asleep {
            return;
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
        self.stack.last().is_some_and(|s| s.has_pending_work())
    }

    pub fn run_pending_work(&mut self) {
        self.needs_redraw = true;
        self.with_screen(|screen, cx| screen.run_pending_work(cx));
    }

    pub fn on_background(&mut self) {
        if self.asleep {
            return;
        }
        self.needs_redraw = true;
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
    pub fn on_quit(&mut self) {
        for s in &mut self.stack {
            s.on_quit();
        }
    }
    pub fn light_request(&mut self) -> Option<Frontlight> {
        self.light_request.take()
    }

    pub fn on_sleep(&mut self) {
        if let Some(s) = self.stack.last_mut() {
            s.on_sleep();
        }
        self.asleep = true;
        self.touch_down = false;
    }

    pub fn on_wake(&mut self) {
        self.asleep = false;
        self.on_background();
        self.refresh_all(RefreshMode::Full);
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
        };
        let action = f(screen.as_mut(), &mut cx);
        match action {
            Action::None => {}
            Action::Push(screen) => self.stack.push(screen),
            Action::Pop => {
                self.stack.pop();
                if self.stack.is_empty() {
                    self.exit = true;
                }
            }
            Action::Exit => self.exit = true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, rc::Rc};
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
