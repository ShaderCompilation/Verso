//! Lookup is an overlay over the unchanged cached book page.
use crate::{
    dictionary::{
        Block, BlockKind, Dictionary, Source, Style,
        sets::Set,
        worker::{Match, State, Worker},
    },
    geom::Rect,
    gfx::{
        HAlign, SpanStyle, StyledBlock, StyledPagination, StyledParagraph, StyledSpan, TextRenderer, TextStyle, Weight,
    },
    platform::TextInput,
    screens::EventCx,
    ui::{
        gesture::Gesture,
        theme::{INK2, LINE, Theme},
        widget::DrawCx,
        widgets::{Button, ButtonKind, Keyboard, List, ListEvent, Pager, Row, Sheet, TextField},
    },
};
use std::rc::Rc;

const MISSING: &str = "No dictionaries found. Add a StarDict dictionary to Verso's dictionaries/ folder, \
                       then open Dictionaries and tap Rescan.";
const DISABLED: &str = "All dictionaries are disabled. Open Dictionaries to enable one.";
const NO_RESULT: &str = "No definition found. Tap the query to edit it, or switch dictionary sets.";
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

fn plain(text: String, base: TextStyle) -> StyledBlock {
    StyledBlock::Paragraph(StyledParagraph { text, spans: Vec::new(), base, muted: INK2, indent: 0.0 })
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
    })
}

/// All visible content shares one pagination, including provenance and errors.
fn content(state: &State, pending: bool, warnings: &[String], t: &Theme) -> Vec<StyledBlock> {
    if pending {
        return vec![plain("Looking up…".into(), t.body)];
    }
    let gap = t.dictionary_body.line_height() / 2.0;
    let rule = || StyledBlock::Rule { space: gap, thickness: t.hairline, gray: LINE };
    let mut blocks = Vec::new();
    match state {
        State::Definition { entries, base_forms } => {
            if let Some(forms) = base_forms {
                let plural = if forms.count > 1 { "s" } else { "" };
                blocks.push(plain(format!("Base form{plural} of “{}”", forms.query), t.small.gray(INK2)));
                blocks.push(StyledBlock::Gap(gap));
            }
            for (i, entry) in entries.iter().enumerate() {
                if i > 0 {
                    blocks.push(rule());
                }
                blocks.push(plain(entry.headword.clone(), t.dictionary_headword));
                blocks.extend(entry.blocks.iter().map(|block| match block.kind {
                    BlockKind::Gap => StyledBlock::Gap(gap),
                    BlockKind::Rule => rule(),
                    BlockKind::Text | BlockKind::Heading => styled(block, t),
                }));
            }
        }
        State::Missing => blocks.push(plain(MISSING.into(), t.body)),
        State::Disabled => blocks.push(plain(DISABLED.into(), t.body)),
        State::NoResult => blocks.push(plain(NO_RESULT.into(), t.body)),
        State::Error(_) => blocks.push(plain(
            "Couldn’t read this entry. Open Manage dictionaries for details, or choose another result.".into(),
            t.body,
        )),
    }
    if !warnings.is_empty() && !matches!(state, State::Definition { .. }) {
        blocks.push(StyledBlock::Gap(gap));
        blocks.push(plain(
            "Search incomplete: some dictionaries couldn’t be read. Open Manage dictionaries for details.".into(),
            t.body,
        ));
    }
    blocks
}

pub enum LookupEvent {
    Close,
    /// Passage actions, retaining the independently edited lookup query.
    Actions(String),
    /// Open dictionary management at this lookup's set.
    Manage(String),
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
    sheet: Sheet,
    worker: Rc<Worker>,
    request: u64,
    selection: crate::dictionary::sets::Selection,
    active_set: String,
    /// The last submitted query. The field may have been edited since.
    query: String,
    pending: bool,
    complete: bool,
    preparing: Option<String>,
    matches: Vec<Match>,
    sets: Vec<Set>,
    revision: u64,
    picker: Option<Picker>,
    counter: Rect,
    field: TextField,
    keyboard: Keyboard,
    search: Button,
    previous: Button,
    dictionaries_button: Button,
    next: Button,
    find: Button,
    manage: Button,
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
    body: Rect,
}
impl Lookup {
    /// `word` is where the looked-up text is on the page. In landscape a
    /// bottom sheet would cover most of the page, so the dictionary stands at
    /// the side away from the word instead.
    pub fn new(
        query: &str,
        worker: Rc<Worker>,
        t: &Theme,
        screen: Rect,
        word: Option<Rect>,
        selection: crate::dictionary::sets::Selection,
    ) -> Self {
        let sheet = if screen.w > screen.h {
            let right = word.is_none_or(|w| w.center().x < screen.center().x);
            Sheet::side("Dictionary", (screen.w * 11 / 20).max(t.mm(72.0)).min(screen.w), right, screen, t)
        } else {
            Sheet::open("Dictionary", screen.h * 4 / 5, screen, t)
        };
        let content = sheet.content(t);
        // A narrow side panel puts Actions on a row of its own.
        let narrow = content.w < t.mm(90.0);
        let gap = t.mm(2.0);
        let mut field = TextField::new("Word");
        let mut search = Button::new("Look up", ButtonKind::Primary);
        let search_w = t.mm(22.0);
        field.place(Rect::new(content.x, content.y, content.w - search_w - gap, t.target));
        field.edit(&TextInput::Insert(query.into()));
        search.place(Rect::new(content.right() - search_w, content.y, search_w, t.target), t);
        let row = Rect::new(content.x, content.y + t.target + gap, content.w, t.target);
        let arrow = t.mm(16.0);
        let find_w = t.mm(30.0);
        let mut previous = Button::new("‹", ButtonKind::Secondary);
        let mut dictionaries_button = Button::new("Dictionaries", ButtonKind::Secondary);
        let mut next = Button::new("›", ButtonKind::Secondary);
        let mut find = Button::new("Actions", ButtonKind::Secondary);
        let row = if narrow {
            find.place(Rect { y: row.bottom() + gap, ..row }, t);
            row
        } else {
            find.place(Rect::new(row.right() - find_w, row.y, find_w, row.h), t);
            Rect { w: row.w - find_w - gap, ..row }
        };
        previous.place(Rect::new(row.x, row.y, arrow, row.h), t);
        next.place(Rect::new(row.right() - arrow, row.y, arrow, row.h), t);
        dictionaries_button.place(Rect::new(row.x + arrow + gap, row.y, row.w - 2 * (arrow + gap), row.h), t);
        let footer = Rect::new(content.x, sheet.rect().bottom() - t.target - gap, content.w, t.target);
        let mut pager = Pager::new(1);
        pager.place(footer, t);
        let mut manage = Button::new("Manage dictionaries", ButtonKind::Secondary);
        manage.place(Rect::new(content.x, footer.y - t.target - gap, content.w, t.target), t);
        let controls = find.rect().bottom().max(row.bottom());
        let counter = Rect::new(content.x, controls + gap, content.w, t.mm(7.0));
        let body = Rect::new(content.x, counter.bottom(), content.w, manage.rect().y - counter.bottom() - gap);
        let request = worker.lookup_in(query.into(), false, selection.clone());
        let mut keyboard = Keyboard::default();
        keyboard.place(screen, t);
        let revision = worker.revision();
        Self {
            sheet,
            worker,
            request,
            selection,
            active_set: crate::dictionary::sets::DEFAULT.into(),
            query: query.into(),
            pending: true,
            complete: false,
            preparing: None,
            matches: vec![],
            sets: vec![],
            revision,
            picker: None,
            counter,
            field,
            keyboard,
            search,
            previous,
            dictionaries_button,
            next,
            find,
            manage,
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
            pager,
            body,
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
    fn status(&self) -> String {
        let count = self.matches.iter().position(|m| Some(&m.dictionary) == self.selected.as_ref()).map_or_else(
            || format!("{} found", self.matches.len()),
            |i| format!("{} of {}", i + 1, self.matches.len()),
        );
        if !self.complete && self.preparing.is_some() {
            format!("{count} · Preparing dictionaries…")
        } else if !self.complete {
            format!("{count} · Searching…")
        } else {
            count
        }
    }
    fn open_picker(&mut self, t: &Theme) {
        let c = self.sheet.content(t);
        let gap = t.mm(2.0);
        let footer = Rect::new(c.x, self.sheet.rect().bottom() - t.target - gap, c.w, t.target);
        let mut back = Button::new("Back", ButtonKind::Secondary);
        let mut manage = Button::new("Manage dictionaries", ButtonKind::Secondary);
        let back_w = t.mm(17.0);
        back.place(Rect::new(c.x, footer.y, back_w, footer.h), t);
        manage.place(Rect::new(c.x + back_w + gap, footer.y, c.w - back_w - gap, footer.h), t);
        let mut pager = Pager::new(1);
        pager.place(Rect::new(c.x, footer.y - t.target - gap, c.w, t.target), t);
        let status = Rect::new(c.x, pager.rect().y - t.mm(8.0), c.w, t.mm(8.0));
        let mut set = Button::new("", ButtonKind::Secondary);
        set.place(Rect::new(c.x, c.y, c.w, t.target), t);
        let mut list = List::new(vec![]);
        list.place(Rect::new(c.x, c.y + t.target + gap, c.w, status.y - c.y - t.target - gap), t.row2, 0);
        self.picker = Some(Picker { sets: false, list, pager, set, back, manage, status });
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
        cx.redraw();
        true
    }
    /// All gestures are consumed by the reader while this overlay is open.
    pub fn on_gesture(&mut self, g: &Gesture, cx: &mut EventCx) -> Option<LookupEvent> {
        if self.keyboard.visible {
            if let Some(input) = self.keyboard.on_gesture(g, cx.theme) {
                self.on_text_input(&input, cx);
            }
            return None;
        }
        if self.sheet.on_gesture(g) {
            return Some(LookupEvent::Close);
        }
        if let Some(p) = &mut self.picker {
            if p.manage.on_gesture(g) {
                self.picker = None;
                self.returning = true;
                if !self.complete {
                    self.worker.cancel();
                }
                return Some(LookupEvent::Manage(self.active_set.clone()));
            }
            if p.back.on_gesture(g) {
                if p.sets {
                    p.sets = false;
                    p.list.page = 0;
                    self.refresh_picker(cx.theme);
                } else {
                    self.picker = None;
                }
                return None;
            }
            if p.set.on_gesture(g) {
                p.sets = !p.sets;
                p.list.page = 0;
                self.refresh_picker(cx.theme);
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
        if self.dictionaries_button.on_gesture(g) {
            self.open_picker(cx.theme);
            return None;
        }
        if (self.pending || !matches!(self.state, State::Definition { .. })) && self.manage.on_gesture(g) {
            self.returning = true;
            if !self.complete {
                self.worker.cancel();
            }
            return Some(LookupEvent::Manage(self.active_set.clone()));
        }
        if self.field.on_gesture(g) {
            self.field.focused = true;
            self.keyboard.visible = true;
        }
        if self.search.on_gesture(g) {
            self.submit(false);
        }
        if self.previous.on_gesture(g) {
            self.cycle(-1);
        }
        if self.next.on_gesture(g) {
            self.cycle(1);
        }
        if self.find.on_gesture(g) {
            return Some(LookupEvent::Actions(self.field.value().to_owned()));
        }
        self.pager.on_gesture(g);
        if let Gesture::Swipe { dir, .. } = g {
            self.pager.on_swipe(*dir);
        }
        None
    }
    pub fn draw(&mut self, cx: &mut DrawCx) {
        self.sheet.draw(cx);
        // Drawing the cached book page takes long enough that a warm lookup
        // may finish while the sheet chrome is drawn. Consume it before
        // laying out content so the first update can contain the definition.
        self.take_reply(cx.theme, true);
        let t = cx.theme;
        let status = self.status();
        log::debug!("dictionary status draw: {status}, set {}", self.active_set);
        if let Some(p) = &mut self.picker {
            p.set.draw(cx);
            p.list.draw(cx);
            p.pager.draw(cx);
            p.back.draw(cx);
            p.manage.draw(cx);
            cx.line(&status, t.small.gray(INK2), p.status, HAlign::Left);
            return;
        }
        self.field.draw(cx);
        self.search.draw(cx);
        let index = self.matches.iter().position(|m| Some(&m.dictionary) == self.selected.as_ref());
        self.previous.enabled = index.is_some_and(|i| i > 0);
        self.next.enabled = index.is_some_and(|i| i + 1 < self.matches.len());
        cx.line(&status, t.small.gray(INK2), self.counter, HAlign::Left);
        self.dictionaries_button.label = self
            .catalog
            .iter()
            .find(|d| Some(&d.id) == self.selected.as_ref())
            .map_or_else(|| "Dictionaries".into(), |d| self.title(d));
        self.previous.draw(cx);
        self.dictionaries_button.draw(cx);
        self.next.draw(cx);
        self.find.draw(cx);
        let definition = !self.pending && matches!(self.state, State::Definition { .. });
        let bottom = if definition { self.pager.rect().y } else { self.manage.rect().y };
        self.body.h = bottom - self.body.y - t.mm(2.0);
        if !definition {
            self.manage.draw(cx);
        }
        if self.pages.is_none() && self.layout_error.is_none() {
            let started = std::time::Instant::now();
            let blocks = content(&self.state, self.pending, &self.warnings, t);
            let bytes: usize =
                blocks.iter().map(|b| if let StyledBlock::Paragraph(p) = b { p.text.len() } else { 0 }).sum();
            let result = StyledPagination::new(blocks, self.body.w, self.body.h).and_then(|mut pages| {
                cx.text.paginate_styled_until(&mut pages, 1)?;
                Ok(pages)
            });
            match result {
                Ok(pages) => {
                    self.pager.set(0, pages.pages().len());
                    self.pager.incomplete = !pages.is_complete();
                    self.pages = Some(pages);
                }
                Err(e) => {
                    self.layout_error = Some(format!("Couldn't lay out this definition: {e}"));
                    self.pager.set(0, 1);
                    self.pager.incomplete = false;
                }
            }
            self.layout_timing = (started, started.elapsed());
            log::debug!(
                "dictionary first page pagination: {:?}, {bytes} text bytes, {} pages, complete {}",
                self.layout_timing.1,
                self.pages.as_ref().map_or(0, |p| p.pages().len()),
                self.pages.as_ref().is_some_and(StyledPagination::is_complete)
            );
        }
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
        self.pager.draw(cx);
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
                frontlight: &mut None,
                light_request: &mut None,
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
            frontlight: &mut None,
            light_request: &mut None,
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
