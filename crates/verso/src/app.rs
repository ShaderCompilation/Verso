//! The Phase 1 demo app: a title, a button with a tap counter, a line showing
//! the last gesture, and an exit button. It exists to prove the platform layer
//! end to end; the real screens replace it from Phase 3 on.

use crate::geom::{Point, Rect};
use crate::gfx::{Canvas, HAlign, Icon, IconRenderer, TextRenderer, TextStyle, Weight};
use crate::platform::{DeviceInfo, RefreshMode, Update};
use crate::ui::gesture::{Direction, Gesture};

const BLACK: u8 = 0;
const WHITE: u8 = 255;
const MUTED: u8 = 96;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Button {
    Tap,
    Exit,
}

struct Layout {
    title: Rect,
    subtitle: Rect,
    tap: Rect,
    counter: Rect,
    gesture: Rect,
    exit: Rect,
    footer: Rect,
}

pub struct App {
    canvas: Canvas,
    text: TextRenderer,
    icons: IconRenderer,
    dpi: f32,
    footer: String,
    layout: Layout,
    counter: u32,
    pressed: Option<Button>,
    last_gesture: String,
    asleep: bool,
    exit: bool,
    updates: Vec<Update>,
}

impl App {
    pub fn new(info: &DeviceInfo) -> Self {
        let dpi = info.dpi as f32;
        let mut app = Self {
            canvas: Canvas::new(info.width, info.height),
            text: TextRenderer::new(),
            icons: IconRenderer::default(),
            dpi,
            footer: format!("{} · {}×{} · {} dpi", info.name, info.width, info.height, info.dpi),
            layout: Layout {
                title: Rect::default(),
                subtitle: Rect::default(),
                tap: Rect::default(),
                counter: Rect::default(),
                gesture: Rect::default(),
                exit: Rect::default(),
                footer: Rect::default(),
            },
            counter: 0,
            pressed: None,
            last_gesture: "Try tapping, holding or swiping anywhere".into(),
            asleep: false,
            exit: false,
            updates: Vec::new(),
        };
        app.layout = app.compute_layout(info.width as i32, info.height as i32);
        app
    }

    pub fn canvas(&self) -> &Canvas {
        &self.canvas
    }

    /// Screen areas changed since the last call, with their refresh intent.
    pub fn take_updates(&mut self) -> Vec<Update> {
        std::mem::take(&mut self.updates)
    }

    pub fn wants_exit(&self) -> bool {
        self.exit
    }

    pub fn on_sleep(&mut self) {
        self.asleep = true;
        self.pressed = None;
    }

    pub fn on_wake(&mut self) {
        self.asleep = false;
        self.redraw_all(RefreshMode::Full);
    }

    pub fn redraw_all(&mut self, mode: RefreshMode) {
        let l = &self.layout;
        let (title, subtitle, footer) = (l.title, l.subtitle, l.footer);
        self.canvas.fill_rect(self.canvas.bounds(), WHITE);
        let style = |pt: f32, weight, gray| TextStyle { size: self.pt(pt), weight, gray };
        let (title_style, sub_style, footer_style) = (
            style(44.0, Weight::SemiBold, BLACK),
            style(14.0, Weight::Regular, MUTED),
            style(8.0, Weight::Regular, MUTED),
        );
        self.text.draw(&mut self.canvas, "Verso", title_style, title, HAlign::Center);
        self.text.draw(&mut self.canvas, "hello, e-ink", sub_style, subtitle, HAlign::Center);
        let footer_text = self.footer.clone();
        self.text.draw(&mut self.canvas, &footer_text, footer_style, footer, HAlign::Center);
        self.draw_button(Button::Tap);
        self.draw_button(Button::Exit);
        self.draw_counter();
        self.draw_gesture();
        self.updates.push(Update { rect: self.canvas.bounds(), mode });
    }

    pub fn on_gesture(&mut self, g: Gesture) {
        if self.asleep {
            return;
        }
        match g {
            Gesture::Down(p) => {
                if let Some(b) = self.hit(p) {
                    self.pressed = Some(b);
                    self.refresh_button(b, RefreshMode::Fast);
                }
                return; // not a finished gesture; keep the last one on screen
            }
            Gesture::Tap(p) => {
                if let Some(b) = self.release() {
                    if self.hit(p) == Some(b) {
                        self.activate(b);
                    }
                }
                self.last_gesture = format!("Tap at {}, {}", p.x, p.y);
            }
            Gesture::LongPress(p) => {
                self.release();
                self.last_gesture = format!("Long-press at {}, {}", p.x, p.y);
            }
            Gesture::Swipe { dir, from, to } => {
                self.release();
                let d = match dir {
                    Direction::Left => "left",
                    Direction::Right => "right",
                    Direction::Up => "up",
                    Direction::Down => "down",
                };
                self.last_gesture = format!("Swipe {d}, {:.0} px", from.distance(to));
            }
            Gesture::Cancel => {
                self.release();
                self.last_gesture = "Cancelled (moved too slowly)".into();
            }
        }
        if !self.exit {
            self.draw_gesture();
            self.updates.push(Update { rect: self.layout.gesture, mode: RefreshMode::Ui });
        }
    }

    /// Un-presses the pressed button, if any, and returns it.
    fn release(&mut self) -> Option<Button> {
        let b = self.pressed.take()?;
        self.refresh_button(b, RefreshMode::Fast);
        Some(b)
    }

    fn activate(&mut self, b: Button) {
        match b {
            Button::Tap => {
                self.counter += 1;
                self.draw_counter();
                self.updates.push(Update { rect: self.layout.counter, mode: RefreshMode::Fast });
            }
            Button::Exit => self.exit = true,
        }
    }

    fn hit(&self, p: Point) -> Option<Button> {
        // Accept touches slightly outside the drawn button: fingers are imprecise.
        let pad = self.mm(2.0);
        [Button::Tap, Button::Exit].into_iter().find(|&b| self.button_rect(b).inset(-pad).contains(p))
    }

    fn button_rect(&self, b: Button) -> Rect {
        match b {
            Button::Tap => self.layout.tap,
            Button::Exit => self.layout.exit,
        }
    }

    fn refresh_button(&mut self, b: Button, mode: RefreshMode) {
        self.draw_button(b);
        self.updates.push(Update { rect: self.button_rect(b), mode });
    }

    fn draw_button(&mut self, b: Button) {
        let rect = self.button_rect(b);
        let pressed = self.pressed == Some(b);
        let (icon, label, pt) = match b {
            Button::Tap => (Icon::Plus, "Tap me", 16.0),
            Button::Exit => (Icon::Exit, "Exit", 12.0),
        };
        let radius = self.mm(3.0) as f32;
        self.canvas.fill_rect(rect, WHITE);
        let fg = if pressed {
            self.canvas.fill_rounded_rect(rect, radius, BLACK);
            WHITE
        } else {
            self.canvas.stroke_rounded_rect(rect, radius, self.mm(0.5) as f32, BLACK);
            BLACK
        };
        // Icon and label, centred together.
        let style = TextStyle { size: self.pt(pt), weight: Weight::SemiBold, gray: fg };
        let (tw, _) = self.text.measure(label, style, None);
        let icon_size = self.pt(pt * 1.1) as i32;
        let gap = self.mm(2.0);
        let total = icon_size + gap + tw.ceil() as i32;
        let x = rect.center().x - total / 2;
        let icon_rect = Rect::new(x, rect.center().y - icon_size / 2, icon_size, icon_size);
        self.icons.draw(&mut self.canvas, icon, icon_rect, fg);
        let label_rect = Rect::new(icon_rect.right() + gap, rect.y, rect.right() - icon_rect.right() - gap, rect.h);
        self.text.draw(&mut self.canvas, label, style, label_rect, HAlign::Left);
    }

    fn draw_counter(&mut self) {
        let rect = self.layout.counter;
        let text = match self.counter {
            1 => "Tapped once".to_string(),
            n => format!("Tapped {n} times"),
        };
        self.canvas.fill_rect(rect, WHITE);
        let style = TextStyle { size: self.pt(16.0), weight: Weight::Regular, gray: BLACK };
        self.text.draw(&mut self.canvas, &text, style, rect, HAlign::Center);
    }

    fn draw_gesture(&mut self) {
        let rect = self.layout.gesture;
        self.canvas.fill_rect(rect, WHITE);
        let style = TextStyle { size: self.pt(10.0), weight: Weight::Regular, gray: MUTED };
        let text = self.last_gesture.clone();
        self.text.draw(&mut self.canvas, &text, style, rect, HAlign::Center);
    }

    fn compute_layout(&self, w: i32, h: i32) -> Layout {
        let margin = self.mm(8.0);
        let line = |pt: f32| (self.pt(pt) * 1.6).ceil() as i32;
        let full_width = |y: i32, height: i32| Rect::new(margin, y, w - 2 * margin, height);

        let title = full_width(h * 14 / 100, line(44.0));
        let subtitle = full_width(title.bottom(), line(14.0));
        let tap = Rect::centered(Point::new(w / 2, h * 45 / 100), self.mm(60.0), self.mm(16.0));
        let counter = full_width(tap.bottom() + self.mm(5.0), line(16.0));
        let gesture = full_width(counter.bottom() + self.mm(2.0), line(10.0));
        let exit = Rect::centered(Point::new(w / 2, h - self.mm(28.0)), self.mm(40.0), self.mm(12.0));
        let footer = full_width(h - self.mm(4.0) - line(8.0), line(8.0));
        Layout { title, subtitle, tap, counter, gesture, exit, footer }
    }

    /// Millimetres to pixels.
    fn mm(&self, v: f32) -> i32 {
        (v * self.dpi / 25.4).round() as i32
    }

    /// Typographic points to pixels.
    fn pt(&self, v: f32) -> f32 {
        (v * self.dpi / 72.0).round()
    }
}
