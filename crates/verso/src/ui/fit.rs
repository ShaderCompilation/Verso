//! The shared check behind `screens/fit_tests.rs`: every sheet fits every
//! screen. Each screen module builds its sheets in their fullest state in a
//! `#[cfg(test)] fn fit_samples` and describes each one as `Parts`; the sweep
//! runs `check` on every sample at every size in `SCREENS`.

use crate::geom::Rect;
use crate::gfx::{TextRenderer, TextStyle};
use crate::ui::theme::Theme;

/// Both orientations of the Paperwhite (300 dpi) and of a 6" 167 dpi Kindle,
/// as (width, height, dpi).
pub const SCREENS: [(i32, i32, u32); 4] = [(1272, 1696, 300), (1696, 1272, 300), (600, 800, 167), (800, 600, 167)];

/// One sheet as `check` sees it.
pub struct Parts {
    pub name: &'static str,
    pub panel: Rect,
    /// Touch targets: buttons, rows, sliders.
    pub buttons: Vec<Rect>,
    /// Text areas, each with the height its text needs.
    pub bodies: Vec<(Rect, i32)>,
    pub keyboard: Option<Rect>,
    /// Buttons may be lower than a touch target, down to this: rows of a
    /// dense card, whose gaps make up for it.
    pub dense: Option<i32>,
}

impl Parts {
    pub fn new(name: &'static str, panel: Rect, buttons: impl IntoIterator<Item = Rect>) -> Self {
        Self { name, panel, buttons: buttons.into_iter().collect(), bodies: Vec::new(), keyboard: None, dense: None }
    }

    pub fn body(mut self, rect: Rect, needed: i32) -> Self {
        self.bodies.push((rect, needed));
        self
    }

    pub fn dense(mut self, height: i32) -> Self {
        self.dense = Some(height);
        self
    }

    pub fn keyboard(mut self, rect: Rect) -> Self {
        self.keyboard = Some(rect);
        self
    }
}

/// The height `text` needs in `style`, wrapped at `width`.
pub fn text_h(text: &mut TextRenderer, s: &str, style: TextStyle, width: i32) -> i32 {
    text.measure(s, style, Some(width as f32)).1.ceil() as i32
}

/// What doesn't fit, one line per broken rule; empty when everything does.
pub fn check(t: &Theme, screen: Rect, p: &Parts) -> Vec<String> {
    let mut errors = Vec::new();
    let mut fail = |what: String| errors.push(format!("{} at {}x{}: {what}", p.name, screen.w, screen.h));
    if !screen.contains_rect(&p.panel) {
        fail(format!("panel {:?} leaves the screen", p.panel));
    }
    let margins = Rect { x: screen.x + t.margin, w: screen.w - 2 * t.margin, ..screen };
    let keyboard_y = p.keyboard.map_or(i32::MAX, |k| k.y);
    for (i, b) in p.buttons.iter().enumerate() {
        if !p.panel.contains_rect(b) || !margins.contains_rect(b) {
            fail(format!("button {b:?} leaves the panel {:?} or the margins", p.panel));
        }
        let lowest = p.dense.unwrap_or(t.target);
        if b.h < lowest {
            fail(format!("button {b:?} is lower than a touch target ({lowest})"));
        }
        if b.bottom() > keyboard_y {
            fail(format!("button {b:?} runs under the keyboard at {keyboard_y}"));
        }
        if let Some(other) = p.buttons[i + 1..].iter().find(|o| o.intersect(b).is_some()) {
            fail(format!("buttons {b:?} and {other:?} overlap"));
        }
    }
    for &(body, needed) in &p.bodies {
        let text = Rect { h: needed, ..body };
        if needed > body.h {
            fail(format!("text needs {needed} px, its area {body:?} has {}", body.h));
        }
        if !p.panel.contains_rect(&text) || text.bottom() > keyboard_y {
            fail(format!("text {text:?} leaves the panel {:?} or runs under the keyboard", p.panel));
        }
        if let Some(b) = p.buttons.iter().find(|b| b.intersect(&text).is_some()) {
            fail(format!("text {text:?} runs under button {b:?}"));
        }
    }
    errors
}
