//! Target-independent e-ink waveform and flash-fence selection.
//!
//! Kept outside `platform/kindle/` so host `cargo test` verifies every light
//! and dark waveform rule, capability gate (`has_eclipse_wfm`, `is_mtk`) and
//! `FlashFence` trigger without needing ARM hardware or FBInk bindings.

use crate::platform::RefreshMode;

/// Panel capabilities reported by FBInk at startup.
#[cfg_attr(not(any(feature = "kindle", test)), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PanelCaps {
    pub has_color_panel: bool,
    pub has_eclipse_wfm: bool,
    pub is_mtk: bool,
}

/// Waveform mode requested from FBInk for one refresh.
#[cfg_attr(not(any(feature = "kindle", test)), allow(dead_code))]
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

/// How dark mode draws on the panel (`Settings::dark_rendering`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DarkRendering {
    /// Software pixel flip with eclipse waveforms (`GLKW16` etc.).
    #[default]
    Eclipse,
    /// Driver night mode (`GRAYSCALE_8BIT_INVERTED`) with eclipse waveforms.
    DriverNight,
    /// Software pixel flip with the light waveforms (`REAGL` etc.).
    Reagl,
}

impl DarkRendering {
    #[cfg_attr(not(feature = "kindle"), allow(dead_code))]
    pub fn uses_driver_night_mode(self) -> bool {
        self == Self::DriverNight
    }
}

/// How dark image pages and cover grids (`RefreshMode::Clean`) refresh
/// (`Settings::dark_pictures`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DarkPictures {
    /// Like a page turn, without the flash fence.
    NoFlash,
    /// The GC-class waveform on changed pixels only (`UPDATE_MODE_PARTIAL`).
    #[default]
    Changed,
    /// The GC-class waveform with a flash (`UPDATE_MODE_FULL`).
    Full,
}

/// The waveform settings of dark mode, as `plan_refresh` needs them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DarkRefresh {
    pub rendering: DarkRendering,
    /// `GLKW16` uses `UPDATE_MODE_FULL` (every pixel in the area redraws).
    pub full_updates: bool,
    pub pictures: DarkPictures,
}

/// What `Display::present` sends to FBInk and whether it arms `FlashFence`.
#[cfg_attr(not(any(feature = "kindle", test)), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RefreshPlan {
    pub waveform: Waveform,
    /// Maps to FBInk's `is_flashing` (`UPDATE_MODE_FULL` when true).
    pub is_flashing: bool,
    /// Whether `Display::present` records the marker in `FlashFence`.
    /// Gated on `RefreshMode::Full` and `Clean`, never on `is_flashing`.
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
///
/// Light mode ignores `dark`. `Clean` is `Full` in light mode. On a dark
/// canvas it keeps the GC-class waveform (pre-flipped pictures are bright and
/// grey, so `GLKW16` would leave ghosts) and `dark.pictures` picks the update
/// mode: `UPDATE_MODE_PARTIAL` by default, so unchanged black stays black.
#[cfg_attr(not(any(feature = "kindle", test)), allow(dead_code))]
pub fn plan_refresh(mode: RefreshMode, is_dark: bool, caps: PanelCaps, dark: DarkRefresh) -> RefreshPlan {
    if is_dark && mode == RefreshMode::Clean {
        match dark.pictures {
            DarkPictures::NoFlash => return plan_refresh(RefreshMode::Page, is_dark, caps, dark),
            DarkPictures::Changed => {}
            DarkPictures::Full => {
                let plan = plan_refresh(RefreshMode::Full, is_dark, caps, dark);
                return RefreshPlan { is_flashing: true, ..plan };
            }
        }
    }
    let mark_flash_fence = mode >= RefreshMode::Clean;
    if !is_dark || !caps.has_eclipse_wfm || dark.rendering == DarkRendering::Reagl {
        let waveform = if caps.has_color_panel {
            match mode {
                RefreshMode::Fast => Waveform::Du,
                RefreshMode::Ui => Waveform::Gl16,
                RefreshMode::Page => Waveform::Glrc16,
                RefreshMode::Clean | RefreshMode::Full => Waveform::Gcc16,
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
                RefreshMode::Clean | RefreshMode::Full => Waveform::Gc16,
            }
        };
        let is_flashing = mode == RefreshMode::Full || mode == RefreshMode::Clean && !is_dark;
        return RefreshPlan { waveform, is_flashing, mark_flash_fence };
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
        RefreshMode::Clean | RefreshMode::Full => {
            if caps.has_color_panel {
                Waveform::Gcck16
            } else {
                Waveform::Gck16
            }
        }
    };
    let is_flashing = mode == RefreshMode::Full || dark.full_updates && waveform == Waveform::Glkw16;
    RefreshPlan { waveform, is_flashing, mark_flash_fence }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_MODES: [RefreshMode; 5] =
        [RefreshMode::Fast, RefreshMode::Ui, RefreshMode::Page, RefreshMode::Clean, RefreshMode::Full];

    /// Every combination of the dark refresh settings.
    fn all_dark() -> Vec<DarkRefresh> {
        let mut all = Vec::new();
        for rendering in [DarkRendering::Eclipse, DarkRendering::DriverNight, DarkRendering::Reagl] {
            for full_updates in [false, true] {
                for pictures in [DarkPictures::NoFlash, DarkPictures::Changed, DarkPictures::Full] {
                    all.push(DarkRefresh { rendering, full_updates, pictures });
                }
            }
        }
        all
    }

    /// What a dark canvas gets without eclipse waveforms (or under `REAGL`):
    /// the light plans, except that `Clean` refreshes only changed pixels.
    fn light_with_partial_clean(caps: PanelCaps) -> [RefreshPlan; 5] {
        ALL_MODES.map(|m| {
            let plan = plan_refresh(m, false, caps, DarkRefresh::default());
            RefreshPlan { is_flashing: plan.is_flashing && m != RefreshMode::Clean, ..plan }
        })
    }

    #[test]
    fn light_waveforms_match_panel_color_and_only_clean_and_full_flash() {
        for has_eclipse_wfm in [false, true] {
            for is_mtk in [false, true] {
                for dark in all_dark() {
                    let mono = PanelCaps { has_color_panel: false, has_eclipse_wfm, is_mtk };
                    let got: Vec<_> = ALL_MODES.map(|m| plan_refresh(m, false, mono, dark)).into();
                    let ui = if is_mtk { Waveform::Gl16 } else { Waveform::Gc16 };
                    assert_eq!(
                        got,
                        vec![
                            RefreshPlan { waveform: Waveform::Du, is_flashing: false, mark_flash_fence: false },
                            RefreshPlan { waveform: ui, is_flashing: false, mark_flash_fence: false },
                            RefreshPlan { waveform: Waveform::Reagl, is_flashing: false, mark_flash_fence: false },
                            RefreshPlan { waveform: Waveform::Gc16, is_flashing: true, mark_flash_fence: true },
                            RefreshPlan { waveform: Waveform::Gc16, is_flashing: true, mark_flash_fence: true },
                        ]
                    );

                    let color = PanelCaps { has_color_panel: true, has_eclipse_wfm, is_mtk };
                    let got: Vec<_> = ALL_MODES.map(|m| plan_refresh(m, false, color, dark)).into();
                    assert_eq!(
                        got,
                        vec![
                            RefreshPlan { waveform: Waveform::Du, is_flashing: false, mark_flash_fence: false },
                            RefreshPlan { waveform: Waveform::Gl16, is_flashing: false, mark_flash_fence: false },
                            RefreshPlan { waveform: Waveform::Glrc16, is_flashing: false, mark_flash_fence: false },
                            RefreshPlan { waveform: Waveform::Gcc16, is_flashing: true, mark_flash_fence: true },
                            RefreshPlan { waveform: Waveform::Gcc16, is_flashing: true, mark_flash_fence: true },
                        ]
                    );
                    // Light Clean is Full, field for field.
                    for caps in [mono, color] {
                        assert_eq!(
                            plan_refresh(RefreshMode::Clean, false, caps, dark),
                            plan_refresh(RefreshMode::Full, false, caps, dark)
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn dark_eclipse_waveforms_gate_on_has_eclipse_wfm_and_dunm_on_is_mtk() {
        let default = DarkRefresh::default();
        // Mono MTK with eclipse waveforms (e.g. PW5 / PW6 / Scribe).
        let pw6 = PanelCaps { has_color_panel: false, has_eclipse_wfm: true, is_mtk: true };
        assert_eq!(
            ALL_MODES.map(|m| plan_refresh(m, true, pw6, default)),
            [
                RefreshPlan { waveform: Waveform::Dunm, is_flashing: false, mark_flash_fence: false },
                RefreshPlan { waveform: Waveform::Glkw16, is_flashing: false, mark_flash_fence: false },
                RefreshPlan { waveform: Waveform::Glkw16, is_flashing: false, mark_flash_fence: false },
                RefreshPlan { waveform: Waveform::Gck16, is_flashing: false, mark_flash_fence: true },
                RefreshPlan { waveform: Waveform::Gck16, is_flashing: true, mark_flash_fence: true },
            ]
        );

        // Non-MTK with eclipse waveforms: DUNM falls back to DU, eclipse modes stay on.
        let imx_eclipse = PanelCaps { has_color_panel: false, has_eclipse_wfm: true, is_mtk: false };
        assert_eq!(
            ALL_MODES.map(|m| plan_refresh(m, true, imx_eclipse, default)),
            [
                RefreshPlan { waveform: Waveform::Du, is_flashing: false, mark_flash_fence: false },
                RefreshPlan { waveform: Waveform::Glkw16, is_flashing: false, mark_flash_fence: false },
                RefreshPlan { waveform: Waveform::Glkw16, is_flashing: false, mark_flash_fence: false },
                RefreshPlan { waveform: Waveform::Gck16, is_flashing: false, mark_flash_fence: true },
                RefreshPlan { waveform: Waveform::Gck16, is_flashing: true, mark_flash_fence: true },
            ]
        );

        // Colorsoft (color + eclipse + MTK).
        let colorsoft = PanelCaps { has_color_panel: true, has_eclipse_wfm: true, is_mtk: true };
        assert_eq!(
            ALL_MODES.map(|m| plan_refresh(m, true, colorsoft, default)),
            [
                RefreshPlan { waveform: Waveform::Dunm, is_flashing: false, mark_flash_fence: false },
                RefreshPlan { waveform: Waveform::Glkw16, is_flashing: false, mark_flash_fence: false },
                RefreshPlan { waveform: Waveform::Glrck16, is_flashing: false, mark_flash_fence: false },
                RefreshPlan { waveform: Waveform::Gcck16, is_flashing: false, mark_flash_fence: true },
                RefreshPlan { waveform: Waveform::Gcck16, is_flashing: true, mark_flash_fence: true },
            ]
        );

        // Without has_eclipse_wfm (PW4, Basic 3, PW3): fall back to light waveforms
        // with PARTIAL for non-Full (Clean included) even with full updates.
        for is_mtk in [false, true] {
            for full_updates in [false, true] {
                let dark = DarkRefresh { full_updates, ..default };
                let old_mono = PanelCaps { has_color_panel: false, has_eclipse_wfm: false, is_mtk };
                assert_eq!(
                    ALL_MODES.map(|m| plan_refresh(m, true, old_mono, dark)),
                    light_with_partial_clean(old_mono),
                    "mono fallback is_mtk={is_mtk} {dark:?}"
                );
                let old_color = PanelCaps { has_color_panel: true, has_eclipse_wfm: false, is_mtk };
                assert_eq!(
                    ALL_MODES.map(|m| plan_refresh(m, true, old_color, dark)),
                    light_with_partial_clean(old_color),
                    "color fallback is_mtk={is_mtk} {dark:?}"
                );
            }
        }
    }

    #[test]
    fn full_updates_flash_only_glkw16_and_never_mark_the_flash_fence() {
        let pw6 = PanelCaps { has_color_panel: false, has_eclipse_wfm: true, is_mtk: true };
        let colorsoft = PanelCaps { has_color_panel: true, has_eclipse_wfm: true, is_mtk: true };
        let eclipse = DarkRefresh::default();
        let full = DarkRefresh { full_updates: true, ..eclipse };
        let driver = DarkRefresh { rendering: DarkRendering::DriverNight, ..eclipse };
        let driver_full = DarkRefresh { full_updates: true, ..driver };
        for caps in [pw6, colorsoft] {
            // Driver night mode uses the same waveforms as the pixel flip.
            let plans = ALL_MODES.map(|m| plan_refresh(m, true, caps, full));
            assert_eq!(plans, ALL_MODES.map(|m| plan_refresh(m, true, caps, driver_full)));
            let partial = ALL_MODES.map(|m| plan_refresh(m, true, caps, eclipse));
            assert_eq!(partial, ALL_MODES.map(|m| plan_refresh(m, true, caps, driver)));
            for (plan, base) in plans.iter().zip(partial) {
                let glkw16 = plan.waveform == Waveform::Glkw16;
                assert_eq!(*plan, RefreshPlan { is_flashing: base.is_flashing || glkw16, ..base }, "{caps:?}");
            }
        }
        assert_eq!(
            ALL_MODES.map(|m| plan_refresh(m, true, pw6, full)),
            [
                RefreshPlan { waveform: Waveform::Dunm, is_flashing: false, mark_flash_fence: false },
                RefreshPlan { waveform: Waveform::Glkw16, is_flashing: true, mark_flash_fence: false },
                RefreshPlan { waveform: Waveform::Glkw16, is_flashing: true, mark_flash_fence: false },
                RefreshPlan { waveform: Waveform::Gck16, is_flashing: false, mark_flash_fence: true },
                RefreshPlan { waveform: Waveform::Gck16, is_flashing: true, mark_flash_fence: true },
            ]
        );
        // REAGL uses light waveforms on dark canvases and ignores full updates.
        for full_updates in [false, true] {
            let reagl = DarkRefresh { rendering: DarkRendering::Reagl, full_updates, ..eclipse };
            assert_eq!(ALL_MODES.map(|m| plan_refresh(m, true, pw6, reagl)), light_with_partial_clean(pw6));
        }
    }

    #[test]
    fn dark_pictures_pick_partial_flashing_or_the_page_plan_for_clean() {
        for has_color_panel in [false, true] {
            for has_eclipse_wfm in [false, true] {
                let caps = PanelCaps { has_color_panel, has_eclipse_wfm, is_mtk: true };
                for dark in all_dark() {
                    let changed = DarkRefresh { pictures: DarkPictures::Changed, ..dark };
                    let plan = plan_refresh(RefreshMode::Clean, true, caps, dark);
                    let partial = plan_refresh(RefreshMode::Clean, true, caps, changed);
                    assert!(!partial.is_flashing && partial.mark_flash_fence, "{caps:?} {dark:?}");
                    let want = match dark.pictures {
                        DarkPictures::Changed => partial,
                        DarkPictures::Full => RefreshPlan { is_flashing: true, ..partial },
                        DarkPictures::NoFlash => plan_refresh(RefreshMode::Page, true, caps, dark),
                    };
                    assert_eq!(plan, want, "{caps:?} {dark:?}");
                    assert!(dark.pictures != DarkPictures::NoFlash || !plan.mark_flash_fence);
                    // Only Clean changes.
                    for m in ALL_MODES.into_iter().filter(|&m| m != RefreshMode::Clean) {
                        assert_eq!(plan_refresh(m, true, caps, dark), plan_refresh(m, true, caps, changed));
                    }
                }
            }
        }
    }
}
