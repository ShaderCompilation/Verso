use super::*;
use crate::{
    dictionary::{Preference, sets::Preferences},
    gfx::Icon,
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
/// The middle of the picker's visible row `i`.
fn picker_row(lookup: &Lookup, i: i32, t: &Theme) -> crate::geom::Point {
    let set = lookup.picker.as_ref().unwrap().set.rect();
    crate::geom::Point::new(set.center().x, set.bottom() + t.mm(2.0) + t.row2 * i + t.row2 / 2)
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
        assert_eq!(lookup.status(), "2 / 13");
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
        let row = picker_row(&lookup, 0, &t);
        tap(&mut lookup, row, &t, screen);
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
        let row = picker_row(&lookup, 1, &t);
        let event = tap(&mut lookup, row, &t, screen);
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

#[test]
fn the_compact_card_stands_away_from_the_word_and_expands_on_demand() {
    let root = std::env::temp_dir().join(format!("verso-lookup-geometry-{}", std::process::id()));
    let worker = Rc::new(Worker::new(Store::at(root.clone()), Waker::new(|| {})));
    for (w, h, dpi) in [(1272, 1696, 300), (1072, 1448, 300), (600, 800, 167)] {
        let t = Theme::for_dpi(dpi);
        let screen = Rect::new(0, 0, w, h);
        let compact = (h * 46 / 100).max(t.mm(64.0)).min(h * 52 / 100);
        let upper = Rect::new(w / 3, h / 4, t.mm(15.0), t.mm(4.0));
        let lower = Rect::new(w / 3, h * 3 / 4, t.mm(15.0), t.mm(4.0));
        for (word, top) in [(None, false), (Some(upper), false), (Some(lower), true)] {
            let mut lookup = Lookup::new("word", worker.clone(), &t, screen, word, Default::default());
            let card = lookup.rect(&t);
            let expected = if top { Rect::new(0, 0, w, compact) } else { Rect::new(0, h - compact, w, compact) };
            assert_eq!(card, expected, "{w}×{h} {word:?}");
            assert!(word.is_none_or(|word| card.intersect(&word).is_none()), "the word stays visible");
            // Two header rows, the definition and the action strip all fit.
            assert!(lookup.body_full.h >= 3 * t.dictionary_body.line_height() as i32, "{w}×{h}");
            assert!(lookup.heading.y >= card.y && lookup.actions[3].rect().bottom() <= card.bottom());
            assert!(
                lookup.switcher.bottom() < lookup.body_full.y && lookup.body_full.bottom() < lookup.actions[0].rect().y
            );
            assert!(lookup.actions.iter().all(|b| b.rect().w >= t.mm(9.0)), "touch targets on {w}×{h}");
            assert_eq!(lookup.expand.icon, Icon::Expand);
            {
                let p = lookup.expand.rect().center();
                tap(&mut lookup, p, &t, screen)
            };
            assert!(lookup.expanded);
            assert_eq!(lookup.rect(&t), Rect::new(0, h - h * 4 / 5, w, h * 4 / 5));
            lookup.place(&t);
            assert_eq!(lookup.expand.icon, Icon::Collapse);
            {
                let p = lookup.expand.rect().center();
                tap(&mut lookup, p, &t, screen)
            };
            assert_eq!(lookup.rect(&t), expected, "collapsing restores the original edge");
            capture(&mut lookup, &t, screen, if top { "compact-top" } else { "compact-bottom" });
        }
    }
    // Landscape: a side panel away from the word, already full height.
    let t = Theme::for_dpi(300);
    let screen = Rect::new(0, 0, 1696, 1272);
    let width = (1696 * 52 / 100).max(t.mm(72.0));
    for (x, right) in [(200, true), (1300, false)] {
        let word = Rect::new(x, 600, 150, 50);
        let lookup = Lookup::new("word", worker.clone(), &t, screen, Some(word), Default::default());
        let card = lookup.rect(&t);
        assert_eq!(card, Rect::new(if right { 1696 - width } else { 0 }, 0, width, 1272));
        assert!(card.intersect(&word).is_none());
        assert_eq!(lookup.expand.rect().w, 0, "no expand button in landscape");
        // Too narrow for four "Highlight"-wide buttons in a row: two rows.
        assert!(lookup.actions[2].rect().y > lookup.actions[0].rect().bottom());
        assert!(lookup.actions.iter().all(|b| b.rect().w >= t.mm(17.0)));
        assert!(lookup.actions[0].rect().x >= card.x && lookup.actions[3].rect().right() <= card.right());
    }
    drop(worker);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn picker_and_typing_expand_temporarily_and_actions_are_direct() {
    let (root, store) = fixture();
    let t = Theme::for_dpi(300);
    let screen = Rect::new(0, 0, 1272, 1696);
    let worker = Rc::new(Worker::new(store, Waker::new(|| {})));
    let word = Rect::new(400, 1300, 200, 50);
    let mut lookup = Lookup::new("boxes", worker.clone(), &t, screen, Some(word), Default::default());
    wait(&mut lookup, &t, screen);
    let compact = lookup.rect(&t);
    assert_eq!(compact.y, 0, "a word in the lower half puts the card at the top");
    let expanded = Rect::new(0, 1696 - 1696 * 4 / 5, 1272, 1696 * 4 / 5);
    // The switcher opens the picker in the expanded layout; Back restores.
    {
        let p = lookup.switcher.center();
        tap(&mut lookup, p, &t, screen)
    };
    assert!(lookup.picker.is_some());
    assert_eq!(lookup.rect(&t), expanded);
    assert!(!lookup.allows_handles());
    let back = lookup.picker.as_ref().unwrap().back.rect();
    assert!(expanded.contains(back.center()) && lookup.picker.as_ref().unwrap().set.rect().y > lookup.heading.y);
    tap(&mut lookup, back.center(), &t, screen);
    assert!(lookup.picker.is_none());
    assert_eq!(lookup.rect(&t), compact);
    // The header word opens the field and keyboard, also expanded.
    {
        let p = lookup.heading.center();
        tap(&mut lookup, p, &t, screen)
    };
    assert!(lookup.keyboard.visible && lookup.field.focused);
    assert_eq!(lookup.rect(&t), expanded);
    capture(&mut lookup, &t, screen, "editing");
    assert!(lookup.body_full.bottom() <= lookup.keyboard.rect().y, "the definition stays above the keyboard");
    assert!(lookup.search.rect().bottom() < lookup.keyboard.rect().y);
    cx(&t, screen, |cx| lookup.on_text_input(&TextInput::Dismiss, cx));
    assert_eq!(lookup.rect(&t), compact);
    // A tap above the keyboard but away from the field puts it away too.
    {
        let p = lookup.heading.center();
        tap(&mut lookup, p, &t, screen)
    };
    tap(&mut lookup, crate::geom::Point::new(600, 300), &t, screen);
    assert!(!lookup.keyboard.visible);
    assert!(lookup.allows_handles());
    // The action strip reports its actions directly; Search takes the query.
    let events: Vec<_> = (0..4)
        .map(|i| {
            let p = lookup.actions[i].rect().center();
            tap(&mut lookup, p, &t, screen)
        })
        .collect();
    assert!(matches!(
        events.as_slice(),
        [Some(LookupEvent::Highlight), Some(LookupEvent::Note), Some(LookupEvent::Search(q)), Some(LookupEvent::Extend)]
            if q == "boxes"
    ));
    // A miss offers the query field and Manage dictionaries at once.
    {
        let p = lookup.heading.center();
        tap(&mut lookup, p, &t, screen)
    };
    cx(&t, screen, |cx| {
        lookup.on_text_input(&TextInput::Clear, cx);
        lookup.on_text_input(&TextInput::Insert("nothing".into()), cx);
        lookup.on_text_input(&TextInput::Submit, cx);
    });
    assert!(!lookup.keyboard.visible);
    wait(&mut lookup, &t, screen);
    assert_eq!(lookup.state, State::NoResult);
    lookup.place(&t);
    assert!(lookup.edit_row() && lookup.rect(&t) == compact);
    // With the incomplete-search notice (the fixture's broken dictionary)
    // the text needs more room than the compact card leaves: it grows once.
    let draw = |lookup: &mut Lookup| {
        let mut canvas = Canvas::new(screen.w as u32, screen.h as u32);
        let (mut text, mut icons) = (TextRenderer::new(), IconRenderer::default());
        lookup.draw(&mut DrawCx::new(&mut canvas, &mut text, &mut icons, &t, &mut vec![]));
    };
    assert!(!lookup.warnings.is_empty());
    draw(&mut lookup);
    assert!(lookup.layout_error.is_none());
    assert!(lookup.expanded && lookup.grew);
    capture(&mut lookup, &t, screen, "no-result");
    // Collapsed again by hand, it stays compact and pages by swipe alone.
    let p = lookup.expand.rect().center();
    tap(&mut lookup, p, &t, screen);
    draw(&mut lookup);
    assert!(lookup.layout_error.is_none() && !lookup.expanded && !lookup.paged);
    assert_eq!(lookup.rect(&t), compact);
    let field = crate::geom::Point::new(lookup.search.rect().x - t.mm(10.0), lookup.search.rect().center().y);
    assert!(compact.contains(field) && field.y < lookup.manage.rect().y);
    tap(&mut lookup, field, &t, screen);
    assert!(lookup.keyboard.visible, "tapping the field opens the keyboard");
    cx(&t, screen, |cx| lookup.on_text_input(&TextInput::Dismiss, cx));
    let event = {
        let p = lookup.manage.rect().center();
        tap(&mut lookup, p, &t, screen)
    };
    assert!(matches!(event, Some(LookupEvent::Manage(_))));
    worker.flush().unwrap();
    drop(lookup);
    drop(worker);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn the_pager_shows_only_for_definitions_longer_than_a_page() {
    let (root, store) = fixture();
    let screen = Rect::new(0, 0, 1272, 1696);
    let t = Theme::for_dpi(300);
    let worker = Rc::new(Worker::new(store, Waker::new(|| {})));
    let mut lookup = Lookup::new("boxes", worker.clone(), &t, screen, None, Default::default());
    wait(&mut lookup, &t, screen);
    let (mut text, mut icons) = (TextRenderer::new(), IconRenderer::default());
    let mut canvas = Canvas::new(screen.w as u32, screen.h as u32);
    let mut draw = |lookup: &mut Lookup| {
        canvas.fill(crate::ui::theme::PAPER);
        lookup.draw(&mut DrawCx::new(&mut canvas, &mut text, &mut icons, &t, &mut vec![]));
    };
    draw(&mut lookup);
    assert!(lookup.paged && lookup.pager.visible());
    assert_eq!(lookup.body, lookup.body_paged);
    assert_eq!(lookup.pages.as_ref().unwrap().size(), (lookup.body.w, lookup.body.h));
    // A short entry takes the pager's row for text.
    lookup.state = State::Definition {
        entries: vec![crate::dictionary::Entry { headword: "box".into(), blocks: vec![Block::plain("A container.")] }],
        base_forms: None,
    };
    lookup.pages = None;
    draw(&mut lookup);
    assert!(!lookup.paged && !lookup.pager.visible());
    assert_eq!(lookup.body, lookup.body_full);
    assert!(lookup.body_full.h > lookup.body_paged.h + t.mm(6.0));
    capture(&mut lookup, &t, screen, "one-page");
    // Expanding lays the entry out again at the new size.
    let compact = lookup.body.h;
    lookup.expanded = true;
    draw(&mut lookup);
    assert_eq!(lookup.pages.as_ref().unwrap().size(), (lookup.body.w, lookup.body.h));
    assert!(lookup.body.h > compact + t.mm(40.0));
    capture(&mut lookup, &t, screen, "expanded");
    worker.flush().unwrap();
    drop(lookup);
    drop(worker);
    let _ = fs::remove_dir_all(root);
}
