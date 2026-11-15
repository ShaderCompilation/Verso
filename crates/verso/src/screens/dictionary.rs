//! Lookup is an overlay over the unchanged cached book page.
use crate::{
    dictionary::{
        Block, BlockKind, Dictionary, Source, Style,
        worker::{State, Worker},
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
        widgets::{Button, ButtonKind, Keyboard, Pager, Sheet, TextField},
    },
};
use std::rc::Rc;

const MISSING: &str = "No dictionaries found. Add a StarDict dictionary to Verso's dictionaries/ folder, \
                       then open Dictionaries and tap Rescan.";
const DISABLED: &str = "All dictionaries are disabled. Open Dictionaries to enable one.";
const NO_RESULT: &str = "No definition found. Tap the query to edit it, or try another dictionary.";
const UNREADABLE_HINT: &str = "Try another dictionary, or replace its files and tap Rescan.";
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
        State::Error(e) => {
            blocks.push(plain(format!("Couldn't read this dictionary.\n\n{e}\n\n{UNREADABLE_HINT}"), t.body));
        }
    }
    if !warnings.is_empty() {
        blocks.push(StyledBlock::Gap(gap));
        blocks.push(plain("Some dictionary folders had problems:".into(), t.body));
        blocks.extend(warnings.iter().map(|warning| plain(format!("• {warning}"), t.body)));
    }
    blocks
}

pub enum LookupEvent {
    Close,
    /// Passage actions, retaining the independently edited lookup query.
    Actions(String),
}

pub struct Lookup {
    sheet: Sheet,
    worker: Rc<Worker>,
    request: u64,
    pending: bool,
    field: TextField,
    keyboard: Keyboard,
    search: Button,
    previous: Button,
    dictionaries_button: Button,
    next: Button,
    find: Button,
    back: Button,
    rescan: Button,
    catalog: Vec<Dictionary>,
    sources: Vec<Source>,
    /// Source problems, shown with every result.
    warnings: Vec<String>,
    selected: Option<String>,
    state: State,
    pages: Option<StyledPagination>,
    layout_error: Option<String>,
    pager: Pager,
    manager_pager: Pager,
    manager: bool,
    rows: Vec<(usize, Rect, Button, Button)>,
    body: Rect,
}
impl Lookup {
    /// `word` is where the looked-up text is on the page. In landscape a
    /// bottom sheet would cover most of the page, so the dictionary stands at
    /// the side away from the word instead.
    pub fn new(query: &str, worker: Rc<Worker>, t: &Theme, screen: Rect, word: Option<Rect>) -> Self {
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
        let mut back = Button::new("Back", ButtonKind::Secondary);
        let mut rescan = Button::new("Rescan", ButtonKind::Secondary);
        back.place(Rect::new(content.x, content.y, content.w / 2 - gap, t.target), t);
        rescan.place(Rect::new(content.x + content.w / 2 + gap, content.y, content.w / 2 - gap, t.target), t);
        let controls = find.rect().bottom().max(row.bottom());
        let body = Rect::new(content.x, controls + gap, content.w, footer.y - controls - 2 * gap);
        let mut manager_pager = Pager::new(1);
        manager_pager.place(footer, t);
        let request = worker.lookup(query.into(), None, true);
        let mut keyboard = Keyboard::default();
        keyboard.place(screen, t);
        Self {
            sheet,
            worker,
            request,
            pending: true,
            field,
            keyboard,
            search,
            previous,
            dictionaries_button,
            next,
            find,
            back,
            rescan,
            catalog: Vec::new(),
            sources: Vec::new(),
            warnings: Vec::new(),
            selected: None,
            state: State::NoResult,
            pages: None,
            layout_error: None,
            pager,
            manager_pager,
            manager: false,
            rows: Vec::new(),
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
    fn with_warnings(&self, mut message: String) -> String {
        if !self.warnings.is_empty() {
            message.push_str("\n\nSome dictionary folders had problems:");
            for warning in &self.warnings {
                message.push_str("\n• ");
                message.push_str(warning);
            }
        }
        message
    }
    pub fn busy(&self) -> bool {
        self.pending || !self.manager && (self.pages.is_none() && self.layout_error.is_none() || self.paginating())
    }
    pub fn paginating(&self) -> bool {
        self.pages.as_ref().is_some_and(|p| !p.is_complete())
    }
    /// One bounded piece per idle turn, after the visible page was presented.
    pub fn on_idle(&mut self, text: &mut TextRenderer) -> bool {
        let Some(pages) = self.pages.as_mut().filter(|p| !p.is_complete()) else { return false };
        let had_next = self.pager.page + 1 < pages.pages().len();
        if let Err(e) = text.paginate_styled_step(pages) {
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
                "dictionary pagination complete: {} pages, {} pieces",
                pages.pages().len(),
                pages.paragraphs().len()
            );
        }
        pages.is_complete() || !had_next && self.pager.page + 1 < pages.pages().len()
    }
    fn submit(&mut self, rescan: bool) {
        self.request = self.worker.lookup(self.field.value().into(), self.selected.clone(), rescan);
        self.pending = true;
        self.pages = None;
        self.layout_error = None;
        self.pager.set(0, 1);
        self.pager.incomplete = true;
        self.field.focused = false;
        self.keyboard.visible = false;
    }
    pub fn receive(&mut self, cx: &mut EventCx) {
        if let Some(reply) = self.worker.receive()
            && reply.id == self.request
        {
            self.pending = false;
            self.catalog = reply.dictionaries;
            self.sources = reply.sources;
            self.warnings = reply.warnings;
            self.selected = reply.selected;
            self.state = reply.state.unwrap_or(State::NoResult);
            self.pages = None;
            self.layout_error = None;
            self.pager.set(0, 1);
            self.pager.incomplete = true;
            if let Some(e) = reply.save_error {
                log::warn!("dictionary preferences: {e}");
                cx.toast("Couldn't save dictionary preferences", None);
            }
            self.place_rows(cx.theme);
            cx.redraw();
        }
    }
    fn place_rows(&mut self, t: &Theme) {
        let top = self.sheet.content(t).y + t.target + t.mm(3.0);
        let height = self.manager_pager.rect().y - top;
        let row_h = t.target + t.mm(10.0);
        let per = (height / row_h).max(1) as usize;
        self.manager_pager.set(self.manager_pager.page, self.catalog.len().div_ceil(per));
        self.rows.clear();
        for (row, index) in (self.manager_pager.page * per..self.catalog.len()).take(per).enumerate() {
            let d = &self.catalog[index];
            let rect = Rect::new(self.body.x, top + row as i32 * row_h, self.body.w, row_h);
            let width = t.mm(20.0);
            let mut toggle = Button::new(if d.disabled { "Enable" } else { "Disable" }, ButtonKind::Secondary);
            toggle.enabled = !self.pending;
            toggle.place(Rect::new(rect.right() - 2 * width - t.mm(2.0), rect.y, width, t.target), t);
            let mut up = Button::new("Move up", ButtonKind::Secondary);
            up.enabled = index > 0 && !self.pending;
            up.place(Rect::new(rect.right() - width, rect.y, width, t.target), t);
            self.rows.push((index, rect, toggle, up));
        }
    }
    fn cycle(&mut self, delta: isize) {
        let enabled: Vec<_> = self.catalog.iter().filter(|d| !d.disabled).map(|d| d.id.clone()).collect();
        if enabled.len() < 2 {
            return;
        }
        let current = enabled.iter().position(|id| Some(id) == self.selected.as_ref()).unwrap_or(0);
        let index = (current as isize + delta).rem_euclid(enabled.len() as isize) as usize;
        self.selected = Some(enabled[index].clone());
        self.submit(false);
    }
    pub fn on_text_input(&mut self, input: &TextInput, cx: &mut EventCx) -> bool {
        if !self.field.focused {
            return false;
        }
        match input {
            TextInput::Submit => {
                self.selected = None;
                self.submit(false);
            }
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
        if self.manager {
            if self.back.on_gesture(g) {
                self.manager = false;
            }
            if self.rescan.on_gesture(g) {
                self.submit(true);
                self.place_rows(cx.theme);
            }
            let old = self.manager_pager.page;
            self.manager_pager.on_gesture(g);
            if let Gesture::Swipe { dir, .. } = g {
                self.manager_pager.on_swipe(*dir);
            }
            if old != self.manager_pager.page {
                self.place_rows(cx.theme);
            }
            let mut change = None;
            for (i, _, toggle, up) in &mut self.rows {
                if toggle.on_gesture(g) {
                    change = Some((*i, false));
                }
                if up.on_gesture(g) {
                    change = Some((*i, true));
                }
            }
            if let Some((i, up)) = change {
                self.worker.change(self.catalog[i].id.clone(), up);
                self.selected = None;
                self.submit(false);
                self.place_rows(cx.theme);
            }
        } else {
            if self.field.on_gesture(g) {
                self.field.focused = true;
                self.keyboard.visible = true;
            }
            if self.search.on_gesture(g) {
                self.selected = None;
                self.submit(false);
            }
            if self.previous.on_gesture(g) {
                self.cycle(-1);
            }
            if self.next.on_gesture(g) {
                self.cycle(1);
            }
            if self.dictionaries_button.on_gesture(g) {
                self.manager = true;
                self.place_rows(cx.theme);
            }
            if self.find.on_gesture(g) {
                return Some(LookupEvent::Actions(self.field.value().to_owned()));
            }
            self.pager.on_gesture(g);
            if let Gesture::Swipe { dir, .. } = g {
                self.pager.on_swipe(*dir);
            }
        }
        None
    }
    pub fn draw(&mut self, cx: &mut DrawCx) {
        self.sheet.draw(cx);
        let t = cx.theme;
        if self.manager {
            self.back.draw(cx);
            self.rescan.draw(cx);
            if self.catalog.is_empty() {
                let message = self.with_warnings(
                    "No dictionaries found. Add StarDict files to Verso's dictionaries/ folder, then tap Rescan."
                        .into(),
                );
                cx.text.draw(cx.canvas, &message, t.body, self.body, HAlign::Left);
            }
            for (i, rect, toggle, up) in &self.rows {
                let d = &self.catalog[*i];
                let label = Rect::new(rect.x, rect.y, toggle.rect().x - rect.x - t.mm(2.0), t.target);
                cx.line(&d.name, t.body, label, HAlign::Left);
                let location = d.location(&self.sources);
                let detail = if d.error.is_some() { format!("Unreadable · {location}") } else { location };
                cx.line(
                    &detail,
                    t.caption.gray(INK2),
                    Rect::new(rect.x, rect.y + t.target, rect.w, t.mm(8.0)),
                    HAlign::Left,
                );
                toggle.draw(cx);
                up.draw(cx);
            }
            self.manager_pager.draw(cx);
            return;
        }
        self.field.draw(cx);
        self.search.draw(cx);
        self.previous.enabled = self.catalog.iter().filter(|d| !d.disabled).count() > 1;
        self.next.enabled = self.previous.enabled;
        self.dictionaries_button.label = self
            .catalog
            .iter()
            .find(|d| Some(&d.id) == self.selected.as_ref())
            .map_or_else(|| "Dictionaries".into(), |d| self.title(d));
        self.previous.draw(cx);
        self.dictionaries_button.draw(cx);
        self.next.draw(cx);
        self.find.draw(cx);
        if self.pages.is_none() && self.layout_error.is_none() {
            let started = std::time::Instant::now();
            let result =
                StyledPagination::new(content(&self.state, self.pending, &self.warnings, t), self.body.w, self.body.h)
                    .and_then(|mut pages| {
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
            log::debug!("dictionary first page pagination: {:?}", started.elapsed());
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
            let mut lookup = Lookup::new("leaves", worker.clone(), &t, screen, None);
            lookup.pending = false;
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
            assert!(
                recovered
                    .ends_with("leave\nTo depart.\nSome dictionary folders had problems:\n• /gone is not available.\n")
            );
            let mut cx = EventCx {
                theme: &t,
                screen,
                now: Instant::now(),
                frontlight: &mut None,
                light_request: &mut None,
                toast: &mut None,
                forced: &mut Vec::new(),
                needs_redraw: &mut false,
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
        let mut lookup = Lookup::new("Hello", worker.clone(), &t, screen, None);
        let mut cx = EventCx {
            theme: &t,
            screen,
            now: Instant::now(),
            frontlight: &mut None,
            light_request: &mut None,
            toast: &mut None,
            forced: &mut Vec::new(),
            needs_redraw: &mut false,
        };
        let wait = |lookup: &mut Lookup, cx: &mut EventCx| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while lookup.pending {
                lookup.receive(cx);
                if !lookup.pending {
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
        lookup.manager = true;
        assert!(!lookup.busy(), "the manager does not wait for hidden pages to be drawn");
        lookup.manager = false;
        // Source warnings accompany results; same-name copies show their source.
        lookup.warnings = vec!["/gone is not available.".into()];
        assert!(lookup.with_warnings("text".into()).ends_with("problems:\n• /gone is not available."));
        lookup.warnings.clear();
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
        worker.change("b.ifo".into(), true);
        lookup.submit(false);
        wait(&mut lookup, &mut cx);
        assert_eq!(lookup.catalog[0].id, "b.ifo");
        worker.change("b.ifo".into(), false);
        worker.flush()?;
        let prefs: Vec<crate::dictionary::Preference> =
            serde_json::from_slice(&std::fs::read(store.dictionary_preferences())?)?;
        assert_eq!(prefs[0].id, "b.ifo");
        assert!(prefs[0].disabled);
        // An obsolete worker reply must not replace a newer request.
        let old = lookup.request;
        lookup.request += 1;
        lookup.pending = true;
        lookup.worker.result_for_test(crate::dictionary::worker::Reply {
            id: old,
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
