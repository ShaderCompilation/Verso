use crate::document::{Position, TocEntry};
use crate::geom::Rect;
use crate::ui::gesture::Gesture;
use crate::ui::theme::Theme;
use crate::ui::widget::DrawCx;
use crate::ui::widgets::{List, ListEvent, Pager, Row, TopBar};

pub enum ContentsEvent {
    Close,
    Position(usize, Position),
}
pub struct Contents {
    top: TopBar,
    list: List,
    pager: Pager,
    entries: Vec<TocEntry>,
}
impl Contents {
    pub fn new(t: &Theme, screen: Rect, toc: &[TocEntry], current: usize) -> Self {
        let mut top = TopBar::new("Contents", true);
        top.place(screen, t);
        let mut pager = Pager::new(1);
        pager.place(Rect::new(t.margin, screen.bottom() - t.mm(11.0), screen.w - 2 * t.margin, t.mm(11.0)), t);
        let current = toc.iter().rposition(|e| e.page <= current);
        let rows = toc
            .iter()
            .enumerate()
            .map(|(i, e)| {
                Row::new(format!(
                    "{}{}{}",
                    "  ".repeat(e.level.saturating_sub(1).min(4)),
                    if Some(i) == current { "• " } else { "" },
                    e.title
                ))
                .value((e.page + 1).to_string())
            })
            .collect();
        let mut list = List::new(rows);
        let area = Rect::new(0, t.top_bar + t.mm(2.0), screen.w, pager.rect().y - t.top_bar - t.mm(2.0));
        list.place(area, t.row, t.margin);
        let per = (area.h / t.row).max(1) as usize;
        pager.set(current.unwrap_or(0) / per, list.paging().pages());
        list.page = pager.page;
        Self { top, list, pager, entries: toc.to_vec() }
    }
    pub fn draw(&mut self, cx: &mut DrawCx) {
        cx.canvas.fill(crate::ui::theme::PAPER);
        self.top.draw(cx);
        self.list.draw(cx);
        self.pager.draw(cx);
        if self.entries.is_empty() {
            cx.line(
                "This book has no table of contents",
                cx.theme.body,
                Rect::new(
                    cx.theme.margin,
                    cx.theme.top_bar,
                    cx.canvas.width() as i32 - 2 * cx.theme.margin,
                    cx.theme.row,
                ),
                crate::gfx::HAlign::Left,
            );
        }
    }
    pub fn on_gesture(&mut self, g: &Gesture) -> Option<ContentsEvent> {
        if self.top.on_gesture(g) {
            return Some(ContentsEvent::Close);
        }
        self.pager.on_gesture(g);
        if let Gesture::Swipe { dir, .. } = g {
            self.pager.on_swipe(*dir);
        }
        self.list.page = self.pager.page;
        if let Some(ListEvent::Tapped(i)) = self.list.on_gesture(g) {
            return Some(ContentsEvent::Position(self.entries[i].page, self.entries[i].position.clone()));
        }
        None
    }
}
