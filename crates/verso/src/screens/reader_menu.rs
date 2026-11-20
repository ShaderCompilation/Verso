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
    Search,
    Highlights,
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
    search: Button,
    text: Button,
    highlights: Button,
    outside: bool,
}
impl ReaderMenu {
    pub fn new(t: &Theme, screen: Rect, title: &str, page: usize, pages: usize, light: Option<Frontlight>) -> Self {
        let mut top = TopBar::new(title, true);
        top.serif = false;
        top.show_battery = true;
        top.place(screen, t);
        // Landscape puts the two light sliders side by side, so more of the
        // short page stays visible above the panel.
        let wide = screen.w > screen.h;
        let panel = screen.split_bottom(t.mm(if wide { 49.0 } else { 61.0 })).0;
        let m = t.margin;
        let info = Rect::new(m, panel.y + t.mm(3.0), screen.w - 2 * m, t.mm(7.0));
        let mut progress = Slider::new(0.0, pages.saturating_sub(1) as f32, 1.0, page as f32);
        progress
            .place(Rect::new(m + t.target, info.bottom() + t.mm(3.0), screen.w - 2 * m - 2 * t.target, t.mm(6.0)), t);
        let mut prev = IconButton::new(Icon::ChevronLeft);
        prev.place_at(crate::geom::Point::new(m + t.target / 2, progress.rect().center().y), t);
        let mut next = IconButton::new(Icon::ChevronRight);
        next.place_at(crate::geom::Point::new(screen.w - m - t.target / 2, progress.rect().center().y), t);
        let (light_slider, warmth) = match light {
            Some(v) => {
                let (l, w) = light_sliders(t, screen, panel, v);
                (Some(l), w)
            }
            None => (None, None),
        };
        let gap = t.mm(3.0);
        let width = (screen.w - 2 * m - 3 * gap) / 4;
        let y = screen.bottom() - t.mm(13.0);
        let mut contents = Button::new("Contents", ButtonKind::Secondary);
        let mut search = Button::new("Search", ButtonKind::Secondary);
        let mut text = Button::new("Text", ButtonKind::Secondary);
        contents.place(Rect::new(m, y, width, t.button_h), t);
        search.place(Rect::new(m + width + gap, y, width, t.button_h), t);
        text.place(Rect::new(screen.w - m - width, y, width, t.button_h), t);
        let mut highlights = Button::new("Highlights", ButtonKind::Secondary);
        highlights.place(Rect::new(m + 2 * (width + gap), y, width, t.button_h), t);
        Self {
            highlights,
            top,
            panel,
            info,
            progress,
            prev,
            next,
            light: light_slider,
            warmth,
            contents,
            search,
            text,
            outside: false,
        }
    }
    /// Populates the light controls if the menu opened before an asynchronous
    /// startup frontlight reading arrived.
    pub fn set_frontlight_if_missing(&mut self, t: &Theme, screen: Rect, light: Frontlight) -> bool {
        if self.light.is_some() {
            return false;
        }
        let (light_slider, warmth) = light_sliders(t, screen, self.panel, light);
        self.light = Some(light_slider);
        self.warmth = warmth;
        true
    }
    pub fn draw(&mut self, cx: &mut DrawCx, chapter: &str, pages: usize, pages_ready: bool) {
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
        if pages_ready {
            self.progress.draw(cx);
        } else {
            cx.line("Laying out…", t.small.gray(INK2), self.progress.bounds(), HAlign::Center);
        }
        self.prev.draw(cx);
        self.next.draw(cx);
        for (label, slider) in [("Light", &self.light), ("Warmth", &self.warmth)] {
            if let Some(s) = slider {
                let r = s.rect();
                cx.line(label, t.small, Rect::new(r.x - t.mm(20.0), r.y, t.mm(18.0), r.h), HAlign::Left);
                s.draw(cx);
            }
        }
        self.contents.draw(cx);
        self.search.draw(cx);
        self.text.draw(cx);
        self.highlights.draw(cx);
    }
    pub fn on_gesture(&mut self, g: &Gesture, pages_ready: bool) -> Option<MenuEvent> {
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
        if self.search.on_gesture(g) {
            return Some(MenuEvent::Search);
        }
        if self.highlights.on_gesture(g) {
            return Some(MenuEvent::Highlights);
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
        if pages_ready && let Some(SliderEvent::Released(v)) = self.progress.on_gesture(g) {
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

fn light_sliders(t: &Theme, screen: Rect, panel: Rect, light: Frontlight) -> (Slider, Option<Slider>) {
    let m = t.margin;
    let wide = screen.w > screen.h;
    let make_slider = |row: Rect, value| {
        let mut s = Slider::new(0.0, 24.0, 1.0, value as f32);
        s.place(Rect::new(row.x + t.mm(20.0), row.y, row.w - t.mm(20.0), t.mm(6.0)), t);
        s
    };
    let full = Rect::new(m, panel.y + t.mm(27.0), screen.w - 2 * m, t.mm(6.0));
    let (light_row, warmth_row) = if light.warmth.is_some() && wide {
        let gap = t.mm(10.0);
        let w = (full.w - gap) / 2;
        (Rect { w, ..full }, Rect { x: full.right() - w, w, ..full })
    } else {
        (full, Rect { y: panel.y + t.mm(39.0), ..full })
    };
    (make_slider(light_row, light.brightness), light.warmth.map(|w| make_slider(warmth_row, w)))
}
