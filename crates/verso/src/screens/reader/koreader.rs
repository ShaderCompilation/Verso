//! The KOReader check on open (docs/koreader-progress.md §4). When KOReader
//! has a newer position for the book, a card over the first page offers to
//! go there. The I/O thread reads the sidecar, the book thread finds the page
//! without moving the reader, and anything other than *Go there* counts as
//! *Stay here*.
use crate::clock::LocalDateTime;
use crate::document::Position;
use crate::document::thread::LocateRequest;
use crate::geom::Rect;
use crate::gfx::{HAlign, Weight};
use crate::koreader::{OnOpen, Progress, Seen};
use crate::store::BookState;
use crate::ui::gesture::Gesture;
use crate::ui::theme::{INK, INK2, PAPER, SHADOW, Theme};
use crate::ui::widget::DrawCx;
use crate::ui::widgets::{Button, ButtonKind};

/// When the saved position came from KOReader and isn't in the book.
pub(super) const LOST: &str = "Couldn't find your KOReader position. Opened at the start.";

#[derive(Default)]
pub(super) struct Check {
    /// What this open loaded: Verso's position and the KOReader position
    /// last dealt with.
    saved: Position,
    seen: Option<Position>,
    stage: Stage,
    /// The first page shown for this open, as (generation, number).
    opened: Option<(u64, usize)>,
    next_id: u64,
}
#[derive(Default)]
enum Stage {
    /// Nothing to do: the setting is off, nothing was found, or it's dealt with.
    #[default]
    Idle,
    /// The I/O thread is checking the sidecar.
    Reading,
    /// A new position, to locate once the book is open.
    Found(Progress),
    /// The book thread is finding the position's page.
    Locating(LocateRequest, Progress),
    Card(Box<Card>),
}
/// What the book thread's answer means (`Check::located`).
#[derive(Debug, PartialEq)]
pub(super) enum Located {
    /// A stale reply, or the reader moved on: nothing shown or recorded, so
    /// the next open asks again.
    Ignore,
    /// The same page, or a position that isn't in the book.
    Record(Seen),
    /// On another page: show the card.
    Offer { progress: Progress, page: usize, page_progress: u32, here: (u64, usize) },
}
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Answer {
    Stay,
    Go,
}
impl Check {
    /// `checking`: the setting is on and the check was sent to the I/O thread.
    pub fn new(saved: &BookState, checking: bool) -> Self {
        Self {
            saved: saved.position.clone(),
            seen: saved.koreader_seen.as_ref().map(|s| s.position.clone()),
            stage: if checking { Stage::Reading } else { Stage::Idle },
            ..Self::default()
        }
    }
    /// Waiting for the I/O or the book thread.
    pub fn busy(&self) -> bool {
        matches!(self.stage, Stage::Reading | Stage::Found(_) | Stage::Locating(..))
    }
    /// The saved position is the KOReader position Verso last dealt with
    /// (imported, or taken from the card).
    pub fn saved_from_koreader(&self) -> bool {
        !self.saved.0.is_empty() && self.seen.as_ref() == Some(&self.saved)
    }
    /// The I/O thread's reply. Returns a position to record at once: one
    /// exactly where Verso opened needs no page lookup.
    pub fn checked(&mut self, result: Result<Option<(OnOpen, Progress)>, String>) -> Option<Seen> {
        if !matches!(self.stage, Stage::Reading) {
            return None;
        }
        self.stage = Stage::Idle;
        match result {
            Err(e) => log::warn!("KOReader progress: {e}"),
            Ok(Some((OnOpen::Offer, progress))) => self.stage = Stage::Found(progress),
            Ok(Some((OnOpen::Record, progress))) => return Some(progress.seen()),
            Ok(Some((OnOpen::Nothing, _)) | None) => {}
        }
        None
    }
    /// The first page shown for this open.
    pub fn shown(&mut self, generation: u64, page: usize) {
        self.opened.get_or_insert((generation, page));
    }
    /// The book-thread query for a found position, once.
    pub fn locate(&mut self, generation: u64) -> Option<(LocateRequest, Position)> {
        if !matches!(self.stage, Stage::Found(_)) {
            return None;
        }
        let Stage::Found(progress) = std::mem::take(&mut self.stage) else { unreachable!() };
        self.next_id += 1;
        let request = LocateRequest { generation, id: self.next_id };
        let position = progress.position.clone();
        self.stage = Stage::Locating(request, progress);
        Some((request, position))
    }
    /// The layout changed or the open failed before the card: a pending
    /// lookup is dropped (the book thread drops it too) and nothing is
    /// recorded.
    pub fn abandon(&mut self) {
        if matches!(self.stage, Stage::Found(_) | Stage::Locating(..)) {
            self.stage = Stage::Idle;
        }
    }
    /// The book thread's answer. `here` is the page on screen with no turn
    /// under way, and `generation` the current layout's.
    pub fn located(
        &mut self,
        request: LocateRequest,
        found: Option<(usize, u32)>,
        here: Option<(u64, usize)>,
        generation: u64,
    ) -> Located {
        let Stage::Locating(pending, _) = &self.stage else { return Located::Ignore };
        if *pending != request {
            return Located::Ignore;
        }
        let Stage::Locating(_, progress) = std::mem::take(&mut self.stage) else { unreachable!() };
        // Still on the page the book opened at, in its layout.
        let Some(here) = here.filter(|&h| request.generation == generation && Some(h) == self.opened) else {
            return Located::Ignore;
        };
        match found {
            Some((page, page_progress)) if page != here.1 => Located::Offer { progress, page, page_progress, here },
            _ => Located::Record(progress.seen()),
        }
    }
    pub fn show(&mut self, card: Card) {
        self.stage = Stage::Card(Box::new(card));
    }
    pub fn card(&self) -> Option<&Card> {
        if let Stage::Card(card) = &self.stage { Some(card) } else { None }
    }
    pub fn card_mut(&mut self) -> Option<&mut Card> {
        if let Stage::Card(card) = &mut self.stage { Some(card) } else { None }
    }
    /// Closes the card, answered either way.
    pub fn take_card(&mut self) -> Option<Box<Card>> {
        match std::mem::take(&mut self.stage) {
            Stage::Card(card) => Some(card),
            other => {
                self.stage = other;
                None
            }
        }
    }
}

/// The card above the bottom bar: KOReader's page and percentage in Verso's
/// layout, how long ago KOReader saved it, and the two answers.
pub(super) struct Card {
    pub progress: Progress,
    /// KOReader's page in layout `here.0`.
    pub page: usize,
    /// The page the card is shown over, as (generation, number).
    pub here: (u64, usize),
    pub title: String,
    pub detail: String,
    stay: Button,
    go: Button,
    rect: Rect,
    /// The current touch started on the card.
    touch: bool,
}
impl Card {
    /// `percent` is Verso's for KOReader's page; `now` the local time.
    /// The card is centred in `area`, at its bottom.
    pub fn new(
        progress: Progress,
        page: usize,
        percent: u32,
        here: (u64, usize),
        now: Option<LocalDateTime>,
        t: &Theme,
        area: Rect,
    ) -> Self {
        let title = format!("KOReader: p. {} · {percent}%", page + 1);
        let side = if page > here.1 { "ahead of this page" } else { "behind this page" };
        let age = now.zip(LocalDateTime::at(progress.modified)).map(|(now, at)| at.ago(now));
        let detail = match age {
            Some(age) => format!("{age} · {side}"),
            None => format!("{}{}", side[..1].to_uppercase(), &side[1..]),
        };
        let w = (area.w - 2 * t.margin).min(t.mm(110.0));
        let pad = t.mm(4.0);
        // The title's line has room above its text: a smaller top padding.
        let h = t.mm(3.0) + t.mm(7.0) + t.mm(6.0) + t.mm(3.0) + t.button_h + pad;
        let rect = Rect::new(area.center().x - w / 2, area.bottom() - h, w, h);
        let button_w = ((w - 2 * pad - t.mm(3.0)) / 2).min(t.mm(36.0));
        let y = rect.bottom() - pad - t.button_h;
        let mut go = Button::new("Go there", ButtonKind::Primary);
        go.place(Rect::new(rect.right() - pad - button_w, y, button_w, t.button_h), t);
        let mut stay = Button::new("Stay here", ButtonKind::Secondary);
        stay.place(Rect::new(go.rect().x - t.mm(3.0) - button_w, y, button_w, t.button_h), t);
        Self { progress, page, here, title, detail, stay, go, rect, touch: false }
    }
    #[cfg(test)]
    pub fn rect(&self) -> Rect {
        self.rect
    }
    #[cfg(test)]
    pub fn buttons(&self) -> (Rect, Rect) {
        (self.stay.rect(), self.go.rect())
    }
    /// A touch that starts on the card belongs to it (true), and a tap on a
    /// button answers it. Touches elsewhere are the reader's.
    pub fn on_gesture(&mut self, g: &Gesture) -> (bool, Option<Answer>) {
        if let Gesture::Down(p) = *g {
            self.touch = self.rect.contains(p);
        }
        if !self.touch {
            return (false, None);
        }
        if g.ends_touch() {
            self.touch = false;
        }
        let answer = if self.go.on_gesture(g) {
            Some(Answer::Go)
        } else if self.stay.on_gesture(g) {
            Some(Answer::Stay)
        } else {
            None
        };
        (true, answer)
    }
    pub fn draw(&self, cx: &mut DrawCx) {
        let t = cx.theme;
        let (r, radius) = (self.rect, t.mm(2.2) as f32);
        cx.canvas.fill_rounded_rect(r.moved(t.mm(1.0), t.mm(1.0)), radius, SHADOW);
        cx.canvas.fill_rounded_rect(r, radius, PAPER);
        cx.canvas.stroke_rounded_rect(r, radius, t.border as f32, INK);
        let inner = Rect { y: r.y + t.mm(3.0), ..r.inset(t.mm(4.0)) };
        cx.line(
            &self.title,
            t.body.weight(Weight::SemiBold),
            Rect::new(inner.x, inner.y, inner.w, t.mm(7.0)),
            HAlign::Left,
        );
        cx.line(
            &self.detail,
            t.small.gray(INK2),
            Rect::new(inner.x, inner.y + t.mm(7.0), inner.w, t.mm(6.0)),
            HAlign::Left,
        );
        self.stay.draw(cx);
        self.go.draw(cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::koreader::Status;

    fn progress(position: &str) -> Progress {
        Progress { position: Position(position.into()), percent: 0.6, status: Status::Reading, modified: 100 }
    }

    #[test]
    fn a_found_position_is_located_once_and_answered_by_page() {
        let mut check = Check::new(&BookState::default(), true);
        assert!(check.busy());
        assert_eq!(check.checked(Ok(Some((OnOpen::Offer, progress("/k"))))), None);
        assert!(check.busy(), "waiting for the book to open");
        check.shown(1, 0);
        check.shown(1, 1);
        let (request, position) = check.locate(1).unwrap();
        assert_eq!(position.0, "/k");
        assert!(check.locate(1).is_none(), "asked once");
        // Another request's reply is ignored; this one's offers the card.
        let other = LocateRequest { id: request.id + 1, ..request };
        assert_eq!(check.located(other, Some((9, 6000)), Some((1, 0)), 1), Located::Ignore);
        assert!(matches!(
            check.located(request, Some((9, 6000)), Some((1, 0)), 1),
            Located::Offer { page: 9, page_progress: 6000, here: (1, 0), .. }
        ));
        assert!(!check.busy());
        // The same page, or not in the book: recorded. Moved, or relaid out: ignored.
        for (found, here, generation, expected) in [
            (Some((0, 0)), Some((1, 0)), 1, Located::Record(progress("/k").seen())),
            (None, Some((1, 0)), 1, Located::Record(progress("/k").seen())),
            (Some((9, 0)), Some((1, 1)), 1, Located::Ignore),
            (Some((9, 0)), None, 1, Located::Ignore),
            (Some((9, 0)), Some((1, 0)), 2, Located::Ignore),
        ] {
            let mut check = Check::new(&BookState::default(), true);
            check.checked(Ok(Some((OnOpen::Offer, progress("/k")))));
            check.shown(1, 0);
            let (request, _) = check.locate(1).unwrap();
            assert_eq!(check.located(request, found, here, generation), expected);
            assert!(!check.busy());
        }
    }

    #[test]
    fn replies_that_need_no_card() {
        // Exactly where Verso opened: recorded without a lookup.
        let mut check = Check::new(&BookState::default(), true);
        assert_eq!(check.checked(Ok(Some((OnOpen::Record, progress("/v"))))), Some(progress("/v").seen()));
        assert!(!check.busy() && check.locate(1).is_none());
        // Nothing, or an unreadable sidecar.
        for result in [Ok(None), Err("bad".to_string())] {
            let mut check = Check::new(&BookState::default(), true);
            assert_eq!(check.checked(result), None);
            assert!(!check.busy());
        }
        // Off: nothing is waited for, and a stray reply changes nothing.
        let mut check = Check::new(&BookState::default(), false);
        assert!(!check.busy());
        assert_eq!(check.checked(Ok(Some((OnOpen::Record, progress("/v"))))), None);
        // A relayout before the lookup drops it.
        let mut check = Check::new(&BookState::default(), true);
        check.checked(Ok(Some((OnOpen::Offer, progress("/k")))));
        check.abandon();
        assert!(!check.busy() && check.locate(1).is_none());
    }

    #[test]
    fn the_saved_position_is_from_koreader_when_it_was_the_one_seen() {
        let mut state = BookState { position: Position("/k".into()), ..Default::default() };
        assert!(!Check::new(&state, false).saved_from_koreader());
        state.koreader_seen = Some(progress("/k").seen());
        assert!(Check::new(&state, false).saved_from_koreader());
        state.position = Position("/v".into());
        assert!(!Check::new(&state, false).saved_from_koreader());
        assert!(!Check::new(&BookState::default(), false).saved_from_koreader());
    }

    #[test]
    fn the_card_says_where_and_when() {
        let t = Theme::for_dpi(300);
        let screen = Rect::new(0, 0, 1272, 1696);
        let at = |day| LocalDateTime::at(100).map(|d| LocalDateTime { day, ..d });
        let now = at(LocalDateTime::at(100).unwrap().day + 3);
        let area = Rect { h: 1600, ..screen };
        let card = Card::new(progress("/k"), 213, 62, (1, 4), now, &t, area);
        assert_eq!(card.title, "KOReader: p. 214 · 62%");
        assert_eq!(card.detail, "3 days ago · ahead of this page");
        assert_eq!(card.rect().bottom(), 1600);
        let (stay, go) = card.buttons();
        assert!(card.rect().contains(stay.center()) && card.rect().contains(go.center()) && stay.right() < go.x);
        let card = Card::new(progress("/k"), 2, 1, (1, 4), None, &t, area);
        assert_eq!(card.detail, "Behind this page");
        // A 6" screen keeps the margins.
        let small = Rect::new(0, 0, 600, 800);
        let t = Theme::for_dpi(167);
        let card = Card::new(progress("/k"), 2, 1, (1, 4), None, &t, Rect { h: 780, ..small });
        assert!(card.rect().x >= t.margin && card.rect().right() <= small.w - t.margin);
        let (stay, _) = card.buttons();
        assert!(stay.x >= card.rect().x);
    }
}
