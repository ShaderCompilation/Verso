//! What Verso knows about each Kindle model, keyed by FBInk's device id
//! (`FBInkState.device_id`, the model code from the serial number; one
//! model has several).
//!
//! Sources: the ids are FBInk's `fbink_device_id.c` (submodule at
//! 886f25f1); lights, page buttons and screen controllers are KOReader's
//! `frontend/device/kindle/device.lua` and `powerd.lua` (master at
//! 1294bb3d, 2026-11-21). KOReader drives every Kindle's light through
//! powerd's `flIntensity` and `currentAmberLevel` with 0..=24 steps.
//! On the PW2, PW3 and Voyage (`canTurnFrontlightOff = no`) step 0 is dim,
//! not off; their firmware is soft-float, so Verso doesn't run there anyway.
//!
//! Only models with a report are `tested`.

use super::{DeviceModel, Frontlight, LightRange, PageButtons};

/// Which lights the model has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Light {
    None,
    White,
    WhiteWarm,
}

/// The e-ink screen controller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Epdc {
    /// NXP i.MX (Kindles up to 2019).
    Imx,
    /// MediaTek (2021 on).
    Mtk,
}

#[derive(Debug)]
pub struct Profile {
    pub ids: &'static [u16],
    pub model: &'static str,
    pub tested: bool,
    pub light: Light,
    pub range: LightRange,
    pub page_buttons: Option<PageButtons>,
    pub epdc: Epdc,
}

const STEPS: LightRange = LightRange { brightness: 24, warmth: 24 };

const fn profile(ids: &'static [u16], model: &'static str, light: Light, epdc: Epdc) -> Profile {
    Profile { ids, model, tested: false, light, range: STEPS, page_buttons: None, epdc }
}

const fn with_buttons(profile: Profile, buttons: PageButtons) -> Profile {
    Profile { page_buttons: Some(buttons), ..profile }
}

use Epdc::{Imx, Mtk};
use Light::{White, WhiteWarm};

pub const PROFILES: &[Profile] = &[
    // MediaTek, 2021 on.
    Profile {
        tested: true,
        ..profile(
            &[0xC89, 0xC86, 0xC7F, 0xC7E, 0xE2A, 0xE25, 0xE23, 0xE28, 0xE45, 0xE5A],
            "Kindle Paperwhite 6 (2024)",
            WhiteWarm,
            Mtk,
        )
    },
    profile(
        &[0x690, 0x700, 0x6FF, 0x7AD, 0x829, 0x82A, 0x971, 0x972, 0x9B3],
        "Kindle Paperwhite 5 (2021)",
        WhiteWarm,
        Mtk,
    ),
    profile(
        &[0xE29, 0xE24, 0xE2B, 0xE26, 0xE22, 0xC9F, 0xE27, 0xE5B, 0xE46, 0x10A6, 0x10A5, 0x11D7],
        "Kindle Colorsoft (2024)",
        WhiteWarm,
        Mtk,
    ),
    profile(&[0x84D, 0x8BB, 0x86A, 0x958, 0x957, 0x7F1, 0x84C], "Kindle Basic 4 (2022)", White, Mtk),
    profile(&[0xE85, 0xE86, 0xE84, 0xE83, 0x2909, 0xE82, 0xE75], "Kindle Basic 5 (2024)", White, Mtk),
    profile(&[0x8F2, 0x974, 0x8C3, 0x847, 0x975, 0x874, 0x875, 0x8E0], "Kindle Scribe (2022)", WhiteWarm, Mtk),
    profile(&[0xFA0, 0xFA1, 0xFE5, 0xF9D, 0xFE4, 0xFE3, 0x102E, 0x102D], "Kindle Scribe 2 (2024)", WhiteWarm, Mtk),
    profile(&[0x12F0, 0x12EE, 0x12F4, 0x11E8, 0x11EA, 0x10A4], "Kindle Scribe 3 (2025)", WhiteWarm, Mtk),
    profile(&[0x13BF, 0x12EF, 0x12F1, 0x11E9, 0x11EB, 0x10D7], "Kindle Scribe Colorsoft (2025)", WhiteWarm, Mtk),
    // i.MX, up to 2019.
    profile(
        &[
            0x2F7, 0x361, 0x362, 0x363, 0x364, 0x365, 0x366, 0x367, 0x372, 0x373, 0x374, 0x375, 0x376, 0x402, 0x403,
            0x4D8, 0x4D9, 0x4DA, 0x4DB, 0x4DC, 0x4DD,
        ],
        "Kindle Paperwhite 4 (2018)",
        White,
        Imx,
    ),
    profile(&[0x414, 0x3CF, 0x3D0, 0x3D1, 0x3D2, 0x3AB], "Kindle Basic 3 (2019)", White, Imx),
    with_buttons(
        profile(&[0x434, 0x3D8, 0x3D7, 0x3D6, 0x3D5, 0x3D4], "Kindle Oasis 3 (2019)", WhiteWarm, Imx),
        PageButtons::Oasis,
    ),
    with_buttons(
        profile(
            &[0x295, 0x296, 0x297, 0x298, 0x2E1, 0x2E2, 0x2E6, 0x2E7, 0x2E8, 0x341, 0x342, 0x343, 0x344, 0x347, 0x34A],
            "Kindle Oasis 2 (2017)",
            White,
            Imx,
        ),
        PageButtons::Oasis,
    ),
    with_buttons(
        profile(&[0x20C, 0x20D, 0x219, 0x21A, 0x21B, 0x21C], "Kindle Oasis (2016)", White, Imx),
        PageButtons::Oasis,
    ),
    profile(
        &[0x201, 0x202, 0x204, 0x205, 0x206, 0x207, 0x26B, 0x26C, 0x26D, 0x26E, 0x26F, 0x270, 0x293, 0x294],
        "Kindle Paperwhite 3 (2015)",
        White,
        Imx,
    ),
    profile(&[0x1BC, 0x269, 0x26A], "Kindle Basic 2 (2016)", Light::None, Imx),
    with_buttons(
        profile(&[0x13, 0x54, 0x2A, 0x4F, 0x52, 0x53], "Kindle Voyage (2014)", White, Imx),
        PageButtons::Named,
    ),
    profile(
        &[0xD4, 0x5A, 0xD5, 0xD6, 0xD7, 0xD8, 0xF2, 0x17, 0x60, 0xF4, 0xF9, 0x62, 0x61, 0x5F],
        "Kindle Paperwhite 2 (2013)",
        White,
        Imx,
    ),
    profile(&[0xC6, 0xDD], "Kindle Basic (2014)", Light::None, Imx),
];

pub fn lookup(id: u16) -> Option<&'static Profile> {
    PROFILES.iter().find(|p| p.ids.contains(&id))
}

/// The model as the app sees it. An id missing from the table is untested,
/// named after FBInk's guess when it has one.
pub fn model(profile: Option<&Profile>, id: u16, fbink_name: &str) -> DeviceModel {
    match profile {
        Some(p) => DeviceModel {
            name: p.model.to_owned(),
            untested: !p.tested,
            key: p.model.to_owned(),
            light_range: p.range,
            page_buttons: p.page_buttons.is_some(),
        },
        None => {
            let known =
                !fbink_name.is_empty() && !fbink_name.starts_with("Unknown") && !fbink_name.starts_with("Unidentified");
            let name =
                if known { format!("this Kindle ({fbink_name})") } else { format!("this Kindle (id {id:#06x})") };
            DeviceModel { name, untested: true, key: format!("id {id:#06x}"), ..DeviceModel::default() }
        }
    }
}

/// How to read and set the light: what the model has (`None` for a model
/// missing from the table, where the startup probe of powerd decides) and
/// its range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lights {
    pub kind: Option<Light>,
    pub range: LightRange,
}

impl Lights {
    pub fn for_profile(profile: Option<&Profile>) -> Self {
        Self { kind: profile.map(|p| p.light), range: profile.map_or_else(LightRange::default, |p| p.range) }
    }

    /// The light from powerd's `flIntensity` and `currentAmberLevel`
    /// (`None` when the property is missing): no controls without a
    /// frontlight, no warmth without warm LEDs.
    pub fn reading(&self, brightness: Option<i32>, warmth: Option<i32>) -> Option<Frontlight> {
        let (has_light, has_warmth) = match self.kind {
            Some(Light::None) => (false, false),
            Some(Light::White) => (true, false),
            Some(Light::WhiteWarm) | None => (true, true),
        };
        let brightness = brightness.filter(|_| has_light)?;
        Some(Frontlight {
            brightness: brightness.clamp(0, self.range.brightness.into()) as u8,
            warmth: warmth.filter(|_| has_warmth).map(|w| w.clamp(0, self.range.warmth.into()) as u8),
        })
    }

    /// A requested light, within the model's range.
    pub fn clamp(&self, value: Frontlight) -> Frontlight {
        Frontlight {
            brightness: value.brightness.min(self.range.brightness),
            warmth: value.warmth.filter(|_| self.kind != Some(Light::White)).map(|w| w.min(self.range.warmth)),
        }
    }

    /// "white + warm, 0..=24 / 0..=24", for the session header.
    pub fn describe(&self) -> String {
        let r = self.range;
        match self.kind {
            Some(Light::None) => "none".into(),
            Some(Light::White) => format!("white, 0..={}", r.brightness),
            Some(Light::WhiteWarm) => format!("white + warm, 0..={} / 0..={}", r.brightness, r.warmth),
            None => format!("probed, 0..={} / 0..={}", r.brightness, r.warmth),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_find_their_model_and_no_id_is_listed_twice() {
        assert_eq!(lookup(0xC89).map(|p| p.model), Some("Kindle Paperwhite 6 (2024)"));
        assert_eq!(lookup(0xE5A).map(|p| p.model), Some("Kindle Paperwhite 6 (2024)"));
        assert_eq!(lookup(0x10A6).map(|p| p.model), Some("Kindle Colorsoft (2024)"));
        assert_eq!(lookup(0x414).map(|p| p.model), Some("Kindle Basic 3 (2019)"));
        assert!(lookup(0x434).is_some_and(|p| p.page_buttons == Some(PageButtons::Oasis)
            && p.light == Light::WhiteWarm
            && p.epdc == Epdc::Imx));
        assert_eq!(lookup(0xFFFF).map(|p| p.model), None);
        let mut ids: Vec<u16> = PROFILES.iter().flat_map(|p| p.ids.iter().copied()).collect();
        let n = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), n, "an id belongs to two models");
    }

    #[test]
    fn only_the_paperwhite_6_is_tested() {
        let tested: Vec<_> = PROFILES.iter().filter(|p| p.tested).map(|p| p.model).collect();
        assert_eq!(tested, ["Kindle Paperwhite 6 (2024)"]);
        let pw6 = model(lookup(0xC89), 0xC89, "PaperWhite 6");
        assert!(!pw6.untested);
        assert_eq!(pw6.light_range, LightRange { brightness: 24, warmth: 24 });
        let basic = model(lookup(0x84D), 0x84D, "Basic 4");
        assert!(basic.untested);
        assert_eq!((basic.name.as_str(), basic.key.as_str()), ("Kindle Basic 4 (2022)", "Kindle Basic 4 (2022)"));
    }

    #[test]
    fn unknown_ids_are_untested_and_named_by_fbink_or_id() {
        let m = model(None, 0x1234, "Scribe 4");
        assert!(m.untested);
        assert_eq!(m.name, "this Kindle (Scribe 4)");
        assert_eq!(m.key, "id 0x1234");
        assert_eq!(m.light_range, LightRange::default());
        assert_eq!(model(None, 0x4FB9, "Unknown!").name, "this Kindle (id 0x4fb9)");
        assert_eq!(model(None, 0x99, "Unidentified!").name, "this Kindle (id 0x0099)");
    }

    #[test]
    fn unknown_models_trust_the_probe() {
        let probe = Lights::for_profile(None);
        assert_eq!(probe.kind, None);
        assert_eq!(probe.reading(None, Some(3)), None, "no flIntensity: no light controls");
        assert_eq!(probe.reading(Some(9), None), Some(Frontlight { brightness: 9, warmth: None }), "no warmth slider");
        assert_eq!(probe.reading(Some(9), Some(4)), Some(Frontlight { brightness: 9, warmth: Some(4) }));
    }

    #[test]
    fn known_models_keep_only_the_lights_they_have() {
        let basic = Lights::for_profile(lookup(0x84D));
        assert_eq!(basic.reading(Some(9), Some(4)), Some(Frontlight { brightness: 9, warmth: None }));
        let dark = Lights::for_profile(lookup(0x1BC));
        assert_eq!(dark.reading(Some(9), Some(4)), None);
        let pw6 = Lights::for_profile(lookup(0xC89));
        assert_eq!(pw6.reading(Some(9), Some(4)), Some(Frontlight { brightness: 9, warmth: Some(4) }));
        assert_eq!(pw6.reading(Some(9), None), Some(Frontlight { brightness: 9, warmth: None }));
    }

    #[test]
    fn readings_and_requests_scale_to_the_range() {
        let pw6 = Lights::for_profile(lookup(0xC89));
        assert_eq!(pw6.reading(Some(40), Some(-3)), Some(Frontlight { brightness: 24, warmth: Some(0) }));
        assert_eq!(
            pw6.clamp(Frontlight { brightness: 30, warmth: Some(25) }),
            Frontlight { brightness: 24, warmth: Some(24) }
        );
        let small = Lights { kind: Some(Light::WhiteWarm), range: LightRange { brightness: 10, warmth: 5 } };
        assert_eq!(small.reading(Some(12), Some(8)), Some(Frontlight { brightness: 10, warmth: Some(5) }));
        assert_eq!(
            small.clamp(Frontlight { brightness: 12, warmth: Some(8) }),
            Frontlight { brightness: 10, warmth: Some(5) }
        );
        let white = Lights::for_profile(lookup(0x2F7));
        assert_eq!(
            white.clamp(Frontlight { brightness: 5, warmth: Some(8) }),
            Frontlight { brightness: 5, warmth: None }
        );
    }
}
