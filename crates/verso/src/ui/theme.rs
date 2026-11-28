//! Verso's look: grays, type scale, spacing and sizes. Defined in millimetres
//! and points and converted for the screen's density,
//! so the same design works on any e-reader.

use crate::gfx::{Family, TextStyle, Weight};

// Grays, all exact steps of the panel's 16-level scale (multiples of 0x11),
// so they show without dithering.
/// Text, icons, borders, pressed fills.
pub const INK: u8 = 0x00;
/// Solid dark area fills (selected, pressed, "on"). Never 0: i.MX partial
/// refreshes skip pixels whose value is unchanged, so book text under a pure
/// black fill ghosts through it and erodes when it closes. Text, icons and
/// borders stay `INK`.
pub const INK_FILL: u8 = 0x22;
/// Secondary text: authors, values, labels.
pub const INK2: u8 = 0x55;
/// Disabled text, page counts.
pub const INK3: u8 = 0x99;
/// Dividers, slider tracks.
pub const LINE: u8 = 0xbb;
/// Subtle fills (e.g. keyboard function keys).
pub const FILL: u8 = 0xee;
/// Background.
pub const PAPER: u8 = 0xff;
/// The flat offset behind pop-up menus.
pub const SHADOW: u8 = 0x88;

/// Every size in pixels for one screen density.
#[derive(Clone, Debug)]
pub struct Theme {
    pub dpi: f32,

    /// 6.5 pt: footer, status, small values.
    pub caption: TextStyle,
    /// 7.5 pt: titles under covers, second lines in list rows.
    pub small: TextStyle,
    /// 8.5 pt: list rows, labels.
    pub body: TextStyle,
    /// Dictionary prose: 9 pt Literata; headwords: 11 pt SemiBold.
    pub dictionary_body: TextStyle,
    pub dictionary_headword: TextStyle,
    /// Per nesting level, capped at four levels by the lookup screen (2.5 mm).
    pub dictionary_indent: i32,
    /// 8.5 pt SemiBold: buttons.
    pub button: TextStyle,
    /// 10 pt SemiBold: section and sheet titles.
    pub heading: TextStyle,
    /// 13 pt Literata SemiBold: screen titles.
    pub title: TextStyle,
    /// 6.5 pt SemiBold, UPPERCASE, +8% letter spacing: section labels.
    pub section: TextStyle,

    /// Left and right edge of every screen except the reader page (5 mm).
    pub margin: i32,
    /// Minimum touch target (9 mm).
    pub target: i32,
    pub top_bar: i32,
    pub row: i32,
    /// A list row with a second line.
    pub row2: i32,
    pub button_h: i32,
    pub icon: i32,
    pub icon_small: i32,
    /// Dividers (0.25 mm).
    pub hairline: i32,
    /// Buttons, sheets, the top bar edge (0.35 mm).
    pub border: i32,
    pub radius: f32,
    pub radius_sheet: f32,
}

impl Theme {
    pub fn for_dpi(dpi: u32) -> Self {
        let dpi = dpi as f32;
        let mm = |v: f32| (v * dpi / 25.4).round() as i32;
        let pt = |v: f32| (v * dpi / 72.0).round();
        let sans = |size: f32| TextStyle::new(pt(size), Weight::Regular, Family::Sans);
        Self {
            dpi,
            caption: sans(6.5),
            small: sans(7.5),
            body: sans(8.5),
            dictionary_body: TextStyle::new(pt(9.0), Weight::Regular, Family::Serif),
            dictionary_headword: TextStyle::new(pt(11.0), Weight::SemiBold, Family::Serif),
            dictionary_indent: mm(2.5),
            button: sans(8.5).weight(Weight::SemiBold),
            heading: sans(10.0).weight(Weight::SemiBold),
            title: TextStyle::new(pt(13.0), Weight::SemiBold, Family::Serif),
            section: sans(6.5).weight(Weight::SemiBold).gray(INK2).tracking(0.08),
            margin: mm(5.0),
            target: mm(9.0),
            top_bar: mm(13.0),
            row: mm(12.0),
            row2: mm(15.0),
            button_h: mm(10.0),
            icon: mm(5.0),
            icon_small: mm(3.5),
            hairline: mm(0.25).max(1),
            border: mm(0.35).max(1),
            radius: (1.5 * dpi / 25.4).round(),
            radius_sheet: (3.0 * dpi / 25.4).round(),
        }
    }

    /// Millimetres to pixels.
    pub fn mm(&self, v: f32) -> i32 {
        (v * self.dpi / 25.4).round() as i32
    }

    /// Typographic points to pixels.
    pub fn pt(&self, v: f32) -> f32 {
        (v * self.dpi / 72.0).round()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_at_300_dpi_match_the_design() {
        let t = Theme::for_dpi(300);
        assert_eq!(t.margin, 59);
        assert_eq!(t.target, 106);
        assert_eq!(t.top_bar, 154);
        assert_eq!(t.row, 142);
        assert_eq!(t.button_h, 118);
        assert_eq!(t.icon, 59);
        assert_eq!(t.hairline, 3);
        assert_eq!(t.border, 4);
        assert_eq!(t.caption.size, 27.0);
        assert_eq!(t.body.size, 35.0);
        assert_eq!(t.title.size, 54.0);
    }

    #[test]
    fn sizes_scale_with_dpi() {
        let t = Theme::for_dpi(167);
        assert_eq!(t.margin, 33);
        assert_eq!(t.target, 59);
        assert_eq!(t.hairline, 2);
        assert_eq!(t.body.size, 20.0);
        assert_eq!(t.mm(25.4), 167);
        // Hairlines never vanish on low-density screens.
        assert_eq!(Theme::for_dpi(60).hairline, 1);
    }

    #[test]
    fn grays_are_on_the_16_level_scale() {
        for g in [INK, INK2, INK3, LINE, FILL, PAPER, SHADOW] {
            assert_eq!(g % 0x11, 0, "{g:#x}");
        }
    }
}
