//! The one-time notice on a Kindle model Verso hasn't been tested on yet. It shows over the first screen and
//! is remembered per model once dismissed.

use anyhow::Result;

use crate::geom::Rect;
use crate::gfx::HAlign;
use crate::platform::DeviceModel;
use crate::store::{Settings, Store};
use crate::ui::gesture::Gesture;
use crate::ui::theme::Theme;
use crate::ui::widget::DrawCx;
use crate::ui::widgets::{Button, ButtonKind, Sheet};

/// True when `model` is untested and its notice hasn't been dismissed.
pub fn due(settings: &Settings, model: &DeviceModel) -> bool {
    model.untested && !settings.untested_seen.contains(&model.key)
}

/// Remembers that the notice for `key` was seen.
pub fn mark_seen(store: &Store, key: &str) -> Result<()> {
    let mut settings = store.settings();
    if !settings.untested_seen.iter().any(|k| k == key) {
        settings.untested_seen.push(key.to_owned());
        store.save_settings(&settings)?;
    }
    Ok(())
}

pub fn message(model: &DeviceModel) -> String {
    format!(
        "Verso hasn’t been tested on {} yet. If something looks wrong, Settings → Device → Create problem report.",
        model.name
    )
}

pub struct UntestedSheet {
    store: Store,
    key: String,
    sheet: Sheet,
    message: String,
    text: Rect,
    ok: Button,
}

impl UntestedSheet {
    pub fn new(t: &Theme, screen: Rect, store: Store, model: &DeviceModel) -> Self {
        let sheet = super::update::sheet("Not tested on this Kindle yet", t.mm(52.0), screen, t);
        let c = sheet.content(t);
        let mut ok = Button::new("OK", ButtonKind::Primary);
        ok.place(Rect::new(c.x, c.bottom() - t.margin - t.button_h, c.w, t.button_h), t);
        let text = Rect { h: ok.rect().y - c.y - t.mm(3.0), ..c };
        Self { store, key: model.key.clone(), sheet, message: message(model), text, ok }
    }

    pub fn draw(&self, cx: &mut DrawCx) {
        self.sheet.draw(cx);
        cx.paragraph(&self.message, cx.theme.body, self.text, HAlign::Left);
        self.ok.draw(cx);
    }

    /// True when OK was tapped (or the sheet closed): it isn't shown again
    /// on this model.
    pub fn on_gesture(&mut self, g: &Gesture) -> bool {
        if self.sheet.on_gesture(g) || self.ok.on_gesture(g) {
            if let Err(e) = mark_seen(&self.store, &self.key) {
                log::warn!("couldn't remember the untested-model notice: {e:#}");
            }
            return true;
        }
        false
    }
}

/// The notice with a long model name, for `screens/fit_tests.rs`.
#[cfg(test)]
pub(super) fn fit_samples(t: &Theme, screen: Rect, text: &mut crate::gfx::TextRenderer) -> Vec<crate::ui::fit::Parts> {
    let dir = std::env::temp_dir().join("verso-untested-fit");
    let model = DeviceModel { name: "Kindle Paperwhite Signature Edition (2024)".into(), ..Default::default() };
    let s = UntestedSheet::new(t, screen, Store::at(dir), &model);
    let needed = crate::ui::fit::text_h(text, &s.message, t.body, s.text.w);
    vec![crate::ui::fit::Parts::new("UntestedSheet", s.sheet.rect(), [s.ok.rect()]).body(s.text, needed)]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(untested: bool) -> DeviceModel {
        DeviceModel {
            name: "Kindle Basic 4 (2022)".into(),
            untested,
            key: "Kindle Basic 4 (2022)".into(),
            ..Default::default()
        }
    }

    #[test]
    fn shows_once_per_untested_model() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = Store::at(dir.path().to_path_buf());
        assert!(!due(&store.settings(), &model(false)), "tested models get no notice");
        assert!(due(&store.settings(), &model(true)));
        mark_seen(&store, "Kindle Basic 4 (2022)")?;
        mark_seen(&store, "Kindle Basic 4 (2022)")?;
        let settings = store.settings();
        assert_eq!(settings.untested_seen, ["Kindle Basic 4 (2022)"]);
        assert!(!due(&settings, &model(true)), "dismissed: not again");
        let other = DeviceModel { key: "id 0x1234".into(), ..model(true) };
        assert!(due(&settings, &other), "another model still gets it");
        Ok(())
    }

    #[test]
    fn message_names_the_model_and_the_report() {
        assert_eq!(
            message(&model(true)),
            "Verso hasn’t been tested on Kindle Basic 4 (2022) yet. If something looks wrong, Settings → Device → Create problem report."
        );
    }
}
