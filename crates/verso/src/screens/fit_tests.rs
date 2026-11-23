//! Every sheet fits every screen: one sweep over `ui::fit::SCREENS` and the
//! samples each screen module builds (see `ui/fit.rs`).

use crate::geom::Rect;
use crate::gfx::TextRenderer;
use crate::ui::fit::{SCREENS, check};
use crate::ui::theme::Theme;

#[test]
fn every_sheet_fits_every_screen() {
    let mut text = TextRenderer::new();
    let mut errors = Vec::new();
    for (w, h, dpi) in SCREENS {
        let (t, screen) = (Theme::for_dpi(dpi), Rect::new(0, 0, w, h));
        let mut samples = super::books::fit_samples(&t, screen, &mut text);
        samples.extend(super::system::fit_samples(&t, screen, &mut text));
        samples.extend(super::transfer::fit_samples(&t, screen, &mut text));
        samples.extend(super::update::fit_samples(&t, screen, &mut text));
        samples.extend(super::untested::fit_samples(&t, screen, &mut text));
        samples.extend(super::koreader_import::fit_samples(&t, screen, &mut text));
        samples.extend(super::footnote::fit_samples(&t, screen));
        samples.extend(super::text_sheet::fit_samples(&t, screen));
        samples.extend(super::reader_menu::fit_samples(&t, screen));
        samples.extend(super::reader::fit_samples(&t, screen));
        for parts in &samples {
            errors.extend(check(&t, screen, parts));
        }
    }
    assert!(errors.is_empty(), "{}", errors.join("\n"));
}
