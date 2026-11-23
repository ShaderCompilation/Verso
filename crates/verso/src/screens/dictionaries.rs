//! Shared dictionary management, reachable from Settings and a retained lookup.
use crate::{
    dictionary::{
        Dictionary, Source, WordFormsSource, WordFormsStatus,
        languages::{all_languages, language_name},
        sets::{DEFAULT, Edit, Preferences, Set},
        worker::Worker,
    },
    geom::Rect,
    gfx::HAlign,
    platform::{TextInput, Waker},
    screens::{
        Action, EventCx, Screen,
        books::{ConfirmDelete, Details},
        dictionary_folders::DictionaryFolders,
    },
    store::Store,
    ui::{
        gesture::Gesture,
        theme::{INK2, Theme},
        widget::DrawCx,
        widgets::{Button, ButtonKind, Keyboard, List, ListEvent, Pager, Row, TextField, TopBar},
    },
};

#[derive(Clone)]
enum View {
    Sets,
    Set(String),
    Languages(String),
    Dictionary(String, String),
    /// The lookup card's placement.
    Card,
}
enum Field {
    New,
    Rename(String),
    Languages(String),
}
struct Editor {
    field: Field,
    input: TextField,
    keyboard: Keyboard,
    cancel: Button,
    save: Button,
    hint: Rect,
}
impl Editor {
    fn new(field: Field, value: String, t: &Theme, screen: Rect) -> Self {
        let area = screen.split_top(t.top_bar).1.inset(t.margin);
        let mut input =
            TextField::new(if matches!(field, Field::Languages(_)) { "Language codes" } else { "Set name" });
        input.edit(&TextInput::Insert(value));
        input.focused = true;
        input.place(Rect { h: t.target, ..area });
        let mut keyboard = Keyboard::default();
        keyboard.set_submit("Save");
        keyboard.visible = true;
        keyboard.place(screen, t);
        let y = area.y + t.target + t.mm(18.0);
        let w = (area.w - t.mm(3.0)) / 2;
        let mut cancel = Button::new("Cancel", ButtonKind::Secondary);
        cancel.place(Rect::new(area.x, y, w, t.button_h), t);
        let mut save = Button::new("Save", ButtonKind::Primary);
        save.place(Rect::new(area.right() - w, y, w, t.button_h), t);
        if screen.w > screen.h {
            let field_w = area.w * 3 / 5 - t.mm(3.0);
            input.place(Rect::new(area.x, area.y, field_w, t.target));
            let w = (area.w - field_w - 2 * t.mm(3.0)) / 2;
            cancel.place(Rect::new(area.x + field_w + t.mm(3.0), area.y, w, t.target), t);
            save.place(Rect::new(area.right() - w, area.y, w, t.target), t);
        }
        Self {
            field,
            input,
            keyboard,
            cancel,
            save,
            hint: Rect::new(area.x, area.y + t.target + t.mm(2.0), area.w, t.mm(15.0)),
        }
    }
}

/// One row of a set: a discovered dictionary, or saved preferences for one
/// that is not on disk now. Only a found dictionary has a path or source.
#[derive(Clone)]
enum Item {
    Found(Dictionary),
    Missing { id: String, disabled: bool },
}
impl Item {
    fn id(&self) -> &str {
        match self {
            Item::Found(d) => &d.id,
            Item::Missing { id, .. } => id,
        }
    }
    fn disabled(&self) -> bool {
        match self {
            Item::Found(d) => d.disabled,
            Item::Missing { disabled, .. } => *disabled,
        }
    }
    fn set_disabled(&mut self, value: bool) {
        match self {
            Item::Found(d) => d.disabled = value,
            Item::Missing { disabled, .. } => *disabled = value,
        }
    }
}
pub struct Dictionaries {
    store: Store,
    waker: Waker,
    worker: Worker,
    screen: Rect,
    top: TopBar,
    view: View,
    sets: Vec<Set>,
    /// The viewed set's rows in set order.
    dictionaries: Vec<Item>,
    unprepared: Vec<String>,
    sources: Vec<Source>,
    warnings: Vec<String>,
    /// Word-form languages, shown read-only below the sets.
    word_forms: Vec<WordFormsStatus>,
    request: u64,
    pending: bool,
    returning: bool,
    list: List,
    list_rect: Rect,
    pager: Pager,
    toolbar: Vec<Button>,
    rescan: Button,
    folders: Button,
    note: Rect,
    details: Option<Details>,
    editor: Option<Editor>,
    /// Asks before deleting this set.
    confirm: Option<ConfirmDelete<String>>,
}
impl Dictionaries {
    /// `set` opens that set directly (from a lookup); Back then shows all sets.
    pub fn new(t: &Theme, screen: Rect, store: Store, waker: Waker, set: Option<&str>) -> Self {
        let worker = Worker::new(store.clone(), waker.clone());
        let request = worker.catalog_in(set.unwrap_or(DEFAULT), false);
        let mut this = Self {
            store,
            waker,
            worker,
            screen,
            top: TopBar::new("Dictionaries", true),
            view: set.map_or(View::Sets, |id| View::Set(id.into())),
            sets: vec![],
            dictionaries: vec![],
            unprepared: vec![],
            sources: vec![],
            warnings: vec![],
            word_forms: vec![],
            request,
            pending: true,
            returning: false,
            list: List::new(vec![]),
            list_rect: Rect::default(),
            pager: Pager::new(1),
            toolbar: vec![],
            rescan: Button::new("Rescan", ButtonKind::Secondary),
            folders: Button::new("Folders", ButtonKind::Secondary),
            note: Rect::default(),
            details: None,
            editor: None,
            confirm: None,
        };
        this.layout(t);
        this
    }
    fn set_id(&self) -> &str {
        match &self.view {
            View::Sets | View::Card => DEFAULT,
            View::Set(id) | View::Languages(id) | View::Dictionary(id, _) => id,
        }
    }
    fn current_set(&self) -> Option<&Set> {
        self.sets.iter().find(|s| s.id == self.set_id())
    }
    fn dictionary_index(&self) -> Option<usize> {
        let View::Dictionary(_, id) = &self.view else { return None };
        self.dictionaries.iter().position(|d| d.id() == id)
    }
    fn dictionary(&self) -> Option<&Item> {
        self.dictionary_index().map(|i| &self.dictionaries[i])
    }
    /// The durable position of the visible neighbour, which also counts
    /// dictionaries that are temporarily missing.
    fn move_target(&self, dictionary: &str, up: bool) -> Option<usize> {
        let visible = self.dictionaries.iter().position(|d| d.id() == dictionary)?;
        let neighbor = if up { visible.checked_sub(1)? } else { visible + 1 };
        let neighbor = self.dictionaries.get(neighbor)?.id();
        self.current_set()?.dictionaries.iter().position(|p| p.id == *neighbor)
    }
    /// Leave views whose set or dictionary disappeared, e.g. after a rescan.
    fn settle_view(&mut self, cx: &mut EventCx) {
        let view = match &self.view {
            View::Sets | View::Card => return,
            View::Set(set) | View::Languages(set) | View::Dictionary(set, _)
                if !self.sets.iter().any(|s| s.id == *set) =>
            {
                View::Sets
            }
            View::Dictionary(set, _) if self.dictionary_index().is_none() => {
                cx.toast("That dictionary is no longer available", None);
                View::Set(set.clone())
            }
            _ => return,
        };
        self.view = view;
        self.list.page = 0;
    }
    fn catalog(&mut self, rescan: bool) {
        self.request = self.worker.catalog_in(self.set_id(), rescan);
        self.pending = true;
    }
    fn edit(&mut self, edit: Edit) {
        self.request = self.worker.edit(edit);
        self.pending = true;
    }
    /// Applies an on/off or order change here first, so the list answers
    /// each tap at once, then saves it through the worker. Rows stay enabled
    /// meanwhile: the worker applies edits in order and its reply to the
    /// latest one replaces this state.
    fn apply(&mut self, edit: Edit, cx: &mut EventCx) {
        let mut preferences = Preferences { sets: self.sets.clone() };
        if let Err(error) = preferences.edit(edit.clone()) {
            cx.toast(error.to_string(), None);
            return;
        }
        self.sets = preferences.sets;
        if let Edit::Disabled { dictionary, disabled, .. } = &edit
            && let Some(item) = self.dictionaries.iter_mut().find(|d| d.id() == dictionary)
        {
            item.set_disabled(*disabled);
        }
        let set = self.set_id().to_owned();
        self.order_items(&set);
        self.request = self.worker.edit(edit);
    }
    /// Sorts the rows by their saved place in `set`.
    fn order_items(&mut self, set: &str) {
        let Some(set) = self.sets.iter().find(|s| s.id == set) else { return };
        let position = |id: &str| set.dictionaries.iter().position(|p| p.id == id).unwrap_or(usize::MAX);
        self.dictionaries.sort_by_key(|d| position(d.id()));
    }
    fn layout(&mut self, t: &Theme) {
        let title = if let Some(editor) = &self.editor {
            match editor.field {
                Field::New => "New set",
                Field::Rename(_) => "Rename set",
                Field::Languages(_) => "Set languages",
            }
            .to_owned()
        } else {
            match &self.view {
                View::Sets => "Dictionaries".into(),
                View::Set(_) => self.current_set().map_or("Dictionary set", |s| &s.name).into(),
                View::Languages(_) => "Set languages".into(),
                View::Dictionary(_, _) => "Dictionary".into(),
                View::Card => "Lookup card".into(),
            }
        };
        self.top = TopBar::new(&title, true);
        self.top.place(self.screen, t);
        let c = self.screen.split_top(t.top_bar).1.inset(t.margin);
        let gap = t.mm(3.0);
        let y = c.bottom() - t.button_h;
        let w = (c.w - gap) / 2;
        self.rescan.place(Rect::new(c.x, y, w, t.button_h), t);
        self.folders.place(Rect::new(c.right() - w, y, w, t.button_h), t);
        self.note = Rect::new(c.x, y - t.mm(13.0) - gap, c.w, t.mm(13.0));
        self.pager.place(Rect::new(c.x, self.note.y - t.mm(12.0), c.w, t.mm(11.0)), t);
        let labels: &[&str] = match &self.view {
            View::Sets => &["New set", "Lookup card"],
            View::Set(id) if id != DEFAULT => &["Rename", "Languages", "Delete"],
            View::Set(_) => &["Rename"],
            View::Languages(_) => &["Edit language codes"],
            View::Dictionary(_, _) | View::Card => &[],
        };
        self.toolbar = labels
            .iter()
            .enumerate()
            .map(|(i, label)| {
                let mut b = Button::new(*label, ButtonKind::Secondary);
                let w = (c.w - gap * (labels.len() as i32 - 1)) / labels.len() as i32;
                b.place(Rect::new(c.x + i as i32 * (w + gap), c.y, w, t.button_h), t);
                b.enabled = !self.pending;
                b
            })
            .collect();
        let start = if labels.is_empty() { c.y } else { c.y + t.button_h + gap };
        self.list_rect = Rect::new(c.x, start, c.w, self.pager.rect().y - start);
        if self.screen.w > self.screen.h {
            let left_w = c.w * 3 / 5;
            let right_x = c.x + left_w + gap;
            let right_w = c.right() - right_x;
            for (i, button) in self.toolbar.iter_mut().enumerate() {
                button.place(Rect::new(right_x, c.y + i as i32 * (t.button_h + gap), right_w, t.button_h), t);
            }
            let y = c.y + self.toolbar.len() as i32 * (t.button_h + gap);
            self.rescan.place(Rect::new(right_x, y, right_w, t.button_h), t);
            self.folders.place(Rect::new(right_x, y + t.button_h + gap, right_w, t.button_h), t);
            self.note = Rect::new(c.x, c.bottom() - t.mm(8.0), left_w, t.mm(8.0));
            self.pager.place(Rect::new(c.x, self.note.y - t.mm(12.0), left_w, t.mm(11.0)), t);
            self.list_rect = Rect::new(c.x, c.y, left_w, self.pager.rect().y - c.y);
        }
        let rows = match &self.view {
            View::Sets => self
                .sets
                .iter()
                .map(|s| Row::new(&s.name).sub(languages(s)).chevron())
                .chain(self.word_forms.iter().map(|forms| {
                    Row::new(format!("{} word forms", language_name(&forms.language))).sub(match &forms.source {
                        WordFormsSource::BuiltIn => "Built-in".into(),
                        WordFormsSource::Pack(source) => source.clone(),
                        WordFormsSource::Hunspell { name, .. } => format!("Hunspell {name}"),
                        WordFormsSource::Unsupported { encoding, .. } => {
                            format!("Unsupported encoding {encoding} · use a UTF-8 copy")
                        }
                        WordFormsSource::Error(_) => "Couldn’t be read · Tap for details".into(),
                    })
                }))
                .collect(),
            View::Set(_) => {
                // Enabled dictionaries come first, numbered in lookup order;
                // only they can move.
                let enabled = self.dictionaries.iter().filter(|d| !d.disabled()).count();
                let mut rank = 0;
                self.dictionaries
                    .iter()
                    .map(|item| {
                        let row = match item {
                            Item::Found(d) => Row::new(&d.name).sub(format!(
                                "{}{}",
                                if self.unprepared.contains(&d.id) {
                                    "Needs preparation · "
                                } else if d.error.is_some() {
                                    "Read error · "
                                } else {
                                    ""
                                },
                                d.location(&self.sources)
                            )),
                            Item::Missing { id, .. } => Row::new(id).sub("Missing"),
                        };
                        if item.disabled() {
                            return row.rank(None).dim();
                        }
                        rank += 1;
                        row.rank(Some(rank)).reorder(rank > 1, rank < enabled)
                    })
                    .collect()
            }
            View::Languages(_) => self
                .language_choices()
                .iter()
                .map(|(code, name)| {
                    let owner = self.sets.iter().find(|s| s.languages.contains(code));
                    Row::new(format!("{name} · {code}"))
                        .sub(owner.map_or("Unassigned".into(), |s| format!("Assigned to {}", s.name)))
                        .toggle(owner.is_some_and(|s| s.id == self.set_id()))
                })
                .collect(),
            View::Dictionary(_, _) => self
                .dictionary_index()
                .map(|index| {
                    let d = match &self.dictionaries[index] {
                        Item::Found(d) => d,
                        Item::Missing { id, .. } => {
                            return vec![
                                Row::new("Missing dictionary").sub(id),
                                Row::new("Forget dictionary").sub("Remove its preferences from every set"),
                            ];
                        }
                    };
                    let mut rows = vec![
                        Row::new("Details").sub(&d.name).chevron(),
                        Row::new(if d.disabled { "Enable" } else { "Disable" }),
                        Row::new("Retry reading"),
                    ];
                    // Retry only after a read failed; it reopens the dictionary.
                    rows[2].enabled = d.error.is_some();
                    rows
                })
                .unwrap_or_default(),
            View::Card => vec![
                Row::new("Always at the bottom")
                    .sub("Off: the small card opens away from the word")
                    .toggle(self.store.settings().dictionary_at_bottom),
            ],
        };
        self.list.rows = rows;
        // The card's switch doesn't wait for the catalog.
        if self.pending && !matches!(self.view, View::Card) {
            for row in &mut self.list.rows {
                row.enabled = false;
            }
        }
        self.list.slot = t.target;
        self.list.place(self.list_rect, t.row2, 0);
        if matches!(self.view, View::Card) {
            // Explains the switch right below it.
            self.note =
                Rect::new(self.list_rect.x, self.list_rect.y + t.row2 + t.mm(3.0), self.list_rect.w, t.mm(15.0));
        }
        self.pager.set(self.list.page, self.list.paging().pages());
        self.list.page = self.pager.page;
    }
    fn open(&mut self, view: View, t: &Theme) {
        self.view = view;
        self.list.page = 0;
        // Dictionary actions use the already ordered catalog.
        if matches!(self.view, View::Set(_)) {
            self.catalog(false);
        }
        self.layout(t);
    }
    fn back(&mut self, t: &Theme) -> Action {
        if self.editor.take().is_some() {
            self.layout(t);
            return Action::None;
        }
        match self.view.clone() {
            View::Sets => return Action::Pop,
            View::Set(_) | View::Card => self.open(View::Sets, t),
            View::Dictionary(set, _) | View::Languages(set) => self.open(View::Set(set), t),
        }
        Action::None
    }
    fn start_editor(&mut self, field: Field, value: String, t: &Theme) {
        self.editor = Some(Editor::new(field, value, t, self.screen));
        self.layout(t);
    }
    fn save_editor(&mut self, cx: &mut EventCx) {
        let Some(editor) = &self.editor else { return };
        let value = editor.input.value().trim().to_owned();
        let edit = match &editor.field {
            Field::New | Field::Rename(_) if value.is_empty() => {
                cx.toast("Enter a set name", None);
                return;
            }
            Field::New => {
                let id = format!(
                    "set-{}",
                    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos()
                );
                Edit::Create { id, name: value }
            }
            Field::Rename(set) => Edit::Rename { set: set.clone(), name: value },
            Field::Languages(set) => Edit::Languages {
                set: set.clone(),
                languages: value
                    .split(|c: char| c == ',' || c.is_whitespace())
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .collect(),
            },
        };
        // Use the backend's validation before closing the editor.
        let mut preferences = Preferences { sets: self.sets.clone() };
        if let Err(error) = preferences.edit(edit.clone()) {
            cx.toast(error.to_string(), None);
            return;
        }
        self.editor = None;
        self.edit(edit);
        self.layout(cx.theme);
    }
    fn toolbar(&mut self, index: usize, cx: &mut EventCx) {
        if matches!(self.view, View::Sets) {
            if index == 1 {
                return self.open(View::Card, cx.theme);
            }
            return self.start_editor(Field::New, String::new(), cx.theme);
        }
        let Some(current) = self.current_set() else { return };
        let (name, languages) = (current.name.clone(), current.languages.join(", "));
        match self.view.clone() {
            View::Set(set) => match index {
                0 => self.start_editor(Field::Rename(set), name, cx.theme),
                1 => self.open(View::Languages(set), cx.theme),
                2 => {
                    let message = format!(
                        "Delete “{name}”? Its dictionary order, on/off choices and languages are removed. \
                         Books that used it switch to the set for their language, or Default."
                    );
                    self.confirm = Some(ConfirmDelete::new(cx.theme, self.screen, set, "Delete set", message));
                }
                _ => {}
            },
            View::Languages(set) => self.start_editor(Field::Languages(set), languages, cx.theme),
            _ => {}
        }
    }
    /// Turns a set's dictionary on (it becomes the last numbered one) or off.
    fn toggle(&mut self, index: usize, cx: &mut EventCx) {
        let (View::Set(set), Some(item)) = (self.view.clone(), self.dictionaries.get(index)) else { return };
        let edit = Edit::Disabled { set, dictionary: item.id().into(), disabled: !item.disabled() };
        self.apply(edit, cx);
        self.layout(cx.theme);
    }
    /// Moves a numbered dictionary one place and keeps it in view.
    fn shift(&mut self, index: usize, up: bool, cx: &mut EventCx) {
        let (View::Set(set), Some(item)) = (self.view.clone(), self.dictionaries.get(index)) else { return };
        let dictionary = item.id().to_owned();
        let Some(position) = self.move_target(&dictionary, up) else {
            cx.toast("Couldn’t move this dictionary. Tap Rescan and try again.", None);
            return;
        };
        self.apply(Edit::MoveDictionary { set, dictionary: dictionary.clone(), position }, cx);
        self.layout(cx.theme);
        if let Some(moved) = self.dictionaries.iter().position(|d| d.id() == dictionary) {
            self.list.page = self.list.paging().page_of(moved);
            self.pager.set(self.list.page, self.list.paging().pages());
        }
    }
    fn language_choices(&self) -> Vec<(String, String)> {
        // English and Russian first, the languages Verso has its own word forms for.
        let first = ["en", "ru"];
        let mut choices: Vec<_> = first
            .iter()
            .map(|code| (*code, language_name(code)))
            .chain(all_languages().filter(|(code, _)| !first.contains(code)))
            .map(|(code, name)| (code.to_owned(), name.to_owned()))
            .collect();
        if let Some(set) = self.current_set() {
            for code in &set.languages {
                if !choices.iter().any(|(c, _)| c == code) {
                    choices.push((code.clone(), code.clone()));
                }
            }
        }
        choices
    }
    fn row(&mut self, index: usize, cx: &mut EventCx) {
        match self.view.clone() {
            View::Sets if index < self.sets.len() => self.open(View::Set(self.sets[index].id.clone()), cx.theme),
            View::Sets => {
                let Some(forms) = self.word_forms.get(index - self.sets.len()) else { return };
                let text = word_forms_details(forms);
                let name = language_name(&forms.language);
                self.details = Some(Details::from_text(cx.theme, self.screen, &format!("{name} word forms"), text));
            }
            View::Set(set) => self.open(View::Dictionary(set, self.dictionaries[index].id().into()), cx.theme),
            View::Card => {
                let mut settings = self.store.settings();
                settings.dictionary_at_bottom = !settings.dictionary_at_bottom;
                if let Err(e) = self.store.save_settings(&settings) {
                    log::error!("saving the lookup card placement: {e:#}");
                    cx.toast("Couldn’t save the setting", None);
                }
                self.layout(cx.theme);
            }
            View::Languages(set) => {
                let code = self.language_choices()[index].0.clone();
                let Some(mut languages) = self.current_set().map(|s| s.languages.clone()) else { return };
                if languages.contains(&code) {
                    languages.retain(|l| l != &code);
                } else {
                    languages.push(code);
                }
                self.edit(Edit::Languages { set, languages });
                self.layout(cx.theme);
            }
            View::Dictionary(set, dictionary) => {
                let d = match self.dictionary() {
                    Some(Item::Found(d)) => d,
                    Some(Item::Missing { id, .. }) => {
                        if index == 1 {
                            self.edit(Edit::Forget { set: set.clone(), dictionary });
                            self.view = View::Set(set);
                            self.layout(cx.theme);
                        } else {
                            let text = format!(
                                "{id}\n\nThis dictionary is no longer on disk. Its order and on/off choices are kept in every set until you forget it."
                            );
                            self.details = Some(Details::from_text(cx.theme, self.screen, "Missing dictionary", text));
                        }
                        return;
                    }
                    None => return,
                };
                let edit = match index {
                    0 => {
                        let text = format!(
                            "{}\n\nSource:\n{}\n\nSource path:\n{}\n\n{}",
                            d.name,
                            d.location(&self.sources),
                            d.path.display(),
                            d.error.as_ref().map_or("No read error.".into(), |e| format!("Read error:\n{e}"))
                        );
                        self.details = Some(Details::from_text(cx.theme, self.screen, "Dictionary details", text));
                        return;
                    }
                    1 => Edit::Disabled { set, dictionary, disabled: !d.disabled },
                    2 => {
                        self.request = self.worker.retry(&set, &dictionary);
                        self.pending = true;
                        self.layout(cx.theme);
                        return;
                    }
                    _ => return,
                };
                self.edit(edit);
                self.layout(cx.theme);
            }
        }
    }
}
/// The details text for a word-form language: its source or error, its
/// Hunspell files and the Hunspell dictionaries it doesn't use.
fn word_forms_details(forms: &WordFormsStatus) -> String {
    let name = language_name(&forms.language);
    let mut text = match &forms.source {
        WordFormsSource::BuiltIn => "Built into Verso.".into(),
        WordFormsSource::Pack(source) => format!("Source: {source}"),
        WordFormsSource::Hunspell { name: dictionary, encoding, .. } => {
            format!("Source: Hunspell dictionary {dictionary} ({encoding}).")
        }
        WordFormsSource::Unsupported { name: dictionary, encoding } => format!(
            "The Hunspell dictionary {dictionary} uses the {encoding} encoding, which Verso can’t read, so {name} words \
             get no base forms. Install a UTF-8 copy of it and tap Rescan."
        ),
        WordFormsSource::Error(error) => {
            format!("Couldn’t be read, so {name} words get no base forms. Rescan tries again.\n\n{error}")
        }
    };
    if !forms.files.is_empty() {
        text.push_str(&format!("\n\nFiles in the hunspell folder: {}", forms.files.join(", ")));
    }
    if !forms.unused.is_empty() {
        let why = match forms.source {
            WordFormsSource::BuiltIn | WordFormsSource::Pack(_) => format!("{name} uses its own word forms"),
            _ => "only the first by name is used".into(),
        };
        text.push_str(&format!("\n\nNot used ({why}): {}", forms.unused.join(", ")));
    }
    text.push_str(&format!(
        "\n\nWhen a dictionary has no entry for a selected {name} word, Verso looks up its base forms instead."
    ));
    text
}

fn languages(set: &Set) -> String {
    if set.id == DEFAULT {
        "Fallback for books without an assigned set".into()
    } else if set.languages.is_empty() {
        "No languages assigned".into()
    } else {
        format!("Languages: {}", set.languages.join(", "))
    }
}
impl Screen for Dictionaries {
    fn draw(&mut self, cx: &mut DrawCx) {
        self.top.draw(cx);
        if let Some(editor) = &self.editor {
            editor.input.draw(cx);
            let hint = if matches!(editor.field, Field::Languages(_)) {
                "Language codes, separated by commas: en, ru, fr. Empty clears them. Assigning a language moves it from its previous set."
            } else if matches!(editor.field, Field::New) {
                "New sets start with every dictionary off. Turn them on in the order you want them tried."
            } else {
                "Choose a name for this dictionary set."
            };
            cx.paragraph(hint, cx.theme.small.gray(INK2), editor.hint, HAlign::Left);
            editor.cancel.draw(cx);
            editor.save.draw(cx);
            editor.keyboard.draw(cx);
            return;
        }
        self.list.draw(cx);
        for button in &self.toolbar {
            button.draw(cx);
        }
        self.pager.draw(cx);
        let card = matches!(self.view, View::Card);
        let note = if card {
            "The card opens at the size you last left it: the expand button beside × switches it. \
             In landscape, the bottom means the right side."
        } else if self.pending {
            "Checking dictionaries…"
        } else if !self.warnings.is_empty() {
            "Some folders or preferences had problems. Tap here for details."
        } else if matches!(self.view, View::Set(_)) && self.list.rows.is_empty() {
            "No dictionaries found. Add a folder or copy StarDict files into Verso’s dictionaries folder."
        } else if matches!(self.view, View::Set(_)) {
            "Lookups try the numbered dictionaries in order. Tap a circle to turn one on or off."
        } else {
            "Changes save immediately. Tap a row to manage it."
        };
        cx.paragraph(note, cx.theme.small.gray(INK2), self.note, HAlign::Left);
        if !card {
            self.rescan.enabled = !self.pending;
            self.rescan.draw(cx);
            self.folders.draw(cx);
        }
        if let Some(details) = &mut self.details {
            details.draw(cx);
        }
        if let Some(confirm) = &self.confirm {
            confirm.draw(cx);
        }
    }
    fn on_gesture(&mut self, g: Gesture, cx: &mut EventCx) -> Action {
        cx.redraw();
        if let Some(details) = &mut self.details {
            if details.on_gesture(&g, cx) {
                self.details = None;
            }
            return Action::None;
        }
        if let Some(confirm) = &mut self.confirm {
            match confirm.on_gesture(&g) {
                Some(true) => {
                    let set = self.confirm.take().map(|c| c.target).unwrap_or_default();
                    self.edit(Edit::Delete { set });
                    self.view = View::Sets;
                    self.list.page = 0;
                    self.layout(cx.theme);
                }
                Some(false) => self.confirm = None,
                None => {}
            }
            return Action::None;
        }
        if let Some(editor) = &mut self.editor {
            if editor.keyboard.visible
                && let Some(input) = editor.keyboard.on_gesture_inside(&g, cx.theme)
            {
                if let Some(input) = input {
                    self.on_text_input(&input, cx);
                }
                return Action::None;
            }
            if editor.cancel.on_gesture(&g) || self.top.on_gesture(&g) {
                return self.back(cx.theme);
            }
            if editor.save.on_gesture(&g) {
                self.save_editor(cx);
            } else if editor.input.on_gesture(&g) {
                editor.input.focused = true;
                editor.keyboard.visible = true;
            }
            return Action::None;
        }
        if let Gesture::Tap(p) = g
            && self.note.contains(p)
            && !self.warnings.is_empty()
        {
            self.details =
                Some(Details::from_text(cx.theme, self.screen, "Dictionary problems", self.warnings.join("\n\n")));
            return Action::None;
        }
        if self.top.on_gesture(&g) {
            return self.back(cx.theme);
        }
        if let Some(i) = self.toolbar.iter_mut().position(|b| b.on_gesture(&g)) {
            self.toolbar(i, cx);
            return Action::None;
        }
        if !matches!(self.view, View::Card) && self.folders.on_gesture(&g) {
            self.returning = true;
            return Action::Push(Box::new(DictionaryFolders::new(
                cx.theme,
                self.screen,
                self.store.clone(),
                self.waker.clone(),
            )));
        }
        if !self.pending && !matches!(self.view, View::Card) && self.rescan.on_gesture(&g) {
            self.catalog(true);
            self.layout(cx.theme);
        }
        let page = self.pager.page;
        self.pager.on_gesture(&g);
        if let Gesture::Swipe { dir, from, .. } = g
            && self.list_rect.contains(from)
        {
            self.pager.on_swipe(dir);
        }
        if page != self.pager.page {
            self.list.page = self.pager.page;
        }
        match self.list.on_gesture(&g) {
            Some(ListEvent::Tapped(i)) => self.row(i, cx),
            Some(ListEvent::Lead(i)) => self.toggle(i, cx),
            Some(ListEvent::Up(i)) => self.shift(i, true, cx),
            Some(ListEvent::Down(i)) => self.shift(i, false, cx),
            _ => {}
        }
        Action::None
    }
    fn on_text_input(&mut self, input: &TextInput, cx: &mut EventCx) -> bool {
        let Some(editor) = &mut self.editor else { return false };
        match input {
            TextInput::Submit => self.save_editor(cx),
            TextInput::Dismiss => {
                editor.keyboard.visible = false;
                editor.input.focused = false;
            }
            _ => {
                editor.input.edit(input);
            }
        }
        cx.redraw();
        true
    }
    fn busy(&self) -> bool {
        self.pending
    }
    fn on_background(&mut self, cx: &mut EventCx) {
        if self.returning {
            self.returning = false;
            self.catalog(false);
            self.layout(cx.theme);
        }
        if let Some(reply) = self.worker.receive()
            && reply.id == self.request
        {
            self.pending = false;
            self.sets = reply.sets;
            self.unprepared = reply.unprepared;
            // The worker applied the active set to the found dictionaries;
            // missing ones take their saved place and state.
            self.dictionaries = reply.dictionaries.into_iter().map(Item::Found).collect();
            if let Some(set) = self.sets.iter().find(|s| s.id == reply.active_set) {
                self.dictionaries.extend(reply.missing.into_iter().map(|id| Item::Missing {
                    disabled: set.dictionaries.iter().find(|p| p.id == id).is_none_or(|p| p.disabled),
                    id,
                }));
                self.order_items(&reply.active_set);
            }
            self.sources = reply.sources;
            self.warnings = reply.warnings;
            // Only catalog replies list word forms; a lookup reply never reaches here.
            self.word_forms = reply.word_forms;
            if let Some(error) = reply.change_error {
                cx.toast(error, None);
            }
            if let Some(error) = reply.save_error {
                cx.toast(format!("Couldn’t save dictionary preferences: {error}"), None);
            }
            self.settle_view(cx);
            self.layout(cx.theme);
            cx.redraw();
        }
    }
    fn on_quit(&mut self) -> anyhow::Result<()> {
        self.worker.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::Point;
    use crate::ui::gesture::Direction;
    use std::{
        fs,
        sync::atomic::{AtomicU64, Ordering},
        time::{Duration, Instant},
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    fn with_cx<R>(t: &Theme, screen: Rect, f: impl FnOnce(&mut EventCx) -> R) -> R {
        f(&mut EventCx {
            theme: t,
            screen,
            now: Instant::now(),
            toast: &mut None,
            forced: &mut vec![],
            needs_redraw: &mut false,
            battery: None,
        })
    }
    fn wait(s: &mut Dictionaries, t: &Theme) {
        let until = Instant::now() + Duration::from_secs(5);
        while s.pending {
            with_cx(t, s.screen, |cx| s.on_background(cx));
            assert!(Instant::now() < until, "management reply timed out");
            std::thread::yield_now();
        }
    }
    fn tap(s: &mut Dictionaries, p: Point, t: &Theme) -> Action {
        with_cx(t, s.screen, |cx| {
            s.on_gesture(Gesture::Down(p), cx);
            s.on_gesture(Gesture::Tap(p), cx)
        })
    }
    fn row(s: &mut Dictionaries, index: usize, t: &Theme) {
        let p = Point::new(s.list_rect.center().x, s.list_rect.y + (index as i32 * t.row2) + t.row2 / 2);
        tap(s, p, t);
    }
    #[derive(Clone, Copy)]
    enum Part {
        Lead,
        Up,
        Down,
    }
    /// Taps the rank circle or an arrow of the `index`th visible row.
    fn part(s: &mut Dictionaries, index: usize, part: Part, t: &Theme) {
        let y = s.list_rect.y + index as i32 * t.row2 + t.row2 / 2;
        let x = match part {
            Part::Lead => s.list_rect.x + t.target / 2,
            Part::Up => s.list_rect.right() - t.target * 3 / 2,
            Part::Down => s.list_rect.right() - t.target / 2,
        };
        tap(s, Point::new(x, y), t);
    }
    /// Waits for the worker's reply to the latest quick edit.
    fn confirm(s: &mut Dictionaries, t: &Theme) {
        s.pending = true;
        wait(s, t);
    }
    fn order(s: &Dictionaries) -> Vec<(&str, Option<usize>)> {
        s.dictionaries
            .iter()
            .zip(&s.list.rows)
            .map(|(d, r)| {
                let rank = match r.lead {
                    crate::ui::widgets::list::Lead::Rank(rank) => rank,
                    crate::ui::widgets::list::Lead::None => panic!("set rows have a rank"),
                };
                (d.id(), rank)
            })
            .collect()
    }
    fn submit(s: &mut Dictionaries, value: &str, t: &Theme) {
        with_cx(t, s.screen, |cx| {
            s.on_text_input(&TextInput::Clear, cx);
            s.on_text_input(&TextInput::Insert(value.into()), cx);
            s.on_text_input(&TextInput::Submit, cx);
        });
        wait(s, t);
    }
    fn install(store: &Store, names: &[&str]) {
        fs::create_dir_all(store.dictionary_dir()).unwrap();
        for name in names {
            fs::write(store.dictionary_dir().join(format!("{name}.ifo")), format!("StarDict's dict ifo file\nversion=2.4.2\nbookname={name}\nwordcount=1\nidxfilesize=14\nsametypesequence=m\n")).unwrap();
            fs::write(store.dictionary_dir().join(format!("{name}.idx")), b"Hello\0\0\0\0\0\0\0\0\x07").unwrap();
            fs::write(store.dictionary_dir().join(format!("{name}.dict")), b"meaning").unwrap();
        }
    }
    fn temp_store(name: &str) -> (std::path::PathBuf, Store) {
        let root = std::env::temp_dir().join(format!(
            "verso-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let store = Store::at(root.clone());
        store.save_dictionary_folders(&[]).unwrap();
        (root, store)
    }
    #[test]
    fn management_edits_sets_languages_order_and_flags_and_returns_from_folders() {
        let root = std::env::temp_dir().join(format!(
            "verso-management-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let store = Store::at(root.clone());
        store.save_dictionary_folders(&[]).unwrap();
        install(&store, &["a", "b", "c"]);
        let t = Theme::for_dpi(300);
        let screen = Rect::new(0, 0, 1272, 1696);
        let mut s = Dictionaries::new(&t, screen, store.clone(), Waker::new(|| {}), None);
        wait(&mut s, &t);
        // Default, then the read-only word-form rows; this store has no packs.
        let rows: Vec<_> = s.list.rows.iter().map(|r| (r.label.as_str(), r.sub.as_deref())).collect();
        assert_eq!(
            rows,
            [
                ("Default", Some("Fallback for books without an assigned set")),
                ("English word forms", Some("Built-in")),
                ("Russian word forms", Some("Couldn’t be read · Tap for details")),
            ]
        );
        // Lookup card: its switch saves the placement the reader reads.
        let card = s.toolbar[1].rect().center();
        tap(&mut s, card, &t);
        assert!(matches!(s.view, View::Card));
        row(&mut s, 0, &t);
        assert!(!store.settings().dictionary_at_bottom);
        s.back(&t);
        assert!(matches!(s.view, View::Sets));
        row(&mut s, 2, &t);
        assert!(s.details.is_some(), "the error opens in details");
        assert!(matches!(s.view, View::Sets));
        s.details = None;
        crate::dictionary::write_pack(&store.wordforms_dir().join("ru.vwf"), "ru", &[("кошки", &["кошка"])]);
        let rescan = s.rescan.rect().center();
        tap(&mut s, rescan, &t);
        wait(&mut s, &t);
        assert_eq!(s.list.rows[2].sub.as_deref(), Some("Test pack, 2026-11-17"), "Rescan reopens the pack");
        let new = s.toolbar[0].rect().center();
        tap(&mut s, new, &t);
        submit(&mut s, "English and Russian", &t);
        assert_eq!(s.sets.len(), 2);
        let id = s.sets[1].id.clone();
        assert!(s.sets[1].dictionaries.iter().all(|d| d.disabled));
        assert_eq!(
            s.sets[1].dictionaries,
            s.sets[0]
                .dictionaries
                .iter()
                .map(|d| crate::dictionary::Preference { id: d.id.clone(), disabled: true })
                .collect::<Vec<_>>()
        );
        row(&mut s, 1, &t);
        wait(&mut s, &t);
        let rename = s.toolbar[0].rect().center();
        tap(&mut s, rename, &t);
        submit(&mut s, "Reading", &t);
        assert_eq!(s.current_set().unwrap().name, "Reading");
        let langs = s.toolbar[1].rect().center();
        tap(&mut s, langs, &t);
        row(&mut s, 0, &t);
        wait(&mut s, &t);
        row(&mut s, 1, &t);
        wait(&mut s, &t);
        assert_eq!(s.current_set().unwrap().languages, ["en", "ru"]);
        s.back(&t);
        wait(&mut s, &t);
        // The dictionary page can still turn one on.
        assert_eq!(order(&s), [("a.ifo", None), ("b.ifo", None), ("c.ifo", None)]);
        assert!(s.list.rows.iter().all(|r| r.dim));
        row(&mut s, 0, &t);
        row(&mut s, 1, &t);
        wait(&mut s, &t);
        assert!(!s.dictionary().unwrap().disabled());
        assert_eq!(s.list.rows.len(), 3);
        assert!(!s.list.rows[2].enabled, "Retry needs a read error");
        s.back(&t);
        wait(&mut s, &t);
        // Circles turn dictionaries on in tap order, at once and without
        // graying the list while the worker saves.
        assert_eq!(order(&s), [("a.ifo", Some(1)), ("b.ifo", None), ("c.ifo", None)]);
        part(&mut s, 2, Part::Lead, &t);
        assert_eq!(order(&s), [("a.ifo", Some(1)), ("c.ifo", Some(2)), ("b.ifo", None)]);
        assert!(!s.pending && s.list.rows.iter().all(|r| r.enabled));
        assert!(s.list.rows[2].dim && !s.list.rows[1].dim);
        part(&mut s, 2, Part::Lead, &t);
        assert_eq!(order(&s), [("a.ifo", Some(1)), ("c.ifo", Some(2)), ("b.ifo", Some(3))]);
        // Arrows move numbered ones; ▲ on the first and ▼ on the last do nothing.
        part(&mut s, 0, Part::Up, &t);
        part(&mut s, 2, Part::Down, &t);
        assert_eq!(order(&s), [("a.ifo", Some(1)), ("c.ifo", Some(2)), ("b.ifo", Some(3))]);
        part(&mut s, 0, Part::Down, &t);
        part(&mut s, 2, Part::Up, &t);
        assert_eq!(order(&s), [("c.ifo", Some(1)), ("b.ifo", Some(2)), ("a.ifo", Some(3))]);
        // Off moves it below the numbered ones.
        part(&mut s, 0, Part::Lead, &t);
        assert_eq!(order(&s), [("b.ifo", Some(1)), ("a.ifo", Some(2)), ("c.ifo", None)]);
        confirm(&mut s, &t);
        assert_eq!(order(&s), [("b.ifo", Some(1)), ("a.ifo", Some(2)), ("c.ifo", None)]);
        let saved: Vec<_> = s.current_set().unwrap().dictionaries.iter().map(|d| (d.id.as_str(), d.disabled)).collect();
        assert_eq!(saved, [("b.ifo", false), ("a.ifo", false), ("c.ifo", true)]);
        // The name still opens the dictionary page.
        row(&mut s, 0, &t);
        assert!(matches!(&s.view, View::Dictionary(_, d) if d == "b.ifo"));
        row(&mut s, 0, &t);
        assert!(s.details.is_some());
        s.details = None;
        s.back(&t);
        wait(&mut s, &t);
        let folders = s.folders.rect().center();
        assert!(matches!(tap(&mut s, folders, &t), Action::Push(_)));
        store.save_dictionary_folders(&[root.join("missing")]).unwrap();
        with_cx(&t, screen, |cx| s.on_background(cx));
        wait(&mut s, &t);
        assert!(!s.warnings.is_empty());
        assert_eq!(s.current_set().unwrap().id, id);
        // Delete asks first; Cancel keeps the set.
        let delete = s.toolbar[2].rect().center();
        tap(&mut s, delete, &t);
        let cancel = s.confirm.as_ref().unwrap().cancel.rect().center();
        tap(&mut s, cancel, &t);
        assert!(s.confirm.is_none() && !s.pending && s.sets.len() == 2);
        tap(&mut s, delete, &t);
        let confirm = s.confirm.as_ref().unwrap().delete.rect().center();
        tap(&mut s, confirm, &t);
        wait(&mut s, &t);
        assert_eq!(s.sets.len(), 1);
        row(&mut s, 0, &t);
        wait(&mut s, &t);
        assert_eq!(s.toolbar.len(), 1, "Default has neither delete nor language actions");
        s.worker.flush().unwrap();
        let prefs: crate::dictionary::sets::Preferences =
            serde_json::from_slice(&fs::read(store.dictionary_preferences()).unwrap()).unwrap();
        assert_eq!(prefs.sets.len(), 1);
        drop(s);
        let _ = fs::remove_dir_all(root);
    }
    #[test]
    fn moving_across_a_page_edge_follows_the_dictionary() {
        let (root, store) = temp_store("management-pages");
        install(&store, &["a", "b", "c", "d", "e", "f", "g", "h", "i", "j"]);
        let t = Theme::for_dpi(300);
        let mut s = Dictionaries::new(&t, Rect::new(0, 0, 1072, 1448), store, Waker::new(|| {}), Some(DEFAULT));
        wait(&mut s, &t);
        let per_page = s.list.paging().range(0).len();
        assert!(s.list.paging().pages() > 1, "the test needs a second page");
        let last = s.dictionaries[per_page - 1].id().to_owned();
        part(&mut s, per_page - 1, Part::Down, &t);
        assert_eq!(s.dictionaries[per_page].id(), last);
        assert_eq!((s.list.page, s.pager.page), (1, 1));
        part(&mut s, 0, Part::Up, &t);
        assert_eq!(s.dictionaries[per_page - 1].id(), last);
        assert_eq!((s.list.page, s.pager.page), (0, 0));
        // Turning one on or off stays on the page.
        part(&mut s, 0, Part::Lead, &t);
        assert_eq!(s.list.page, 0);
        confirm(&mut s, &t);
        drop(s);
        let _ = fs::remove_dir_all(root);
    }
    #[test]
    fn compact_orientations_page_many_sets_and_keep_editor_controls_above_keyboard() {
        let t = Theme::for_dpi(300);
        for screen in [Rect::new(0, 0, 1072, 1448), Rect::new(0, 0, 1448, 1072)] {
            let root = std::env::temp_dir().join(format!(
                "verso-management-layout-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let store = Store::at(root.clone());
            store.save_dictionary_folders(&[]).unwrap();
            let mut s = Dictionaries::new(&t, screen, store, Waker::new(|| {}), None);
            wait(&mut s, &t);
            for i in 0..20 {
                s.sets.push(Set {
                    id: i.to_string(),
                    name: "A very long multilingual dictionary set name".repeat(3),
                    languages: vec!["en".into(), "ru".into()],
                    dictionaries: vec![],
                });
            }
            s.layout(&t);
            assert!(s.pager.pages > 1);
            with_cx(&t, screen, |cx| {
                s.on_gesture(
                    Gesture::Swipe { dir: Direction::Left, from: s.list_rect.center(), to: s.list_rect.center() },
                    cx,
                )
            });
            assert_eq!(s.list.page, 1);
            s.start_editor(Field::New, "Unsubmitted".into(), &t);
            let e = s.editor.as_ref().unwrap();
            assert!(e.save.rect().bottom() < e.keyboard.rect().y);
            assert!(e.hint.bottom() < e.keyboard.rect().y);
            s.back(&t);
            assert_eq!(s.sets.len(), 21, "cancel never edits preferences");
            drop(s);
            let _ = fs::remove_dir_all(root);
        }
    }

    #[test]
    fn opens_at_a_set_and_leaves_views_that_disappear() {
        let (root, store) = temp_store("management-views");
        install(&store, &["a", "b"]);
        let t = Theme::for_dpi(300);
        let screen = Rect::new(0, 0, 1272, 1696);
        // A lookup opens its own set; Back then lists every set.
        let mut s = Dictionaries::new(&t, screen, store.clone(), Waker::new(|| {}), Some(DEFAULT));
        wait(&mut s, &t);
        assert!(matches!(&s.view, View::Set(id) if id == DEFAULT));
        assert_eq!(s.list.rows.len(), 2);
        assert!(matches!(s.back(&t), Action::None));
        assert!(matches!(s.view, View::Sets));
        // A set deleted meanwhile falls back to the set list.
        let mut s = Dictionaries::new(&t, screen, store.clone(), Waker::new(|| {}), Some("gone"));
        wait(&mut s, &t);
        assert!(matches!(s.view, View::Sets));
        // An inconsistent order refuses to move instead of panicking.
        s.open(View::Set(DEFAULT.into()), &t);
        wait(&mut s, &t);
        row(&mut s, 0, &t);
        assert!(s.dictionary().is_some());
        s.sets[0].dictionaries.clear();
        row(&mut s, 3, &t);
        assert!(!s.pending, "no edit is sent");
        // A dictionary removed by a rescan returns to its set.
        fs::remove_file(store.dictionary_dir().join("a.ifo")).unwrap();
        s.catalog(true);
        wait(&mut s, &t);
        assert!(matches!(&s.view, View::Dictionary(id, _) if id == DEFAULT));
        assert!(matches!(s.dictionary(), Some(Item::Missing { id, .. }) if id == "a.ifo"));
        assert_eq!(s.list.rows[1].label, "Forget dictionary");
        row(&mut s, 1, &t);
        wait(&mut s, &t);
        assert!(matches!(&s.view, View::Set(id) if id == DEFAULT));
        assert_eq!(s.list.rows.len(), 1);
        drop(s);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn hunspell_languages_get_rows_with_their_files_and_errors() {
        let (root, store) = temp_store("management-hunspell");
        let dir = store.hunspell_dir();
        crate::dictionary::install_hunspell_fixture(&dir.join("de"), "de_DE");
        crate::dictionary::install_hunspell_fixture(&dir, "de_DE");
        fs::rename(dir.join("de_DE.aff"), dir.join("de_DE_frami.aff")).unwrap();
        fs::rename(dir.join("de_DE.dic"), dir.join("de_DE_frami.dic")).unwrap();
        crate::dictionary::write_hunspell(&dir, "fr_FR", "SET KOI8-R\n", "1\nmot\n");
        crate::dictionary::write_hunspell(&dir, "it_IT", "SET UTF-8\n", "no count\n");
        crate::dictionary::write_hunspell(&dir, "en_US", "SET UTF-8\n", "1\nword\n");
        let t = Theme::for_dpi(300);
        let screen = Rect::new(0, 0, 1272, 1696);
        let mut s = Dictionaries::new(&t, screen, store.clone(), Waker::new(|| {}), None);
        wait(&mut s, &t);
        let rows: Vec<_> = s.list.rows.iter().map(|r| (r.label.as_str(), r.sub.as_deref().unwrap_or(""))).collect();
        assert_eq!(
            rows[1..],
            [
                ("English word forms", "Built-in"),
                ("French word forms", "Unsupported encoding KOI8-R · use a UTF-8 copy"),
                ("German word forms", "Hunspell de_DE"),
                ("Italian word forms", "Couldn’t be read · Tap for details"),
                ("Russian word forms", "Couldn’t be read · Tap for details"),
            ]
        );
        let details: Vec<_> = s.word_forms.iter().map(word_forms_details).collect();
        assert!(details[0].contains("Not used (English uses its own word forms): en_US.aff"), "{}", details[0]);
        assert!(
            details[1].contains("KOI8-R encoding") && details[1].contains("fr_FR.aff, fr_FR.dic"),
            "{}",
            details[1]
        );
        assert!(details[2].contains("Hunspell dictionary de_DE (UTF-8)"), "{}", details[2]);
        assert!(details[2].contains("Files in the hunspell folder: de/de_DE.aff, de/de_DE.dic"), "{}", details[2]);
        assert!(details[2].contains("only the first by name is used): de_DE_frami.aff"), "{}", details[2]);
        assert!(details[3].contains("doesn't start with a word count"), "{}", details[3]);
        row(&mut s, 3, &t);
        assert!(s.details.is_some(), "a Hunspell row opens its details");
        drop(s);
        let _ = fs::remove_dir_all(root);
    }
}
