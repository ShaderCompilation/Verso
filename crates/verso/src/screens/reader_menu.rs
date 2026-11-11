//! The centre-tap menu. Controls use the shared toolkit.
use crate::geom::Rect;
use crate::gfx::{HAlign, Icon};
use crate::platform::Frontlight;
use crate::ui::gesture::Gesture;
use crate::ui::theme::{INK, INK2, PAPER, Theme};
use crate::ui::widget::DrawCx;
use crate::ui::widgets::{Button, ButtonKind, IconButton, Slider, SliderEvent, TopBar};

pub enum MenuEvent {
    Books,
    Close,
    Contents,
    Text,
    Page(usize),
    Chapter(isize),
    Light(Frontlight),
}
pub struct ReaderMenu {
    top: TopBar,
    panel: Rect,
    info: Rect,
    pub progress: Slider,
    prev: IconButton,
    next: IconButton,
    light: Option<Slider>,
    warmth: Option<Slider>,
    contents: Button,
    text: Button,
    outside: bool,
}
impl ReaderMenu {
    pub fn new(t: &Theme, screen: Rect, title: &str, page: usize, pages: usize, light: Option<Frontlight>) -> Self {
        let mut top = TopBar::new(title, true);
        top.serif = false;
        top.show_battery = true;
        top.place(screen, t);
        let panel = screen.split_bottom(t.mm(61.0)).0;
        let m = t.margin;
        let info = Rect::new(m, panel.y + t.mm(3.0), screen.w - 2 * m, t.mm(7.0));
        let mut progress = Slider::new(0.0, pages.saturating_sub(1) as f32, 1.0, page as f32);
        progress
            .place(Rect::new(m + t.target, info.bottom() + t.mm(3.0), screen.w - 2 * m - 2 * t.target, t.mm(6.0)), t);
        let mut prev = IconButton::new(Icon::ChevronLeft);
        prev.place_at(crate::geom::Point::new(m + t.target / 2, progress.rect().center().y), t);
        let mut next = IconButton::new(Icon::ChevronRight);
        next.place_at(crate::geom::Point::new(screen.w - m - t.target / 2, progress.rect().center().y), t);
        let make_slider = |y, value| {
            let mut s = Slider::new(0.0, 24.0, 1.0, value as f32);
            s.place(Rect::new(m + t.mm(20.0), y, screen.w - 2 * m - t.mm(20.0), t.mm(6.0)), t);
            s
        };
        let light_slider = light.map(|v| make_slider(panel.y + t.mm(27.0), v.brightness));
        let warmth = light.and_then(|v| v.warmth.map(|w| make_slider(panel.y + t.mm(39.0), w)));
        let width = (screen.w - 2 * m - t.mm(4.0)) / 2;
        let mut contents = Button::new("Contents", ButtonKind::Secondary).with_icon(Icon::List);
        let mut text = Button::new("Text", ButtonKind::Secondary).with_icon(Icon::TextSize);
        contents.place(Rect::new(m, screen.bottom() - t.mm(13.0), width, t.button_h), t);
        text.place(Rect::new(m + width + t.mm(4.0), screen.bottom() - t.mm(13.0), width, t.button_h), t);
        Self { top, panel, info, progress, prev, next, light: light_slider, warmth, contents, text, outside: false }
    }
    pub fn draw(&mut self, cx: &mut DrawCx, chapter: &str, pages: usize) {
        let t = cx.theme;
        cx.fill(self.top.rect(), PAPER);
        self.top.draw(cx);
        cx.fill(self.panel, PAPER);
        cx.hline(self.panel.y, 0, self.panel.right(), t.border, INK);
        cx.line(chapter, t.body, Rect { w: self.info.w * 3 / 4, ..self.info }, HAlign::Left);
        cx.line(
            &format!("{} / {}", self.progress.value as usize + 1, pages),
            t.small.gray(INK2),
            self.info,
            HAlign::Right,
        );
        self.progress.draw(cx);
        self.prev.draw(cx);
        self.next.draw(cx);
        for (label, slider) in [("Light", &self.light), ("Warmth", &self.warmth)] {
            if let Some(s) = slider {
                let r = s.rect();
                cx.line(label, t.small, Rect::new(t.margin, r.y, t.mm(18.0), r.h), HAlign::Left);
                s.draw(cx);
            }
        }
        self.contents.draw(cx);
        self.text.draw(cx);
    }
    pub fn on_gesture(&mut self, g: &Gesture) -> Option<MenuEvent> {
        if let Gesture::Down(p) = g {
            self.outside = !self.panel.contains(*p) && !self.top.rect().contains(*p);
        }
        if self.outside {
            return matches!(g, Gesture::Tap(_)).then_some(MenuEvent::Close);
        }
        if self.top.on_gesture(g) {
            return Some(MenuEvent::Books);
        }
        if self.contents.on_gesture(g) {
            return Some(MenuEvent::Contents);
        }
        if self.text.on_gesture(g) {
            return Some(MenuEvent::Text);
        }
        if self.prev.on_gesture(g) {
            return Some(MenuEvent::Chapter(-1));
        }
        if self.next.on_gesture(g) {
            return Some(MenuEvent::Chapter(1));
        }
        if let Some(SliderEvent::Released(v)) = self.progress.on_gesture(g) {
            return Some(MenuEvent::Page(v as usize));
        }
        let brightness_changed = self.light.as_mut().is_some_and(|s| s.on_gesture(g).is_some());
        let warmth_changed = self.warmth.as_mut().is_some_and(|s| s.on_gesture(g).is_some());
        if brightness_changed || warmth_changed {
            return Some(MenuEvent::Light(Frontlight {
                brightness: self.light.as_ref().map_or(0, |s| s.value as u8),
                warmth: self.warmth.as_ref().map(|s| s.value as u8),
            }));
        }
        None
    }
}
