//! A multi-line box that wraps its text, for notes. Editing shares the
//! single-line field's grapheme-safe logic, and the box scrolls to keep the
//! cursor in view. There is no newline key; line breaks already in a value
//! (an imported note) are kept.
use unicode_segmentation::UnicodeSegmentation;

use super::text_field;
use crate::geom::Rect;
use crate::gfx::HAlign;
use crate::platform::TextInput;
use crate::ui::gesture::Gesture;
use crate::ui::theme::{INK, INK2};
use crate::ui::widget::{DrawCx, Press, PressEvent};

#[derive(Default)]
pub struct TextArea {
    value: String,
    cursor: usize,
    rect: Rect,
    press: Press,
    /// The first wrapped line shown.
    scroll: usize,
    pub focused: bool,
    /// Shown while empty and unfocused.
    pub placeholder: &'static str,
    /// The most characters the text may hold; longer inserts are cut short.
    pub limit: usize,
}

impl TextArea {
    pub fn new(placeholder: &'static str, limit: usize) -> Self {
        Self { placeholder, limit, ..Self::default() }
    }
    pub fn value(&self) -> &str {
        &self.value
    }
    /// Replaces the text (cut to the limit) and puts the cursor at its end.
    pub fn set_value(&mut self, text: &str) {
        self.value.clear();
        self.cursor = 0;
        self.scroll = 0;
        let mut count = 0;
        for g in text.graphemes(true) {
            count += g.chars().count();
            if count > self.limit {
                break;
            }
            self.value.push_str(g);
        }
        self.cursor = self.value.len();
    }
    /// True when no more characters fit.
    pub fn full(&self) -> bool {
        self.value.chars().count() >= self.limit
    }
    pub fn place(&mut self, rect: Rect) {
        self.rect = rect;
    }
    pub fn rect(&self) -> Rect {
        self.rect
    }
    pub fn on_gesture(&mut self, g: &Gesture) -> bool {
        matches!(self.press.on_gesture(g, self.rect), Some(PressEvent::Tap | PressEvent::LongPressEnd))
    }
    /// Returns true only when the text changed.
    pub fn edit(&mut self, input: &TextInput) -> bool {
        if let TextInput::Insert(text) = input {
            let room = self.limit.saturating_sub(self.value.chars().count());
            let mut taken = String::new();
            let mut count = 0;
            for g in text.graphemes(true) {
                count += g.chars().filter(|c| !c.is_control()).count();
                if count > room {
                    break;
                }
                taken.push_str(g);
            }
            return text_field::edit(&mut self.value, &mut self.cursor, &TextInput::Insert(taken));
        }
        text_field::edit(&mut self.value, &mut self.cursor, input)
    }
    pub fn draw(&mut self, cx: &mut DrawCx) {
        let t = cx.theme;
        cx.canvas.stroke_rounded_rect(self.rect, t.radius, t.border as f32, INK);
        let inner = self.rect.inset(t.mm(2.0));
        let style = t.body;
        let line_h = style.line_height() as i32;
        if self.value.is_empty() && !self.focused {
            cx.line(self.placeholder, style.gray(INK2), Rect { h: line_h, ..inner }, HAlign::Left);
            return;
        }
        // Leave room at the right edge for the cursor.
        let starts = cx.text.line_starts(&self.value, style, inner.w - t.mm(1.0));
        let line = starts.partition_point(|&s| s <= self.cursor).saturating_sub(1);
        let visible = (inner.h / line_h).max(1) as usize;
        if line < self.scroll {
            self.scroll = line;
        } else if line >= self.scroll + visible {
            self.scroll = line + 1 - visible;
        }
        self.scroll = self.scroll.min(starts.len() - 1);
        // A wrapped (not hard-broken) line doesn't show the space it broke at.
        let shown_start = |k: usize| {
            let s = starts[k];
            if k > 0 && !self.value[..s].ends_with('\n') {
                s + self.value[s..].len() - self.value[s..].trim_start_matches(' ').len()
            } else {
                s
            }
        };
        for k in self.scroll..starts.len().min(self.scroll + visible) {
            let end = starts.get(k + 1).copied().unwrap_or(self.value.len());
            let start = shown_start(k).min(end);
            let text = self.value[start..end].trim_end();
            let y = inner.y + (k - self.scroll) as i32 * line_h;
            cx.line(text, style, Rect::new(inner.x, y, inner.w, line_h), HAlign::Left);
        }
        if self.focused {
            let start = shown_start(line).min(self.cursor);
            let x = inner.x + cx.text.width(&self.value[start..self.cursor], style).ceil() as i32;
            let y = inner.y + (line - self.scroll) as i32 * line_h;
            cx.fill(Rect::new(x.min(inner.right() - t.hairline), y + line_h / 8, t.hairline, line_h - line_h / 4), INK);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gfx::{Canvas, IconRenderer, TextRenderer};
    use crate::ui::theme::Theme;
    #[test]
    fn edits_graphemes_respects_the_limit_and_wraps_long_text() {
        let mut a = TextArea::new("Note", 10);
        a.edit(&TextInput::Insert("Яe\u{301}Z".into()));
        a.edit(&TextInput::Left);
        a.edit(&TextInput::Backspace);
        assert_eq!(a.value(), "ЯZ");
        a.edit(&TextInput::Insert("0123456789abc".into()));
        assert_eq!(a.value().chars().count(), 10, "inserts are cut at the limit");
        assert!(a.full());
        assert!(!a.edit(&TextInput::Insert("x".into())));
        a.set_value("line one\nline two");
        assert_eq!(a.value(), "line one\nl", "set_value keeps breaks and cuts at the limit");

        let t = Theme::for_dpi(300);
        let mut text = TextRenderer::new();
        let long = "Calm reading notes wrap across several lines. ".repeat(40);
        let starts = text.line_starts(&long, t.body, 600);
        assert!(starts.len() > 10);
        assert!(starts.windows(2).all(|w| w[0] < w[1]) && starts.iter().all(|&s| long.is_char_boundary(s)));
        assert_eq!(text.line_starts("", t.body, 600), vec![0]);
        assert_eq!(text.line_starts("a\n\nb", t.body, 600).len(), 3);

        let mut a = TextArea::new("Note", 4000);
        a.edit(&TextInput::Insert(long.clone()));
        a.focused = true;
        a.place(Rect::new(0, 0, 700, 400));
        let mut canvas = Canvas::new(700, 400);
        let mut icons = IconRenderer::default();
        a.draw(&mut DrawCx::new(&mut canvas, &mut text, &mut icons, &t, &mut Vec::new()));
        assert!(a.scroll > 0, "the cursor at the end scrolls the box");
        a.edit(&TextInput::Home);
        a.draw(&mut DrawCx::new(&mut canvas, &mut text, &mut icons, &t, &mut Vec::new()));
        assert_eq!(a.scroll, 0);
    }
}
