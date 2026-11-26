//! Lookup is an overlay over the unchanged cached book page: a card with
//! three tabs. This file holds the card and its Dictionary tab; `tabs.rs`
//! holds the online ones (`wikipedia.rs` and `translate.rs`), `pickers.rs`
//! what they choose with and `passage_bar.rs` the bar beside a passage.
mod passage_bar;
mod pickers;
mod tabs;
mod translate;
mod wikipedia;

pub use passage_bar::{SelectionAction, SelectionActions};
pub use tabs::{Languages, Tab, Tabs};

use crate::{
    dictionary::{
        Block, BlockKind, Dictionary, Source, Style,
        languages::language_name,
        sets::Set,
        worker::{Match, State, Worker},
    },
    geom::{Point, Rect},
    gfx::{
        HAlign, Icon, SpanStyle, StyledBlock, StyledPagination, StyledParagraph, StyledSpan, StyledWordHit,
        TextRenderer, TextStyle, Weight,
    },
    online::{Error, QUERY_LIMIT, translate::Provider, wikipedia::Edition},
    platform::{RefreshMode, TextInput},
    screens::EventCx,
    ui::{
        gesture::Gesture,
        theme::{INK, INK2, LINE, PAPER, Theme},
        widget::{DrawCx, Press, PressEvent},
        widgets::{Button, ButtonKind, IconButton, Keyboard, List, ListEvent, Pager, Row, Sheet, TextField},
    },
};
use pickers::{Chooser, Chosen, DropDown};
use std::rc::Rc;
use tabs::{Replies, too_long};
use wikipedia::Wikipedia;

const MISSING: &str = "No dictionaries found. Add a StarDict dictionary to Verso's dictionaries/ folder, \
                       then open Dictionaries and tap Rescan.";
const DISABLED: &str = "All dictionaries are disabled. Open Dictionaries to enable one.";
const NO_RESULT: &str = "No definition found. Edit the word above, or switch dictionary sets.";
/// Without the field, for text too long to edit.
const NO_RESULT_LONG: &str = "No definition found. Try another dictionary set.";
const FIRST_DRAW_WAIT: std::time::Duration = std::time::Duration::from_millis(12);
/// Nesting levels that indent; deeper blocks stay at the last level.
const MAX_DEPTH: u8 = 4;
/// Earlier lookups Back can return to in a chain of looked-up words.
const HISTORY: usize = 24;
/// The most recent of them keep their reply, so Back shows them at once.
const CACHED: usize = 6;

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
    StyledBlock::Paragraph(StyledParagraph {
        text,
        spans: Vec::new(),
        base,
        muted: INK2,
        indent: 0.0,
        inert,
        keep: false,
    })
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
        keep: false,
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
fn content(state: &State, pending: bool, long: bool, warnings: &[String], t: &Theme) -> Vec<StyledBlock> {
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
        State::NoResult => blocks.push(plain(if long { NO_RESULT_LONG } else { NO_RESULT }.into(), t.body, true)),
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
    /// Another Wikipedia was chosen, for this book.
    Edition(Edition),
    /// A language to translate into was chosen, for every book.
    Target(&'static str),
    /// Another translation service was chosen, for every book.
    Provider(Provider),
    /// ⤢ expanded (true) or collapsed the card; the next lookup opens so.
    Resized(bool),
}

/// A finished reply, kept so Back can show it again without the worker.
struct Snapshot {
    revision: u64,
    matches: Vec<Match>,
    state: State,
    active_set: String,
    set_known: bool,
    catalog: Vec<Dictionary>,
    sources: Vec<Source>,
    warnings: Vec<String>,
    sets: Vec<Set>,
}

/// An earlier lookup in a chain: Back returns to its word, tab, dictionary
/// and page.
struct Visit {
    query: String,
    tab: Tab,
    /// The page each tab showed.
    pages: [usize; 3],
    selected: Option<String>,
    snapshot: Option<Box<Snapshot>>,
    online: Replies,
}

/// A touch on the definition, which looks up the word under it on release.
struct WordPress {
    at: Point,
    /// The word, found by the next draw (which has the text renderer):
    /// `None` until then, `Some(None)` when no word is there.
    hit: Option<Option<StyledWordHit>>,
    long: bool,
    /// Released before a draw found the word: that draw looks it up.
    released: bool,
}

/// Whether two queries are the same words, ignoring case and line breaks.
fn same_query(a: &str, b: &str) -> bool {
    one_line(a).to_lowercase() == one_line(b).to_lowercase()
}

/// A passage may have several paragraphs, which the header and the field
/// (which drops line breaks) show as one line.
fn one_line(query: &str) -> String {
    query.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// What the online tabs' chooser is for.
enum Choosing {
    /// One of Wikipedia's matches.
    Match,
    /// A Wikipedia, in the rows' order.
    Edition(Vec<Edition>),
    /// A language to translate from (None detects it), in the rows' order.
    Source(Vec<Option<&'static str>>),
    /// A language to translate into, in the rows' order.
    Target(Vec<&'static str>),
    /// A translation service, in `Provider::ALL`'s order.
    Provider,
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
    /// Wikipedia, which opens at 80%, was collapsed. Until the tab changes.
    folded: bool,
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
    /// The dictionaries were asked for `query`: only once their tab shows.
    asked: bool,
    preparing: Option<String>,
    matches: Vec<Match>,
    sets: Vec<Set>,
    revision: u64,
    tabs: Tabs,
    /// The page each tab showed when it was last left.
    tab_pages: [usize; 3],
    picker: Option<Picker>,
    chooser: Option<(Choosing, Chooser)>,
    /// What the body lists instead of text, paged by `pager`: Wikipedia's
    /// matches, or the languages to translate into while none is chosen.
    list: Option<List>,
    /// Earlier lookups of a chain, newest last. Depth 0 (empty) is the
    /// word from the book.
    history: Vec<Visit>,
    /// The page to show once the content is laid out, after Back or another
    /// tab.
    restore_page: Option<usize>,
    /// The worker's session (which `open` reads) is this lookup's query.
    /// Not after Back restored a cached reply: switching dictionaries then
    /// asks again.
    session_live: bool,
    word_press: Option<WordPress>,
    /// ← at depth 1 and deeper.
    back: IconButton,
    /// The tappable query in the header row, which opens query editing.
    heading: Rect,
    heading_press: Press,
    expand: IconButton,
    close: IconButton,
    previous: Button,
    /// The dictionary switcher between ‹ and ›, which opens the picker.
    switcher: DropDown,
    next: Button,
    /// Wikipedia's row: the other matches (or how many there are while they
    /// show as a list), and its language.
    other_matches: DropDown,
    hit_count: Rect,
    edition: DropDown,
    /// Translate's row: the languages it translates from and into, and the
    /// service, while a DeepL key is saved.
    source: DropDown,
    target: DropDown,
    service: DropDown,
    translate_row: Rect,
    /// Below an online tab's error.
    retry: Button,
    /// In its place when the text is in the language to translate into.
    another_language: Button,
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
    /// How far down the first page's text goes.
    text_height: Option<i32>,
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
    /// unknown, which is how the at-bottom setting keeps it there), so the
    /// word, its sentence and the selection handles stay visible; in
    /// landscape it stands at the side away from the word. It opens on, and
    /// asks for, the tab `tabs` names and no other.
    pub fn new(
        query: &str,
        worker: Rc<Worker>,
        t: &Theme,
        screen: Rect,
        word: Option<Rect>,
        selection: crate::dictionary::sets::Selection,
        tabs: Tabs,
    ) -> Self {
        let mut field = TextField::new("Word");
        field.limit = Some(QUERY_LIMIT);
        field.edit(&TextInput::Insert(one_line(query)));
        let asked = tabs.current() == Tab::Dictionary;
        let request = if asked { worker.lookup_in(query.into(), false, selection.clone()) } else { 0 };
        let mut keyboard = Keyboard::default();
        keyboard.set_submit("Look up");
        keyboard.place(screen, t);
        let revision = worker.revision();
        let action = |label| Button::new(label, ButtonKind::Secondary);
        let mut lookup = Self {
            screen,
            word,
            expanded: false,
            folded: false,
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
            asked,
            preparing: None,
            matches: vec![],
            sets: vec![],
            revision,
            tabs,
            tab_pages: [0; 3],
            picker: None,
            chooser: None,
            list: None,
            history: Vec::new(),
            restore_page: None,
            session_live: true,
            word_press: None,
            back: IconButton::new(Icon::ArrowLeft),
            heading: Rect::default(),
            heading_press: Press::default(),
            expand: IconButton::new(Icon::Expand),
            close: IconButton::new(Icon::Close),
            previous: Button::new("‹", ButtonKind::Secondary),
            switcher: DropDown::default(),
            next: Button::new("›", ButtonKind::Secondary),
            other_matches: DropDown::default(),
            hit_count: Rect::default(),
            edition: DropDown::default(),
            source: DropDown::default(),
            target: DropDown::default(),
            service: DropDown::default(),
            translate_row: Rect::default(),
            retry: Button::new("Retry", ButtonKind::Primary),
            another_language: Button::new("Choose language", ButtonKind::Primary),
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
            text_height: None,
            pager: Pager::new(1),
            paged: false,
            body_full: Rect::default(),
            body_paged: Rect::default(),
            body: Rect::default(),
        };
        lookup.tabs.ask(&lookup.query);
        lookup.place(t);
        lookup
    }
    /// Opens expanded, as ⤢ last left the card.
    pub fn expanded(mut self, t: &Theme) -> Self {
        self.expanded = true;
        self.place(t);
        self
    }
    /// The card's rectangle on screen. Touches outside it belong to the book
    /// page (a selection handle) or close the card.
    pub fn rect(&self, t: &Theme) -> Rect {
        self.frame(t)
    }
    /// Whether the book selection's handles can be dragged beside the card:
    /// only on the plain card, not while picking a dictionary or typing.
    pub fn allows_handles(&self) -> bool {
        !self.picking() && !self.keyboard.visible && self.history.is_empty()
    }
    /// How many looked-up words lead back to the book's word.
    #[cfg(test)]
    pub(crate) fn depth(&self) -> usize {
        self.history.len()
    }
    #[cfg(test)]
    pub(crate) fn tab(&self) -> Tab {
        self.tabs.current()
    }
    /// The middle of a tab.
    #[cfg(test)]
    pub(crate) fn tab_point(&self, tab: Tab) -> Point {
        self.tabs.rect().columns(3, 0)[tab.index()].center()
    }
    /// The middle of Wikipedia's language button.
    #[cfg(test)]
    pub(crate) fn edition_point(&self) -> Point {
        self.edition.rect().center()
    }
    /// The middle of Translate's service button; None while it's hidden.
    #[cfg(test)]
    pub(crate) fn service_point(&self) -> Option<Point> {
        (!self.service.rect().is_empty()).then(|| self.service.rect().center())
    }
    /// The middle of Translate's source and target buttons.
    #[cfg(test)]
    pub(crate) fn language_points(&self) -> (Point, Point) {
        (self.source.rect().center(), self.target.rect().center())
    }
    /// The middle of a visible row of the body's list.
    #[cfg(test)]
    pub(crate) fn list_point(&self, row: usize) -> Point {
        self.list.as_ref().unwrap().row_rect(row).center()
    }
    /// The middle of a visible row of the open chooser.
    #[cfg(test)]
    pub(crate) fn choice_point(&self, row: usize) -> Point {
        self.chooser.as_ref().unwrap().1.row_point(row)
    }
    /// The middle of ⤢ (⤡ while expanded).
    #[cfg(test)]
    pub(crate) fn expand_point(&self) -> crate::geom::Point {
        self.expand.rect().center()
    }
    /// The middle of an action strip button (Highlight, Note, Search, Extend).
    #[cfg(test)]
    pub(crate) fn action_point(&self, index: usize) -> crate::geom::Point {
        self.actions[index].rect().center()
    }
    fn landscape(&self) -> bool {
        self.screen.w > self.screen.h
    }
    fn on_dictionary(&self) -> bool {
        self.tabs.current() == Tab::Dictionary
    }
    fn on_wikipedia(&self) -> bool {
        self.tabs.current() == Tab::Wikipedia
    }
    /// A picker takes the card's place.
    fn picking(&self) -> bool {
        self.picker.is_some() || self.chooser.is_some()
    }
    /// Portrait: at 80% height, as ⤢ left the card, or because the tab, the
    /// picker or the keyboard needs the room.
    fn tall(&self) -> bool {
        let tab = if self.on_wikipedia() { !self.folded } else { self.expanded };
        tab || self.picking() || self.tabs.needs_target() || self.keyboard.visible
    }
    /// Portrait: compact (~54%) or 80%, at the edge away from the word in
    /// every state, so the tabs stay under the finger. Landscape: a
    /// full-height side panel.
    fn frame(&self, t: &Theme) -> Rect {
        let s = self.screen;
        if self.landscape() {
            let right = self.word.is_none_or(|w| w.center().x < s.center().x);
            let width = (s.w * 52 / 100).max(t.mm(72.0)).min(s.w);
            return if right { s.split_right(width).0 } else { s.split_left(width).0 };
        }
        let top = self.word.is_some_and(|w| w.center().y >= s.center().y);
        let height = if !self.tall() {
            (s.h * 54 / 100).max(t.mm(72.0)).min(s.h * 60 / 100)
        } else if top && self.keyboard.visible {
            // At the top, the card ends where the keyboard starts.
            self.keyboard.rect().y - s.y
        } else {
            s.h * 4 / 5
        };
        if top { s.split_top(height).0 } else { s.split_bottom(height).0 }
    }
    fn definition_shown(&self) -> bool {
        !self.pending && matches!(self.state, State::Definition { .. })
    }
    /// The query field and Look up: while typing, and on a miss so editing
    /// the query is one tap away, unless the field would cut it. Pickers take
    /// their place.
    fn edit_row(&self) -> bool {
        let missed = match self.tabs.current() {
            Tab::Dictionary => !self.pending && self.state == State::NoResult,
            Tab::Wikipedia => self.tabs.wikipedia().found_nothing(),
            Tab::Translate => false,
        };
        self.keyboard.visible || (!self.picking() && missed && !too_long(&self.query))
    }
    /// An online tab's error that may go away.
    fn retry_shown(&self) -> bool {
        !self.keyboard.visible && !self.picking() && self.tabs.error().is_some_and(Error::retryable)
    }
    /// The way out of translating a text into its own language.
    fn another_language_shown(&self) -> bool {
        !self.keyboard.visible && !self.picking() && self.tabs.needs_another_language()
    }
    /// Manage dictionaries, below anything but a definition.
    fn manage_shown(&self) -> bool {
        self.on_dictionary() && !self.definition_shown()
    }
    /// The book-selection actions, only for the book's own word.
    fn actions_shown(&self) -> bool {
        !self.picking() && !self.keyboard.visible && self.history.is_empty()
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
        let expandable = !landscape && !self.picking() && !self.tabs.needs_target() && !typing;
        self.expand.icon = if self.tall() { Icon::Collapse } else { Icon::Expand };
        self.expand.place(if expandable {
            Rect::new(self.close.rect().x - side, cy - side / 2, side, side)
        } else {
            Rect::default()
        });
        let end = if expandable { self.expand.rect().x } else { self.close.rect().x };
        let mut start = c.x;
        if !self.history.is_empty() && !self.picking() && !typing {
            // Inset like the mockup's bordered square, so its edge lines up
            // with the switcher's ‹ below.
            let back = Rect::new(c.x, cy - side / 2, side, side);
            self.back.place(back);
            start = back.right() + gap;
        } else {
            self.back.place(Rect::default());
        }
        self.heading = Rect::new(start, header.y, (end - gap - start).max(0), header.h);
        let mut y = header.bottom();
        // Dictionary · Wikipedia · Translate, over pickers too.
        if typing {
            self.tabs.place(Rect::default(), &self.query);
        } else {
            let row = Rect::new(c.x, y + t.mm(1.0), c.w, t.mm(7.8));
            self.tabs.place(row, &self.query);
            y = row.bottom();
        }
        let header_bottom = y;
        let row = Rect::new(c.x, y + t.mm(1.2), c.w, t.mm(7.8));
        let row = (!typing && !self.picking()).then_some(row);
        // Switcher: ‹ · name · i / N ▾ · ›
        if let Some(row) = row.filter(|_| self.on_dictionary()) {
            let arrow = t.mm(if landscape { 7.1 } else { 8.1 });
            self.previous.place(Rect::new(row.x, row.y, arrow, row.h), t);
            self.next.place(Rect::new(row.right() - arrow, row.y, arrow, row.h), t);
            self.switcher.place(Rect::new(row.x + arrow + gap, row.y, row.w - 2 * (arrow + gap), row.h));
            y = row.bottom();
        }
        // Wikipedia: Other matches · n ▾, language ▾
        for hidden in
            [&mut self.other_matches, &mut self.edition, &mut self.source, &mut self.target, &mut self.service]
        {
            hidden.place(Rect::default());
        }
        if let Some(row) = row.filter(|_| self.on_wikipedia()) {
            let (edition, rest) = row.split_right(row.w * 42 / 100);
            self.edition.place(edition);
            self.hit_count = Rect { w: rest.w - gap, ..rest };
            let found = self.tabs.wikipedia();
            if found.opened().is_some() && found.matches().len() > 1 {
                self.other_matches.place(self.hit_count);
            }
            y = row.bottom();
        }
        // Translate: from ▾ → into ▾, and Google ▾ / DeepL ▾ with a key
        if let Some(row) = row.filter(|_| self.tabs.current() == Tab::Translate) {
            self.translate_row = row;
            if !self.tabs.needs_target() {
                let (mut row, mut share) = (row, 56);
                if self.tabs.can_switch_provider() {
                    let (service, rest) = row.split_right(t.mm(20.0).min(row.w * 3 / 10));
                    self.service.place(service);
                    // The source then shortens to "Auto" where it must.
                    (row, share) = (Rect { w: rest.w - gap, ..rest }, 50);
                }
                let arrow = t.mm(4.4);
                // "Auto (Language)" takes more room than the target.
                let (source, rest) = row.split_left((row.w - arrow) * share / 100);
                self.source.place(source);
                self.target.place(rest.split_right(rest.w - arrow).0);
            }
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
        let (lowest, pager_bottom) = if self.manage_shown() {
            (self.manage.rect().y - t.mm(2.0), self.manage.rect().y - t.mm(1.0))
        } else {
            (bottom, bottom)
        };
        let pager = Rect::new(c.x, pager_bottom - t.mm(7.0), c.w, t.mm(7.0));
        self.pager.place(pager, t);
        // Retry or Choose language stands right below the message.
        let retry_gap = t.mm(2.4);
        let retry_h = if self.retry_shown() || self.another_language_shown() { t.target + retry_gap } else { 0 };
        self.body_full = Rect::new(c.x, top, c.w, (lowest - top - retry_h).max(1));
        self.body_paged = Rect::new(c.x, top, c.w, (pager.y - t.mm(1.0) - top - retry_h).max(1));
        self.place_list(t);
        self.body = if self.paged { self.body_paged } else { self.body_full };
        let message = self.text_height.map_or(self.body.h, |h| h.min(self.body.h));
        let below = Rect::new(c.x, top + message + retry_gap, t.mm(30.0), t.target);
        self.retry.place(below, t);
        self.another_language.place(Rect { w: t.mm(38.0), ..below }, t);
        // Hidden, they keep the places where they last showed: they ignore
        // touches and let go of a press under way, as a reply may hide them.
        self.search.enabled = self.edit_row();
        self.retry.enabled = self.retry_shown();
        self.another_language.enabled = self.another_language_shown();
        self.manage.enabled = self.manage_shown();
        let actions = self.actions_shown();
        self.actions.iter_mut().for_each(|b| b.enabled = actions);
        if let Some((_, chooser)) = &mut self.chooser {
            let area = Rect::new(c.x, top, c.w, frame.bottom() - t.mm(2.4) - top);
            chooser.place(area, t);
        }
        if let Some(p) = &mut self.picker {
            let area = Rect::new(c.x, header_bottom + gap, c.w, frame.bottom() - t.mm(2.4) - header_bottom - gap);
            let gap = t.mm(2.0);
            let footer = Rect::new(area.x, area.bottom() - t.target, area.w, t.target);
            let back_w = t.mm(17.0);
            p.back.place(Rect::new(area.x, footer.y, back_w, footer.h), t);
            p.manage.place(Rect::new(area.x + back_w + gap, footer.y, area.w - back_w - gap, footer.h), t);
            p.pager.place(Rect::new(area.x, footer.y - t.target - gap, area.w, t.target), t);
            p.status = Rect::new(area.x, p.pager.rect().y - t.mm(6.5), area.w, t.mm(6.5));
            p.set.place(Rect::new(area.x, area.y, area.w, t.target), t);
            let list_y = area.y + t.target + gap;
            p.list.place(Rect::new(area.x, list_y, area.w, p.status.y - list_y), t.row2, 0);
        }
    }
    /// Wikipedia's matches, or the first choice of a language to translate
    /// into, as a list in the body, with the pager once they need it.
    fn place_list(&mut self, t: &Theme) {
        let matches = self.tabs.wikipedia().listing().filter(|_| self.on_wikipedia());
        if matches.is_none() && !self.tabs.needs_target() {
            self.list = None;
            return;
        }
        let languages = self.tabs.languages();
        let list = self.list.get_or_insert_with(|| {
            List::new(match matches {
                Some(matches) => matches.iter().map(Wikipedia::row).collect(),
                None => target_rows(languages),
            })
        });
        // Two matches a page even on a short side panel. Languages are dense,
        // as in the chooser.
        let row = match matches {
            Some(_) if self.body_paged.h >= 2 * t.row2 => t.row2,
            Some(_) => t.row,
            None => t.target,
        };
        list.place(self.body_full, row, 0);
        self.paged = list.paging().pages() > 1 && !self.keyboard.visible;
        if self.paged {
            list.place(self.body_paged, row, 0);
        }
        self.pager.set(self.restore_page.take().unwrap_or(self.pager.page), list.paging().pages());
        self.pager.incomplete = false;
        list.page = self.pager.page;
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
        let waiting = if self.on_dictionary() { !self.complete || self.pending } else { self.tabs.waiting() };
        let laid_out = self.pages.is_some() || self.layout_error.is_some() || self.list.is_some();
        // Nothing is laid out under a picker.
        waiting || !laid_out && !self.picking() || self.paginating()
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
            self.layout_error = Some(format!("Couldn't lay out this text: {e}"));
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
    /// Looks up the field's text. A different word is a step in the chain,
    /// which Back returns from.
    fn submit(&mut self, rescan: bool) {
        let query = self.field.value().to_owned();
        let changed = !same_query(&query, &self.query);
        if changed {
            self.push_history();
        }
        self.query = query;
        if self.on_dictionary() {
            self.repeat(rescan);
            return;
        }
        if changed {
            self.forget_dictionary();
            self.reset_layout();
        } else if self.tabs.error().is_some() {
            self.tabs.retry();
            self.reset_layout();
        }
        self.stop_typing();
        self.tabs.ask(&self.query);
    }
    fn stop_typing(&mut self) {
        self.field.focused = false;
        self.keyboard.visible = false;
    }
    /// The content changed: it's laid out again, from its first page.
    fn reset_layout(&mut self) {
        self.pages = None;
        self.list = None;
        self.layout_error = None;
        self.text_height = None;
        self.pager.set(0, 1);
        self.pager.incomplete = true;
    }
    /// The dictionaries haven't been asked for this word; they are once
    /// their tab shows.
    fn forget_dictionary(&mut self) {
        if self.asked && (self.pending || !self.complete) {
            self.worker.cancel();
        }
        self.asked = false;
        self.pending = true;
        self.complete = false;
        self.matches.clear();
        self.selected = None;
        self.preparing = None;
    }
    fn show_tab(&mut self, tab: Tab) {
        let left = self.tabs.current();
        if tab == left {
            return;
        }
        // A tab left before it was laid out again still has its page to show.
        self.tab_pages[left.index()] = self.restore_page.unwrap_or(self.pager.page);
        self.tabs.show(tab);
        self.picker = None;
        self.chooser = None;
        self.word_press = None;
        self.folded = false;
        self.reset_layout();
        self.restore_page = Some(self.tab_pages[tab.index()]);
        if tab == Tab::Dictionary && !self.asked {
            self.repeat(false);
        }
        self.tabs.ask(&self.query);
    }
    /// Looks up a word tapped in the definition, unless it is the word shown.
    fn look_up_word(&mut self, query: String) {
        if same_query(&query, &self.query) {
            return;
        }
        self.push_history();
        self.field.edit(&TextInput::Clear);
        self.field.edit(&TextInput::Insert(query.clone()));
        self.query = query;
        self.repeat(false);
    }
    /// Remembers the current lookup before the chain moves on, with its
    /// reply when finished. Only the newest `CACHED` visits keep theirs.
    fn push_history(&mut self) {
        let snapshot = (self.asked && self.complete && !self.pending).then(|| {
            Box::new(Snapshot {
                revision: self.revision,
                matches: self.matches.clone(),
                state: self.state.clone(),
                active_set: self.active_set.clone(),
                set_known: self.set_known,
                catalog: self.catalog.clone(),
                sources: self.sources.clone(),
                warnings: self.warnings.clone(),
                sets: self.sets.clone(),
            })
        });
        let tab = self.tabs.current();
        let mut pages = std::mem::take(&mut self.tab_pages);
        pages[tab.index()] = self.restore_page.take().unwrap_or(self.pager.page);
        self.history.push(Visit {
            query: self.query.clone(),
            tab,
            pages,
            selected: self.selected.clone(),
            snapshot,
            online: self.tabs.take_replies(),
        });
        self.folded = false;
        if self.history.len() > HISTORY {
            self.history.remove(0);
        }
        let mut cached = self.history.iter().filter(|v| v.snapshot.is_some()).count();
        let mut online = self.history.iter().filter(|v| !v.online.is_empty()).count();
        for visit in &mut self.history {
            if cached > CACHED && visit.snapshot.take().is_some() {
                cached -= 1;
            }
            if online > CACHED && !visit.online.is_empty() {
                visit.online = Replies::default();
                online -= 1;
            }
        }
    }
    /// Returns to the previous word of the chain: at once from its kept
    /// reply while the dictionaries are unchanged, otherwise by asking again
    /// for the same dictionary and page.
    fn go_back(&mut self) {
        let Some(visit) = self.history.pop() else { return };
        // A reply still coming is for the word being left.
        self.forget_dictionary();
        self.tabs.restore(visit.tab, visit.online);
        self.folded = false;
        self.word_press = None;
        self.picker = None;
        self.chooser = None;
        self.field.edit(&TextInput::Clear);
        self.field.edit(&TextInput::Insert(one_line(&visit.query)));
        self.query = visit.query;
        self.tab_pages = visit.pages;
        self.restore_page = Some(visit.pages[visit.tab.index()]);
        self.reset_layout();
        self.stop_typing();
        match visit.snapshot.filter(|s| s.revision == self.worker.revision()) {
            Some(s) => {
                let s = *s;
                self.revision = s.revision;
                self.matches = s.matches;
                self.state = s.state;
                self.active_set = s.active_set;
                self.set_known = s.set_known;
                self.catalog = s.catalog;
                self.sources = s.sources;
                self.warnings = s.warnings;
                self.sets = s.sets;
                self.selected = visit.selected;
                self.asked = true;
                self.pending = false;
                self.complete = true;
                self.first_frame = false;
                self.session_live = false;
            }
            // Asked again only if it shows.
            None if visit.tab != Tab::Dictionary => {}
            None => {
                self.repeat(false);
                if let Some(dictionary) = visit.selected {
                    // Opening queues behind the search, in its session.
                    self.worker.open(&dictionary);
                    self.selected = Some(dictionary);
                }
            }
        }
        self.tabs.ask(&self.query);
    }
    /// Runs the last submitted query again, leaving any unsubmitted field edit.
    fn repeat(&mut self, rescan: bool) {
        self.request = self.worker.lookup_in(self.query.clone(), rescan, self.selection.clone());
        self.session_live = true;
        self.asked = true;
        self.pending = true;
        self.complete = false;
        self.first_frame = true;
        self.matches.clear();
        self.selected = None;
        self.preparing = None;
        self.revision = self.worker.revision();
        if self.on_dictionary() {
            self.reset_layout();
            self.stop_typing();
        }
    }
    /// Consumes the worker's reply for the current request. `drawing` lets the
    /// first frame of a request wait briefly for it.
    fn take_reply(&mut self, t: &Theme, drawing: bool) -> bool {
        if !self.asked {
            return false;
        }
        if self.revision != self.worker.revision() || self.returning && !self.complete {
            self.repeat(false);
        }
        self.returning = false;
        let first_frame = drawing && self.on_dictionary() && std::mem::take(&mut self.first_frame);
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
            // A final count must never replace a dictionary selected since the
            // first publication, or throw away its pagination/page. A chosen
            // dictionary the finished search no longer has (Back after a
            // rescan) gives way to the worker's choice.
            let gone = self
                .selected
                .as_ref()
                .is_some_and(|s| reply.complete && !reply.matches.iter().any(|m| &m.dictionary == s));
            self.matches = reply.matches;
            self.sets = reply.sets;
            if (self.selected.is_none() || self.selected == reply.selected || gone)
                && let Some(state) = reply.state
            {
                let changed = self.selected != reply.selected || self.state != state || self.pending;
                self.pending = !self.complete && !matches!(&state, State::Definition { .. });
                self.selected = reply.selected;
                self.state = state;
                if changed && self.on_dictionary() {
                    self.reset_layout();
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
        if self.tabs.receive() {
            self.reset_layout();
            cx.redraw();
        }
        if let Some(error) = self.pending_save_error.take() {
            log::warn!("dictionary preferences: {error}");
            cx.toast("Couldn't save dictionary preferences", None);
        }
    }
    fn choose_match(&mut self) {
        let found = self.tabs.wikipedia();
        let rows = found
            .matches()
            .iter()
            .enumerate()
            .map(|(i, hit)| Wikipedia::row(hit).value(if found.opened() == Some(i) { "Current" } else { "" }));
        self.chooser = Some((Choosing::Match, Chooser::new(rows.collect())));
    }
    /// The likely ones first: the book's, the one in use and English.
    fn choose_edition(&mut self) {
        let languages = self.tabs.languages();
        let mut editions: Vec<Edition> = Vec::new();
        for edition in languages.book.into_iter().chain([languages.wikipedia, Edition::ENGLISH]).chain(Edition::all()) {
            if !editions.contains(&edition) {
                editions.push(edition);
            }
        }
        let rows = editions.iter().map(|&edition| {
            Row::new(edition.name()).value(if edition == languages.wikipedia {
                "Chosen"
            } else if languages.book == Some(edition) {
                "This book"
            } else {
                ""
            })
        });
        let chooser = Chooser::new(rows.collect());
        self.chooser = Some((Choosing::Edition(editions), chooser));
    }
    fn choose_source(&mut self) {
        let languages = self.tabs.languages();
        let sources = translate::sources(languages, self.tabs.translate().detected());
        let rows = sources.iter().map(|&source| {
            Row::new(source.map_or("Detect automatically", language_name)).value(if source == languages.source {
                "Chosen"
            } else if source.is_some() && source == languages.written {
                "This book"
            } else {
                ""
            })
        });
        let chooser = Chooser::new(rows.collect());
        self.chooser = Some((Choosing::Source(sources), chooser));
    }
    fn choose_target(&mut self) {
        let languages = self.tabs.languages();
        let chooser = Chooser::new(target_rows(languages));
        self.chooser = Some((Choosing::Target(translate::targets(languages)), chooser));
    }
    fn choose_provider(&mut self) {
        let chooser = Chooser::new(translate::provider_rows(self.tabs.languages()));
        self.chooser = Some((Choosing::Provider, chooser));
    }
    fn chosen(&mut self, choosing: Choosing, row: usize) -> Option<LookupEvent> {
        match choosing {
            Choosing::Match => {
                self.open_match(row);
                None
            }
            Choosing::Edition(editions) => {
                let edition = editions[row];
                if edition == self.tabs.languages().wikipedia {
                    return None;
                }
                self.tabs.set_edition(edition);
                // The chain's earlier articles are in the other language.
                for visit in &mut self.history {
                    visit.online.forget_wikipedia();
                    visit.pages[Tab::Wikipedia.index()] = 0;
                }
                self.show_new_reply();
                Some(LookupEvent::Edition(edition))
            }
            Choosing::Source(sources) => {
                if sources[row] != self.tabs.languages().source {
                    self.tabs.set_source(sources[row]);
                    self.forget_translations();
                }
                None
            }
            Choosing::Target(targets) => {
                if self.tabs.languages().target == Some(targets[row]) {
                    return None;
                }
                self.tabs.set_target(targets[row]);
                self.forget_translations();
                Some(LookupEvent::Target(targets[row]))
            }
            Choosing::Provider => {
                let provider = Provider::ALL[row];
                if self.tabs.languages().provider == provider {
                    return None;
                }
                self.tabs.set_provider(provider);
                self.forget_translations();
                Some(LookupEvent::Provider(provider))
            }
        }
    }
    /// The chain's earlier translations are between other languages.
    fn forget_translations(&mut self) {
        for visit in &mut self.history {
            visit.online.forget_translation();
            visit.pages[Tab::Translate.index()] = 0;
        }
        self.show_new_reply();
    }
    fn open_match(&mut self, index: usize) {
        if self.tabs.open_match(index) {
            self.show_new_reply();
        }
    }
    /// The online tab has something else to show, which may have to be asked
    /// for first.
    fn show_new_reply(&mut self) {
        self.restore_page = None;
        self.reset_layout();
        self.tabs.ask(&self.query);
    }
    fn choose(&mut self, index: usize) {
        let Some(matched) = self.matches.get(index) else { return };
        if self.selected.as_ref() == Some(&matched.dictionary) {
            return;
        }
        let dictionary = matched.dictionary.clone();
        if !self.session_live {
            // Back showed a kept reply; the worker's session is another word.
            self.request = self.worker.lookup_in(self.query.clone(), false, self.selection.clone());
            self.session_live = true;
            self.complete = false;
            self.revision = self.worker.revision();
        }
        self.worker.open(&dictionary);
        self.selected = Some(dictionary);
        self.restore_page = None;
        self.pending = true;
        self.reset_layout();
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
            TextInput::Dismiss => self.stop_typing(),
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
        let was_outside = !self.keyboard.visible && !self.sheet.rect().contains(g.pos());
        self.place(cx.theme);
        let event = self.gesture(g, cx);
        // Closing the picker or the keyboard, or expanding, moves the rows.
        self.place(cx.theme);
        if event.is_some() || !(was_outside && matches!(g, Gesture::Down(_) | Gesture::Move(_) | Gesture::LongPress(_)))
        {
            cx.redraw();
        }
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
        if self.back.on_gesture(g) {
            self.go_back();
            return None;
        }
        if self.expand.on_gesture(g) {
            if self.on_wikipedia() {
                // Only for now: its size isn't the card's.
                self.folded = !self.folded;
                return None;
            }
            self.expanded = !self.expanded;
            return Some(LookupEvent::Resized(self.expanded));
        }
        if let Some(tab) = self.tabs.on_gesture(g) {
            self.show_tab(tab);
            return None;
        }
        if let Some((_, chooser)) = &mut self.chooser {
            let chosen = chooser.on_gesture(g)?;
            let (choosing, _) = self.chooser.take()?;
            return match chosen {
                Chosen::Back => None,
                Chosen::Row(row) => self.chosen(choosing, row),
            };
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
        if !too_long(&self.query) && tapped(&mut self.heading_press, self.heading) {
            self.start_editing();
            return None;
        }
        if self.on_dictionary() && self.switcher.on_gesture(g) {
            self.open_picker(t);
            return None;
        }
        if self.other_matches.on_gesture(g) {
            self.choose_match();
            return None;
        }
        if self.edition.on_gesture(g) {
            self.choose_edition();
            return None;
        }
        if self.source.on_gesture(g) {
            self.choose_source();
            return None;
        }
        if self.target.on_gesture(g) || self.another_language.on_gesture(g) {
            self.choose_target();
            return None;
        }
        if self.service.on_gesture(g) {
            self.choose_provider();
            return None;
        }
        if self.retry.on_gesture(g) {
            self.tabs.retry();
            self.show_new_reply();
            return None;
        }
        if self.manage.on_gesture(g) {
            return Some(self.manage_event());
        }
        if self.edit_row() && self.field.on_gesture(g) {
            self.start_editing();
            return None;
        }
        if self.search.on_gesture(g) {
            self.submit(false);
            return None;
        }
        if self.on_dictionary() {
            if self.previous.on_gesture(g) {
                self.cycle(-1);
            }
            if self.next.on_gesture(g) {
                self.cycle(1);
            }
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
        if self.on_dictionary() {
            self.word_gesture(g, t);
        }
        if self.paged {
            self.pager.on_gesture(g);
        }
        // Swipes turn pages even when the pager row had no room.
        if let Gesture::Swipe { dir, .. } = g {
            self.pager.on_swipe(*dir);
        }
        if let Some(list) = &mut self.list {
            list.page = self.pager.page;
            if let Some(ListEvent::Tapped(index)) = list.on_gesture(g) {
                if self.tabs.needs_target() {
                    return self.chosen(Choosing::Target(translate::targets(self.tabs.languages())), index);
                }
                self.open_match(index);
            }
        }
        None
    }
    /// Touches on the definition: a tap, or a long press released in
    /// place, looks up the word under the finger, which shows inverted while
    /// it is down. Moving away or swiping (which turns pages) lets go.
    fn word_gesture(&mut self, g: &Gesture, t: &Theme) {
        match *g {
            Gesture::Down(p) => {
                let touchable = self.body.contains(p) && self.pages.is_some() && self.layout_error.is_none();
                self.word_press = touchable.then_some(WordPress { at: p, hit: None, long: false, released: false });
            }
            Gesture::LongPress(_) => {
                if let Some(press) = &mut self.word_press {
                    press.long = true;
                }
            }
            // Before a long press, Move means the finger left the tap slop.
            Gesture::Move(p) => {
                if self.word_press.as_ref().is_some_and(|w| !w.long || w.at.distance(p) > t.mm(3.0) as f32) {
                    self.word_press = None;
                }
            }
            Gesture::Tap(_) | Gesture::End(_) => {
                let Some(mut press) = self.word_press.take() else { return };
                if matches!(g, Gesture::End(_)) && !press.long {
                    return;
                }
                match press.hit {
                    Some(Some(hit)) => self.look_up_word(hit.query),
                    Some(None) => {}
                    None => {
                        press.released = true;
                        self.word_press = Some(press);
                    }
                }
            }
            Gesture::Swipe { .. } | Gesture::Cancel(_) => self.word_press = None,
        }
    }
    /// Finds the pressed word on the page drawn last, and looks it up if the
    /// finger already lifted.
    fn resolve_word(&mut self, cx: &mut DrawCx) {
        let Some(press) = &mut self.word_press else { return };
        if press.hit.is_none() {
            press.hit = Some(self.pages.as_mut().and_then(|pages| {
                cx.text.hit_test_styled_page(pages, self.pager.page, self.body, press.at, cx.theme.mm(2.0))
            }));
        }
        if press.released {
            let hit = press.hit.take().flatten();
            self.word_press = None;
            if let Some(hit) = hit {
                self.look_up_word(hit.query);
            }
        }
    }
    /// The query (or "Look up word" while typing), then ⤢ and ×. Deeper in
    /// a chain: ← first, and the previous word beside the query.
    fn draw_header(&mut self, cx: &mut DrawCx) {
        let t = cx.theme;
        let r = self.heading;
        if self.keyboard.visible {
            let title = match self.tabs.current() {
                Tab::Dictionary => "Look up word",
                Tab::Wikipedia => "Search Wikipedia",
                Tab::Translate => "Translate text",
            };
            cx.line(title, t.heading, r, HAlign::Left);
        } else {
            // As much as may show.
            let start = |query: &str| one_line(&query.chars().take(QUERY_LIMIT).collect::<String>());
            let words = start(&self.query);
            let label = if words.is_empty() { "Look up a word" } else { words.as_str() };
            let style = t.dictionary_headword;
            let gap = t.mm(1.5);
            // Beside the query: ✎ at the book's word, "← previous" deeper, or
            // the length of a passage too long to edit.
            let crumb = match self.history.last() {
                Some(v) => Some(format!("← {}", start(&v.query))),
                // Characters, as Translate counts: not every script has spaces.
                None => too_long(&self.query).then(|| format!("{} characters", self.query.trim().chars().count())),
            };
            let crumb_style = t.small;
            let after = match &crumb {
                // A length has to show whole, even on a side panel.
                Some(crumb) => {
                    let most = if self.history.is_empty() { r.w / 2 } else { r.w / 3 };
                    (cx.text.width(crumb, crumb_style).ceil() as i32 + 2).min(most)
                }
                None if !self.picking() => t.icon_small,
                None => 0,
            };
            let w = (cx.text.width(label, style).ceil() as i32).min((r.w - gap - after).max(0));
            let pressed = self.heading_press.is_down();
            let (fg, quiet) = if pressed { (PAPER, PAPER) } else { (INK, INK2) };
            if pressed {
                let pill = Rect::new(r.x - t.mm(1.0), r.y + t.mm(0.6), w + gap + after + t.mm(2.0), r.h - t.mm(1.2));
                cx.canvas.fill_rounded_rect(pill, t.radius, INK);
                cx.hint(pill, RefreshMode::Fast);
            }
            cx.line(label, style.gray(fg), Rect::new(r.x, r.y, w + 2, r.h), HAlign::Left);
            let beside = Rect::new(r.x + w + gap, r.y, after, r.h);
            match &crumb {
                Some(crumb) => cx.line(crumb, crumb_style.gray(quiet), beside, HAlign::Left),
                None if !self.picking() => cx.icon(Icon::Pencil, beside, t.icon_small, quiet),
                None => {}
            }
        }
        if self.back.rect().w > 0 {
            if !self.back.is_pressed() {
                cx.canvas.stroke_rounded_rect(self.back.rect(), t.radius, t.border as f32, LINE);
            }
            self.back.draw(cx);
        }
        if self.expand.rect().w > 0 {
            self.expand.draw(cx);
        }
        self.close.draw(cx);
    }
    fn draw_switcher(&mut self, cx: &mut DrawCx, status: &str) {
        let index = self.matches.iter().position(|m| Some(&m.dictionary) == self.selected.as_ref());
        self.previous.enabled = index.is_some_and(|i| i > 0);
        self.next.enabled = index.is_some_and(|i| i + 1 < self.matches.len());
        self.previous.draw(cx);
        self.next.draw(cx);
        let name = self
            .catalog
            .iter()
            .find(|d| Some(&d.id) == self.selected.as_ref())
            .map_or_else(|| "Dictionaries".into(), |d| self.title(d));
        self.switcher.draw(cx, &name, status);
    }
    fn draw_wikipedia_row(&self, cx: &mut DrawCx) {
        let found = self.tabs.wikipedia();
        self.other_matches.draw(cx, "Other matches", &found.matches().len().saturating_sub(1).to_string());
        if let Some(hits) = found.listing() {
            let style = cx.theme.small.gray(INK2);
            let count = format!("{} {}", hits.len(), if hits.len() == 1 { "result" } else { "results" });
            let full = format!("No exact match · {count}");
            let fits = cx.text.width(&full, style) <= self.hit_count.w as f32;
            cx.line(if fits { &full } else { &count }, style, self.hit_count, HAlign::Left);
        }
        self.edition.draw(cx, self.tabs.languages().wikipedia.name(), "");
    }
    fn draw_translate_row(&self, cx: &mut DrawCx) {
        let (t, row) = (cx.theme, self.translate_row);
        let languages = self.tabs.languages();
        let Some(target) = languages.target else {
            return cx.line("Translate into which language?", t.button, row, HAlign::Left);
        };
        let (from, into) = (self.source.rect(), self.target.rect());
        let mut source = self.tabs.translate().source_label(languages);
        // Beside the service button "Auto (Portuguese)" may not fit: the
        // detected language is in the source list too.
        let room = from.w - 2 * t.mm(2.2) - t.icon_small - t.mm(1.0);
        if languages.source.is_none() && cx.text.width(&source, t.button) > room as f32 {
            source = "Auto".into();
        }
        self.source.draw(cx, &source, "");
        cx.line("→", t.body, Rect::new(from.right(), row.y, into.x - from.right(), row.h), HAlign::Center);
        self.target.draw(cx, language_name(target), "");
        self.service.draw(cx, languages.provider.short_name(), "");
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
        if self.pages.is_some() || self.layout_error.is_some() || self.list.is_some() {
            return;
        }
        let started = std::time::Instant::now();
        // After Back, lay out as far as the page that was showing.
        let waiting = if self.on_dictionary() { self.pending } else { self.tabs.waiting() };
        let target = if waiting { 0 } else { self.restore_page.take().unwrap_or(0) };
        let (mut result, mut bytes);
        loop {
            let blocks = if self.on_dictionary() {
                content(&self.state, self.pending, too_long(&self.query), &self.warnings, t)
            } else {
                self.tabs.content(t)
            };
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
                cx.text.paginate_styled_until(&mut pages, target + 1)?;
                Ok((pages, paged))
            });
            // Long status text in a compact card with Manage and the query
            // field: grow once to the expanded card rather than squeezing it.
            if matches!(&result, Ok((_, true))) && !roomy && !self.landscape() && !self.tall() && !self.grew {
                self.grew = true;
                // As ⤢ does: Wikipedia's size isn't the card's.
                if self.on_wikipedia() {
                    self.folded = false;
                } else {
                    self.expanded = true;
                }
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
                let count = pages.pages().len();
                self.pager.set(target.min(count.saturating_sub(1)), count);
                self.pager.incomplete = !pages.is_complete();
                self.text_height = pages.pages().first().map(|p| p.height.ceil() as i32);
                self.pages = Some(pages);
            }
            Err(e) => {
                self.layout_error = Some(format!("Couldn't lay out this text: {e}"));
                self.paged = false;
                self.pager.set(0, 1);
                self.pager.incomplete = false;
            }
        }
        self.place(t);
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
        // On the page the finger touched, before a reply replaces it.
        self.resolve_word(cx);
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
        if !self.keyboard.visible {
            self.tabs.draw(cx);
        }
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
        if let Some((_, chooser)) = &mut self.chooser {
            rule(cx, self.divider, self.body_full);
            chooser.draw(cx);
            return;
        }
        if !self.keyboard.visible && self.on_dictionary() {
            self.draw_switcher(cx, &status);
        }
        if !self.keyboard.visible && self.on_wikipedia() {
            self.draw_wikipedia_row(cx);
        }
        if !self.keyboard.visible && self.tabs.current() == Tab::Translate {
            self.draw_translate_row(cx);
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
        if self.manage_shown() {
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
        if let Some(WordPress { hit: Some(Some(hit)), .. }) = &self.word_press {
            for r in &hit.rects {
                let r = r.inset_xy(-t.mm(0.5), 0);
                cx.canvas.invert_rect(r);
                cx.hint(r, RefreshMode::Fast);
            }
        }
        if let Some(list) = self.list.as_ref().filter(|l| l.fits()) {
            list.draw(cx);
        }
        if self.retry_shown() {
            self.retry.draw(cx);
        }
        if self.another_language_shown() {
            self.another_language.draw(cx);
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

fn target_rows(languages: &Languages) -> Vec<Row> {
    let row = |code: &'static str| {
        Row::new(language_name(code)).value(if languages.target == Some(code) {
            "Chosen"
        } else if languages.written == Some(code) {
            "This book"
        } else {
            ""
        })
    };
    translate::targets(languages).into_iter().map(row).collect()
}

/// The card on each tab, at both edges, with the pager where it has one.
#[cfg(test)]
pub(super) fn fit_samples(t: &Theme, screen: Rect) -> Vec<crate::ui::fit::Parts> {
    use crate::{platform::Waker, store::Store, ui::fit::Parts};
    let root = std::env::temp_dir().join(format!("verso-lookup-fit-{}", std::process::id()));
    let worker = Rc::new(Worker::new(Store::at(root), Waker::new(|| {})));
    let word = |y| Rect::new(screen.w / 4, y, t.mm(15.0), t.mm(4.0));
    let line = t.dictionary_body.line_height().ceil() as i32;
    let mut samples = Vec::new();
    for word in [word(screen.h / 8), word(screen.h * 7 / 8)] {
        for (tab, typing) in [
            (Tab::Dictionary, false),
            (Tab::Wikipedia, false),
            (Tab::Translate, false),
            (Tab::Dictionary, true),
            (Tab::Wikipedia, true),
        ] {
            let tabs = Tabs::offline(tab);
            let mut l = Lookup::new("word", worker.clone(), t, screen, Some(word), Default::default(), tabs);
            worker.cancel();
            // A definition of several pages.
            l.pending = false;
            l.state = State::Definition { entries: Vec::new(), base_forms: None };
            l.keyboard.visible = typing;
            l.paged = true;
            l.place(t);
            let mut buttons = vec![];
            if typing {
                buttons.push(l.search.rect());
            } else {
                buttons.extend([l.tabs.rect(), l.pager.rect()]);
                buttons.extend(l.actions.iter().map(Button::rect));
                if tab == Tab::Dictionary {
                    buttons.extend([l.previous.rect(), l.switcher.rect(), l.next.rect()]);
                }
                if tab == Tab::Wikipedia {
                    // Other matches has the place of their count.
                    buttons.extend([l.hit_count, l.edition.rect()]);
                }
            }
            let body = if typing { l.body_full } else { l.body };
            let mut parts = Parts::new("Lookup", l.sheet.rect(), buttons).body(l.heading, l.heading.h);
            parts = parts.body(body, 3 * line).dense(t.mm(7.0));
            if typing {
                parts = parts.keyboard(l.keyboard.rect());
            }
            samples.push(parts);
            if tab == Tab::Translate {
                // First use: at least two languages a page.
                let list = l.list.as_ref().unwrap();
                let rows = (0..2).map(|i| list.row_rect(i));
                let buttons = rows.chain([l.pager.rect()]).chain(l.actions.iter().map(Button::rect));
                samples.push(Parts::new("Lookup first language", l.sheet.rect(), buttons).dense(t.mm(7.0)));
                // The same language on both sides: its message and the way out.
                let languages = Languages { source: Some("en"), ..Languages::for_test(None, Some("en")) };
                l.tabs.set_languages(languages);
                l.show_new_reply();
                l.paged = false;
                l.place(t);
                let pickers = [l.source.rect(), l.target.rect(), l.another_language.rect()];
                let buttons = pickers.into_iter().chain(l.actions.iter().map(Button::rect));
                samples.push(
                    Parts::new("Lookup same language", l.sheet.rect(), buttons).body(l.body, 3 * line).dense(t.mm(7.0)),
                );
            }
            if tab != Tab::Wikipedia || typing {
                continue;
            }
            // Offline: the message, Retry and no pager.
            while l.tabs.waiting() {
                l.tabs.receive();
                std::thread::yield_now();
            }
            l.paged = false;
            l.place(t);
            let buttons = [l.edition.rect(), l.retry.rect()].into_iter().chain(l.actions.iter().map(Button::rect));
            samples.push(Parts::new("Lookup error", l.sheet.rect(), buttons).body(l.body, 3 * line).dense(t.mm(7.0)));
            l.choose_edition();
            l.place(t);
            let (_, chooser) = l.chooser.as_ref().unwrap();
            let buttons = chooser.targets().into_iter().chain([l.tabs.rect()]);
            samples.push(Parts::new("Lookup languages", l.sheet.rect(), buttons).dense(t.mm(7.0)));
        }
    }
    samples
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
            let mut lookup = Lookup::new(
                "leaves",
                worker.clone(),
                &t,
                screen,
                None,
                Default::default(),
                Tabs::offline(Tab::Dictionary),
            );
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
            let input = content(&lookup.state, false, false, &lookup.warnings, &t);
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
        let mut lookup =
            Lookup::new("Hello", worker.clone(), &t, screen, None, Default::default(), Tabs::offline(Tab::Dictionary));
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
            let StyledBlock::Paragraph(p) = content(state, false, false, &[], &t).remove(0) else {
                panic!("first paragraph")
            };
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
