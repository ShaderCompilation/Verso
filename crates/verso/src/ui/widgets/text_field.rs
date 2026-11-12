//! A single-line field. Cursor positions always lie on grapheme boundaries.
use unicode_segmentation::UnicodeSegmentation;

use crate::geom::Rect;
use crate::gfx::HAlign;
use crate::platform::TextInput;
use crate::ui::gesture::Gesture;
use crate::ui::theme::{INK, INK2};
use crate::ui::widget::{DrawCx, Press, PressEvent};

#[derive(Default)]
pub struct TextField {
    value: String,
    cursor: usize,
    rect: Rect,
    press: Press,
    pub focused: bool,
    /// Shown while empty and unfocused.
    pub placeholder: &'static str,
}

impl TextField {
    pub fn new(placeholder: &'static str) -> Self {
        Self { placeholder, ..Self::default() }
    }
    pub fn value(&self) -> &str {
        &self.value
    }
    pub fn place(&mut self, rect: Rect) {
        self.rect = rect;
    }
    pub fn on_gesture(&mut self, g: &Gesture) -> bool {
        matches!(self.press.on_gesture(g, self.rect), Some(PressEvent::Tap | PressEvent::LongPressEnd))
    }
    /// Returns true only when the text changed (cursor movement is still drawn).
    pub fn edit(&mut self, input: &TextInput) -> bool {
        let old = self.value.clone();
        let previous = || self.value[..self.cursor].grapheme_indices(true).next_back().map_or(0, |(i, _)| i);
        let next = || self.value[self.cursor..].graphemes(true).next().map_or(self.cursor, |g| self.cursor + g.len());
        match input {
            TextInput::Insert(text) => {
                let text: String = text.chars().filter(|c| !c.is_control()).collect();
                self.value.insert_str(self.cursor, &text);
                self.cursor += text.len();
                // An inserted combining mark/ZWJ may merge with adjacent text.
                self.cursor = self
                    .value
                    .grapheme_indices(true)
                    .map(|(i, _)| i)
                    .find(|&i| i >= self.cursor)
                    .unwrap_or(self.value.len());
            }
            TextInput::Backspace => {
                let start = previous();
                self.value.replace_range(start..self.cursor, "");
                self.cursor = start;
            }
            TextInput::Delete => {
                let end = next();
                self.value.replace_range(self.cursor..end, "");
            }
            TextInput::Left => self.cursor = previous(),
            TextInput::Right => self.cursor = next(),
            TextInput::Home => self.cursor = 0,
            TextInput::End => self.cursor = self.value.len(),
            TextInput::Clear => {
                self.value.clear();
                self.cursor = 0;
            }
            TextInput::Submit | TextInput::Dismiss => {}
        }
        self.cursor =
            self.value.grapheme_indices(true).map(|(i, _)| i).find(|&i| i >= self.cursor).unwrap_or(self.value.len());
        old != self.value
    }
    pub fn draw(&self, cx: &mut DrawCx) {
        let t = cx.theme;
        cx.canvas.stroke_rounded_rect(self.rect, t.radius, t.border as f32, INK);
        let inner = self.rect.inset_xy(t.mm(2.0), 0);
        if self.value.is_empty() && !self.focused {
            cx.line(self.placeholder, t.body.gray(INK2), inner, HAlign::Left);
            return;
        }
        let budget = (inner.w - t.mm(2.0)).max(1) as f32;
        let mut start = self.cursor;
        for (i, _) in self.value[..self.cursor].grapheme_indices(true).rev() {
            if cx.text.width(&self.value[i..self.cursor], t.body) > budget {
                break;
            }
            start = i;
        }
        cx.line(&self.value[start..], t.body, inner, HAlign::Left);
        if self.focused {
            let x = inner.x + cx.text.width(&self.value[start..self.cursor], t.body).ceil() as i32;
            cx.fill(
                Rect::new(x.min(inner.right() - t.hairline), inner.y + t.mm(2.0), t.hairline, inner.h - t.mm(4.0)),
                INK,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn edits_whole_graphemes_and_inserts_at_cursor() {
        let mut f = TextField::default();
        f.edit(&TextInput::Insert("Яe\u{301}👩‍👩‍👧‍👦Z".into()));
        f.edit(&TextInput::Left);
        f.edit(&TextInput::Backspace);
        assert_eq!(f.value(), "Яe\u{301}Z");
        f.edit(&TextInput::Left);
        f.edit(&TextInput::Delete);
        assert_eq!(f.value(), "ЯZ");
        f.edit(&TextInput::Insert("ё".into()));
        assert_eq!(f.value(), "ЯёZ");
        f.edit(&TextInput::Home);
        assert!(!f.edit(&TextInput::Backspace));
        f.edit(&TextInput::End);
        assert!(!f.edit(&TextInput::Delete));
        f.edit(&TextInput::Clear);
        assert_eq!(f.value(), "");
        f.edit(&TextInput::Insert("e".into()));
        f.edit(&TextInput::Insert("\u{301}".into()));
        f.edit(&TextInput::Backspace);
        assert!(f.value().is_empty());
    }
}
