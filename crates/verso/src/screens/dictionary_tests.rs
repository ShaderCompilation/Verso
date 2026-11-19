use super::*;
use crate::{
    dictionary::{Preference, sets::Preferences},
    gfx::{Canvas, IconRenderer},
    platform::Waker,
    store::Store,
    ui::gesture::Direction,
};
use std::{
    fs,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};
static NEXT: AtomicU64 = AtomicU64::new(0);

fn fixture() -> (std::path::PathBuf, Store) {
    let root = std::env::temp_dir().join(format!(
        "verso-picker-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let store = Store::at(root.clone());
    store.save_dictionary_folders(&[]).unwrap();
    fs::create_dir_all(store.dictionary_dir()).unwrap();
    for i in 0..14 {
        let name = format!("{i:02}");
        let word = if i == 13 {
            "absent"
        } else if i == 0 {
            "box"
        } else {
            "boxes"
        };
        let definition = format!("Definition {i}. A long definition remains readable across many pages. ").repeat(150);
        let mut idx = word.as_bytes().to_vec();
        idx.push(0);
        idx.extend_from_slice(&0u32.to_be_bytes());
        idx.extend_from_slice(&(definition.len() as u32).to_be_bytes());
        let path = store.dictionary_dir().join(name);
        fs::write(path.with_extension("idx"), &idx).unwrap();
        fs::write(path.with_extension("dict"), definition).unwrap();
        fs::write(path.with_extension("ifo"), format!("StarDict's dict ifo file\nversion=2.4.2\nbookname=Dictionary {i}\nwordcount=1\nidxfilesize={}\nsametypesequence=m\n", idx.len())).unwrap();
    }
    fs::write(store.dictionary_dir().join("broken.ifo"), "broken").unwrap();
    let mut prefs = Preferences::default();
    prefs.sets[0].dictionaries = (0..14).map(|i| Preference { id: format!("{i:02}.ifo"), disabled: false }).collect();
    prefs.sets.push(Set {
        id: "english".into(),
        name: "English".into(),
        languages: vec!["en".into()],
        dictionaries: prefs.sets[0]
            .dictionaries
            .iter()
            .map(|d| Preference { id: d.id.clone(), disabled: d.id != "01.ifo" })
            .collect(),
    });
    fs::write(store.dictionary_preferences(), serde_json::to_vec(&prefs).unwrap()).unwrap();
    (root, store)
}
fn cx<R>(t: &Theme, screen: Rect, f: impl FnOnce(&mut EventCx) -> R) -> R {
    f(&mut EventCx {
        theme: t,
        screen,
        now: Instant::now(),
        frontlight: &mut None,
        light_request: &mut None,
        toast: &mut None,
        forced: &mut vec![],
        needs_redraw: &mut false,
        battery: None,
    })
}
fn wait(lookup: &mut Lookup, t: &Theme, screen: Rect) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while lookup.pending || !lookup.complete {
        cx(t, screen, |cx| lookup.receive(cx));
        assert!(Instant::now() < deadline, "lookup timed out");
        std::thread::yield_now();
    }
}
fn tap(lookup: &mut Lookup, point: crate::geom::Point, t: &Theme, screen: Rect) -> Option<LookupEvent> {
    cx(t, screen, |cx| {
        lookup.on_gesture(&Gesture::Down(point), cx);
        lookup.on_gesture(&Gesture::Tap(point), cx)
    })
}
fn reply(lookup: &Lookup) -> crate::dictionary::worker::Reply {
    crate::dictionary::worker::Reply {
        id: lookup.request,
        matches: lookup.matches.clone(),
        complete: true,
        preparing: None,
        unprepared: vec![],
        missing: vec![],
        dictionaries: lookup.catalog.clone(),
        sources: lookup.sources.clone(),
        warnings: vec![],
        selected: lookup.selected.clone(),
        sets: lookup.sets.clone(),
        active_set: lookup.active_set.clone(),
        change_error: None,
        word_forms: vec![],
        state: Some(lookup.state.clone()),
        save_error: None,
    }
}

fn capture(lookup: &mut Lookup, t: &Theme, screen: Rect, name: &str) {
    let Some(folder) = std::env::var_os("VERSO_DICTIONARY_CAPTURES") else { return };
    let folder = std::path::PathBuf::from(folder);
    fs::create_dir_all(&folder).unwrap();
    let mut canvas = Canvas::new(screen.w as u32, screen.h as u32);
    canvas.fill(crate::ui::theme::PAPER);
    lookup.draw(&mut DrawCx::new(&mut canvas, &mut TextRenderer::new(), &mut IconRenderer::default(), t, &mut vec![]));
    image::save_buffer(
        folder.join(format!("{}x{}-{name}.png", screen.w, screen.h)),
        canvas.gray_data(),
        screen.w as u32,
        screen.h as u32,
        image::ColorType::L8,
    )
    .unwrap();
}

#[test]
fn picker_pages_only_matches_switches_sets_and_preserves_pages_on_final_counts() {
    for (w, h, dpi) in
        [(1272, 1696, 300), (1696, 1272, 300), (1072, 1448, 300), (1448, 1072, 300), (600, 800, 167), (800, 600, 167)]
    {
        let (root, store) = fixture();
        let t = Theme::for_dpi(dpi);
        let screen = Rect::new(0, 0, w, h);
        let worker = Rc::new(Worker::new(store.clone(), Waker::new(|| {})));
        let mut lookup = Lookup::new("boxes", worker.clone(), &t, screen, None, Default::default());
        wait(&mut lookup, &t, screen);
        assert_eq!(lookup.matches.len(), 13);
        capture(&mut lookup, &t, screen, "lookup");
        assert_eq!(lookup.selected.as_deref(), Some("00.ifo"));
        assert!(lookup.matches[0].hits.forms.is_some());
        lookup.cycle(-1);
        assert!(!lookup.pending, "no wrap at the first dictionary");
        lookup.cycle(1);
        wait(&mut lookup, &t, screen);
        assert_eq!(lookup.selected.as_deref(), Some("01.ifo"));
        let mut text = TextRenderer::new();
        let mut canvas = Canvas::new(w as u32, h as u32);
        let mut icons = IconRenderer::default();
        lookup.draw(&mut DrawCx::new(&mut canvas, &mut text, &mut icons, &t, &mut vec![]));
        while lookup.paginating() {
            lookup.on_idle(&mut text);
        }
        assert!(lookup.pager.pages > 1);
        lookup.pager.page = 1;
        let final_reply = reply(&lookup);
        lookup.matches.truncate(2);
        lookup.complete = false;
        capture(&mut lookup, &t, screen, "searching");
        lookup.preparing = Some("02.ifo".into());
        capture(&mut lookup, &t, screen, "preparing");
        worker.result_for_test(final_reply);
        cx(&t, screen, |cx| lookup.receive(cx));
        assert_eq!(lookup.pager.page, 1, "final count retains the definition page");
        assert!(lookup.pages.is_some());
        assert_eq!(lookup.selected.as_deref(), Some("01.ifo"));
        assert_eq!(lookup.status(), "2 of 13");
        lookup.open_picker(&t);
        capture(&mut lookup, &t, screen, "picker");
        let p = lookup.picker.as_ref().unwrap();
        assert!(p.pager.pages > 1);
        assert!(p.list.paging().per_page >= 1);
        assert!(p.back.rect().bottom() <= screen.bottom());
        assert!(lookup.body.h > t.target);
        assert!(p.list.rows[0].sub.as_ref().unwrap().contains("base form"));
        let swipe = Gesture::Swipe { dir: Direction::Left, from: lookup.body.center(), to: lookup.body.center() };
        cx(&t, screen, |cx| lookup.on_gesture(&swipe, cx));
        assert_eq!(lookup.picker.as_ref().unwrap().list.page, 1);
        capture(&mut lookup, &t, screen, "picker-second");
        let index = lookup.picker.as_ref().unwrap().list.paging().range(1).start;
        let c = lookup.sheet.content(&t);
        tap(&mut lookup, crate::geom::Point::new(c.center().x, c.y + t.target + t.mm(2.0) + t.row2 / 2), &t, screen);
        assert!(lookup.picker.is_none());
        wait(&mut lookup, &t, screen);
        assert_eq!(lookup.selected, Some(format!("{index:02}.ifo")));
        lookup.choose(12);
        wait(&mut lookup, &t, screen);
        lookup.cycle(1);
        assert!(!lookup.pending, "no wrap at the last dictionary");
        // Returning from management without edits keeps both selection and page.
        lookup.returning = true;
        cx(&t, screen, |cx| lookup.receive(cx));
        assert_eq!(lookup.selected.as_deref(), Some("12.ifo"));
        lookup.open_picker(&t);
        let set_button = lookup.picker.as_ref().unwrap().set.rect().center();
        tap(&mut lookup, set_button, &t, screen);
        assert!(lookup.picker.as_ref().unwrap().sets);
        capture(&mut lookup, &t, screen, "sets");
        let event = tap(
            &mut lookup,
            crate::geom::Point::new(c.center().x, c.y + t.target + t.mm(2.0) + t.row2 * 3 / 2),
            &t,
            screen,
        );
        assert!(matches!(event, Some(LookupEvent::Set(id)) if id == "english"));
        wait(&mut lookup, &t, screen);
        assert_eq!(lookup.active_set, "english");
        capture(&mut lookup, &t, screen, "switched-set");
        assert_eq!(lookup.matches.len(), 1);
        assert_eq!(lookup.selection.book_set.as_deref(), Some("english"));
        // Settings edits invalidate a retained lookup without rebuilding the reader.
        let settings = Worker::new(store.clone(), Waker::new(|| {}));
        settings.edit(crate::dictionary::sets::Edit::Disabled {
            set: "english".into(),
            dictionary: "01.ifo".into(),
            disabled: true,
        });
        cx(&t, screen, |cx| lookup.receive(cx));
        wait(&mut lookup, &t, screen);
        assert_eq!(lookup.state, State::Disabled);
        assert!(lookup.matches.is_empty());
        settings.edit(crate::dictionary::sets::Edit::Disabled {
            set: "english".into(),
            dictionary: "01.ifo".into(),
            disabled: false,
        });
        cx(&t, screen, |cx| lookup.receive(cx));
        wait(&mut lookup, &t, screen);
        // New submission starts at preference order and ignores an old selection.
        lookup.field.edit(&TextInput::Clear);
        lookup.field.edit(&TextInput::Insert("absent".into()));
        lookup.submit(false);
        lookup.field.edit(&TextInput::Clear);
        lookup.field.edit(&TextInput::Insert("boxes".into()));
        lookup.submit(false);
        wait(&mut lookup, &t, screen);
        assert_eq!(lookup.selected.as_deref(), Some("01.ifo"));
        worker.flush().unwrap();
        drop(lookup);
        drop(worker);
        let _ = fs::remove_dir_all(root);
    }
}

#[test]
fn a_late_first_result_cannot_replace_a_new_picker_choice_or_show_diagnostics_beside_text() {
    let (root, store) = fixture();
    let t = Theme::for_dpi(300);
    let screen = Rect::new(0, 0, 1272, 1696);
    let worker = Rc::new(Worker::new(store, Waker::new(|| {})));
    let mut lookup = Lookup::new("boxes", worker.clone(), &t, screen, None, Default::default());
    wait(&mut lookup, &t, screen);
    let old = reply(&lookup);
    // Model the two publications with an intervening user choice.
    lookup.selected = Some("01.ifo".into());
    lookup.pending = true;
    worker.result_for_test(old);
    cx(&t, screen, |cx| lookup.receive(cx));
    assert_eq!(lookup.selected.as_deref(), Some("01.ifo"));
    assert!(lookup.pending);
    let blocks = content(&lookup.state, false, &["Broken dictionary".into()], &t);
    assert!(blocks.iter().all(
        |b| !matches!(b, StyledBlock::Paragraph(p) if p.text.contains("Broken") || p.text.contains("incomplete"))
    ));
    let blocks = content(&State::NoResult, false, &["Broken dictionary".into()], &t);
    assert!(blocks.iter().any(|b| matches!(b, StyledBlock::Paragraph(p) if p.text.contains("Search incomplete"))));
    worker.flush().unwrap();
    drop(lookup);
    drop(worker);
    let _ = fs::remove_dir_all(root);
}
