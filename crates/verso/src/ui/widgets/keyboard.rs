//! Bottom keyboard overlay; its owner consumes every touch while it is open.
use std::time::{Duration, Instant};

use crate::geom::Rect;
use crate::gfx::{Family, HAlign, Icon, TextStyle, Weight};
use crate::platform::{RefreshMode, TextInput};
use crate::ui::gesture::Gesture;
use crate::ui::theme::{FILL, INK, INK3, LINE, PAPER, Theme};
use crate::ui::widget::{DrawCx, Press, PressEvent};

/// Maximum interval between two Shift taps to engage Caps Lock.
const SHIFT_DOUBLE_TAP: Duration = Duration::from_millis(400);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Shift {
    #[default]
    Off,
    /// Capitalises the next letter, then turns off.
    Once,
    /// Stays on until Shift is tapped again (or layout changes).
    Locked,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum KeyAction {
    Letter(String),
    Input(TextInput),
    Backspace,
    Language,
    Shift,
    Symbols,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KeyStyle {
    Letter,
    Space,
    Function,
    Active,
    Primary,
}

#[derive(Clone, Debug)]
enum KeyFace {
    Text(String),
    Icon(Icon),
}

struct Key {
    face: KeyFace,
    style: KeyStyle,
    caps_bar: bool,
    rect: Rect,
    hit: Rect,
    press: Press,
    action: KeyAction,
}

impl Key {
    fn new(face: KeyFace, style: KeyStyle, action: KeyAction, rect: Rect, gap: i32) -> Self {
        Self { face, style, caps_bar: false, rect, hit: rect.inset(-gap / 2), press: Press::default(), action }
    }

    fn draw(&self, cx: &mut DrawCx) {
        let t = cx.theme;
        let (r, radius, border) = (self.rect, t.radius, t.border as f32);
        let pressed = self.press.is_down();
        let dark = matches!(self.style, KeyStyle::Active | KeyStyle::Primary);
        let fg = if pressed {
            cx.hint(r, RefreshMode::Fast);
            if dark {
                cx.canvas.stroke_rounded_rect(r, radius, border, INK);
                INK
            } else {
                cx.canvas.fill_rounded_rect(r, radius, INK);
                PAPER
            }
        } else {
            match self.style {
                KeyStyle::Letter => {
                    cx.canvas.stroke_rounded_rect(r, radius, border, INK);
                    INK
                }
                KeyStyle::Space => {
                    cx.canvas.stroke_rounded_rect(r, radius, border, INK);
                    INK3
                }
                KeyStyle::Function => {
                    cx.canvas.fill_rounded_rect(r, radius, FILL);
                    cx.canvas.stroke_rounded_rect(r, radius, border, INK);
                    INK
                }
                KeyStyle::Active | KeyStyle::Primary => {
                    cx.canvas.fill_rounded_rect(r, radius, INK);
                    PAPER
                }
            }
        };
        match &self.face {
            KeyFace::Icon(icon) => {
                let size = t.pt(10.5) as i32;
                cx.icon(*icon, r, size, fg);
            }
            KeyFace::Text(label) => {
                let style = match self.style {
                    KeyStyle::Letter => TextStyle::new(t.pt(10.5), Weight::Regular, Family::Sans).gray(fg),
                    KeyStyle::Function | KeyStyle::Active => {
                        TextStyle::new(t.pt(9.5), Weight::Regular, Family::Sans).gray(fg)
                    }
                    KeyStyle::Primary => t.button.gray(fg),
                    KeyStyle::Space => t.small.gray(fg),
                };
                if self.caps_bar {
                    let bar_h = t.border + 1;
                    let text_rect = Rect::new(r.x, r.y - bar_h, r.w, r.h);
                    cx.line(label, style, text_rect.inset_xy(t.mm(0.8), 0), HAlign::Center);
                    let bar_w = (style.size * 0.8).round() as i32;
                    let bar_y = r.center().y + (style.size * 0.52).round() as i32;
                    cx.canvas.fill_rect(Rect::new(r.center().x - bar_w / 2, bar_y, bar_w, bar_h), fg);
                } else {
                    cx.line(label, style, r.inset_xy(t.mm(0.8), 0), HAlign::Center);
                }
            }
        }
    }
}

#[derive(Default)]
pub struct Keyboard {
    pub visible: bool,
    language: usize,
    shift: Shift,
    shift_at: Option<Instant>,
    symbols: bool,
    rect: Rect,
    screen: Rect,
    keys: Vec<Key>,
    outside: bool,
    /// The submit key's label; empty means "Search".
    submit: &'static str,
}

impl Keyboard {
    /// Names the submit key (for example "Save"); call before `place`.
    pub fn set_submit(&mut self, label: &'static str) {
        self.submit = label;
    }

    pub fn place(&mut self, screen: Rect, t: &Theme) {
        self.screen = screen;
        self.keys.clear();
        let key_h = t.target;
        let pad = t.mm(1.5);
        let gap = t.mm(0.65).max(1);
        let h = 4 * key_h + 5 * gap;
        self.rect = Rect::new(screen.x, screen.bottom() - h, screen.w, h);
        let x0 = screen.x + pad;
        let w_span = screen.w - 2 * pad + gap;

        let mut y = self.rect.y + gap;
        if self.symbols {
            self.add_chars("1234567890", 0, 20, y, x0, w_span, key_h, gap, false);
            y += key_h + gap;
            self.add_chars("-_'\".,:;!?", 0, 20, y, x0, w_span, key_h, gap, false);
            y += key_h + gap;
            self.add_chars("()/&+@#%=", 0, 20, y, x0, w_span, key_h, gap, false);
            self.add_key(
                18,
                20,
                20,
                y,
                x0,
                w_span,
                key_h,
                gap,
                KeyFace::Text("⌫".into()),
                KeyStyle::Function,
                KeyAction::Backspace,
            );
            y += key_h + gap;
        } else {
            let (total_half, row1, row2, row2_offset, row3, side_span): (i32, &str, &str, i32, &str, i32) =
                match self.language {
                    1 => (24, "йцукенгшщзхъ", "фывапролджэё", 0, "ячсмитьбю", 3),
                    2 => (22, "qwertyuiopå", "asdfghjklæø", 0, "zxcvbnm", 4),
                    _ => (20, "qwertyuiop", "asdfghjkl", 1, "zxcvbnm", 3),
                };
            self.add_chars(row1, 0, total_half, y, x0, w_span, key_h, gap, true);
            y += key_h + gap;
            self.add_chars(row2, row2_offset, total_half, y, x0, w_span, key_h, gap, true);
            y += key_h + gap;
            let shift_style = if self.shift == Shift::Off { KeyStyle::Function } else { KeyStyle::Active };
            self.add_key(
                0,
                side_span,
                total_half,
                y,
                x0,
                w_span,
                key_h,
                gap,
                KeyFace::Text("⇧".into()),
                shift_style,
                KeyAction::Shift,
            );
            if self.shift == Shift::Locked {
                self.keys.last_mut().expect("shift key just added").caps_bar = true;
            }
            self.add_chars(row3, side_span, total_half, y, x0, w_span, key_h, gap, true);
            self.add_key(
                total_half - side_span,
                total_half,
                total_half,
                y,
                x0,
                w_span,
                key_h,
                gap,
                KeyFace::Text("⌫".into()),
                KeyStyle::Function,
                KeyAction::Backspace,
            );
            y += key_h + gap;
        }

        // Shared bottom row, always 10 units (20 half-units) wide:
        // 123/ABC (1.5) · Globe (1) · Space (4) · ← (1) · → (1) · Search/Save (1.5)
        let sym_label = if self.symbols { "ABC" } else { "123" };
        let lang_label = match self.language {
            1 => "Русский",
            2 => "Dansk",
            _ => "English",
        };
        let submit_label = if self.submit.is_empty() { "Search" } else { self.submit };
        let bottom = [
            (0, 3, KeyFace::Text(sym_label.into()), KeyStyle::Function, KeyAction::Symbols),
            (3, 5, KeyFace::Icon(Icon::Globe), KeyStyle::Function, KeyAction::Language),
            (5, 13, KeyFace::Text(lang_label.into()), KeyStyle::Space, KeyAction::Input(TextInput::Insert(" ".into()))),
            (13, 15, KeyFace::Text("←".into()), KeyStyle::Function, KeyAction::Input(TextInput::Left)),
            (15, 17, KeyFace::Text("→".into()), KeyStyle::Function, KeyAction::Input(TextInput::Right)),
            (17, 20, KeyFace::Text(submit_label.into()), KeyStyle::Primary, KeyAction::Input(TextInput::Submit)),
        ];
        for (u0, u1, face, style, action) in bottom {
            self.add_key(u0, u1, 20, y, x0, w_span, key_h, gap, face, style, action);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn add_chars(
        &mut self,
        chars: &str,
        u_start: i32,
        total_half: i32,
        y: i32,
        x0: i32,
        w_span: i32,
        key_h: i32,
        gap: i32,
        is_letter: bool,
    ) {
        let upper = is_letter && self.shift != Shift::Off;
        for (i, ch) in chars.chars().enumerate() {
            let label = if upper { ch.to_uppercase().collect::<String>() } else { ch.to_string() };
            let action = if is_letter {
                KeyAction::Letter(label.clone())
            } else {
                KeyAction::Input(TextInput::Insert(label.clone()))
            };
            let u0 = u_start + i as i32 * 2;
            self.add_key(
                u0,
                u0 + 2,
                total_half,
                y,
                x0,
                w_span,
                key_h,
                gap,
                KeyFace::Text(label),
                KeyStyle::Letter,
                action,
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn add_key(
        &mut self,
        u0: i32,
        u1: i32,
        total_half: i32,
        y: i32,
        x0: i32,
        w_span: i32,
        key_h: i32,
        gap: i32,
        face: KeyFace,
        style: KeyStyle,
        action: KeyAction,
    ) {
        let left = x0 + (u0 * w_span + total_half / 2) / total_half;
        let right = x0 + (u1 * w_span + total_half / 2) / total_half - gap;
        self.keys.push(Key::new(face, style, action, Rect::new(left, y, right - left, key_h), gap));
    }

    /// The area the keyboard covers (when visible).
    pub fn rect(&self) -> Rect {
        self.rect
    }

    /// Snapshot of visible state that gestures can change without returning text input.
    pub fn visual_state(&self) -> (bool, usize, bool, u8, Option<usize>) {
        let shift = match self.shift {
            Shift::Off => 0,
            Shift::Once => 1,
            Shift::Locked => 2,
        };
        let pressed = self.keys.iter().position(|k| k.press.is_down());
        (self.visible, self.language, self.symbols, shift, pressed)
    }

    pub fn on_gesture(&mut self, g: &Gesture, t: &Theme) -> Option<TextInput> {
        self.on_gesture_at(g, t, Instant::now())
    }

    /// Handles a gesture only when the touch started inside the keyboard,
    /// leaving outside touches for the owner's own buttons (e.g. Cancel/Save).
    /// Returns `Some(input)` when the keyboard consumed the gesture, or `None`
    /// when the owner should route it to its own controls. A `Cancel` clears
    /// any pressed key and still returns `None` so the owner can cancel its
    /// controls too.
    pub fn on_gesture_inside(&mut self, g: &Gesture, t: &Theme) -> Option<Option<TextInput>> {
        if let Gesture::Down(p) = g {
            self.outside = !self.rect.contains(*p);
        }
        if self.outside {
            return None;
        }
        let input = self.on_gesture(g, t);
        if matches!(g, Gesture::Cancel(_)) {
            return None;
        }
        Some(input)
    }

    /// Like `on_gesture`, with an explicit timestamp so Shift double-taps can be tested.
    pub fn on_gesture_at(&mut self, g: &Gesture, t: &Theme, now: Instant) -> Option<TextInput> {
        if let Gesture::Down(p) = g {
            self.outside = !self.rect.contains(*p);
        }
        if self.outside {
            return matches!(g, Gesture::Tap(_)).then_some(TextInput::Dismiss);
        }
        let mut fired = None;
        for key in &mut self.keys {
            if let Some(ev) = key.press.on_gesture(g, key.hit) {
                fired = Some((ev, key.action.clone()));
            }
        }
        let (ev, action) = fired?;
        match action {
            KeyAction::Backspace => match ev {
                PressEvent::Tap => {
                    self.shift_at = None;
                    Some(TextInput::Backspace)
                }
                PressEvent::LongPress => {
                    self.shift_at = None;
                    Some(TextInput::Clear)
                }
                PressEvent::LongPressEnd => None,
            },
            _ if matches!(ev, PressEvent::LongPress) => None,
            KeyAction::Letter(ch) => {
                self.shift_at = None;
                if self.shift == Shift::Once {
                    self.shift = Shift::Off;
                    self.place(self.screen, t);
                }
                Some(TextInput::Insert(ch))
            }
            KeyAction::Input(input) => {
                self.shift_at = None;
                Some(input)
            }
            KeyAction::Language => {
                self.language = (self.language + 1) % 3;
                self.symbols = false;
                self.shift = Shift::Off;
                self.shift_at = None;
                self.place(self.screen, t);
                None
            }
            KeyAction::Shift => {
                match self.shift {
                    Shift::Off => {
                        self.shift = Shift::Once;
                        self.shift_at = Some(now);
                    }
                    Shift::Once => {
                        if self.shift_at.is_some_and(|at| now.saturating_duration_since(at) <= SHIFT_DOUBLE_TAP) {
                            self.shift = Shift::Locked;
                        } else {
                            self.shift = Shift::Off;
                        }
                        self.shift_at = None;
                    }
                    Shift::Locked => {
                        self.shift = Shift::Off;
                        self.shift_at = None;
                    }
                }
                self.place(self.screen, t);
                None
            }
            KeyAction::Symbols => {
                self.symbols = !self.symbols;
                self.shift = Shift::Off;
                self.shift_at = None;
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
        for key in &self.keys {
            key.draw(cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::geom::Point;

    fn key_center(k: &Keyboard, pred: impl Fn(&Key) -> bool) -> Point {
        k.keys.iter().find(|key| pred(key)).expect("key present").rect.center()
    }

    fn tap_at(k: &mut Keyboard, p: Point, t: &Theme, now: Instant) -> Option<TextInput> {
        assert!(k.on_gesture_at(&Gesture::Down(p), t, now).is_none());
        k.on_gesture_at(&Gesture::Tap(p), t, now)
    }

    fn tap_label(k: &mut Keyboard, label: &str, t: &Theme, now: Instant) -> Option<TextInput> {
        let p = key_center(k, |key| matches!(&key.face, KeyFace::Text(s) if s == label));
        tap_at(k, p, t, now)
    }

    fn tap_globe(k: &mut Keyboard, t: &Theme, now: Instant) -> Option<TextInput> {
        let p = key_center(k, |key| matches!(key.face, KeyFace::Icon(Icon::Globe)));
        tap_at(k, p, t, now)
    }

    #[test]
    fn touch_entry_switches_layout_edits_and_submits() {
        let t = Theme::for_dpi(300);
        let t0 = Instant::now();
        let mut k = Keyboard::default();
        k.place(Rect::new(0, 0, 1272, 1696), &t);

        assert_eq!(tap_label(&mut k, "q", &t, t0), Some(TextInput::Insert("q".into())));
        // Switch to Russian via Globe key.
        assert_eq!(tap_globe(&mut k, &t, t0), None);
        assert_eq!(tap_label(&mut k, "ё", &t, t0), Some(TextInput::Insert("ё".into())));
        assert_eq!(tap_label(&mut k, "ъ", &t, t0), Some(TextInput::Insert("ъ".into())));
        // Space shows current language and inserts a space.
        assert_eq!(tap_label(&mut k, "Русский", &t, t0), Some(TextInput::Insert(" ".into())));

        // Shift once: capitalises one letter, then returns to lowercase.
        assert_eq!(tap_label(&mut k, "⇧", &t, t0), None);
        assert_eq!(k.shift, Shift::Once);
        assert_eq!(tap_label(&mut k, "Г", &t, t0), Some(TextInput::Insert("Г".into())));
        assert_eq!(k.shift, Shift::Off);
        assert_eq!(tap_label(&mut k, "г", &t, t0), Some(TextInput::Insert("г".into())));

        // Switch to Danish via Globe key.
        assert_eq!(tap_globe(&mut k, &t, t0), None);
        assert_eq!(tap_label(&mut k, "ø", &t, t0), Some(TextInput::Insert("ø".into())));
        assert_eq!(tap_label(&mut k, "Dansk", &t, t0), Some(TextInput::Insert(" ".into())));

        // Backspace tap vs long-press clear.
        assert_eq!(tap_label(&mut k, "⌫", &t, t0), Some(TextInput::Backspace));
        let bs = key_center(&k, |key| matches!(key.action, KeyAction::Backspace));
        assert!(k.on_gesture_at(&Gesture::Down(bs), &t, t0).is_none());
        assert_eq!(k.on_gesture_at(&Gesture::LongPress(bs), &t, t0), Some(TextInput::Clear));
        assert_eq!(k.on_gesture_at(&Gesture::End(bs), &t, t0), None);

        // Submit label and custom submit label.
        assert_eq!(tap_label(&mut k, "Search", &t, t0), Some(TextInput::Submit));
        k.set_submit("Save");
        k.place(k.screen, &t);
        assert_eq!(tap_label(&mut k, "Save", &t, t0), Some(TextInput::Submit));

        // Tap outside dismisses.
        let p = Point::new(500, 100);
        k.on_gesture_at(&Gesture::Down(p), &t, t0);
        assert_eq!(k.on_gesture_at(&Gesture::Tap(p), &t, t0), Some(TextInput::Dismiss));
    }

    #[test]
    fn shift_once_caps_lock_and_resets() {
        let t = Theme::for_dpi(300);
        let t0 = Instant::now();
        let mut k = Keyboard::default();
        k.place(Rect::new(0, 0, 1272, 1696), &t);

        // Slow second tap (> 400 ms) turns Shift off instead of locking.
        tap_label(&mut k, "⇧", &t, t0);
        assert_eq!(k.shift, Shift::Once);
        tap_label(&mut k, "⇧", &t, t0 + Duration::from_millis(500));
        assert_eq!(k.shift, Shift::Off);

        // Double tap within 400 ms engages Caps Lock and stays on across letters.
        tap_label(&mut k, "⇧", &t, t0);
        tap_label(&mut k, "⇧", &t, t0 + Duration::from_millis(200));
        assert_eq!(k.shift, Shift::Locked);
        assert!(k.keys.iter().any(|key| key.action == KeyAction::Shift && key.caps_bar));
        assert_eq!(tap_label(&mut k, "A", &t, t0 + Duration::from_millis(300)), Some(TextInput::Insert("A".into())));
        assert_eq!(k.shift, Shift::Locked);
        assert_eq!(tap_label(&mut k, "B", &t, t0 + Duration::from_millis(400)), Some(TextInput::Insert("B".into())));
        assert_eq!(k.shift, Shift::Locked);

        // Tapping Shift while locked turns it off.
        tap_label(&mut k, "⇧", &t, t0 + Duration::from_millis(500));
        assert_eq!(k.shift, Shift::Off);

        // Switching language or symbols turns Shift off.
        tap_label(&mut k, "⇧", &t, t0);
        assert_eq!(k.shift, Shift::Once);
        tap_globe(&mut k, &t, t0);
        assert_eq!(k.shift, Shift::Off);

        tap_label(&mut k, "⇧", &t, t0);
        tap_label(&mut k, "⇧", &t, t0 + Duration::from_millis(100));
        assert_eq!(k.shift, Shift::Locked);
        tap_label(&mut k, "123", &t, t0 + Duration::from_millis(200));
        assert_eq!(k.shift, Shift::Off);
        assert!(k.symbols);
        assert_eq!(tap_label(&mut k, "⌫", &t, t0 + Duration::from_millis(300)), Some(TextInput::Backspace));
    }

    #[test]
    fn layouts_have_usable_nonoverlapping_keys_and_all_russian_letters() {
        for (w, h, dpi) in [(1272, 1696, 300), (600, 800, 167), (1696, 1272, 300), (800, 600, 167)] {
            let t = Theme::for_dpi(dpi);
            let mut k = Keyboard::default();
            for (language, symbols) in [(0, false), (1, false), (2, false), (0, true)] {
                k.language = language;
                k.symbols = symbols;
                k.place(Rect::new(0, 0, w, h), &t);
                assert!(k.rect.y > t.top_bar + 2 * t.target + t.mm(10.0), "{w}x{h} lang={language} sym={symbols}");
                let rows: BTreeSet<i32> = k.keys.iter().map(|key| key.rect.y).collect();
                assert_eq!(rows.len(), 4, "{w}x{h} lang={language} sym={symbols} should have 4 rows");
                for (i, key) in k.keys.iter().enumerate() {
                    assert_eq!(key.rect.h, t.target, "{w}x{h}: 9 mm key height");
                    assert!(key.rect.w >= t.mm(6.5), "{w}x{h} lang={language}: width {} < 6.5 mm", key.rect.w);
                    assert!(
                        !matches!(&key.face, KeyFace::Text(s) if s == "Clear" || s == "Hide"),
                        "Clear and Hide are removed"
                    );
                    for other in &k.keys[i + 1..] {
                        assert!(key.rect.intersect(&other.rect).is_none());
                        assert!(key.hit.intersect(&other.hit).is_none());
                    }
                }
                if !symbols && language == 2 {
                    for ch in ['æ', 'ø', 'å'] {
                        assert!(
                            k.keys.iter().any(|key| matches!(&key.face, KeyFace::Text(s) if s == &ch.to_string())),
                            "{ch}"
                        );
                    }
                }
                if !symbols && language == 1 {
                    for ch in 'а'..='я' {
                        assert!(
                            k.keys.iter().any(|key| matches!(&key.face, KeyFace::Text(s) if s == &ch.to_string())),
                            "{ch}"
                        );
                    }
                    assert!(k.keys.iter().any(|key| matches!(&key.face, KeyFace::Text(s) if s == "ё")));
                }
            }
        }
    }

    #[test]
    fn on_gesture_inside_clears_press_on_drag_and_cancel() {
        let t = Theme::for_dpi(300);
        let mut k = Keyboard::default();
        k.place(Rect::new(0, 0, 1272, 1696), &t);

        let q = key_center(&k, |key| matches!(&key.face, KeyFace::Text(s) if s == "q"));
        let outside = Point::new(500, 100);

        // Press a key, then drag outside the keyboard rect: the press clears and is consumed.
        assert_eq!(k.on_gesture_inside(&Gesture::Down(q), &t), Some(None));
        assert!(k.keys.iter().any(|key| key.press.is_down()));
        assert_eq!(k.on_gesture_inside(&Gesture::Move(outside), &t), Some(None));
        assert!(!k.keys.iter().any(|key| key.press.is_down()));
        assert_eq!(k.on_gesture_inside(&Gesture::End(outside), &t), Some(None));

        // Cancel (e.g. sleep or USB) clears any held key and returns None so the owner cancels too.
        assert_eq!(k.on_gesture_inside(&Gesture::Down(q), &t), Some(None));
        assert!(k.keys.iter().any(|key| key.press.is_down()));
        assert_eq!(k.on_gesture_inside(&Gesture::Cancel(Point::default()), &t), None);
        assert!(!k.keys.iter().any(|key| key.press.is_down()));

        // A touch starting outside is left to the owner even if dragged onto the keyboard.
        assert_eq!(k.on_gesture_inside(&Gesture::Down(outside), &t), None);
        assert_eq!(k.on_gesture_inside(&Gesture::Move(q), &t), None);
        assert_eq!(k.on_gesture_inside(&Gesture::End(q), &t), None);
        assert!(!k.keys.iter().any(|key| key.press.is_down()));
    }
}
