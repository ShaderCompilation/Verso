//! Bottom keyboard overlay; its owner consumes every touch while it is open.
use super::{Button, ButtonKind};
use crate::geom::Rect;
use crate::platform::TextInput;
use crate::ui::gesture::Gesture;
use crate::ui::theme::{LINE, PAPER, Theme};
use crate::ui::widget::DrawCx;

#[derive(Clone)]
enum KeyAction {
    Input(TextInput),
    Language,
    Shift,
    Symbols,
}

#[derive(Default)]
pub struct Keyboard {
    pub visible: bool,
    language: usize,
    shift: bool,
    symbols: bool,
    rect: Rect,
    screen: Rect,
    keys: Vec<(Button, KeyAction)>,
    outside: bool,
}
impl Keyboard {
    pub fn place(&mut self, screen: Rect, t: &Theme) {
        self.screen = screen;
        self.keys.clear();
        let gap = t.mm(0.5).max(1);
        let width = screen.w - 2 * t.margin;
        let cols = ((width + gap) / (t.target + gap)).clamp(1, 10) as usize;
        let rows: &[&str] = if self.symbols {
            &["1234567890", "-_'\".,:;!?", "()/&+@#%="]
        } else if self.language == 1 {
            &["йцукенгшщз", "фывапролдж", "ячсмитьбю", "ёхъэ"]
        } else if self.language == 2 {
            &["qwertyuiop", "asdfghjkl", "zxcvbnm", "æøå"]
        } else {
            &["qwertyuiop", "asdfghjkl", "zxcvbnm"]
        };
        // On narrow panels pack across logical rows rather than leaving one or
        // two letters on an otherwise empty row. Keep the scope visible above us.
        let rows: Vec<String> = if cols < 10 {
            rows.iter()
                .flat_map(|row| row.chars())
                .collect::<Vec<_>>()
                .chunks(cols)
                .map(|chunk| chunk.iter().collect())
                .collect()
        } else {
            rows.iter().map(|row| (*row).to_owned()).collect()
        };
        let count: usize = rows.iter().map(|row| row.chars().count().div_ceil(cols)).sum();
        let h = (count as i32 + 2) * (t.target + gap) + 2 * gap;
        self.rect = Rect::new(screen.x, screen.bottom() - h, screen.w, h);
        let mut y = self.rect.y + gap;
        for row in &rows {
            let chars: Vec<_> = row.chars().collect();
            for chunk in chars.chunks(cols) {
                let key_w = (width - (cols as i32 - 1) * gap) / cols as i32;
                let x = screen.x + t.margin + (width - (chunk.len() as i32 * (key_w + gap) - gap)) / 2;
                for (i, ch) in chunk.iter().enumerate() {
                    let label = if self.shift { ch.to_uppercase().collect::<String>() } else { ch.to_string() };
                    self.add(
                        &label,
                        KeyAction::Input(TextInput::Insert(label.clone())),
                        Rect::new(x + i as i32 * (key_w + gap), y, key_w, t.target),
                        t,
                    );
                }
                y += t.target + gap;
            }
        }
        let edits = [
            (if self.shift { "SHIFT" } else { "Shift" }, KeyAction::Shift),
            ("←", KeyAction::Input(TextInput::Left)),
            ("→", KeyAction::Input(TextInput::Right)),
            ("⌫", KeyAction::Input(TextInput::Backspace)),
            ("Clear", KeyAction::Input(TextInput::Clear)),
        ];
        self.control_row(&edits, y, t);
        y += t.target + gap;
        let controls = [
            (
                match self.language {
                    1 => "RU → DA",
                    2 => "DA → EN",
                    _ => "EN → RU",
                },
                KeyAction::Language,
            ),
            (if self.symbols { "ABC" } else { "123" }, KeyAction::Symbols),
            ("Space", KeyAction::Input(TextInput::Insert(" ".into()))),
            ("Search", KeyAction::Input(TextInput::Submit)),
            ("Hide", KeyAction::Input(TextInput::Dismiss)),
        ];
        self.control_row(&controls, y, t);
    }
    fn add(&mut self, label: &str, action: KeyAction, rect: Rect, t: &Theme) {
        let mut b = Button::new(label, ButtonKind::Secondary);
        b.place(rect, t);
        self.keys.push((b, action));
    }
    fn control_row(&mut self, row: &[(&str, KeyAction)], y: i32, t: &Theme) {
        let gap = t.mm(0.5).max(1);
        let width = self.screen.w - 2 * t.margin;
        let w = (width - (row.len() as i32 - 1) * gap) / row.len() as i32;
        for (i, (label, action)) in row.iter().enumerate() {
            self.add(
                label,
                action.clone(),
                Rect::new(self.screen.x + t.margin + i as i32 * (w + gap), y, w, t.target),
                t,
            );
        }
    }
    pub fn on_gesture(&mut self, g: &Gesture, t: &Theme) -> Option<TextInput> {
        if let Gesture::Down(p) = g {
            self.outside = !self.rect.contains(*p);
        }
        if self.outside {
            return matches!(g, Gesture::Tap(_)).then_some(TextInput::Dismiss);
        }
        let action = self.keys.iter_mut().find_map(|(b, action)| b.on_gesture(g).then(|| action.clone()));
        match action? {
            KeyAction::Input(input) => Some(input),
            KeyAction::Language => {
                self.language = (self.language + 1) % 3;
                self.symbols = false;
                self.place(self.screen, t);
                None
            }
            KeyAction::Shift => {
                self.shift = !self.shift;
                self.place(self.screen, t);
                None
            }
            KeyAction::Symbols => {
                self.symbols = !self.symbols;
                self.place(self.screen, t);
                None
            }
        }
    }
    pub fn draw(&self, cx: &mut DrawCx) {
        if !self.visible {
            return;
        }
        cx.fill(self.rect, PAPER);
        cx.hline(self.rect.y, self.rect.x, self.rect.right(), cx.theme.hairline, LINE);
        for (b, _) in &self.keys {
            b.draw(cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn touch_entry_switches_layout_edits_and_submits() {
        use crate::geom::Point;
        fn tap(k: &mut Keyboard, label: &str, t: &Theme) -> Option<TextInput> {
            let p = k.keys.iter().find(|(b, _)| b.label == label).unwrap().0.rect().center();
            assert!(k.on_gesture(&Gesture::Down(p), t).is_none());
            k.on_gesture(&Gesture::Tap(p), t)
        }
        let t = Theme::for_dpi(300);
        let mut k = Keyboard::default();
        k.place(Rect::new(0, 0, 1272, 1696), &t);
        assert_eq!(tap(&mut k, "q", &t), Some(TextInput::Insert("q".into())));
        tap(&mut k, "EN → RU", &t);
        assert_eq!(tap(&mut k, "ё", &t), Some(TextInput::Insert("ё".into())));
        tap(&mut k, "Shift", &t);
        assert_eq!(tap(&mut k, "Г", &t), Some(TextInput::Insert("Г".into())));
        tap(&mut k, "SHIFT", &t);
        tap(&mut k, "RU → DA", &t);
        assert_eq!(tap(&mut k, "ø", &t), Some(TextInput::Insert("ø".into())));
        assert_eq!(tap(&mut k, "⌫", &t), Some(TextInput::Backspace));
        assert_eq!(tap(&mut k, "Clear", &t), Some(TextInput::Clear));
        assert_eq!(tap(&mut k, "Search", &t), Some(TextInput::Submit));
        let p = Point::new(500, 100);
        k.on_gesture(&Gesture::Down(p), &t);
        assert_eq!(k.on_gesture(&Gesture::Tap(p), &t), Some(TextInput::Dismiss));
    }
    #[test]
    fn layouts_have_usable_nonoverlapping_keys_and_all_russian_letters() {
        for (w, h, dpi) in [(1272, 1696, 300), (600, 800, 167)] {
            let t = Theme::for_dpi(dpi);
            let mut k = Keyboard::default();
            k.place(Rect::new(0, 0, w, h), &t);
            for language in 0..3 {
                k.language = language;
                k.place(k.screen, &t);
                assert!(k.rect.y > t.top_bar + 2 * t.target + t.mm(10.0));
                for (i, (b, _)) in k.keys.iter().enumerate() {
                    assert!(b.rect().w >= t.target && b.rect().h >= t.target);
                    for (other, _) in &k.keys[i + 1..] {
                        assert!(b.rect().intersect(&other.rect()).is_none());
                    }
                }
                if language == 2 {
                    for ch in ['æ', 'ø', 'å'] {
                        assert!(k.keys.iter().any(|(b, _)| b.label == ch.to_string()));
                    }
                }
                if language == 1 {
                    for ch in 'а'..='я' {
                        assert!(k.keys.iter().any(|(b, _)| b.label == ch.to_string()), "{ch}");
                    }
                    assert!(k.keys.iter().any(|(b, _)| b.label == "ё"));
                }
            }
        }
    }
}
