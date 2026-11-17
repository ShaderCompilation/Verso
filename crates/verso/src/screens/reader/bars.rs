//! Draws the reader's top and bottom bars over the rendered page.
use crate::geom::Rect;
use crate::gfx::HAlign;
use crate::page_layout::{Bar, PageGeometry, PageLayout, ProgressLine};
use crate::reader::bars::{BarData, Need, fit};
use crate::ui::theme::{INK2, LINE, PAPER};
use crate::ui::widget::DrawCx;

/// Space kept between items in a bar.
const GAP_MM: f32 = 3.0;
/// The progress line and its chapter ticks.
const LINE_MM: f32 = 0.5;
const TICK_MM: f32 = 1.2;

/// Draws every bar that is on. Bars are part of the page frame, so they
/// cost no extra screen update.
pub fn draw(cx: &mut DrawCx, layout: &PageLayout, geometry: &PageGeometry, data: &BarData) {
    let bars = [
        (&layout.top, geometry.top_bar, ProgressLine::Top),
        (&layout.bottom, geometry.bottom_bar, ProgressLine::Bottom),
    ];
    for (bar, rect, line) in bars {
        let Some(rect) = rect else { continue };
        cx.fill(rect, PAPER);
        slots(cx, bar, rect, data);
        if layout.progress == line {
            // The line sits on the bar's edge towards the text.
            progress(cx, rect, line == ProgressLine::Bottom, data);
        }
    }
}

fn slots(cx: &mut DrawCx, bar: &Bar, rect: Rect, data: &BarData) {
    let style = cx.theme.caption.gray(INK2);
    let texts = bar.slots.map(|slot| slot.and_then(|item| data.text(item)));
    let mut widths = [(0, 0); 3];
    let needs: [Option<Need>; 3] = std::array::from_fn(|i| {
        let text = texts[i].as_ref()?;
        let mut width = |s: &str| if s.is_empty() { 0 } else { cx.text.width(s, style).ceil() as i32 };
        let (head, tail) = (width(&text.head), width(&text.tail));
        widths[i] = (head, tail);
        Some(Need { min: tail, full: head + tail })
    });
    let given = fit(needs, rect.w, cx.theme.mm(GAP_MM));
    for (i, text) in texts.iter().enumerate() {
        let Some(text) = text else { continue };
        let w = given[i];
        let x = match i {
            0 => rect.x,
            1 => rect.x + (rect.w - w) / 2,
            _ => rect.right() - w,
        };
        let (head_w, tail_w) = widths[i];
        // Glyphs are clipped to the bar, not to their measured width.
        let from = |x: i32| Rect::new(x, rect.y, rect.right() - x, rect.h);
        let tail_x = if head_w + tail_w <= w {
            cx.line_measured(&text.head, style, from(x));
            x + head_w
        } else {
            cx.line(&text.head, style, Rect::new(x, rect.y, w - tail_w, rect.h), HAlign::Left);
            x + w - tail_w
        };
        cx.line_measured(&text.tail, style, from(tail_x));
    }
}

fn progress(cx: &mut DrawCx, rect: Rect, on_top_edge: bool, data: &BarData) {
    let t = cx.theme;
    let thickness = t.mm(LINE_MM).max(2);
    let tick = t.mm(TICK_MM);
    let y = if on_top_edge { rect.y } else { rect.bottom() - thickness };
    cx.fill(Rect::new(rect.x, y, rect.w, thickness), LINE);
    let filled = (rect.w as f32 * data.fraction()).round() as i32;
    cx.fill(Rect::new(rect.x, y, filled, thickness), INK2);
    let tick_y = if on_top_edge { rect.y } else { rect.bottom() - tick };
    for start in data.chapter_starts() {
        let x = rect.x + (rect.w as f32 * start).round() as i32;
        cx.fill(Rect::new(x - t.hairline / 2, tick_y, t.hairline, tick), INK2);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{Position, TocEntry};
    use crate::gfx::{Canvas, IconRenderer, TextRenderer};
    use crate::page_layout::BarItem;
    use crate::ui::theme::Theme;

    /// Renders the bars on a blank 600 × 800 page at 150 dpi.
    fn render(layout: &PageLayout, title: &str) -> (Vec<u8>, PageGeometry) {
        let t = Theme::for_dpi(150);
        let mut canvas = Canvas::new(600, 800);
        canvas.fill(PAPER);
        let toc: Vec<_> = [(title, 2), ("Two", 10)]
            .into_iter()
            .map(|(title, page)| TocEntry { title: title.into(), level: 1, page, position: Position::default() })
            .collect();
        let data = BarData { toc: &toc, title: "Dune", author: "Frank Herbert", page: 4, pages: 20, progress: 2500 };
        let geometry = layout.geometry(600, 800, t.dpi, 21);
        let (mut text, mut icons) = (TextRenderer::new(), IconRenderer::default());
        draw(&mut DrawCx::new(&mut canvas, &mut text, &mut icons, &t, &mut vec![]), layout, &geometry, &data);
        (canvas.gray_data().to_vec(), geometry)
    }
    fn ink(pixels: &[u8], r: Rect) -> usize {
        (r.y..r.bottom())
            .flat_map(|y| (r.x..r.right()).map(move |x| pixels[y as usize * 600 + x as usize]))
            .filter(|&p| p < PAPER)
            .count()
    }

    #[test]
    fn bars_draw_only_inside_their_rects() {
        let mut layout = PageLayout::default();
        layout.top.on = true;
        layout.top.slots = [Some(BarItem::BookTitle), Some(BarItem::PageTotal), Some(BarItem::Author)];
        layout.progress = ProgressLine::Bottom;
        let (pixels, g) = render(&layout, "One");
        let (top, bottom) = (g.top_bar.unwrap(), g.bottom_bar.unwrap());
        let all = ink(&pixels, Rect::new(0, 0, 600, 800));
        assert!(ink(&pixels, top) > 0 && ink(&pixels, bottom) > 0);
        assert_eq!(all, ink(&pixels, top) + ink(&pixels, bottom), "nothing outside the bars");
        // The progress line runs along the bottom bar's top edge, a quarter filled.
        let line = |x| pixels[bottom.y as usize * 600 + x as usize];
        assert_eq!(line(bottom.x + bottom.w / 8), INK2);
        assert_eq!(line(bottom.x + bottom.w * 3 / 4), LINE);
        // A tick at the second chapter (page 10 of 20) reaches into the bar.
        let tick_y = (bottom.y + Theme::for_dpi(150).mm(TICK_MM) - 1) as usize;
        assert_eq!(pixels[tick_y * 600 + (bottom.x + bottom.w / 2) as usize], INK2);
    }

    #[test]
    fn a_long_chapter_title_keeps_its_pages_left_and_the_percent() {
        let short = render(&PageLayout::default(), "One").0;
        let long = render(&PageLayout::default(), &"A very long chapter title ".repeat(8)).0;
        let g = PageLayout::default().geometry(600, 800, 150.0, 21);
        let bar = g.bottom_bar.unwrap();
        // The percent is untouched; the title is cut short rather than overflowing it.
        let percent = TextRenderer::new().width("25%", Theme::for_dpi(150).caption).ceil() as i32;
        let right = Rect::new(bar.right() - percent, bar.y, percent, bar.h);
        let column = |p: &[u8]| {
            (right.y..right.bottom())
                .flat_map(|y| (right.x..right.right()).map(move |x| p[(y * 600 + x) as usize]))
                .collect::<Vec<_>>()
        };
        assert_eq!(column(&short), column(&long));
        // The pages left stay whole, just before the percent.
        let gap = Theme::for_dpi(150).mm(GAP_MM);
        let before = Rect::new(bar.right() - percent - gap - 10, bar.y, 10, bar.h);
        assert!(ink(&long, before) > 0, "the tail ends one gap before the percent");
        // The middle of the bar is used by the long title only.
        let middle = Rect::new(bar.x + bar.w / 2, bar.y, bar.w / 8, bar.h);
        assert_eq!(ink(&short, middle), 0);
        assert!(ink(&long, middle) > 0);
        let off = PageLayout { bottom: Bar::default(), ..PageLayout::default() };
        assert!(render(&off, "One").0.iter().all(|&p| p == PAPER), "no bars, no ink");
    }
}
