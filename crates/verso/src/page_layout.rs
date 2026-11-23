//! Page layout: margins and the reader's top and bottom bars. Shared by the
//! engine (which lays text out inside the margins), the reader (which draws
//! the bars) and the layout screen.
use crate::geom::Rect;
use serde::{Deserialize, Serialize};

/// Height of a bar, in millimetres: one caption line.
pub const BAR_MM: f32 = 6.0;
/// Margin range, in millimetres.
pub const MIN_MARGIN_MM: u32 = 2;
pub const MAX_MARGIN_MM: u32 = 20;
/// In landscape the text column is at most this many ems wide (about 70
/// characters), centred; the side margin is then only a minimum.
const LANDSCAPE_COLUMN_EM: f32 = 30.0;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PageLayout {
    pub top: Bar,
    pub bottom: Bar,
    pub progress: ProgressLine,
    /// Ticks at chapter starts along the progress line.
    pub chapter_marks: bool,
    pub margins: Margins,
}
impl Default for PageLayout {
    fn default() -> Self {
        // The bottom bar is the footer Verso always had.
        Self {
            top: Bar { on: false, slots: [Some(BarItem::BookTitle), None, Some(BarItem::Clock24)] },
            bottom: Bar {
                on: true,
                slots: [Some(BarItem::ChapterTitle), Some(BarItem::PageTotal), Some(BarItem::Percent)],
            },
            progress: ProgressLine::Bottom,
            chapter_marks: true,
            margins: Margins::default(),
        }
    }
}

/// One bar: on or off, and what its left, centre and right slots show.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Bar {
    pub on: bool,
    pub slots: [Option<BarItem>; 3],
}

/// What a slot can show.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BarItem {
    ChapterTitle,
    ChapterPagesLeft,
    BookTitle,
    Author,
    PagesLeft,
    PageInChapter,
    TimeLeftChapter,
    Percent,
    PageTotal,
    TimeLeftBook,
    #[serde(rename = "clock_24h")]
    Clock24,
    #[serde(rename = "clock_12h")]
    Clock12,
    Battery,
}
impl BarItem {
    /// Every item, in the order the slot picker lists them.
    pub const ALL: [BarItem; 13] = [
        BarItem::ChapterTitle,
        BarItem::ChapterPagesLeft,
        BarItem::BookTitle,
        BarItem::Author,
        BarItem::PagesLeft,
        BarItem::PageInChapter,
        BarItem::TimeLeftChapter,
        BarItem::Percent,
        BarItem::PageTotal,
        BarItem::TimeLeftBook,
        BarItem::Clock24,
        BarItem::Clock12,
        BarItem::Battery,
    ];

    /// The item's name in the slot picker.
    pub fn label(self) -> &'static str {
        match self {
            BarItem::ChapterTitle => "Chapter title",
            BarItem::ChapterPagesLeft => "Chapter and pages left",
            BarItem::BookTitle => "Book title",
            BarItem::Author => "Author",
            BarItem::PagesLeft => "Pages left in chapter",
            BarItem::PageInChapter => "Page in chapter",
            BarItem::TimeLeftChapter => "Time left in chapter",
            BarItem::Percent => "Percent",
            BarItem::PageTotal => "Page / total",
            BarItem::TimeLeftBook => "Time left in book",
            BarItem::Clock24 => "Clock (24 h)",
            BarItem::Clock12 => "Clock (12 h)",
            BarItem::Battery => "Battery",
        }
    }
}

/// A thin line across one bar, filled to the book percentage.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgressLine {
    #[default]
    Off,
    Top,
    Bottom,
}

/// White space between the screen edge and the text when the bar on that
/// side is off, in whole millimetres. An enabled bar adds `BAR_MM`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Margins {
    pub sides: u32,
    pub top: u32,
    pub bottom: u32,
}
impl Default for Margins {
    fn default() -> Self {
        Self { sides: 3, top: 2, bottom: 2 }
    }
}

/// Margins and bar rectangles in pixels for one screen and font size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageGeometry {
    /// Left and right text margin.
    pub side: i32,
    pub top: i32,
    pub bottom: i32,
    pub top_bar: Option<Rect>,
    pub bottom_bar: Option<Rect>,
}

impl PageLayout {
    /// Older settings had a footer toggle and three margin presets.
    pub fn migrate(&mut self, footer: Option<bool>, margins: Option<usize>) {
        if let Some(on) = footer {
            self.bottom.on = on;
        }
        if let Some(preset) = margins {
            self.margins.sides = [4, 7, 10][preset.min(2)];
        }
    }

    /// True when going from `other` to this layout moves the text: margins
    /// and bar toggles do; slot contents and the progress line don't.
    pub fn moves_text(&self, other: &PageLayout) -> bool {
        self.margins != other.margins || self.top.on != other.top.on || self.bottom.on != other.bottom.on
    }

    pub fn normalize(&mut self) {
        for m in [&mut self.margins.sides, &mut self.margins.top, &mut self.margins.bottom] {
            *m = (*m).clamp(MIN_MARGIN_MM, MAX_MARGIN_MM);
        }
    }

    /// The text margins and bars on a `width` × `height` screen. The text
    /// column and the bars share their side margins; vertically a margin is
    /// split evenly around its bar.
    pub fn geometry(&self, width: i32, height: i32, dpi: f32, font_px: i32) -> PageGeometry {
        let mm = |v: f32| (v * dpi / 25.4).round() as i32;
        let mut side = mm(self.margins.sides as f32);
        if width > height {
            let column = (font_px as f32 * LANDSCAPE_COLUMN_EM) as i32;
            side = side.max((width - column) / 2);
        }
        let column = width - 2 * side;
        // Outer edge at half the margin, inner edge one bar height further.
        let band = |margin: u32| (mm(margin as f32 / 2.0), mm(margin as f32 / 2.0 + BAR_MM));
        let top_bar = self.top.on.then(|| {
            let (outer, inner) = band(self.margins.top);
            Rect::new(side, outer, column, inner - outer)
        });
        let bottom_bar = self.bottom.on.then(|| {
            let (outer, inner) = band(self.margins.bottom);
            Rect::new(side, height - inner, column, inner - outer)
        });
        let text = |margin: u32, on: bool| mm(margin as f32 + if on { BAR_MM } else { 0.0 });
        PageGeometry {
            side,
            top: text(self.margins.top, self.top.on),
            bottom: text(self.margins.bottom, self.bottom.on),
            top_bar,
            bottom_bar,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_old_margins_reproduce_the_old_footer() {
        let layout = PageLayout { margins: Margins { sides: 7, top: 6, bottom: 6 }, ..PageLayout::default() };
        let g = layout.geometry(1272, 1696, 300.0, 42);
        let mm = |v: f32| (v * 300.0 / 25.4).round() as i32;
        // Old crengine margins: Normal sides 7 mm, top 6 mm, bottom 12 mm with the footer.
        assert_eq!((g.side, g.top, g.bottom), (mm(7.0), mm(6.0), mm(12.0)));
        // Old footer: inset 7 mm, 9 mm from the bottom edge, 6 mm tall.
        assert_eq!(g.bottom_bar, Some(Rect::new(mm(7.0), 1696 - mm(9.0), 1272 - 2 * mm(7.0), mm(6.0))));
        assert_eq!(g.top_bar, None);
    }

    #[test]
    fn bars_follow_the_margins_and_text_column() {
        let mut layout = PageLayout::default();
        layout.top.on = true;
        layout.margins = Margins { sides: 4, top: 10, bottom: 2 };
        let g = layout.geometry(1272, 1696, 300.0, 42);
        let mm = |v: f32| (v * 300.0 / 25.4).round() as i32;
        assert_eq!(g.top_bar, Some(Rect::new(mm(4.0), mm(5.0), 1272 - 2 * mm(4.0), mm(11.0) - mm(5.0))));
        assert_eq!(g.top, mm(16.0));
        let bottom = g.bottom_bar.unwrap();
        assert_eq!((bottom.y, bottom.bottom()), (1696 - mm(7.0), 1696 - mm(1.0)));
        assert_eq!(g.bottom, mm(8.0));
        layout.bottom.on = false;
        let g = layout.geometry(1272, 1696, 300.0, 42);
        assert_eq!((g.bottom, g.bottom_bar), (mm(2.0), None));
    }

    #[test]
    fn landscape_keeps_a_readable_column() {
        let layout = PageLayout::default();
        // 10 pt (42 px): a 1260 px column, centred; bars span the same column.
        let g = layout.geometry(1696, 1272, 300.0, 42);
        assert_eq!(g.side, 218);
        assert_eq!(g.bottom_bar.map(|r| (r.x, r.w)), Some((218, 1696 - 436)));
        // Large type already makes short lines: the margin wins.
        assert_eq!(layout.geometry(1696, 1272, 300.0, 100).side, 35);
        // A wide side margin is still the minimum.
        let mut wide = layout.clone();
        wide.margins.sides = 20;
        assert_eq!(wide.geometry(1696, 1272, 300.0, 42).side, 236);
    }

    #[test]
    fn old_settings_migrate_and_values_are_clamped() {
        let mut layout = PageLayout::default();
        layout.migrate(Some(false), Some(2));
        assert!(!layout.bottom.on);
        assert_eq!(layout.margins.sides, 10);
        layout.migrate(None, Some(0));
        assert_eq!(layout.margins.sides, 4);
        layout.migrate(Some(true), Some(9));
        assert!(layout.bottom.on);
        assert_eq!(layout.margins.sides, 10);
        layout.margins = Margins { sides: 0, top: 99, bottom: 5 };
        layout.normalize();
        assert_eq!(layout.margins, Margins { sides: 2, top: 20, bottom: 5 });
    }

    #[test]
    fn only_margins_and_bar_toggles_move_the_text() {
        let base = PageLayout::default();
        let mut slots = base.clone();
        slots.top.slots = [None, Some(BarItem::Battery), None];
        slots.bottom.slots[1] = Some(BarItem::PageTotal);
        slots.progress = ProgressLine::Bottom;
        assert!(!slots.moves_text(&base), "slot contents and the line keep the layout");
        let mut bar = base.clone();
        bar.top.on = true;
        assert!(bar.moves_text(&base));
        for change in [|m: &mut Margins| m.sides += 1, |m: &mut Margins| m.top -= 1, |m: &mut Margins| m.bottom += 3] {
            let mut margins = base.clone();
            change(&mut margins.margins);
            assert!(margins.moves_text(&base));
        }
        let labels: std::collections::HashSet<_> = BarItem::ALL.iter().map(|i| i.label()).collect();
        assert_eq!(labels.len(), BarItem::ALL.len(), "every item has its own name");
    }

    #[test]
    fn layout_round_trips_with_readable_names() {
        let mut layout = PageLayout::default();
        layout.top.slots = [Some(BarItem::Clock12), None, Some(BarItem::Battery)];
        layout.progress = ProgressLine::Top;
        let json = serde_json::to_string(&layout).unwrap();
        assert!(json.contains(r#""clock_12h""#) && json.contains(r#""progress":"top""#), "{json}");
        assert_eq!(serde_json::from_str::<PageLayout>(&json).unwrap(), layout);
        let partial: PageLayout = serde_json::from_str(r#"{"margins":{"top":9}}"#).unwrap();
        assert_eq!(partial.margins, Margins { sides: 3, top: 9, bottom: 2 });
        assert!(partial.chapter_marks, "marks are on unless saved off");
        let off: PageLayout = serde_json::from_str(r#"{"chapter_marks":false}"#).unwrap();
        assert!(!off.chapter_marks);
        assert!(partial.bottom.on);
    }
}
