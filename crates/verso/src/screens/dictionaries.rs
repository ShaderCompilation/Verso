//! Shared dictionary management, reachable from Settings and a retained lookup.
use crate::{
    dictionary::{
        Dictionary, Source,
        sets::{DEFAULT, Edit, Set},
        worker::Worker,
    },
    geom::Rect,
    gfx::HAlign,
    platform::{TextInput, Waker},
    screens::{Action, EventCx, Screen, books::Details, dictionary_folders::DictionaryFolders},
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

pub struct Dictionaries {
    store: Store,
    waker: Waker,
    worker: Worker,
    screen: Rect,
    top: TopBar,
    view: View,
    sets: Vec<Set>,
    dictionaries: Vec<Dictionary>,
    sources: Vec<Source>,
    warnings: Vec<String>,
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
}
impl Dictionaries {
    pub fn new(t: &Theme, screen: Rect, store: Store, waker: Waker) -> Self {
        let worker = Worker::new(store.clone(), waker.clone());
        let request = worker.catalog(false);
        let mut this = Self {
            store,
            waker,
            worker,
            screen,
            top: TopBar::new("Dictionaries", true),
            view: View::Sets,
            sets: vec![],
            dictionaries: vec![],
            sources: vec![],
            warnings: vec![],
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
        };
        this.layout(t);
        this
    }
    fn set_id(&self) -> &str {
        match &self.view {
            View::Sets => DEFAULT,
            View::Set(id) | View::Languages(id) | View::Dictionary(id, _) => id,
        }
    }
    fn current_set(&self) -> Option<&Set> {
        self.sets.iter().find(|s| s.id == self.set_id())
    }
    fn dictionary(&self) -> Option<&Dictionary> {
        let View::Dictionary(_, id) = &self.view else { return None };
        self.dictionaries.iter().find(|d| d.id == *id)
    }
    fn catalog(&mut self, rescan: bool) {
        self.request = self.worker.catalog_in(self.set_id(), rescan);
        self.pending = true;
    }
    fn edit(&mut self, edit: Edit) {
        self.request = self.worker.edit(edit);
        self.pending = true;
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
            View::Sets => &["New set"],
            View::Set(id) if id != DEFAULT => &["Rename", "Languages", "Delete"],
            View::Set(_) => &["Rename"],
            View::Languages(_) => &["Edit language codes"],
            View::Dictionary(_, _) => &[],
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
            View::Sets => self.sets.iter().map(|s| Row::new(&s.name).sub(languages(s)).chevron()).collect(),
            View::Set(_) => self
                .dictionaries
                .iter()
                .map(|d| {
                    Row::new(&d.name)
                        .sub(format!(
                            "{}{} · {}",
                            if d.disabled { "Off" } else { "On" },
                            if d.error.is_some() { " · Read error" } else { "" },
                            d.location(&self.sources)
                        ))
                        .chevron()
                })
                .collect(),
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
                .dictionary()
                .map(|d| {
                    let index = self.dictionaries.iter().position(|o| o.id == d.id).unwrap();
                    let mut rows = vec![
                        Row::new("Details").sub(&d.name).chevron(),
                        Row::new(if d.disabled { "Enable" } else { "Disable" }),
                        Row::new("Move up"),
                        Row::new("Move down"),
                        Row::new("Retry reading"),
                    ];
                    rows[2].enabled = index > 0;
                    rows[3].enabled = index + 1 < self.dictionaries.len();
                    rows
                })
                .unwrap_or_default(),
        };
        self.list.rows = rows;
        if self.pending {
            for row in &mut self.list.rows {
                row.enabled = false;
            }
        }
        self.list.place(self.list_rect, t.row2, 0);
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
            View::Set(_) => self.open(View::Sets, t),
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
        let mut preferences = crate::dictionary::sets::Preferences { sets: self.sets.clone() };
        if let Err(error) = preferences.edit(edit.clone()) {
            cx.toast(error.to_string(), None);
            return;
        }
        self.editor = None;
        self.edit(edit);
        self.layout(cx.theme);
    }
    fn toolbar(&mut self, index: usize, cx: &mut EventCx) {
        match self.view.clone() {
            View::Sets => self.start_editor(Field::New, String::new(), cx.theme),
            View::Set(set) => match index {
                0 => self.start_editor(Field::Rename(set), self.current_set().unwrap().name.clone(), cx.theme),
                1 => self.open(View::Languages(set), cx.theme),
                2 => {
                    self.edit(Edit::Delete { set });
                    self.view = View::Sets;
                    self.list.page = 0;
                    self.layout(cx.theme);
                }
                _ => {}
            },
            View::Languages(set) => {
                self.start_editor(Field::Languages(set), self.current_set().unwrap().languages.join(", "), cx.theme)
            }
            _ => {}
        }
    }
    fn language_choices(&self) -> Vec<(String, String)> {
        let mut choices: Vec<_> =
            LANGUAGES.iter().map(|(code, name)| ((*code).to_owned(), (*name).to_owned())).collect();
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
            View::Sets => self.open(View::Set(self.sets[index].id.clone()), cx.theme),
            View::Set(set) => self.open(View::Dictionary(set, self.dictionaries[index].id.clone()), cx.theme),
            View::Languages(set) => {
                let code = self.language_choices()[index].0.clone();
                let mut languages = self.current_set().unwrap().languages.clone();
                if languages.contains(&code) {
                    languages.retain(|l| l != &code);
                } else {
                    languages.push(code);
                }
                self.edit(Edit::Languages { set, languages });
                self.layout(cx.theme);
            }
            View::Dictionary(set, dictionary) => {
                let Some(d) = self.dictionary() else { return };
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
                    2 | 3 => {
                        let visible = self.dictionaries.iter().position(|o| o.id == dictionary).unwrap();
                        let neighbor = &self.dictionaries[if index == 2 { visible - 1 } else { visible + 1 }].id;
                        // Durable order includes temporarily missing dictionaries.
                        let position =
                            self.current_set().unwrap().dictionaries.iter().position(|p| p.id == *neighbor).unwrap();
                        Edit::MoveDictionary { set, dictionary, position }
                    }
                    4 => {
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
const LANGUAGES: &[(&str, &str)] = &[
    ("en", "English"),
    ("ru", "Russian"),
    ("hy", "Armenian"),
    ("ar", "Arabic"),
    ("be", "Belarusian"),
    ("bg", "Bulgarian"),
    ("zh", "Chinese"),
    ("cs", "Czech"),
    ("da", "Danish"),
    ("nl", "Dutch"),
    ("fi", "Finnish"),
    ("fr", "French"),
    ("de", "German"),
    ("el", "Greek"),
    ("he", "Hebrew"),
    ("hu", "Hungarian"),
    ("it", "Italian"),
    ("ja", "Japanese"),
    ("ko", "Korean"),
    ("la", "Latin"),
    ("mk", "Macedonian"),
    ("no", "Norwegian"),
    ("fa", "Persian"),
    ("pl", "Polish"),
    ("pt", "Portuguese"),
    ("ro", "Romanian"),
    ("sr", "Serbian"),
    ("sk", "Slovak"),
    ("es", "Spanish"),
    ("sv", "Swedish"),
    ("tr", "Turkish"),
    ("uk", "Ukrainian"),
    ("ur", "Urdu"),
    ("vi", "Vietnamese"),
    ("yi", "Yiddish"),
];
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
                "New sets copy Default’s order with every dictionary off."
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
        let note = if self.pending {
            "Checking dictionaries…"
        } else if !self.warnings.is_empty() {
            "Some folders or preferences had problems. Tap here for details."
        } else if self.list.rows.is_empty() {
            "No dictionaries found. Add a folder or copy StarDict files into Verso’s dictionaries folder."
        } else {
            "Changes save immediately. Tap a row to manage it."
        };
        cx.paragraph(note, cx.theme.small.gray(INK2), self.note, HAlign::Left);
        self.rescan.enabled = !self.pending;
        self.rescan.draw(cx);
        self.folders.draw(cx);
        if let Some(details) = &mut self.details {
            details.draw(cx);
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
        if let Some(editor) = &mut self.editor {
            if editor.keyboard.visible && editor.keyboard.rect().contains(g.pos()) {
                if let Some(input) = editor.keyboard.on_gesture(&g, cx.theme) {
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
        if self.folders.on_gesture(&g) {
            self.returning = true;
            return Action::Push(Box::new(DictionaryFolders::new(
                cx.theme,
                self.screen,
                self.store.clone(),
                self.waker.clone(),
            )));
        }
        if !self.pending && self.rescan.on_gesture(&g) {
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
        if let Some(ListEvent::Tapped(i)) = self.list.on_gesture(&g) {
            self.row(i, cx);
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
            self.dictionaries = reply.dictionaries;
            self.sources = reply.sources;
            self.warnings = reply.warnings;
            if let Some(error) = reply.change_error {
                cx.toast(error, None);
            }
            if let Some(error) = reply.save_error {
                cx.toast(format!("Couldn’t save dictionary preferences: {error}"), None);
            }

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
            frontlight: &mut None,
            light_request: &mut None,
            toast: &mut None,
            forced: &mut vec![],
            needs_redraw: &mut false,
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
    fn submit(s: &mut Dictionaries, value: &str, t: &Theme) {
        with_cx(t, s.screen, |cx| {
            s.on_text_input(&TextInput::Clear, cx);
            s.on_text_input(&TextInput::Insert(value.into()), cx);
            s.on_text_input(&TextInput::Submit, cx);
        });
        wait(s, t);
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
        fs::create_dir_all(store.dictionary_dir()).unwrap();
        for name in ["a", "b", "c"] {
            fs::write(store.dictionary_dir().join(format!("{name}.ifo")), format!("StarDict's dict ifo file\nversion=2.4.2\nbookname={name}\nwordcount=1\nidxfilesize=14\nsametypesequence=m\n")).unwrap();
            fs::write(store.dictionary_dir().join(format!("{name}.idx")), b"Hello\0\0\0\0\0\0\0\0\x07").unwrap();
            fs::write(store.dictionary_dir().join(format!("{name}.dict")), b"meaning").unwrap();
        }
        let t = Theme::for_dpi(300);
        let screen = Rect::new(0, 0, 1272, 1696);
        let mut s = Dictionaries::new(&t, screen, store.clone(), Waker::new(|| {}));
        wait(&mut s, &t);
        assert_eq!(s.list.rows.len(), 1);
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
        row(&mut s, 0, &t);
        row(&mut s, 1, &t);
        wait(&mut s, &t);
        assert!(!s.dictionary().unwrap().disabled);
        row(&mut s, 3, &t);
        wait(&mut s, &t);
        assert_eq!(s.dictionaries.iter().map(|d| d.id.as_str()).collect::<Vec<_>>(), ["b.ifo", "a.ifo", "c.ifo"]);
        row(&mut s, 2, &t);
        wait(&mut s, &t);
        assert_eq!(s.dictionaries[0].id, "a.ifo");
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
        let delete = s.toolbar[2].rect().center();
        tap(&mut s, delete, &t);
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
            let mut s = Dictionaries::new(&t, screen, store, Waker::new(|| {}));
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
}
