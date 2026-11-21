//! Search in book: the session (query, hits, where the search began), the
//! full-screen results list and the bar shown while visiting results.
//! Hits arrive in bounded batches from the book thread; only the reply for the
//! current request ID and layout generation is accepted.
use crate::document::{
    Position, SearchBatch, SearchHit, SearchStatus, TocEntry,
    thread::{Command, SearchRequest, Worker},
};
use crate::geom::Rect;
use crate::gfx::{HAlign, Icon, Weight};
use crate::platform::{RefreshMode, TextInput};
use crate::reader;
use crate::ui::gesture::Gesture;
use crate::ui::theme::{INK, INK2, LINE, PAPER, Theme};
use crate::ui::widget::{DrawCx, Press, PressEvent};
use crate::ui::widgets::{Button, ButtonKind, IconButton, Keyboard, Pager, TextField, TopBar};

/// Must match `SNIPPET_BEFORE` in the crengine shim: a context this long was cut.
const SNIPPET_BEFORE: usize = 80;

/// The page the reader showed when the user first opened a result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Origin {
    pub position: Position,
    pub page: usize,
    pub generation: u64,
}

#[derive(Default)]
pub(super) struct Session {
    pub query: String,
    request: Option<SearchRequest>,
    next_id: u64,
    pub hits: Vec<SearchHit>,
    /// None before the first search.
    pub status: Option<SearchStatus>,
    pub searched_pages: usize,
    pub pages: usize,
    pub error: Option<String>,
    /// Cancelled before finishing (sleep); resumed when search is used again.
    interrupted: bool,
    /// The last opened hit.
    pub current: Option<usize>,
    pub origin: Option<Origin>,
    pub list_page: usize,
}

impl Session {
    /// Starts a new query from the beginning of the book.
    pub fn start(&mut self, query: &str, generation: u64, worker: &Worker) {
        self.query = query.split_whitespace().collect::<Vec<_>>().join(" ");
        self.hits.clear();
        self.error = None;
        self.current = None;
        self.list_page = 0;
        self.searched_pages = 0;
        self.interrupted = false;
        if self.query.is_empty() {
            self.request = None;
            self.status = None;
            worker.send(Command::CancelSearch);
            return;
        }
        self.next_id += 1;
        let request = SearchRequest { generation, id: self.next_id };
        self.request = Some(request);
        self.status = Some(SearchStatus::Searching);
        worker.send(Command::Search { request, query: self.query.clone() });
    }

    /// Re-runs the query after a relayout or interruption. Results come back in
    /// the same order, so the opened hit, list page and origin stay meaningful.
    pub fn refresh(&mut self, generation: u64, worker: &Worker) {
        if self.request.is_none() || !self.interrupted && !self.stale(generation) {
            return;
        }
        self.refresh_after_layout(generation, worker);
    }

    /// A search queued during layout debounce can be dropped by the worker
    /// before it receives `Layout`. Reissue after that command is queued.
    pub fn refresh_after_layout(&mut self, generation: u64, worker: &Worker) {
        if self.request.is_none() {
            return;
        }
        let (current, list_page) = (self.current, self.list_page);
        let query = self.query.clone();
        self.start(&query, generation, worker);
        self.current = current;
        self.list_page = list_page;
    }

    pub fn stale(&self, generation: u64) -> bool {
        self.request.is_some_and(|r| r.generation != generation)
    }

    pub fn running(&self) -> bool {
        self.status == Some(SearchStatus::Searching) && !self.interrupted
    }

    pub fn cancel(&mut self, worker: &Worker) {
        if self.running() {
            worker.send(Command::CancelSearch);
            self.interrupted = true;
        }
    }

    /// True when the reply belongs to the current request.
    pub fn receive(&mut self, request: SearchRequest, result: Result<SearchBatch, String>) -> bool {
        if self.request != Some(request) || self.interrupted {
            return false;
        }
        match result {
            Ok(batch) => {
                self.hits.extend(batch.hits);
                self.searched_pages = batch.searched_pages;
                self.pages = batch.pages;
                self.status = Some(batch.status);
            }
            Err(message) => {
                log::warn!("search: {message}");
                self.error = Some(message);
                self.status = Some(SearchStatus::Finished);
            }
        }
        true
    }

    fn summary(&self) -> String {
        let n = self.hits.len();
        let matches = if n == 1 { "1 match".to_owned() } else { format!("{n} matches") };
        if let Some(e) = &self.error {
            return format!("Couldn't search this book: {e}");
        }
        match self.status {
            None => "Type a word or phrase".into(),
            Some(SearchStatus::Searching) => {
                let percent = (self.searched_pages * 100).checked_div(self.pages).unwrap_or(0);
                if n == 0 { format!("Searching… {percent}%") } else { format!("{matches} so far · {percent}%") }
            }
            Some(SearchStatus::Finished) if n == 0 => format!("No matches for “{}”", self.query),
            Some(SearchStatus::Finished) => matches,
            Some(SearchStatus::Limited) => format!("First {n} matches"),
        }
    }
}

pub(super) enum SearchEvent {
    Close,
    Submit(String),
    Open(usize),
    Return,
}

/// The full-screen query and results list.
pub(super) struct SearchView {
    top: TopBar,
    field: TextField,
    submit: Button,
    keyboard: Keyboard,
    status: Rect,
    back: Button,
    list: Rect,
    row_h: i32,
    pager: Pager,
    press: Press,
    pressed: Option<usize>,
}

impl SearchView {
    pub fn new(t: &Theme, screen: Rect, session: &Session, focus: bool) -> Self {
        let mut top = TopBar::new("Search in book", true);
        top.place(screen, t);
        let gap = t.mm(2.0);
        let content = Rect::new(screen.x + t.margin, top.rect().bottom() + gap, screen.w - 2 * t.margin, t.target);
        let submit_w = t.mm(22.0);
        let mut field = TextField::new("Word or phrase");
        field.place(Rect { w: content.w - submit_w - gap, ..content });
        field.edit(&TextInput::Insert(session.query.clone()));
        field.focused = focus;
        let mut submit = Button::new("Search", ButtonKind::Primary);
        submit.place(Rect::new(content.right() - submit_w, content.y, submit_w, t.target), t);
        let row = Rect::new(content.x, content.bottom() + gap, content.w, t.target);
        let back_w = t.mm(42.0);
        let mut back = Button::new("Return", ButtonKind::Secondary);
        back.place(Rect::new(row.right() - back_w, row.y, back_w, row.h), t);
        let status = Rect { w: row.w - back_w - gap, ..row };
        let mut pager = Pager::new(1);
        pager.place(Rect::new(content.x, screen.bottom() - t.mm(11.0), content.w, t.mm(11.0)), t);
        let row_h = t.caption.line_height().ceil() as i32 + 2 * t.body.line_height().ceil() as i32 + 2 * t.mm(2.0);
        let list = Rect::new(screen.x, row.bottom() + gap, screen.w, pager.rect().y - row.bottom() - gap);
        let mut keyboard = Keyboard::default();
        keyboard.place(screen, t);
        keyboard.visible = focus;
        let mut view = Self {
            top,
            field,
            submit,
            keyboard,
            status,
            back,
            list,
            row_h,
            pager,
            press: Press::default(),
            pressed: None,
        };
        view.sync(session);
        view
    }

    fn per_page(&self) -> usize {
        (self.list.h / self.row_h).max(1) as usize
    }

    fn sync(&mut self, session: &Session) {
        let pages = session.hits.len().div_ceil(self.per_page());
        self.pager.set(session.list_page, pages);
    }

    /// Shows the page holding the last opened hit.
    pub fn reveal(&mut self, session: &mut Session) {
        if let Some(current) = session.current {
            session.list_page = current / self.per_page();
        }
        self.sync(session);
    }

    fn rows(&self, session: &Session) -> impl Iterator<Item = (usize, Rect)> + use<> {
        let per = self.per_page();
        let (list, row_h) = (self.list, self.row_h);
        let first = session.list_page * per;
        (first..session.hits.len().min(first + per))
            .enumerate()
            .map(move |(k, i)| (i, Rect::new(list.x, list.y + k as i32 * row_h, list.w, row_h)))
    }

    pub fn on_text_input(&mut self, input: &TextInput) -> (bool, Option<SearchEvent>) {
        if !self.field.focused {
            return (false, None);
        }
        match input {
            TextInput::Submit => {
                self.field.focused = false;
                self.keyboard.visible = false;
                return (true, Some(SearchEvent::Submit(self.field.value().to_owned())));
            }
            TextInput::Dismiss => {
                self.field.focused = false;
                self.keyboard.visible = false;
            }
            _ => {
                self.field.edit(input);
            }
        }
        (true, None)
    }

    pub fn on_gesture(&mut self, g: &Gesture, session: &mut Session, t: &Theme) -> Option<SearchEvent> {
        if self.keyboard.visible {
            let input = self.keyboard.on_gesture(g, t)?;
            return self.on_text_input(&input).1;
        }
        if self.top.on_gesture(g) {
            return Some(SearchEvent::Close);
        }
        if self.field.on_gesture(g) {
            self.field.focused = true;
            self.keyboard.visible = true;
        }
        if self.submit.on_gesture(g) {
            return Some(SearchEvent::Submit(self.field.value().to_owned()));
        }
        if session.origin.is_some() && self.back.on_gesture(g) {
            return Some(SearchEvent::Return);
        }
        self.sync(session);
        self.pager.on_gesture(g);
        if let Gesture::Swipe { dir, .. } = g {
            self.pager.on_swipe(*dir);
        }
        session.list_page = self.pager.page;
        if let Gesture::Down(p) = *g {
            self.pressed = self.rows(session).find(|(_, r)| r.contains(p)).map(|(i, _)| i);
        }
        let index = self.pressed?;
        let area = self.rows(session).find(|(i, _)| *i == index).map_or_else(Rect::default, |(_, r)| r);
        let event = self.press.on_gesture(g, area);
        if !self.press.is_down() {
            self.pressed = None;
        }
        (event == Some(PressEvent::Tap)).then_some(SearchEvent::Open(index))
    }

    pub fn draw(&mut self, cx: &mut DrawCx, session: &Session, toc: &[TocEntry], generation: u64) {
        let t = cx.theme;
        cx.canvas.fill(PAPER);
        self.top.draw(cx);
        self.field.draw(cx);
        self.submit.draw(cx);
        let status = if let Some(origin) = &session.origin {
            self.back.label = if origin.generation == generation {
                format!("Return to page {}", origin.page + 1)
            } else {
                "Return".into()
            };
            self.back.draw(cx);
            self.status
        } else {
            Rect { w: self.back.rect().right() - self.status.x, ..self.status }
        };
        cx.line(&session.summary(), t.small.gray(INK2), status, HAlign::Left);
        self.sync(session);
        for (i, r) in self.rows(session) {
            let pressed = self.pressed == Some(i) && self.press.is_down();
            if pressed {
                cx.fill(r, INK);
                cx.hint(r, RefreshMode::Fast);
            }
            let hit = &session.hits[i];
            let (fg, fg2) = if pressed { (PAPER, PAPER) } else { (INK, INK2) };
            let inner = r.inset_xy(t.margin, t.mm(2.0));
            let chapter = reader::chapter(toc, hit.page).map_or("Book", |c| toc[c].title.as_str());
            let caption = t.caption.line_height().ceil() as i32;
            let context = if session.stale(generation) {
                chapter.to_owned()
            } else {
                format!("{chapter} · page {}", hit.page + 1)
            };
            cx.line(&context, t.caption.gray(fg2), Rect { h: caption, ..inner }, HAlign::Left);
            let lines = Rect { y: inner.y + caption, h: inner.h - caption, ..inner };
            draw_snippet(cx, hit, lines, fg, !pressed);
            if session.current == Some(i) && !pressed {
                cx.fill(Rect::new(r.x + t.margin - t.mm(2.5), inner.y, t.mm(0.8), inner.h), INK);
            }
            if !pressed {
                cx.hline(r.bottom() - t.hairline / 2 - 1, inner.x, inner.right(), t.hairline, LINE);
            }
        }
        self.pager.draw(cx);
        self.keyboard.draw(cx);
    }
}

#[derive(Clone, Debug, PartialEq)]
struct Piece {
    text: String,
    matched: bool,
    x: i32,
}

/// Lays out "…before MATCH after…" into at most `lines` lines of `width`,
/// keeping the match on the first line where possible. `measure(text, bold)`
/// returns a width in pixels.
fn snippet_lines(
    hit: &SearchHit,
    width: i32,
    lines: usize,
    mut measure: impl FnMut(&str, bool) -> i32,
) -> Vec<Vec<Piece>> {
    let clean = |s: &str| s.replace('\u{ad}', "").split_whitespace().collect::<Vec<_>>().join(" ");
    let spaced_end = hit.before.ends_with(char::is_whitespace);
    let spaced_start = hit.after.starts_with(char::is_whitespace);
    let mut before: Vec<String> = clean(&hit.before).split(' ').filter(|w| !w.is_empty()).map(Into::into).collect();
    let mut cut = hit.before.chars().count() >= SNIPPET_BEFORE && !before.is_empty();
    // A partial first word is dropped, but only when there are other words to keep: text
    // without spaces (Chinese, Japanese, Thai) is one token, and dropping it would lose it all.
    if cut && before.len() > 1 && !hit.before.starts_with(char::is_whitespace) {
        before.remove(0); // a partial word
    }
    let space = (measure("x x", false) - measure("xx", false)).max(1);
    let ellipsis = measure("…", false);
    // Keep the match within the first part of the first line.
    let joined = |words: &[String], measure: &mut dyn FnMut(&str, bool) -> i32| measure(&words.join(" "), false);
    while !before.is_empty() && joined(&before, &mut measure) + ellipsis > width * 2 / 5 {
        cut = true;
        if before.len() == 1 {
            // One unspaced run: shorten it from the front a character at a time.
            let mut chars = before[0].chars();
            chars.next();
            before[0] = chars.as_str().to_string();
            if before[0].is_empty() {
                before.clear();
            }
        } else {
            before.remove(0);
        }
    }
    // (text, matched, space before)
    let mut tokens: Vec<(String, bool, bool)> = Vec::new();
    if cut {
        tokens.push(("…".into(), false, false));
    }
    for (k, w) in before.iter().enumerate() {
        tokens.push((w.clone(), false, k > 0));
    }
    for (k, w) in clean(&hit.matched).split(' ').filter(|w| !w.is_empty()).enumerate() {
        tokens.push((w.into(), true, if k == 0 { spaced_end && !before.is_empty() } else { true }));
    }
    for (k, w) in clean(&hit.after).split(' ').filter(|w| !w.is_empty()).enumerate() {
        tokens.push((w.into(), false, k > 0 || spaced_start));
    }
    let mut out: Vec<Vec<Piece>> = vec![Vec::new()];
    let mut x = 0;
    for (i, (text, matched, spaced)) in tokens.iter().enumerate() {
        let w = measure(text, *matched);
        let gap = if *spaced && x > 0 { space } else { 0 };
        let last_line = out.len() == lines;
        let more = i + 1 < tokens.len();
        let limit = if last_line && more { width - ellipsis } else { width };
        if x > 0 && x + gap + w > limit {
            if last_line {
                out.last_mut().unwrap().push(Piece { text: "…".into(), matched: false, x });
                break;
            }
            out.push(Vec::new());
            x = 0;
            out.last_mut().unwrap().push(Piece { text: text.clone(), matched: *matched, x });
            x = w;
            continue;
        }
        out.last_mut().unwrap().push(Piece { text: text.clone(), matched: *matched, x: x + gap });
        x += gap + w;
    }
    out
}

fn draw_snippet(cx: &mut DrawCx, hit: &SearchHit, rect: Rect, fg: u8, invert_match: bool) {
    let t = cx.theme;
    let regular = t.body.gray(fg);
    let bold = t.body.weight(Weight::SemiBold).gray(fg);
    let line_h = t.body.line_height().ceil() as i32;
    let lines = (rect.h / line_h).max(1) as usize;
    let laid = snippet_lines(hit, rect.w, lines, |s, matched| {
        cx.text.width(s, if matched { bold } else { regular }).ceil() as i32
    });
    let pad = t.mm(0.5);
    for (n, line) in laid.iter().enumerate() {
        let y = rect.y + n as i32 * line_h;
        for (k, piece) in line.iter().enumerate() {
            let style = if piece.matched { bold } else { regular };
            let w = cx.text.width(&piece.text, style).ceil() as i32;
            let r = Rect::new(rect.x + piece.x, y, (w + pad).min(rect.right() - rect.x - piece.x).max(0), line_h);
            if piece.matched && invert_match {
                // Exactly the glyphs, so touching punctuation stays visible;
                // adjacent words of a phrase join into one block.
                let joined = k > 0 && line[k - 1].matched;
                let left =
                    if joined { line[k - 1].x + cx.text.width(&line[k - 1].text, bold).ceil() as i32 } else { piece.x };
                cx.fill(Rect::new(rect.x + left, y + pad, piece.x + w - left, line_h - 2 * pad), INK);
                cx.line(&piece.text, style.gray(PAPER), r, HAlign::Left);
            } else {
                cx.line(&piece.text, style, r, HAlign::Left);
            }
        }
    }
}

pub(super) enum BarEvent {
    Previous,
    Next,
    Results,
    Return,
    Close,
}

/// A strip over the bottom margin while visiting results: previous/next hit,
/// back to the list, return to where the search began, or stay here.
pub(super) struct SearchBar {
    rect: Rect,
    previous: IconButton,
    next: IconButton,
    counter: Rect,
    results: Button,
    back: Button,
    close: IconButton,
    owns: bool,
}

impl SearchBar {
    pub fn new(t: &Theme, screen: Rect) -> Self {
        let pad = t.mm(1.5);
        let rect = screen.split_bottom(t.target + 2 * pad).0;
        let row = rect.inset_xy(t.margin, pad);
        let gap = t.mm(1.5);
        let flexible = (row.w - 3 * t.target - 5 * gap) / 3;
        let mut x = row.x;
        let mut take = |w: i32| {
            let r = Rect::new(x, row.y, w, row.h);
            x += w + gap;
            r
        };
        let mut previous = IconButton::new(Icon::ChevronLeft);
        previous.place(take(t.target));
        let counter = take(flexible);
        let mut next = IconButton::new(Icon::ChevronRight);
        next.place(take(t.target));
        let mut results = Button::new("Results", ButtonKind::Secondary);
        results.place(take(flexible), t);
        let mut back = Button::new("Return", ButtonKind::Secondary);
        back.place(take(flexible), t);
        let mut close = IconButton::new(Icon::Close);
        close.place(take(t.target));
        Self { rect, previous, next, counter, results, back, close, owns: false }
    }

    #[cfg(test)]
    pub fn rect(&self) -> Rect {
        self.rect
    }

    /// Returns whether the bar owns this touch (it started on the bar).
    pub fn on_gesture(&mut self, g: &Gesture) -> (bool, Option<BarEvent>) {
        if let Gesture::Down(p) = *g {
            self.owns = self.rect.contains(p);
        }
        if !self.owns {
            return (false, None);
        }
        if g.ends_touch() {
            self.owns = false;
        }
        let event = if self.previous.on_gesture(g) {
            Some(BarEvent::Previous)
        } else if self.next.on_gesture(g) {
            Some(BarEvent::Next)
        } else if self.results.on_gesture(g) {
            Some(BarEvent::Results)
        } else if self.back.on_gesture(g) {
            Some(BarEvent::Return)
        } else if self.close.on_gesture(g) {
            Some(BarEvent::Close)
        } else {
            None
        };
        (true, event)
    }

    pub fn draw(&mut self, cx: &mut DrawCx, session: &Session) {
        let t = cx.theme;
        cx.fill(self.rect, PAPER);
        cx.hline(self.rect.y, self.rect.x, self.rect.right(), t.border, INK);
        let current = session.current.unwrap_or(0);
        self.previous.enabled = current > 0;
        self.next.enabled = current + 1 < session.hits.len();
        let total =
            if session.running() { format!("{}…", session.hits.len()) } else { session.hits.len().to_string() };
        cx.line(&format!("{} of {total}", current + 1), t.small, self.counter, HAlign::Center);
        self.previous.draw(cx);
        self.next.draw(cx);
        self.results.draw(cx);
        self.back.enabled = session.origin.is_some();
        self.back.draw(cx);
        self.close.draw(cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::TextRange;

    fn hit(before: &str, matched: &str, after: &str) -> SearchHit {
        SearchHit {
            range: TextRange { start: Position("s".into()), end: Position("e".into()) },
            page: 3,
            before: before.into(),
            matched: matched.into(),
            after: after.into(),
        }
    }

    fn text(lines: &[Vec<Piece>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                let mut s = String::new();
                for p in line {
                    while s.chars().count() < p.x as usize {
                        s.push(' ');
                    }
                    if p.matched {
                        s.push('[');
                        s.push_str(&p.text);
                        s.push(']');
                    } else {
                        s.push_str(&p.text);
                    }
                }
                s
            })
            .collect()
    }

    /// One pixel per character; brackets mark the match in `text`.
    fn mono(s: &str, matched: bool) -> i32 {
        s.chars().count() as i32 + if matched { 2 } else { 0 }
    }

    #[test]
    fn snippets_keep_the_match_early_and_respect_punctuation_and_width() {
        let laid = snippet_lines(&hit("“Paragraph 12. ", "Ada", ", watched the tide while ships came"), 40, 2, mono);
        assert_eq!(text(&laid), ["“Paragraph 12. [Ada], watched the tide", "while ships came"]);
        // Long context before the match is shortened to a leading ellipsis,
        // a partial first word is dropped, and the last line ends in an ellipsis.
        let long = format!("{}tail words before ", "x".repeat(SNIPPET_BEFORE));
        let laid =
            snippet_lines(&hit(&long, "New York", " and a long list of places after it and more beyond"), 30, 2, mono);
        let lines = text(&laid);
        assert!(lines[0].starts_with('…') && lines[0].contains("[New] [York]"), "{lines:?}");
        assert!(lines[1].ends_with('…') && lines[1].chars().count() <= 30, "{lines:?}");
        assert!(laid.iter().flatten().all(|p| p.x + mono(&p.text, p.matched) <= 30));
        // Matches inside a word keep their neighbours attached; soft hyphens go.
        let laid = snippet_lines(&hit("Love", "lace", "\u{ad}ly"), 30, 2, mono);
        assert_eq!(text(&laid), ["Love[lace]ly"]);
    }

    #[test]
    fn unspaced_text_keeps_its_before_context() {
        let before = "字".repeat(SNIPPET_BEFORE);
        let laid = snippet_lines(&hit(&before, "匹配", "后文"), 40, 2, mono);
        let lines = text(&laid);
        assert!(lines[0].starts_with('…') && lines[0].contains("字字字[匹配]"), "{lines:?}");
    }

    #[test]
    fn session_accepts_only_current_replies_and_refreshes_stale_layouts() {
        let (worker, commands) = Worker::recording_stub();
        let mut session = Session::default();
        session.start("  Ada   Lovelace ", 4, &worker);
        assert_eq!(session.query, "Ada Lovelace");
        let first = match commands.try_recv().unwrap() {
            Command::Search { request, query } => {
                assert_eq!(query, "Ada Lovelace");
                request
            }
            _ => panic!("expected a search"),
        };
        let batch = |n: usize, status| SearchBatch {
            hits: (0..n).map(|_| hit("", "Ada", "")).collect(),
            searched_pages: 5,
            pages: 10,
            status,
        };
        assert!(session.receive(first, Ok(batch(2, SearchStatus::Searching))));
        assert_eq!(session.summary(), "2 matches so far · 50%");
        session.current = Some(1);
        session.start("other", 4, &worker);
        assert!(!session.receive(first, Ok(batch(3, SearchStatus::Finished))), "obsolete request");
        assert!(session.hits.is_empty() && session.current.is_none());
        let second = match commands.try_recv().unwrap() {
            Command::Search { request, .. } => request,
            _ => panic!("expected a search"),
        };
        assert!(second.id > first.id);
        session.cancel(&worker);
        assert!(matches!(commands.try_recv(), Ok(Command::CancelSearch)));
        assert!(!session.receive(second, Ok(batch(1, SearchStatus::Finished))), "cancelled before sleep");
        // Using search again resumes an interrupted or relaid-out session,
        // keeping the opened hit.
        session.current = Some(0);
        session.refresh(4, &worker);
        let third = match commands.try_recv().unwrap() {
            Command::Search { request, .. } => request,
            _ => panic!("expected a resumed search"),
        };
        assert_eq!(session.current, Some(0));
        assert!(session.receive(third, Ok(batch(1, SearchStatus::Finished))));
        assert_eq!(session.summary(), "1 match");
        session.refresh(4, &worker);
        assert!(commands.try_recv().is_err(), "fresh results are kept");
        session.refresh(5, &worker);
        assert!(matches!(
            commands.try_recv(),
            Ok(Command::Search { request: SearchRequest { generation: 5, .. }, .. })
        ));
        assert!(session.stale(4) && !session.stale(5));
        session.start("   ", 5, &worker);
        assert!(matches!(commands.try_recv(), Ok(Command::CancelSearch)));
        assert_eq!(session.summary(), "Type a word or phrase");
    }
}
