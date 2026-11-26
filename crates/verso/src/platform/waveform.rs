//! Target-independent e-ink waveform and flash-fence selection.
//!
//! Kept outside `platform/kindle/` so host `cargo test` verifies every light
//! and dark waveform rule, capability gate (`has_eclipse_wfm`, `is_mtk`) and
//! `FlashFence` trigger without needing ARM hardware or FBInk bindings.

use crate::platform::RefreshMode;

/// Panel capabilities reported by FBInk at startup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PanelCaps {
    pub has_color_panel: bool,
    pub has_eclipse_wfm: bool,
    pub is_mtk: bool,
}

/// Waveform mode requested from FBInk for one refresh.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Waveform {
    Du,
    Dunm,
    Gl16,
    Glkw16,
    Reagl,
    Glrc16,
    Glrck16,
    Gc16,
    Gck16,
    Gcc16,
    Gcck16,
}

#[cfg(feature = "kindle")]
impl Waveform {
    pub fn to_fbink(self) -> fbink_sys::WFM_MODE_INDEX_T {
        use fbink_sys as fb;
        match self {
            Self::Du => fb::WFM_MODE_INDEX_E_WFM_DU,
            Self::Dunm => fb::WFM_MODE_INDEX_E_WFM_DUNM,
            Self::Gl16 => fb::WFM_MODE_INDEX_E_WFM_GL16,
            Self::Glkw16 => fb::WFM_MODE_INDEX_E_WFM_GLKW16,
            Self::Reagl => fb::WFM_MODE_INDEX_E_WFM_REAGL,
            Self::Glrc16 => fb::WFM_MODE_INDEX_E_WFM_GLRC16,
            Self::Glrck16 => fb::WFM_MODE_INDEX_E_WFM_GLRCK16,
            Self::Gc16 => fb::WFM_MODE_INDEX_E_WFM_GC16,
            Self::Gck16 => fb::WFM_MODE_INDEX_E_WFM_GCK16,
            Self::Gcc16 => fb::WFM_MODE_INDEX_E_WFM_GCC16,
            Self::Gcck16 => fb::WFM_MODE_INDEX_E_WFM_GCCK16,
        }
    }
}

/// Temporary Phase 1 dark-mode waveform variant (`VERSO_DARK_VARIANT`),
/// removed before committing once the Kindle comparison settles A vs B vs C.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DarkVariant {
    /// Eclipse waveforms; `GLKW16` uses `UPDATE_MODE_PARTIAL`.
    #[default]
    A,
    /// Eclipse waveforms; `GLKW16` uses `UPDATE_MODE_FULL` (`is_flashing = true`).
    B,
    /// Driver night mode (`GRAYSCALE_8BIT_INVERTED`); `GLKW16` uses `UPDATE_MODE_FULL`.
    C,
    /// Driver night mode (`GRAYSCALE_8BIT_INVERTED`); `GLKW16` uses `UPDATE_MODE_PARTIAL`.
    CPartial,
    /// Light waveforms (`REAGL` etc.) on flipped pixels, as a baseline.
    Reagl,
}

impl DarkVariant {
    pub fn from_env() -> Self {
        match std::env::var("VERSO_DARK_VARIANT").ok().as_deref().map(str::to_ascii_lowercase).as_deref() {
            Some("b") => Self::B,
            Some("c") => Self::C,
            Some("c-partial") => Self::CPartial,
            Some("reagl") => Self::Reagl,
            _ => Self::A,
        }
    }

    pub fn uses_driver_night_mode(self) -> bool {
        matches!(self, Self::C | Self::CPartial)
    }
}

/// What `Display::present` sends to FBInk and whether it arms `FlashFence`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RefreshPlan {
    pub waveform: Waveform,
    /// Maps to FBInk's `is_flashing` (`UPDATE_MODE_FULL` when true).
    pub is_flashing: bool,
    /// Whether `Display::present` records the marker in `FlashFence`.
    /// Always gated strictly on `RefreshMode::Full`, never on `is_flashing`.
    pub mark_flash_fence: bool,
}

/// Picks the waveform, update mode and flash-fence flag for `mode`.
///
/// On monochrome i.MX (`!is_mtk`), `GL16` is strictly from-white-to-any and
/// leaves gray halos on antialiased text transitioning to dark UI fills, so the
/// light `Ui` mode uses non-flashing `GC16` (from-any-to-any 16-level gray).
///
/// Eclipse waveforms (`GLKW16`, `GCK16`, `GLRCK16`, `GCCK16`) are gated on
/// `caps.has_eclipse_wfm`; when absent, dark canvases fall back to the light
/// waveforms (`DU` / `GL16` / `REAGL` / `GC16` or color equivalents) with
/// `UPDATE_MODE_PARTIAL` for non-`Full`. `DUNM` is gated on `caps.is_mtk`
/// because FBInk maps `DUNM` to `AUTO` on i.MX.
pub fn plan_refresh(mode: RefreshMode, dark: bool, caps: PanelCaps, variant: DarkVariant) -> RefreshPlan {
    let mark_flash_fence = mode == RefreshMode::Full;
    if !dark || !caps.has_eclipse_wfm || variant == DarkVariant::Reagl {
        let waveform = if caps.has_color_panel {
            match mode {
                RefreshMode::Fast => Waveform::Du,
                RefreshMode::Ui => Waveform::Gl16,
                RefreshMode::Page => Waveform::Glrc16,
                RefreshMode::Full => Waveform::Gcc16,
            }
        } else {
            match mode {
                RefreshMode::Fast => Waveform::Du,
                RefreshMode::Ui => {
                    if caps.is_mtk {
                        Waveform::Gl16
                    } else {
                        Waveform::Gc16
                    }
                }
                RefreshMode::Page => Waveform::Reagl,
                RefreshMode::Full => Waveform::Gc16,
            }
        };
        return RefreshPlan { waveform, is_flashing: mode == RefreshMode::Full, mark_flash_fence };
    }

    let waveform = match mode {
        RefreshMode::Fast => {
            if caps.is_mtk {
                Waveform::Dunm
            } else {
                Waveform::Du
            }
        }
        RefreshMode::Ui => Waveform::Glkw16,
        RefreshMode::Page => {
            if caps.has_color_panel {
                Waveform::Glrck16
            } else {
                Waveform::Glkw16
            }
        }
        RefreshMode::Full => {
            if caps.has_color_panel {
                Waveform::Gcck16
            } else {
                Waveform::Gck16
            }
        }
    };
    let is_flashing = mode == RefreshMode::Full
        || (matches!(variant, DarkVariant::B | DarkVariant::C) && waveform == Waveform::Glkw16);
    RefreshPlan { waveform, is_flashing, mark_flash_fence }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_MODES: [RefreshMode; 4] = [RefreshMode::Fast, RefreshMode::Ui, RefreshMode::Page, RefreshMode::Full];

    #[test]
    fn light_waveforms_match_panel_color_and_only_full_flashes() {
        for has_eclipse_wfm in [false, true] {
            for is_mtk in [false, true] {
                for variant in
                    [DarkVariant::A, DarkVariant::B, DarkVariant::C, DarkVariant::CPartial, DarkVariant::Reagl]
                {
                    let mono = PanelCaps { has_color_panel: false, has_eclipse_wfm, is_mtk };
                    let got: Vec<_> = ALL_MODES.map(|m| plan_refresh(m, false, mono, variant)).into();
                    let ui = if is_mtk { Waveform::Gl16 } else { Waveform::Gc16 };
                    assert_eq!(
                        got,
                        vec![
                            RefreshPlan { waveform: Waveform::Du, is_flashing: false, mark_flash_fence: false },
                            RefreshPlan { waveform: ui, is_flashing: false, mark_flash_fence: false },
                            RefreshPlan { waveform: Waveform::Reagl, is_flashing: false, mark_flash_fence: false },
                            RefreshPlan { waveform: Waveform::Gc16, is_flashing: true, mark_flash_fence: true },
                        ]
                    );

                    let color = PanelCaps { has_color_panel: true, has_eclipse_wfm, is_mtk };
                    let got: Vec<_> = ALL_MODES.map(|m| plan_refresh(m, false, color, variant)).into();
                    assert_eq!(
                        got,
                        vec![
                            RefreshPlan { waveform: Waveform::Du, is_flashing: false, mark_flash_fence: false },
                            RefreshPlan { waveform: Waveform::Gl16, is_flashing: false, mark_flash_fence: false },
                            RefreshPlan { waveform: Waveform::Glrc16, is_flashing: false, mark_flash_fence: false },
                            RefreshPlan { waveform: Waveform::Gcc16, is_flashing: true, mark_flash_fence: true },
                        ]
                    );
                }
            }
        }
    }

    #[test]
    fn dark_eclipse_waveforms_gate_on_has_eclipse_wfm_and_dunm_on_is_mtk() {
        // Mono MTK with eclipse waveforms (e.g. PW5 / PW6 / Scribe).
        let pw6 = PanelCaps { has_color_panel: false, has_eclipse_wfm: true, is_mtk: true };
        assert_eq!(
            ALL_MODES.map(|m| plan_refresh(m, true, pw6, DarkVariant::A)),
            [
                RefreshPlan { waveform: Waveform::Dunm, is_flashing: false, mark_flash_fence: false },
                RefreshPlan { waveform: Waveform::Glkw16, is_flashing: false, mark_flash_fence: false },
                RefreshPlan { waveform: Waveform::Glkw16, is_flashing: false, mark_flash_fence: false },
                RefreshPlan { waveform: Waveform::Gck16, is_flashing: true, mark_flash_fence: true },
            ]
        );

        // Non-MTK with eclipse waveforms: DUNM falls back to DU, eclipse modes stay on.
        let imx_eclipse = PanelCaps { has_color_panel: false, has_eclipse_wfm: true, is_mtk: false };
        assert_eq!(
            ALL_MODES.map(|m| plan_refresh(m, true, imx_eclipse, DarkVariant::A)),
            [
                RefreshPlan { waveform: Waveform::Du, is_flashing: false, mark_flash_fence: false },
                RefreshPlan { waveform: Waveform::Glkw16, is_flashing: false, mark_flash_fence: false },
                RefreshPlan { waveform: Waveform::Glkw16, is_flashing: false, mark_flash_fence: false },
                RefreshPlan { waveform: Waveform::Gck16, is_flashing: true, mark_flash_fence: true },
            ]
        );

        // Colorsoft (color + eclipse + MTK).
        let colorsoft = PanelCaps { has_color_panel: true, has_eclipse_wfm: true, is_mtk: true };
        assert_eq!(
            ALL_MODES.map(|m| plan_refresh(m, true, colorsoft, DarkVariant::A)),
            [
                RefreshPlan { waveform: Waveform::Dunm, is_flashing: false, mark_flash_fence: false },
                RefreshPlan { waveform: Waveform::Glkw16, is_flashing: false, mark_flash_fence: false },
                RefreshPlan { waveform: Waveform::Glrck16, is_flashing: false, mark_flash_fence: false },
                RefreshPlan { waveform: Waveform::Gcck16, is_flashing: true, mark_flash_fence: true },
            ]
        );

        // Without has_eclipse_wfm (PW4, Basic 3, PW3): fall back to light waveforms
        // with PARTIAL for non-Full even under Variant B.
        for is_mtk in [false, true] {
            for variant in [DarkVariant::A, DarkVariant::B] {
                let old_mono = PanelCaps { has_color_panel: false, has_eclipse_wfm: false, is_mtk };
                assert_eq!(
                    ALL_MODES.map(|m| plan_refresh(m, true, old_mono, variant)),
                    ALL_MODES.map(|m| plan_refresh(m, false, old_mono, variant)),
                    "mono fallback is_mtk={is_mtk} {variant:?}"
                );
                let old_color = PanelCaps { has_color_panel: true, has_eclipse_wfm: false, is_mtk };
                assert_eq!(
                    ALL_MODES.map(|m| plan_refresh(m, true, old_color, variant)),
                    ALL_MODES.map(|m| plan_refresh(m, false, old_color, variant)),
                    "color fallback is_mtk={is_mtk} {variant:?}"
                );
            }
        }
    }

    #[test]
    fn only_full_marks_the_flash_fence_even_when_glkw16_uses_full_update_mode() {
        let pw6 = PanelCaps { has_color_panel: false, has_eclipse_wfm: true, is_mtk: true };
        let plans = ALL_MODES.map(|m| plan_refresh(m, true, pw6, DarkVariant::B));
        assert_eq!(plans, ALL_MODES.map(|m| plan_refresh(m, true, pw6, DarkVariant::C)));
        assert_eq!(
            ALL_MODES.map(|m| plan_refresh(m, true, pw6, DarkVariant::CPartial)),
            ALL_MODES.map(|m| plan_refresh(m, true, pw6, DarkVariant::A))
        );
        assert_eq!(
            plans,
            [
                RefreshPlan { waveform: Waveform::Dunm, is_flashing: false, mark_flash_fence: false },
                RefreshPlan { waveform: Waveform::Glkw16, is_flashing: true, mark_flash_fence: false },
                RefreshPlan { waveform: Waveform::Glkw16, is_flashing: true, mark_flash_fence: false },
                RefreshPlan { waveform: Waveform::Gck16, is_flashing: true, mark_flash_fence: true },
            ]
        );
        // REAGL baseline uses light waveforms on dark canvases.
        assert_eq!(
            ALL_MODES.map(|m| plan_refresh(m, true, pw6, DarkVariant::Reagl)),
            ALL_MODES.map(|m| plan_refresh(m, false, pw6, DarkVariant::A))
        );
    }
}
