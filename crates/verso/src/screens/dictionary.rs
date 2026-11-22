//! Lookup is an overlay over the unchanged cached book page.
use crate::{
    dictionary::{
        Block, BlockKind, Dictionary, Source, Style,
        sets::Set,
        worker::{Match, State, Worker},
    },
    geom::Rect,
    gfx::{
        HAlign, Icon, SpanStyle, StyledBlock, StyledPagination, StyledParagraph, StyledSpan, TextRenderer, TextStyle,
        Weight,
    },
    platform::{RefreshMode, TextInput},
    screens::EventCx,
    ui::{
        gesture::Gesture,
        theme::{INK, INK2, LINE, PAPER, Theme},
        widget::{DrawCx, Press, PressEvent},
        widgets::{Button, ButtonKind, IconButton, Keyboard, List, ListEvent, Pager, Row, Sheet, TextField},
    },
};
use std::rc::Rc;

const MISSING: &str = "No dictionaries found. Add a StarDict dictionary to Verso's dictionaries/ folder, \
                       then open Dictionaries and tap Rescan.";
const DISABLED: &str = "All dictionaries are disabled. Open Dictionaries to enable one.";
const NO_RESULT: &str = "No definition found. Edit the word above, or switch dictionary sets.";
const FIRST_DRAW_WAIT: std::time::Duration = std::time::Duration::from_millis(12);
/// Nesting levels that indent; deeper blocks stay at the last level.
const MAX_DEPTH: u8 = 4;

/// The renderer's flags for a dictionary span style.
fn span_style(style: Style) -> SpanStyle {
    [
        (Style::BOLD, SpanStyle::BOLD),
        (Style::ITALIC, SpanStyle::ITALIC),
        (Style::MUTED, SpanStyle::MUTED),
        (Style::PHONETIC, SpanStyle::PHONETIC),
        (Style::LINK, SpanStyle::UNDERLINE),
    ]
    .into_iter()
    .filter(|(from, _)| style.contains(*from))
    .fold(SpanStyle::default(), |all, (_, to)| all.union(to))
}

/// Unstyled text. Status and provenance lines are `inert`: touches never
/// look up their words.
fn plain(text: String, base: TextStyle, inert: bool) -> StyledBlock {
    StyledBlock::Paragraph(StyledParagraph { text, spans: Vec::new(), base, muted: INK2, indent: 0.0, inert })
}

/// A text or heading block with its styles and indentation.
fn styled(block: &Block, t: &Theme) -> StyledBlock {
    let spans = block
        .spans
        .iter()
        .map(|span| StyledSpan { bytes: span.start as usize..span.end as usize, style: span_style(span.style) })
        .collect();
    let base =
        if block.kind == BlockKind::Heading { t.dictionary_body.weight(Weight::SemiBold) } else { t.dictionary_body };
    StyledBlock::Paragraph(StyledParagraph {
        text: block.text.clone(),
        spans,
        base,
        muted: INK2,
        indent: (i32::from(block.depth.min(MAX_DEPTH)) * t.dictionary_indent) as f32,
        inert: false,
    })
}

/// Definition blocks for the text renderer: gaps, rules and styled text.
pub(crate) fn styled_blocks<'a>(blocks: &'a [Block], t: &'a Theme) -> impl Iterator<Item = StyledBlock> + 'a {
    let gap = t.dictionary_body.line_height() / 2.0;
    blocks.iter().map(move |block| match block.kind {
        BlockKind::Gap => StyledBlock::Gap(gap),
        BlockKind::Rule => StyledBlock::Rule { space: gap, thickness: t.hairline, gray: LINE },
        BlockKind::Text | BlockKind::Heading => styled(block, t),
    })
}

/// All visible content shares one pagination, including provenance and errors.
fn content(state: &State, pending: bool, warnings: &[String], t: &Theme) -> Vec<StyledBlock> {
    if pending {
        return vec![plain("Looking up…".into(), t.body, true)];
    }
    let gap = t.dictionary_body.line_height() / 2.0;
    let rule = || StyledBlock::Rule { space: gap, thickness: t.hairline, gray: LINE };
    let mut blocks = Vec::new();
    match state {
        State::Definition { entries, base_forms } => {
            if let Some(forms) = base_forms {
                let plural = if forms.count > 1 { "s" } else { "" };
                blocks.push(plain(format!("Base form{plural} of “{}”", forms.query), t.small.gray(INK2), true));
                blocks.push(StyledBlock::Gap(gap));
            }
            for (i, entry) in entries.iter().enumerate() {
                if i > 0 {
                    blocks.push(rule());
                }
                blocks.push(plain(entry.headword.clone(), t.dictionary_headword, false));
                blocks.extend(styled_blocks(&entry.blocks, t));
            }
        }
        State::Missing => blocks.push(plain(MISSING.into(), t.body, true)),
        State::Disabled => blocks.push(plain(DISABLED.into(), t.body, true)),
        State::NoResult => blocks.push(plain(NO_RESULT.into(), t.body, true)),
        State::Error(_) => blocks.push(plain(
            "Couldn’t read this entry. Open Manage dictionaries for details, or choose another result.".into(),
            t.body,
            true,
        )),
    }
    if !warnings.is_empty() && !matches!(state, State::Definition { .. }) {
        blocks.push(StyledBlock::Gap(gap));
        blocks.push(plain(
            "Search incomplete: some dictionaries couldn’t be read. Open Manage dictionaries for details.".into(),
            t.body,
            true,
        ));
    }
    blocks
}

pub enum LookupEvent {
    Close,
    /// The action strip's book-selection actions.
    Highlight,
    Note,
    /// Search the book for the submitted query.
    Search(String),
    Extend,
    /// Open dictionary management at this lookup's set, or at the list of
    /// sets while the worker hasn't yet said which set the lookup resolves to.
    Manage(Option<String>),
    Set(String),
}

struct Picker {
    sets: bool,
    list: List,
    pager: Pager,
    set: Button,
    back: Button,
    manage: Button,
    status: Rect,
}

pub struct Lookup {
    screen: Rect,
    /// Where the looked-up text is on the book page: the card stands away
    /// from it.
    word: Option<Rect>,
    /// Portrait only: the compact card was expanded to 80% height.
    expanded: bool,
    /// The card already grew once by itself for content that needed it.
    grew: bool,
    sheet: Sheet,
    worker: Rc<Worker>,
    request: u64,
    selection: crate::dictionary::sets::Selection,
    active_set: String,
    /// Whether `active_set` came from a worker reply rather than the placeholder.
    set_known: bool,
    /// The last submitted query, shown as the card's title. The field may
    /// have been edited since.
    query: String,
    pending: bool,
    complete: bool,
    preparing: Option<String>,
    matches: Vec<Match>,
    sets: Vec<Set>,
    revision: u64,
    picker: Option<Picker>,
    /// The tappable query in the header row, which opens query editing.
    heading: Rect,
    heading_press: Press,
    expand: IconButton,
    close: IconButton,
    previous: Button,
    /// The dictionary switcher between ‹ and ›, which opens the picker.
    switcher: Rect,
    switcher_press: Press,
    next: Button,
    /// The divider under the header rows, and the one above the action
    /// strip while it shows.
    divider: i32,
    actions_divider: Option<i32>,
    field: TextField,
    keyboard: Keyboard,
    search: Button,
    manage: Button,
    /// Highlight, Note, Search, Extend.
    actions: [Button; 4],
    returning: bool,
    catalog: Vec<Dictionary>,
    sources: Vec<Source>,
    /// Search diagnostics, shown as an incomplete notice only on a miss.
    warnings: Vec<String>,
    selected: Option<String>,
    state: State,
    /// A reply consumed during drawing still needs its save failure shown by
    /// the next event pass, which can create an app toast.
    pending_save_error: Option<String>,
    /// No frame has been drawn for the current request yet; only that draw
    /// waits briefly for its reply.
    first_frame: bool,
    pages: Option<StyledPagination>,
    /// When the current pagination started and the layout time spent on it,
    /// for the completion debug log.
    layout_timing: (std::time::Instant, std::time::Duration),
    layout_error: Option<String>,
    pager: Pager,
    /// The pager shows: the content needs more than one page.
    paged: bool,
    /// The definition area without the pager row, and with it.
    body_full: Rect,
    body_paged: Rect,
    body: Rect,
}
impl Lookup {
    /// `word` is where the looked-up text is on the page. The compact card
    /// opens on the half of the screen away from it (the bottom half when
    /// unknown), so the word, its sentence and the selection handles stay
    /// visible; in landscape it stands at the side away from the word.
    pub fn new(
        query: &str,
        worker: Rc<Worker>,
        t: &Theme,
        screen: Rect,
        word: Option<Rect>,
        selection: crate::dictionary::sets::Selection,
    ) -> Self {
        let mut field = TextField::new("Word");
        field.edit(&TextInput::Insert(query.into()));
        let request = worker.lookup_in(query.into(), false, selection.clone());
        let mut keyboard = Keyboard::default();
        keyboard.set_submit("Look up");
        keyboard.place(screen, t);
        let revision = worker.revision();
        let action = |label| Button::new(label, ButtonKind::Secondary);
        let mut lookup = Self {
            screen,
            word,
            expanded: false,
            grew: false,
            sheet: Sheet::panel(Rect::default(), screen),
            worker,
            request,
            selection,
            active_set: crate::dictionary::sets::DEFAULT.into(),
            set_known: false,
            query: query.into(),
            pending: true,
            complete: false,
            preparing: None,
            matches: vec![],
            sets: vec![],
            revision,
            picker: None,
            heading: Rect::default(),
            heading_press: Press::default(),
            expand: IconButton::new(Icon::Expand),
            close: IconButton::new(Icon::Close),
            previous: Button::new("‹", ButtonKind::Secondary),
            switcher: Rect::default(),
            switcher_press: Press::default(),
            next: Button::new("›", ButtonKind::Secondary),
            divider: 0,
            actions_divider: None,
            field,
            keyboard,
            search: Button::new("Look up", ButtonKind::Primary),
            manage: Button::new("Manage dictionaries", ButtonKind::Secondary),
            actions: [action("Highlight"), action("Note"), action("Search"), action("Extend")],
            returning: false,
            catalog: Vec::new(),
            sources: Vec::new(),
            warnings: Vec::new(),
            selected: None,
            state: State::NoResult,
            pending_save_error: None,
            first_frame: true,
            pages: None,
            layout_timing: (std::time::Instant::now(), std::time::Duration::ZERO),
            layout_error: None,
            pager: Pager::new(1),
            paged: false,
            body_full: Rect::default(),
            body_paged: Rect::default(),
            body: Rect::default(),
        };
        lookup.place(t);
        lookup
    }
    /// The card's rectangle on screen. Touches outside it belong to the book
    /// page (a selection handle) or close the card.
    pub fn rect(&self, t: &Theme) -> Rect {
        self.frame(t)
    }
    /// Whether the book selection's handles can be dragged beside the card:
    /// only on the plain card, not while picking a dictionary or typing.
    pub fn allows_handles(&self) -> bool {
        self.picker.is_none() && !self.keyboard.visible
    }
    /// The middle of an action strip button (Highlight, Note, Search, Extend).
    #[cfg(test)]
    pub(crate) fn action_point(&self, index: usize) -> crate::geom::Point {
        self.actions[index].rect().center()
    }
    fn landscape(&self) -> bool {
        self.screen.w > self.screen.h
    }
    /// Portrait: compact (~46%) at the bottom or top, away from the word, or
    /// 80% at the bottom when expanded and while the picker or the keyboard
    /// needs the room. Landscape: a full-height side panel.
    fn frame(&self, t: &Theme) -> Rect {
        let s = self.screen;
        if self.landscape() {
            let right = self.word.is_none_or(|w| w.center().x < s.center().x);
            let width = (s.w * 52 / 100).max(t.mm(72.0)).min(s.w);
            return if right { s.split_right(width).0 } else { s.split_left(width).0 };
        }
        if self.expanded || self.picker.is_some() || self.keyboard.visible {
            return s.split_bottom(s.h * 4 / 5).0;
        }
        let height = (s.h * 46 / 100).max(t.mm(64.0)).min(s.h * 52 / 100);
        if self.word.is_none_or(|w| w.center().y < s.center().y) {
            s.split_bottom(height).0
        } else {
            s.split_top(height).0
        }
    }
    fn definition_shown(&self) -> bool {
        !self.pending && matches!(self.state, State::Definition { .. })
    }
    /// The query field and Look up: while typing, and on a miss so editing
    /// the query is one tap away.
    fn edit_row(&self) -> bool {
        self.keyboard.visible || !self.pending && self.state == State::NoResult
    }
    fn actions_shown(&self) -> bool {
        self.picker.is_none() && !self.keyboard.visible
    }
    /// Places everything for the current mode. Cheap, and called before
    /// each gesture and draw, since replies, the keyboard and the picker
    /// change which rows show.
    fn place(&mut self, t: &Theme) {
        let frame = self.frame(t);
        if self.sheet.rect() != frame {
            self.sheet = Sheet::panel(frame, self.screen);
        }
        let landscape = self.landscape();
        let typing = self.keyboard.visible;
        let c = frame.inset_xy(t.margin, 0);
        let gap = t.mm(1.4);
        // Header: query ✎ … ⤢ ×
        let header = Rect::new(c.x, frame.y + t.mm(1.7), c.w, t.mm(8.6));
        let (side, cy) = (t.target, header.center().y);
        self.close.place(Rect::new(c.right() - side + t.mm(2.0), cy - side / 2, side, side));
        let expandable = !landscape && self.picker.is_none() && !typing;
        self.expand.icon = if self.expanded { Icon::Collapse } else { Icon::Expand };
        self.expand.place(if expandable {
            Rect::new(self.close.rect().x - side, cy - side / 2, side, side)
        } else {
            Rect::default()
        });
        let end = if expandable { self.expand.rect().x } else { self.close.rect().x };
        self.heading = Rect::new(c.x, header.y, (end - gap - c.x).max(0), header.h);
        let mut y = header.bottom();
        // Switcher: ‹ · name · i / N ▾ · ›
        if !typing && self.picker.is_none() {
            let row = Rect::new(c.x, y + t.mm(0.4), c.w, t.mm(7.8));
            let arrow = t.mm(if landscape { 7.1 } else { 8.1 });
            self.previous.place(Rect::new(row.x, row.y, arrow, row.h), t);
            self.next.place(Rect::new(row.right() - arrow, row.y, arrow, row.h), t);
            self.switcher = Rect::new(row.x + arrow + gap, row.y, row.w - 2 * (arrow + gap), row.h);
            y = row.bottom();
        }
        if self.edit_row() {
            let row = Rect::new(c.x, y + gap, c.w, t.target);
            let search_w = t.mm(18.6).min(row.w / 3);
            self.field.place(Rect::new(row.x, row.y, row.w - search_w - gap, row.h));
            self.search.place(Rect::new(row.right() - search_w, row.y, search_w, row.h), t);
            y = row.bottom();
        }
        self.divider = y + t.mm(1.7);
        let top = self.divider + t.hairline + t.mm(1.5);
        // From the bottom edge up: actions, then Manage or the pager.
        let mut bottom = frame.bottom() - t.mm(2.4);
        if typing {
            bottom = bottom.min(self.keyboard.rect().y - t.mm(2.0));
        }
        self.actions_divider = None;
        if self.actions_shown() {
            // One row, or two in a side panel too narrow for "Highlight".
            let gap = t.mm(if landscape { 1.2 } else { 1.7 });
            let cols = if (c.w - 3 * gap) / 4 < t.mm(17.0) { 2 } else { 4 };
            let (h, rows) = (t.mm(8.8), 4 / cols);
            let strip = Rect::new(c.x, bottom - rows * h - (rows - 1) * gap, c.w, rows * h + (rows - 1) * gap);
            let w = (strip.w - (cols - 1) * gap) / cols;
            for (i, b) in self.actions.iter_mut().enumerate() {
                let (col, row) = (i as i32 % cols, i as i32 / cols);
                b.place(Rect::new(strip.x + col * (w + gap), strip.y + row * (h + gap), w, h), t);
            }
            let line = strip.y - t.mm(1.4) - t.hairline;
            self.actions_divider = Some(line);
            bottom = line - t.mm(1.4);
        }
        self.manage.place(Rect::new(c.x, bottom - t.target, c.w, t.target), t);
        let (lowest, pager_bottom) = if self.definition_shown() {
            (bottom, bottom)
        } else {
            (self.manage.rect().y - t.mm(2.0), self.manage.rect().y - t.mm(1.0))
        };
        let pager = Rect::new(c.x, pager_bottom - t.mm(7.0), c.w, t.mm(7.0));
        self.pager.place(pager, t);
        self.body_full = Rect::new(c.x, top, c.w, (lowest - top).max(1));
        self.body_paged = Rect::new(c.x, top, c.w, (pager.y - t.mm(1.0) - top).max(1));
        self.body = if self.paged { self.body_paged } else { self.body_full };
        if let Some(p) = &mut self.picker {
            let area = Rect::new(c.x, header.bottom() + gap, c.w, frame.bottom() - t.mm(2.4) - header.bottom() - gap);
            let gap = t.mm(2.0);
            let footer = Rect::new(area.x, area.bottom() - t.target, area.w, t.target);
            let back_w = t.mm(17.0);
            p.back.place(Rect::new(area.x, footer.y, back_w, footer.h), t);
            p.manage.place(Rect::new(area.x + back_w + gap, footer.y, area.w - back_w - gap, footer.h), t);
            p.pager.place(Rect::new(area.x, footer.y - t.target - gap, area.w, t.target), t);
            p.status = Rect::new(area.x, p.pager.rect().y - t.mm(8.0), area.w, t.mm(8.0));
            p.set.place(Rect::new(area.x, area.y, area.w, t.target), t);
            let list_y = area.y + t.target + gap;
            p.list.place(Rect::new(area.x, list_y, area.w, p.status.y - list_y), t.row2, 0);
        }
    }
    /// The name, plus its source when another dictionary has the same name.
    fn title(&self, d: &Dictionary) -> String {
        if self.catalog.iter().filter(|o| o.name == d.name).count() > 1 {
            let source = self.sources.get(d.source).map_or_else(String::new, Source::label);
            format!("{} · {source}", d.name)
        } else {
            d.name.clone()
        }
    }
    pub fn busy(&self) -> bool {
        !self.complete || self.pending || (self.pages.is_none() && self.layout_error.is_none() || self.paginating())
    }
    pub fn paginating(&self) -> bool {
        self.pages.as_ref().is_some_and(|p| !p.is_complete())
    }
    /// One bounded piece per idle turn, after the visible page was presented.
    pub fn on_idle(&mut self, text: &mut TextRenderer) -> bool {
        let Some(pages) = self.pages.as_mut().filter(|p| !p.is_complete()) else { return false };
        let had_next = self.pager.page + 1 < pages.pages().len();
        let started = std::time::Instant::now();
        let step = text.paginate_styled_step(pages);
        self.layout_timing.1 += started.elapsed();
        if let Err(e) = step {
            self.layout_error = Some(format!("Couldn't lay out this definition: {e}"));
            self.pages = None;
            self.pager.set(0, 1);
            self.pager.incomplete = false;
            return true;
        }
        self.pager.set(self.pager.page, pages.pages().len());
        self.pager.incomplete = !pages.is_complete();
        if pages.is_complete() {
            log::debug!(
                "dictionary pagination complete: {} pages, {} pieces, layout {:?}, wall {:?}",
                pages.pages().len(),
                pages.paragraphs().len(),
                self.layout_timing.1,
                self.layout_timing.0.elapsed()
            );
        }
        pages.is_complete() || !had_next && self.pager.page + 1 < pages.pages().len()
    }
    fn submit(&mut self, rescan: bool) {
        self.query = self.field.value().into();
        self.repeat(rescan);
    }
    /// Runs the last submitted query again, leaving any unsubmitted field edit.
    fn repeat(&mut self, rescan: bool) {
        self.request = self.worker.lookup_in(self.query.clone(), rescan, self.selection.clone());
        self.pending = true;
        self.complete = false;
        self.first_frame = true;
        self.matches.clear();
        self.selected = None;
        self.preparing = None;
        self.revision = self.worker.revision();
        self.pages = None;
        self.layout_error = None;
        self.pager.set(0, 1);
        self.pager.incomplete = true;
        self.field.focused = false;
        self.keyboard.visible = false;
    }
    /// Consumes the worker's reply for the current request. `drawing` lets the
    /// first frame of a request wait briefly for it.
    fn take_reply(&mut self, t: &Theme, drawing: bool) -> bool {
        if self.revision != self.worker.revision() || self.returning && !self.complete {
            self.repeat(false);
        }
        self.returning = false;
        let first_frame = drawing && std::mem::take(&mut self.first_frame);
        let reply = if first_frame && self.pending && self.selected.is_none() && self.pages.is_none() {
            self.worker.receive_wait(FIRST_DRAW_WAIT)
        } else {
            self.worker.receive()
        };
        if let Some(reply) = reply
            && reply.id == self.request
        {
            self.complete = reply.complete;
            self.preparing = reply.preparing;
            self.catalog = reply.dictionaries;
            self.sources = reply.sources;
            self.warnings = reply.warnings;
            self.warnings.extend(self.catalog.iter().filter(|d| !d.disabled).filter_map(|d| d.error.clone()));
            self.active_set = reply.active_set;
            self.set_known = true;
            self.matches = reply.matches;
            self.sets = reply.sets;
            // A final count must never replace a dictionary selected since the
            // first publication, or throw away its pagination/page.
            if (self.selected.is_none() || self.selected == reply.selected)
                && let Some(state) = reply.state
            {
                let changed = self.selected != reply.selected || self.state != state || self.pending;
                self.pending = !self.complete && !matches!(&state, State::Definition { .. });
                self.selected = reply.selected;
                self.state = state;
                if changed {
                    self.pages = None;
                    self.layout_error = None;
                    self.pager.set(0, 1);
                    self.pager.incomplete = true;
                }
            }
            if let Some(error) = reply.save_error {
                self.pending_save_error = Some(error);
            }
            self.refresh_picker(t);
            true
        } else {
            false
        }
    }
    pub fn receive(&mut self, cx: &mut EventCx) {
        if self.take_reply(cx.theme, false) {
            cx.redraw();
        }
        if let Some(error) = self.pending_save_error.take() {
            log::warn!("dictionary preferences: {error}");
            cx.toast("Couldn't save dictionary preferences", None);
        }
    }
    fn choose(&mut self, index: usize) {
        let Some(matched) = self.matches.get(index) else { return };
        if self.selected.as_ref() == Some(&matched.dictionary) {
            return;
        }
        self.selected = Some(matched.dictionary.clone());
        self.worker.open(&matched.dictionary);
        self.pending = true;
        self.pages = None;
        self.layout_error = None;
        self.pager.set(0, 1);
        self.pager.incomplete = true;
    }
    fn cycle(&mut self, delta: isize) {
        let current = self.matches.iter().position(|m| Some(&m.dictionary) == self.selected.as_ref()).unwrap_or(0);
        let target = current as isize + delta;
        if target >= 0 {
            self.choose(target as usize);
        }
    }
    /// The switcher's counter ("2 / 13"), with a quiet suffix while the
    /// search is still running.
    fn status(&self) -> String {
        let n = self.matches.len();
        let mut parts = Vec::new();
        match self.matches.iter().position(|m| Some(&m.dictionary) == self.selected.as_ref()) {
            Some(i) => parts.push(format!("{} / {n}", i + 1)),
            None if n > 0 || self.complete => parts.push(format!("{n} found")),
            None => {}
        }
        if !self.complete {
            parts.push(if self.preparing.is_some() { "Preparing…" } else { "Searching…" }.to_owned());
        }
        parts.join(" · ")
    }
    fn open_picker(&mut self, t: &Theme) {
        let button = |label| Button::new(label, ButtonKind::Secondary);
        self.picker = Some(Picker {
            sets: false,
            list: List::new(vec![]),
            pager: Pager::new(1),
            set: button(""),
            back: button("Back"),
            manage: button("Manage dictionaries"),
            status: Rect::default(),
        });
        self.refresh_picker(t);
        self.place(t);
        self.refresh_picker(t);
    }
    fn refresh_picker(&mut self, _t: &Theme) {
        let rows = if self.picker.as_ref().is_some_and(|p| p.sets) {
            self.sets
                .iter()
                .map(|s| {
                    Row::new(&s.name)
                        .sub(if s.languages.is_empty() {
                            "No assigned languages".into()
                        } else {
                            s.languages.join(", ")
                        })
                        .value(if self.selection.book_set.as_ref() == Some(&s.id) { "Chosen" } else { "" })
                })
                .collect()
        } else {
            self.matches
                .iter()
                .map(|m| {
                    let name = self
                        .catalog
                        .iter()
                        .find(|d| d.id == m.dictionary)
                        .map_or_else(|| m.dictionary.clone(), |d| self.title(d));
                    let headwords = m.headwords().join(", ");
                    let label = if m.hits.forms.is_some() { format!("{headwords} (base form)") } else { headwords };
                    Row::new(name)
                        .sub(format!(
                            "{label} · {} {}",
                            m.hits.records.len(),
                            if m.hits.records.len() == 1 { "entry" } else { "entries" }
                        ))
                        .value(if self.selected.as_ref() == Some(&m.dictionary) { "Current" } else { "" })
                })
                .collect()
        };
        let set_name = self.sets.iter().find(|s| s.id == self.active_set).map_or("Default", |s| s.name.as_str());
        if let Some(p) = &mut self.picker {
            p.set.label =
                if p.sets { "Choose a set for this book".into() } else { format!("Set: {set_name} · Change") };
            p.list.rows = rows;
            p.pager.set(p.list.page, p.list.paging().pages());
            p.list.page = p.pager.page;
        }
    }
    pub fn on_text_input(&mut self, input: &TextInput, cx: &mut EventCx) -> bool {
        if !self.field.focused {
            return false;
        }
        match input {
            TextInput::Submit => self.submit(false),
            TextInput::Dismiss => {
                self.field.focused = false;
                self.keyboard.visible = false;
            }
            _ => {
                self.field.edit(input);
            }
        }
        self.place(cx.theme);
        cx.redraw();
        true
    }
    fn start_editing(&mut self) {
        self.field.focused = true;
        self.keyboard.visible = true;
    }
    fn manage_event(&mut self) -> LookupEvent {
        self.picker = None;
        self.returning = true;
        if !self.complete {
            self.worker.cancel();
        }
        LookupEvent::Manage(self.set_known.then(|| self.active_set.clone()))
    }
    /// All gestures are consumed by the reader while this overlay is open.
    pub fn on_gesture(&mut self, g: &Gesture, cx: &mut EventCx) -> Option<LookupEvent> {
        self.place(cx.theme);
        let event = self.gesture(g, cx);
        // Closing the picker or the keyboard, or expanding, moves the rows.
        self.place(cx.theme);
        event
    }
    fn gesture(&mut self, g: &Gesture, cx: &mut EventCx) -> Option<LookupEvent> {
        let t = cx.theme;
        if self.keyboard.visible {
            if let Some(input) = self.keyboard.on_gesture_inside(g, t) {
                if let Some(input) = input {
                    self.on_text_input(&input, cx);
                }
                return None;
            }
            // Beside the keyboard: ×, Look up and the field; anything else
            // puts the keyboard away.
            if self.close.on_gesture(g) {
                return Some(LookupEvent::Close);
            }
            if self.search.on_gesture(g) {
                self.submit(false);
                return None;
            }
            if !self.field.on_gesture(g) && matches!(g, Gesture::Tap(_)) {
                self.on_text_input(&TextInput::Dismiss, cx);
            }
            return None;
        }
        if self.sheet.on_gesture(g) || self.close.on_gesture(g) {
            return Some(LookupEvent::Close);
        }
        if self.expand.on_gesture(g) {
            self.expanded = !self.expanded;
            return None;
        }
        if let Some(p) = &mut self.picker {
            if p.manage.on_gesture(g) {
                return Some(self.manage_event());
            }
            if p.back.on_gesture(g) {
                if p.sets {
                    p.sets = false;
                    p.list.page = 0;
                    self.refresh_picker(t);
                } else {
                    self.picker = None;
                }
                return None;
            }
            if p.set.on_gesture(g) {
                p.sets = !p.sets;
                p.list.page = 0;
                self.refresh_picker(t);
                return None;
            }
            p.pager.on_gesture(g);
            if let Gesture::Swipe { dir, .. } = g {
                p.pager.on_swipe(*dir);
            }
            p.list.page = p.pager.page;
            if let Some(ListEvent::Tapped(index)) = p.list.on_gesture(g) {
                if p.sets {
                    let id = self.sets[index].id.clone();
                    self.selection.book_set = Some(id.clone());
                    self.picker = None;
                    self.repeat(false);
                    return Some(LookupEvent::Set(id));
                }
                self.choose(index);
                self.picker = None;
            }
            return None;
        }
        let tapped = |press: &mut Press, area: Rect| {
            matches!(press.on_gesture(g, area), Some(PressEvent::Tap | PressEvent::LongPressEnd))
        };
        if tapped(&mut self.heading_press, self.heading) {
            self.start_editing();
            return None;
        }
        if tapped(&mut self.switcher_press, self.switcher) {
            self.open_picker(t);
            return None;
        }
        if !self.definition_shown() && self.manage.on_gesture(g) {
            return Some(self.manage_event());
        }
        if self.edit_row() {
            if self.field.on_gesture(g) {
                self.start_editing();
                return None;
            }
            if self.search.on_gesture(g) {
                self.submit(false);
                return None;
            }
        }
        if self.previous.on_gesture(g) {
            self.cycle(-1);
        }
        if self.next.on_gesture(g) {
            self.cycle(1);
        }
        let [highlight, note, search, extend] = &mut self.actions;
        if highlight.on_gesture(g) {
            return Some(LookupEvent::Highlight);
        }
        if note.on_gesture(g) {
            return Some(LookupEvent::Note);
        }
        if search.on_gesture(g) {
            return Some(LookupEvent::Search(self.query.clone()));
        }
        if extend.on_gesture(g) {
            return Some(LookupEvent::Extend);
        }
        if self.paged {
            self.pager.on_gesture(g);
        }
        // Swipes turn pages even when the pager row had no room.
        if let Gesture::Swipe { dir, .. } = g {
            self.pager.on_swipe(*dir);
        }
        None
    }
    /// The query (or "Look up word" while typing), then ⤢ and ×.
    fn draw_header(&mut self, cx: &mut DrawCx) {
        let t = cx.theme;
        let r = self.heading;
        if self.keyboard.visible {
            cx.line("Look up word", t.heading, r, HAlign::Left);
        } else {
            let label = if self.query.trim().is_empty() { "Look up a word" } else { self.query.as_str() };
            let style = t.dictionary_headword;
            let (gap, icon) = (t.mm(1.5), t.icon_small);
            let w = (cx.text.width(label, style).ceil() as i32).min((r.w - gap - icon).max(0));
            let pressed = self.heading_press.is_down();
            let (fg, quiet) = if pressed { (PAPER, PAPER) } else { (INK, INK2) };
            if pressed {
                let pill = Rect::new(r.x - t.mm(1.0), r.y + t.mm(0.6), w + gap + icon + t.mm(2.0), r.h - t.mm(1.2));
                cx.canvas.fill_rounded_rect(pill, t.radius, INK);
                cx.hint(pill, RefreshMode::Fast);
            }
            cx.line(label, style.gray(fg), Rect::new(r.x, r.y, w + 2, r.h), HAlign::Left);
            if self.picker.is_none() {
                cx.icon(Icon::Pencil, Rect::new(r.x + w + gap, r.y, icon, r.h), icon, quiet);
            }
        }
        if self.expand.rect().w > 0 {
            self.expand.draw(cx);
        }
        self.close.draw(cx);
    }
    fn draw_switcher(&mut self, cx: &mut DrawCx, status: &str) {
        let t = cx.theme;
        let index = self.matches.iter().position(|m| Some(&m.dictionary) == self.selected.as_ref());
        self.previous.enabled = index.is_some_and(|i| i > 0);
        self.next.enabled = index.is_some_and(|i| i + 1 < self.matches.len());
        self.previous.draw(cx);
        self.next.draw(cx);
        let r = self.switcher;
        let pressed = self.switcher_press.is_down();
        let (fg, quiet) = if pressed {
            cx.canvas.fill_rounded_rect(r, t.radius, INK);
            cx.hint(r, RefreshMode::Fast);
            (PAPER, PAPER)
        } else {
            cx.canvas.stroke_rounded_rect(r, t.radius, t.border as f32, INK);
            (INK, INK2)
        };
        let (pad, icon) = (t.mm(2.2), t.icon_small);
        let chevron = Rect::new(r.right() - pad - icon, r.y, icon, r.h);
        cx.icon(Icon::ChevronDown, chevron, icon, quiet);
        let meta_style = t.small.gray(quiet);
        let meta_w = (cx.text.width(status, meta_style).ceil() as i32).min(r.w / 2);
        let meta = Rect::new(chevron.x - t.mm(1.0) - meta_w, r.y, meta_w + 1, r.h);
        cx.line(status, meta_style, meta, HAlign::Right);
        let name = self
            .catalog
            .iter()
            .find(|d| Some(&d.id) == self.selected.as_ref())
            .map_or_else(|| "Dictionaries".into(), |d| self.title(d));
        let name_rect = Rect::new(r.x + pad, r.y, (meta.x - t.mm(1.5) - r.x - pad).max(0), r.h);
        cx.line(&name, t.button.gray(fg), name_rect, HAlign::Left);
    }
    /// Lays out the first page. A definition that fits one page uses the
    /// pager's row too; a longer one gives that row back to the pager,
    /// re-breaking the lines already shaped.
    fn paginate(&mut self, cx: &mut DrawCx) {
        let t = cx.theme;
        if let Some(pages) = &self.pages {
            let want = if self.paged { self.body_paged } else { self.body_full };
            if pages.size() != (want.w, want.h) {
                // The card changed size (expanded, collapsed, or typing).
                self.pages = None;
            }
        }
        if self.pages.is_some() || self.layout_error.is_some() {
            return;
        }
        let started = std::time::Instant::now();
        let (mut result, mut bytes);
        loop {
            let blocks = content(&self.state, self.pending, &self.warnings, t);
            bytes =
                blocks.iter().map(|b| if let StyledBlock::Paragraph(p) = b { p.text.len() } else { 0 }).sum::<usize>();
            let (full, paged_h) = (self.body_full, self.body_paged.h);
            // The pager row is worth it only if a few lines stay beside it.
            let roomy = paged_h >= 3 * t.dictionary_body.line_height().ceil() as i32;
            result = StyledPagination::new(blocks, full.w, full.h).and_then(|mut pages| {
                cx.text.paginate_styled_until(&mut pages, 1)?;
                let paged = !(pages.is_complete() && pages.pages().len() <= 1);
                if paged && roomy {
                    pages.repage(paged_h)?;
                    cx.text.paginate_styled_until(&mut pages, 1)?;
                }
                Ok((pages, paged))
            });
            // Long status text in a compact card with Manage and the query
            // field: grow once to the expanded card rather than squeezing it.
            if matches!(&result, Ok((_, true))) && !roomy && !self.landscape() && !self.expanded && !self.grew {
                self.grew = true;
                self.expanded = true;
                self.place(t);
                continue;
            }
            // Otherwise pages turn by swipe alone.
            if let Ok((_, paged)) = &mut result {
                *paged &= roomy;
            }
            break;
        }
        match result {
            Ok((pages, paged)) => {
                self.paged = paged;
                self.pager.set(0, pages.pages().len());
                self.pager.incomplete = !pages.is_complete();
                self.pages = Some(pages);
            }
            Err(e) => {
                self.layout_error = Some(format!("Couldn't lay out this definition: {e}"));
                self.paged = false;
                self.pager.set(0, 1);
                self.pager.incomplete = false;
            }
        }
        self.body = if self.paged { self.body_paged } else { self.body_full };
        self.layout_timing = (started, started.elapsed());
        log::debug!(
            "dictionary first page pagination: {:?}, {bytes} text bytes, {} pages, complete {}",
            self.layout_timing.1,
            self.pages.as_ref().map_or(0, |p| p.pages().len()),
            self.pages.as_ref().is_some_and(StyledPagination::is_complete)
        );
    }
    pub fn draw(&mut self, cx: &mut DrawCx) {
        let t = cx.theme;
        self.place(t);
        self.sheet.draw(cx);
        // Drawing the cached book page takes long enough that a warm lookup
        // may finish while the sheet chrome is drawn. Consume it before
        // laying out content so the first update can contain the definition.
        self.take_reply(t, true);
        // The reply may change which rows show.
        self.place(t);
        let status = self.status();
        log::debug!("dictionary status draw: {status}, set {}", self.active_set);
        self.draw_header(cx);
        let rule = |cx: &mut DrawCx, y: i32, r: Rect| cx.fill(Rect::new(r.x, y, r.w, t.hairline), LINE);
        if let Some(p) = &mut self.picker {
            p.set.draw(cx);
            p.list.draw(cx);
            p.pager.draw(cx);
            p.back.draw(cx);
            p.manage.draw(cx);
            cx.line(&status, t.small.gray(INK2), p.status, HAlign::Left);
            return;
        }
        if !self.keyboard.visible {
            self.draw_switcher(cx, &status);
        }
        if self.edit_row() {
            self.field.draw(cx);
            self.search.draw(cx);
        }
        rule(cx, self.divider, self.body_full);
        if let Some(y) = self.actions_divider {
            rule(cx, y, self.body_full);
            for b in &self.actions {
                b.draw(cx);
            }
        }
        if !self.definition_shown() {
            self.manage.draw(cx);
        }
        self.paginate(cx);
        if let Some(pages) = &mut self.pages {
            let started = std::time::Instant::now();
            // Keep accents, italic overhangs and descenders in the controls' gap.
            let clip = self.body.inset(-t.mm(0.5).max(2));
            if let Err(e) = cx.text.draw_styled_page(cx.canvas, pages, self.pager.page, self.body, clip, t.hairline) {
                log::error!("dictionary page draw: {e:#}");
            }
            log::debug!("dictionary page draw: {:?}", started.elapsed());
        }
        if let Some(error) = &self.layout_error {
            cx.text.draw(cx.canvas, error, t.body, self.body, HAlign::Left);
        }
        if self.paged {
            self.pager.draw(cx);
        }
        self.keyboard.draw(cx);
    }
}

pub enum SelectionAction {
    Dismiss,
    Dictionary,
    Search,
    Highlight,
    Note,
    Extend,
}
pub struct SelectionActions {
    sheet: Sheet,
    highlight: Button,
    note: Button,
    extend: Button,
    search: Button,
    dictionary: Button,
}
impl SelectionActions {
    /// Two columns in portrait; landscape uses three, so the sheet stays low
    /// and leaves the selection visible.
    pub fn new(t: &Theme, screen: Rect) -> Self {
        let cols = if screen.w > screen.h { 3 } else { 2 };
        let rows = (5 + cols - 1) / cols;
        let gap = t.mm(3.0);
        let sheet = Sheet::open("Selection", t.mm(22.0) + rows * t.target + (rows - 1) * gap, screen, t);
        let body = sheet.content(t);
        let width = (body.w - (cols - 1) * gap) / cols;
        let place = |b: &mut Button, i: i32| {
            b.place(
                Rect::new(body.x + (i % cols) * (width + gap), body.y + (i / cols) * (t.target + gap), width, t.target),
                t,
            )
        };
        let mut search = Button::new("Search in book", ButtonKind::Secondary);
        let mut dictionary = Button::new("Dictionary", ButtonKind::Primary);
        let mut highlight = Button::new("Highlight", ButtonKind::Primary);
        let mut note = Button::new("Add note", ButtonKind::Secondary);
        let mut extend = Button::new("Extend selection", ButtonKind::Secondary);
        for (i, b) in [&mut search, &mut dictionary, &mut highlight, &mut note, &mut extend].into_iter().enumerate() {
            place(b, i as i32);
        }
        Self { sheet, search, dictionary, highlight, note, extend }
    }
    pub fn on_gesture(&mut self, g: &Gesture) -> Option<SelectionAction> {
        if self.sheet.on_gesture(g) {
            Some(SelectionAction::Dismiss)
        } else if self.highlight.on_gesture(g) {
            Some(SelectionAction::Highlight)
        } else if self.note.on_gesture(g) {
            Some(SelectionAction::Note)
        } else if self.extend.on_gesture(g) {
            Some(SelectionAction::Extend)
        } else if self.search.on_gesture(g) {
            Some(SelectionAction::Search)
        } else if self.dictionary.on_gesture(g) {
            Some(SelectionAction::Dictionary)
        } else {
            None
        }
    }
    pub fn draw(&self, cx: &mut DrawCx) {
        self.sheet.draw(cx);
        self.search.draw(cx);
        self.dictionary.draw(cx);
        self.highlight.draw(cx);
        self.note.draw(cx);
        self.extend.draw(cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        dictionary::sets::{DEFAULT, Edit},
        platform::{TextInput, Waker},
        store::Store,
    };
    use std::{
        sync::mpsc,
        time::{Duration, Instant},
    };

    #[test]
    fn styled_lookup_orders_content_and_pages_identically_in_both_orientations() -> anyhow::Result<()> {
        use crate::{
            dictionary::{Block, Entry, Span, worker::BaseForms},
            gfx::{Canvas, IconRenderer},
            ui::gesture::Direction,
        };
        let t = Theme::for_dpi(300);
        let root = std::env::temp_dir().join(format!("verso-styled-lookup-{}", std::process::id()));
        let worker = Rc::new(Worker::new(Store::at(root.clone()), Waker::new(|| {})));
        for screen in [Rect::new(0, 0, 1272, 1696), Rect::new(0, 0, 1696, 1272)] {
            let mut lookup = Lookup::new("leaves", worker.clone(), &t, screen, None, Default::default());
            worker.cancel(); // This test supplies its own styled state.
            lookup.pending = false;
            lookup.complete = true;
            lookup.state = State::Definition {
                entries: vec![
                    Entry {
                        headword: "leaf".into(),
                        blocks: vec![
                            Block {
                                kind: BlockKind::Text,
                                depth: 9,
                                text: "An example /liːf/".into(),
                                spans: vec![
                                    Span {
                                        start: 0,
                                        end: 10,
                                        style: Style::BOLD.union(Style::ITALIC).union(Style::MUTED).union(Style::LINK),
                                    },
                                    Span { start: 11, end: 17, style: Style::PHONETIC },
                                ],
                            },
                            Block::plain("A long original definition with enough words to wrap. ".repeat(1000)),
                        ],
                    },
                    Entry { headword: "leave".into(), blocks: vec![Block::plain("To depart.")] },
                ],
                base_forms: Some(BaseForms { query: "leaves".into(), count: 2 }),
            };
            lookup.warnings = vec!["/gone is not available.".into()];
            let input = content(&lookup.state, false, &lookup.warnings, &t);
            let StyledBlock::Paragraph(p) = &input[3] else { panic!("definition paragraph") };
            assert_eq!(p.indent, (4 * t.dictionary_indent) as f32);
            assert_eq!(p.muted, INK2);
            assert!(p.spans[0].style.contains(
                SpanStyle::BOLD.union(SpanStyle::ITALIC).union(SpanStyle::MUTED).union(SpanStyle::UNDERLINE)
            ));
            assert!(p.spans[1].style.contains(SpanStyle::PHONETIC));
            let mut text = TextRenderer::new();
            let mut canvas = Canvas::new(screen.w as u32, screen.h as u32);
            let mut icons = IconRenderer::default();
            let draw = |lookup: &mut Lookup, canvas: &mut Canvas, text: &mut TextRenderer, icons: &mut IconRenderer| {
                canvas.fill(crate::ui::theme::PAPER);
                lookup.draw(&mut DrawCx::new(canvas, text, icons, &t, &mut Vec::new()));
                let r = lookup.body;
                (r.y..r.bottom())
                    .flat_map(|y| {
                        canvas.gray_data()[y as usize * screen.w as usize + r.x as usize
                            ..y as usize * screen.w as usize + r.right() as usize]
                            .to_vec()
                    })
                    .collect::<Vec<_>>()
            };
            let first = draw(&mut lookup, &mut canvas, &mut text, &mut icons);
            assert!(!lookup.pages.as_ref().unwrap().pages().is_empty());
            assert!(lookup.pager.incomplete);
            assert!(lookup.busy());
            while lookup.paginating() {
                lookup.on_idle(&mut text);
            }
            assert!(!lookup.busy());
            assert!(!lookup.pager.incomplete);
            let pages = lookup.pages.as_ref().unwrap();
            let mut recovered = String::new();
            for page in pages.pages() {
                for part in &page.paragraphs {
                    let piece = pages.piece(part.paragraph);
                    let lines = &pages.paragraphs()[part.paragraph].lines[part.lines.clone()];
                    recovered
                        .push_str(&piece.text[lines.first().unwrap().bytes.start..lines.last().unwrap().bytes.end]);
                    recovered.push('\n');
                }
            }
            assert_eq!(recovered.lines().filter(|s| *s == "leaf").count(), 1);
            assert_eq!(recovered.lines().filter(|s| *s == "leave").count(), 1);
            assert!(recovered.starts_with("Base forms of “leaves”\nleaf\n"));
            assert!(recovered.ends_with("leave\nTo depart.\n"));
            let mut cx = EventCx {
                theme: &t,
                screen,
                now: Instant::now(),
                toast: &mut None,
                forced: &mut Vec::new(),
                needs_redraw: &mut false,
                battery: None,
            };
            let swipe = |dir| Gesture::Swipe { dir, from: screen.center(), to: screen.center() };
            lookup.on_gesture(&swipe(Direction::Left), &mut cx);
            assert_eq!(lookup.pager.page, 1);
            let second = draw(&mut lookup, &mut canvas, &mut text, &mut icons);
            assert_ne!(first, second);
            lookup.on_gesture(&swipe(Direction::Right), &mut cx);
            assert_eq!(first, draw(&mut lookup, &mut canvas, &mut text, &mut icons));
            lookup.on_gesture(&swipe(Direction::Left), &mut cx);
            assert_eq!(second, draw(&mut lookup, &mut canvas, &mut text, &mut icons));
        }
        // The worker may still be writing its first reply's preferences.
        drop(worker);
        let _ = std::fs::remove_dir_all(root);
        Ok(())
    }

    #[test]
    fn query_editing_switching_preferences_and_stale_replies() -> anyhow::Result<()> {
        let root = std::env::temp_dir().join(format!("verso-lookup-ui-{}", std::process::id()));
        let store = Store::at(root.clone());
        std::fs::create_dir_all(store.dictionary_dir())?;
        for name in ["a", "b"] {
            let path = store.dictionary_dir().join(name);
            std::fs::write(
                path.with_extension("ifo"),
                format!(
                    "StarDict's dict ifo file\nversion=2.4.2\nbookname={name}\nwordcount=1\nidxfilesize=14\nsametypesequence=m\n"
                ),
            )?;
            std::fs::write(path.with_extension("idx"), b"Hello\0\0\0\0\0\0\0\0\x07")?;
            std::fs::write(path.with_extension("dict"), b"meaning")?;
        }
        let (tx, wake) = mpsc::channel();
        let worker = Rc::new(Worker::new(
            store.clone(),
            Waker::new(move || {
                let _ = tx.send(());
            }),
        ));
        let t = Theme::for_dpi(300);
        let screen = Rect::new(0, 0, 1272, 1696);
        let mut lookup = Lookup::new("Hello", worker.clone(), &t, screen, None, Default::default());
        let mut cx = EventCx {
            theme: &t,
            screen,
            now: Instant::now(),
            toast: &mut None,
            forced: &mut Vec::new(),
            needs_redraw: &mut false,
            battery: None,
        };
        let wait = |lookup: &mut Lookup, cx: &mut EventCx| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while lookup.pending || !lookup.complete {
                lookup.receive(cx);
                if !lookup.pending && lookup.complete {
                    break;
                }
                assert!(Instant::now() < deadline);
                let _ = wake.recv_timeout(Duration::from_millis(20));
            }
        };
        wait(&mut lookup, &mut cx);
        assert!(matches!(&lookup.state, State::Definition { entries, .. } if entries[0].blocks[0].text == "meaning"));
        let exact = lookup.state.clone();
        let first_paragraph = |state: &State| {
            let StyledBlock::Paragraph(p) = content(state, false, &[], &t).remove(0) else { panic!("first paragraph") };
            p.text
        };
        assert_eq!(first_paragraph(&exact), "Hello");
        for (count, label) in [(1, "Base form of “axes”"), (2, "Base forms of “axes”")] {
            if let State::Definition { base_forms, .. } = &mut lookup.state {
                *base_forms = Some(crate::dictionary::worker::BaseForms { query: "axes".into(), count });
            }
            assert_eq!(first_paragraph(&lookup.state), label);
        }
        assert!(!first_paragraph(&State::NoResult).contains("Base form"));
        lookup.state = exact;
        // Same-name copies show their source.
        assert_eq!(lookup.title(&lookup.catalog[0]), lookup.catalog[0].name);
        let mut copy = lookup.catalog[0].clone();
        copy.name = lookup.catalog[1].name.clone();
        lookup.catalog.push(copy.clone());
        assert_eq!(lookup.title(&copy), format!("{} · Verso", copy.name));
        lookup.catalog.pop();
        assert_eq!(copy.location(&lookup.sources), format!("Verso · {}", copy.id));
        let initial = lookup.selected.clone();
        lookup.cycle(1);
        wait(&mut lookup, &mut cx);
        assert_ne!(lookup.selected, initial);
        // A field edit leaves the source selection/query untouched until submit.
        lookup.field.focused = true;
        lookup.keyboard.visible = true;
        assert!(lookup.on_text_input(&TextInput::Clear, &mut cx));
        assert!(lookup.on_text_input(&TextInput::Insert("absent".into()), &mut cx));
        assert!(lookup.on_text_input(&TextInput::Submit, &mut cx));
        wait(&mut lookup, &mut cx);
        assert_eq!(lookup.state, State::NoResult);
        // Change requests are persisted before the next result or exit barrier.
        worker.edit(Edit::MoveDictionary { set: DEFAULT.into(), dictionary: "b.ifo".into(), position: 0 });
        lookup.submit(false);
        wait(&mut lookup, &mut cx);
        assert_eq!(lookup.catalog[0].id, "b.ifo");
        // Returning from management repeats the submitted query and keeps an unsubmitted edit.
        lookup.field.edit(&TextInput::Clear);
        lookup.field.edit(&TextInput::Insert("draft".into()));
        lookup.returning = true;
        lookup.receive(&mut cx);
        wait(&mut lookup, &mut cx);
        assert_eq!((lookup.query.as_str(), lookup.field.value()), ("absent", "draft"));
        assert_eq!(lookup.state, State::NoResult);
        worker.edit(Edit::Disabled { set: DEFAULT.into(), dictionary: "b.ifo".into(), disabled: true });
        worker.flush()?;
        let prefs: crate::dictionary::sets::Preferences =
            serde_json::from_slice(&std::fs::read(store.dictionary_preferences())?)?;
        // Disabled, it moves below the enabled dictionaries.
        let default = prefs.set(crate::dictionary::sets::DEFAULT);
        let b = default.dictionaries.iter().position(|d| d.id == "b.ifo").unwrap();
        assert!(default.dictionaries[b].disabled);
        assert_eq!(b, default.boundary());
        // The edit changed the service revision, so the next receive repeats
        // the query. Let that finish first: a repeat would clear the injected
        // reply below, and its real reply could end the pending request.
        lookup.receive(&mut cx);
        wait(&mut lookup, &mut cx);
        // An obsolete worker reply must not replace a newer request.
        let old = lookup.request;
        lookup.request += 1;
        lookup.pending = true;
        lookup.worker.result_for_test(crate::dictionary::worker::Reply {
            id: old,
            matches: vec![],
            complete: true,
            preparing: None,
            unprepared: vec![],
            missing: vec![],
            sets: vec![],
            active_set: crate::dictionary::sets::DEFAULT.into(),
            change_error: None,
            word_forms: vec![],
            dictionaries: Vec::new(),
            sources: Vec::new(),
            warnings: Vec::new(),
            selected: None,
            state: Some(State::Missing),
            save_error: None,
        });
        lookup.receive(&mut cx);
        assert!(lookup.pending);
        assert_eq!(lookup.catalog.len(), 2);
        worker.flush()?;
        drop(lookup);
        drop(worker);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "dictionary_tests.rs"]
mod result_tests;
