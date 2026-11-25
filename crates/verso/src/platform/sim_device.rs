//! The simulator's device presets: screen size, density and capabilities of
//! the Kindle models Verso targets, picked with `--device <name>` or
//! `VERSO_SIM_DEVICE=<name>` (the flag wins). `VERSO_SIM_SIZE`,
//! `VERSO_SIM_DPI`, `VERSO_SIM_SCALE` and `VERSO_SIM_LIGHT` still override
//! single fields. `VERSO_SIM_UNTESTED=1` makes the model untested, to see
//! the untested-model notice. On a color preset `VERSO_SIM_KALEIDO=1`
//! starts with the Kaleido 3 preview on (kaleido.rs).

use anyhow::{Context, Result, bail};

use super::{DeviceModel, Frontlight};
use crate::gfx::ColorMode;

/// Which lights the device has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Light {
    None,
    White,
    WhiteWarm,
}

/// One row of the preset table.
#[derive(Clone, Copy, Debug)]
pub struct Preset {
    pub name: &'static str,
    pub width: u32,
    pub height: u32,
    pub dpi: u32,
    pub light: Light,
    pub page_buttons: bool,
    /// A color panel: the app draws on a `Color32` canvas.
    pub color: bool,
    /// The window's starting size relative to the screen.
    pub scale: f64,
}

const fn preset(name: &'static str, width: u32, height: u32, dpi: u32, light: Light) -> Preset {
    Preset { name, width, height, dpi, light, page_buttons: false, color: false, scale: 0.5 }
}

pub const DEFAULT: &str = "pw6";

pub const PRESETS: &[Preset] = &[
    // 6" Basic (10th and earlier): small enough to show at full size.
    Preset { scale: 1.0, ..preset("basic-167", 600, 800, 167, Light::White) },
    preset("basic-300", 1072, 1448, 300, Light::White),
    preset("pw5", 1236, 1648, 300, Light::WhiteWarm),
    preset("pw6", 1272, 1696, 300, Light::WhiteWarm),
    Preset { page_buttons: true, ..preset("oasis3", 1264, 1680, 300, Light::WhiteWarm) },
    Preset { color: true, ..preset("colorsoft", 1264, 1680, 300, Light::WhiteWarm) },
    // 10.2": a third of the panel still fits a laptop display.
    Preset { scale: 0.33, ..preset("scribe", 1860, 2480, 300, Light::WhiteWarm) },
];

/// The simulated device after the environment's overrides.
#[derive(Clone, Debug, PartialEq)]
pub struct SimDevice {
    pub name: &'static str,
    pub width: u32,
    pub height: u32,
    pub dpi: u32,
    pub light: Light,
    pub page_buttons: bool,
    pub color: bool,
    pub scale: f64,
    /// `VERSO_SIM_SIZE` or `VERSO_SIM_DPI` changed the preset's screen.
    pub custom_screen: bool,
    /// `VERSO_SIM_UNTESTED=1`: show the untested-model notice.
    pub untested: bool,
    /// `VERSO_SIM_KALEIDO=1` on a color preset: show (and save) the screen
    /// through the Kaleido 3 preview.
    pub kaleido: bool,
}

impl SimDevice {
    /// From the `--device` flag (if given) and the process environment.
    pub fn from_env(flag: Option<&str>) -> Result<Self> {
        resolve(flag, |name| std::env::var(name).ok())
    }

    /// How the app's canvas stores pixels on this device.
    pub fn color_mode(&self) -> ColorMode {
        if self.color { ColorMode::Color32 } else { ColorMode::Gray8 }
    }

    /// The frontlight a fresh simulator starts with.
    pub fn initial_light(&self) -> Option<Frontlight> {
        match self.light {
            Light::None => None,
            Light::White => Some(Frontlight { brightness: 12, warmth: None }),
            Light::WhiteWarm => Some(Frontlight { brightness: 12, warmth: Some(0) }),
        }
    }

    /// What the device can take of a requested light: nothing without a
    /// frontlight, no warmth without warm LEDs.
    pub fn accept_light(&self, value: Frontlight) -> Option<Frontlight> {
        match self.light {
            Light::None => None,
            Light::White => Some(Frontlight { warmth: None, ..value }),
            Light::WhiteWarm => Some(value),
        }
    }

    /// The model for the app: the preset, tested unless `VERSO_SIM_UNTESTED`.
    pub fn model(&self) -> DeviceModel {
        DeviceModel {
            name: format!("the simulated {}", self.name),
            untested: self.untested,
            key: format!("sim {}", self.name),
            page_buttons: self.page_buttons,
            ..DeviceModel::default()
        }
    }

    /// "pw6 · 1272×1696 @ 300 dpi", for the window title.
    pub fn label(&self) -> String {
        let custom = if self.custom_screen { " (custom screen)" } else { "" };
        format!("{}{custom} · {}×{} @ {} dpi", self.name, self.width, self.height, self.dpi)
    }

    /// One line for the session header.
    pub fn details(&self) -> String {
        let light = match self.light {
            Light::None => "none",
            Light::White => "white",
            Light::WhiteWarm => "white + warm",
        };
        format!(
            "sim device: {}{}, frontlight: {light}, page buttons: {}, color: {}",
            self.name,
            if self.custom_screen { " (custom screen)" } else { "" },
            yes_no(self.page_buttons),
            yes_no(self.color),
        )
    }
}

fn yes_no(v: bool) -> &'static str {
    if v { "yes" } else { "no" }
}

pub fn find(name: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.name.eq_ignore_ascii_case(name.trim()))
}

/// The preset named by `flag` or `VERSO_SIM_DEVICE` (default pw6), with
/// `VERSO_SIM_SIZE`, `VERSO_SIM_DPI`, `VERSO_SIM_SCALE` and `VERSO_SIM_LIGHT`
/// applied on top.
pub fn resolve(flag: Option<&str>, env: impl Fn(&str) -> Option<String>) -> Result<SimDevice> {
    let from_env = env("VERSO_SIM_DEVICE").filter(|s| !s.trim().is_empty());
    let (source, name) = match (flag, from_env.as_deref()) {
        (Some(flag), _) => ("--device", flag),
        (None, Some(env)) => ("VERSO_SIM_DEVICE", env),
        (None, None) => ("default", DEFAULT),
    };
    let Some(p) = find(name) else {
        let names: Vec<_> = PRESETS.iter().map(|p| p.name).collect();
        bail!("{source} {name:?} isn't a device; try {}", names.join(", "));
    };
    let mut device = SimDevice {
        name: p.name,
        width: p.width,
        height: p.height,
        dpi: p.dpi,
        light: p.light,
        page_buttons: p.page_buttons,
        color: p.color,
        scale: p.scale,
        custom_screen: false,
        untested: env("VERSO_SIM_UNTESTED").is_some_and(|v| v.trim() == "1"),
        kaleido: p.color && env("VERSO_SIM_KALEIDO").is_some_and(|v| v.trim() == "1"),
    };
    if let Some(s) = env("VERSO_SIM_SIZE") {
        (device.width, device.height) =
            parse_size(&s).with_context(|| format!("VERSO_SIM_SIZE={s:?}, expected e.g. 1272x1696"))?;
        device.custom_screen |= (device.width, device.height) != (p.width, p.height);
    }
    if let Some(s) = env("VERSO_SIM_DPI") {
        device.dpi = positive("VERSO_SIM_DPI", &s)?.round() as u32;
        device.custom_screen |= device.dpi != p.dpi;
    }
    if let Some(s) = env("VERSO_SIM_SCALE") {
        device.scale = positive("VERSO_SIM_SCALE", &s)?;
    }
    if let Some(s) = env("VERSO_SIM_LIGHT") {
        device.light = match s.trim() {
            "none" => Light::None,
            "white" => Light::White,
            "warm" => Light::WhiteWarm,
            _ => bail!("VERSO_SIM_LIGHT={s:?}, expected none, white or warm"),
        };
    }
    Ok(device)
}

fn parse_size(s: &str) -> Option<(u32, u32)> {
    let (w, h) = s.split_once('x')?;
    let (w, h) = (w.trim().parse().ok()?, h.trim().parse().ok()?);
    (w > 0 && h > 0).then_some((w, h))
}

fn positive(name: &str, s: &str) -> Result<f64> {
    s.trim().parse::<f64>().ok().filter(|v| *v > 0.0).with_context(|| format!("{name}={s:?} isn't a positive number"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| vars.iter().find(|(k, _)| *k == name).map(|(_, v)| v.to_string())
    }

    #[test]
    fn default_is_the_paperwhite_6() {
        let d = resolve(None, env(&[])).unwrap();
        assert_eq!((d.name, d.width, d.height, d.dpi), ("pw6", 1272, 1696, 300));
        assert_eq!(d.light, Light::WhiteWarm);
        assert!(!d.page_buttons && !d.color && !d.custom_screen);
        assert_eq!(d.scale, 0.5);
    }

    #[test]
    fn untested_override_marks_the_model() {
        let d = resolve(Some("basic-300"), env(&[])).unwrap();
        assert!(!d.model().untested, "presets are tested by default, so captures show no notice");
        let d = resolve(Some("basic-300"), env(&[("VERSO_SIM_UNTESTED", "1")])).unwrap();
        let m = d.model();
        assert!(m.untested);
        assert_eq!((m.name.as_str(), m.key.as_str()), ("the simulated basic-300", "sim basic-300"));
        assert_eq!(m.light_range, super::super::LightRange::default());
    }

    #[test]
    fn every_preset_resolves_by_name() {
        for p in PRESETS {
            let d = resolve(Some(p.name), env(&[])).unwrap();
            assert_eq!((d.width, d.height, d.dpi), (p.width, p.height, p.dpi), "{}", p.name);
            // Portrait panels, at a density the theme supports.
            assert!(d.width < d.height && d.dpi >= 160, "{}", p.name);
        }
        let names: Vec<_> = PRESETS.iter().map(|p| p.name).collect();
        assert_eq!(names, ["basic-167", "basic-300", "pw5", "pw6", "oasis3", "colorsoft", "scribe"]);
    }

    #[test]
    fn capabilities_follow_the_table() {
        let get = |name| resolve(Some(name), env(&[])).unwrap();
        assert_eq!(get("basic-167").light, Light::White);
        assert_eq!(get("basic-300").initial_light(), Some(Frontlight { brightness: 12, warmth: None }));
        assert_eq!(get("pw5").initial_light(), Some(Frontlight { brightness: 12, warmth: Some(0) }));
        assert!(get("oasis3").page_buttons && !get("pw6").page_buttons);
        assert!(get("colorsoft").color && !get("oasis3").color);
        assert_eq!(get("colorsoft").color_mode(), ColorMode::Color32);
        assert_eq!(get("pw6").color_mode(), ColorMode::Gray8);
        assert!(!get("colorsoft").kaleido, "the preview starts off");
        let kaleido = [("VERSO_SIM_KALEIDO", "1")];
        assert!(resolve(Some("colorsoft"), env(&kaleido)).unwrap().kaleido);
        assert!(!resolve(Some("pw6"), env(&kaleido)).unwrap().kaleido, "only color panels have one");
        assert!(get("scribe").scale < 0.5);
        let warm = resolve(Some("basic-300"), env(&[("VERSO_SIM_LIGHT", "warm")])).unwrap();
        assert_eq!(warm.light, Light::WhiteWarm);
    }

    #[test]
    fn flag_beats_environment_and_names_ignore_case() {
        let vars = [("VERSO_SIM_DEVICE", "scribe")];
        assert_eq!(resolve(None, env(&vars)).unwrap().name, "scribe");
        assert_eq!(resolve(Some("Basic-167"), env(&vars)).unwrap().name, "basic-167");
        assert_eq!(resolve(None, env(&[("VERSO_SIM_DEVICE", " ")])).unwrap().name, "pw6");
    }

    #[test]
    fn unknown_names_list_the_presets() {
        let e = resolve(Some("kindle2"), env(&[])).unwrap_err().to_string();
        assert!(e.contains("--device") && e.contains("basic-167") && e.contains("scribe"), "{e}");
        let e = resolve(None, env(&[("VERSO_SIM_DEVICE", "nope")])).unwrap_err().to_string();
        assert!(e.contains("VERSO_SIM_DEVICE"), "{e}");
    }

    #[test]
    fn size_dpi_and_scale_override_single_fields() {
        let vars = [("VERSO_SIM_SIZE", "800x1000"), ("VERSO_SIM_SCALE", "0.8")];
        let d = resolve(Some("basic-167"), env(&vars)).unwrap();
        assert_eq!((d.width, d.height, d.dpi, d.scale), (800, 1000, 167, 0.8));
        assert_eq!(d.light, Light::White, "capabilities stay the preset's");
        assert!(d.custom_screen);
        assert!(d.label().contains("custom screen"));

        let d = resolve(Some("scribe"), env(&[("VERSO_SIM_DPI", "212")])).unwrap();
        assert_eq!((d.width, d.height, d.dpi), (1860, 2480, 212));

        // Restating the preset's own values isn't custom.
        let d = resolve(Some("pw6"), env(&[("VERSO_SIM_SIZE", "1272x1696"), ("VERSO_SIM_DPI", "300")])).unwrap();
        assert!(!d.custom_screen);
        assert_eq!(d.label(), "pw6 · 1272×1696 @ 300 dpi");
    }

    #[test]
    fn bad_overrides_are_errors() {
        assert!(resolve(None, env(&[("VERSO_SIM_SIZE", "big")])).is_err());
        assert!(resolve(None, env(&[("VERSO_SIM_SIZE", "0x800")])).is_err());
        assert!(resolve(None, env(&[("VERSO_SIM_DPI", "-3")])).is_err());
        assert!(resolve(None, env(&[("VERSO_SIM_SCALE", "zero")])).is_err());
        assert!(resolve(None, env(&[("VERSO_SIM_LIGHT", "bright")])).is_err());
    }

    #[test]
    fn lights_the_device_lacks_are_dropped() {
        let want = Frontlight { brightness: 20, warmth: Some(7) };
        let get = |name| resolve(Some(name), env(&[])).unwrap();
        assert_eq!(get("pw6").accept_light(want), Some(want));
        assert_eq!(get("basic-300").accept_light(want), Some(Frontlight { brightness: 20, warmth: None }));
        let dark = resolve(Some("pw6"), env(&[("VERSO_SIM_LIGHT", "none")])).unwrap();
        assert_eq!(dark.initial_light(), None);
        assert_eq!(dark.accept_light(want), None);
        assert!(dark.details().contains("frontlight: none"));
    }
}
