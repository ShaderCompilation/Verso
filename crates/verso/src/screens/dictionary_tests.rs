use super::*;
use crate::{
    dictionary::{Preference, sets::Preferences},
    gfx::Icon,
    gfx::{Canvas, IconRenderer},
    net::tests::{Reply as Http, Server},
    online::{Service, wikipedia::Edition},
    platform::{Waker, wifi::Link},
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

/// Wikipedia always lists its hits; Translate always answers.
fn online() -> Server {
    Server::answering(|request| {
        let reply = if request.path.starts_with("/translate") {
            r#"[[["Übersetzung","x"]],null,"en"]"#.to_string()
        } else {
            let hit = |title| format!(r#"{{"ns":0,"title":"{title}","snippet":""}}"#);
            format!(
                r#"{{"batchcomplete":true,"query":{{"searchinfo":{{"totalhits":2}},"search":[{},{}]}}}}"#,
                hit("First match"),
                hit("Second match")
            )
        };
        Http::Ok(reply.into_bytes())
    })
}
/// Waits for the tab shown to have its reply.
fn answered(lookup: &mut Lookup, t: &Theme, screen: Rect) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while lookup.tabs.waiting() {
        cx(t, screen, |cx| lookup.receive(cx));
        assert!(Instant::now() < deadline, "no online reply");
        std::thread::yield_now();
    }
}
/// The text of the online tab shown.
fn online_text(lookup: &Lookup, t: &Theme) -> String {
    let blocks = lookup.tabs.content(t);
    let texts =
        blocks.iter().filter_map(|b| if let StyledBlock::Paragraph(p) = b { Some(p.text.as_str()) } else { None });
    texts.collect::<Vec<_>>().join("\n")
}

#[test]
fn tabs_ask_when_first_shown_and_once_per_word() {
    let (root, store) = fixture();
    let t = Theme::for_dpi(300);
    let screen = Rect::new(0, 0, 1272, 1696);
    let worker = Rc::new(Worker::new(store, Waker::new(|| {})));
    let server = online();
    let service = Service::for_test(&server.base, Duration::from_secs(5), Box::new(|| Ok(Link::Connected)));
    let tabs = |tab, translate: Option<&str>| {
        let languages = Languages { wikipedia: Edition::ENGLISH, translate: translate.map(Into::into) };
        Tabs::new(tab, &service, Waker::new(|| {}), languages)
    };
    let hits = || server.requests.lock().unwrap().len();
    let go = |lookup: &mut Lookup, tab| {
        let p = lookup.tab_point(tab);
        tap(lookup, p, &t, screen);
        assert_eq!(lookup.tabs.current(), tab);
        answered(lookup, &t, screen);
    };
    let mut lookup =
        Lookup::new("boxes", worker.clone(), &t, screen, None, Default::default(), tabs(Tab::Dictionary, Some("de")));
    wait(&mut lookup, &t, screen);
    let mut text = TextRenderer::new();
    draw_hints(&mut lookup, &t, screen, &mut text);
    let swipe = Gesture::Swipe { dir: Direction::Left, from: screen.center(), to: screen.center() };
    cx(&t, screen, |cx| lookup.on_gesture(&swipe, cx));
    draw_hints(&mut lookup, &t, screen, &mut text);
    assert_eq!((lookup.pager.page, hits()), (1, 0), "Dictionary stays offline");
    let (compact, request) = (lookup.rect(&t), lookup.request);
    // Wikipedia asks, and grows the card; Dictionary comes back as it was.
    go(&mut lookup, Tab::Wikipedia);
    assert_eq!(hits(), 1);
    assert_eq!(online_text(&lookup, &t), "First match\nSecond match");
    assert_eq!(lookup.rect(&t).h, 1696 * 4 / 5);
    capture(&mut lookup, &t, screen, "wikipedia");
    draw_hints(&mut lookup, &t, screen, &mut text);
    // Its words can't be looked up.
    let body = lookup.body;
    tap(&mut lookup, crate::geom::Point::new(body.x + t.mm(3.0), body.y + t.mm(2.0)), &t, screen);
    draw_hints(&mut lookup, &t, screen, &mut text);
    assert_eq!(lookup.depth(), 0);
    go(&mut lookup, Tab::Dictionary);
    draw_hints(&mut lookup, &t, screen, &mut text);
    assert_eq!((lookup.rect(&t), lookup.request, lookup.pager.page), (compact, request, 1));
    go(&mut lookup, Tab::Translate);
    assert_eq!((lookup.rect(&t), hits()), (compact, 2));
    assert!(online_text(&lookup, &t).starts_with("Übersetzung"));
    capture(&mut lookup, &t, screen, "translate");
    // Seen before: no request.
    go(&mut lookup, Tab::Wikipedia);
    go(&mut lookup, Tab::Translate);
    go(&mut lookup, Tab::Wikipedia);
    assert_eq!(hits(), 2);
    // An edited query is a step, asked only on the tab shown; tabs hide while typing.
    cx(&t, screen, |cx| {
        lookup.start_editing();
        lookup.place(&t);
        assert_eq!(lookup.tabs.rect(), Rect::default());
        lookup.on_text_input(&TextInput::Clear, cx);
        lookup.on_text_input(&TextInput::Insert("fox".into()), cx);
        lookup.on_text_input(&TextInput::Submit, cx);
    });
    answered(&mut lookup, &t, screen);
    assert_eq!((lookup.depth(), hits(), lookup.tabs.current()), (1, 3, Tab::Wikipedia));
    assert!(!lookup.asked && lookup.request == request, "the dictionaries weren't asked");
    assert_eq!(server.requests.lock().unwrap()[2].param("srsearch").as_deref(), Some("fox"));
    lookup.place(&t);
    go(&mut lookup, Tab::Dictionary);
    wait(&mut lookup, &t, screen);
    assert_eq!(lookup.state, State::NoResult);
    // ← returns to the tab the word was on, without asking again.
    lookup.place(&t);
    let back = lookup.back.rect().center();
    tap(&mut lookup, back, &t, screen);
    assert_eq!((lookup.tabs.current(), lookup.query.as_str()), (Tab::Wikipedia, "boxes"));
    assert!(!lookup.tabs.waiting() && !online_text(&lookup, &t).is_empty());
    go(&mut lookup, Tab::Dictionary);
    wait(&mut lookup, &t, screen);
    draw_hints(&mut lookup, &t, screen, &mut text);
    assert_eq!(lookup.pager.page, 1);
    assert_eq!(hits(), 3);
    // Opened on Wikipedia, the dictionaries wait. Without a language chosen,
    // Translate asks for one and sends nothing.
    let mut lookup =
        Lookup::new("boxes", worker.clone(), &t, screen, None, Default::default(), tabs(Tab::Wikipedia, None));
    assert!(!lookup.asked);
    answered(&mut lookup, &t, screen);
    go(&mut lookup, Tab::Translate);
    assert_eq!(online_text(&lookup, &t), "Translate into which language?");
    assert_eq!(hits(), 4);
    go(&mut lookup, Tab::Dictionary);
    wait(&mut lookup, &t, screen);
    assert_eq!(lookup.matches.len(), 13);
    worker.flush().unwrap();
    drop(lookup);
    drop(worker);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn a_long_passage_opens_on_translate_and_cannot_be_edited_or_looked_up() {
    let root = std::env::temp_dir().join(format!("verso-lookup-long-{}", std::process::id()));
    let worker = Rc::new(Worker::new(Store::at(root.clone()), Waker::new(|| {})));
    let t = Theme::for_dpi(300);
    let screen = Rect::new(0, 0, 1272, 1696);
    // 120 characters, trimmed, is still a word.
    let longest = format!(" {} ", "ж".repeat(QUERY_LIMIT));
    let lookup =
        Lookup::new(&longest, worker.clone(), &t, screen, None, Default::default(), Tabs::offline(Tab::Dictionary));
    assert!(lookup.asked && lookup.tabs.current() == Tab::Dictionary);
    let long = "words ".repeat(QUERY_LIMIT / 6 + 1);
    let mut lookup =
        Lookup::new(&long, worker.clone(), &t, screen, None, Default::default(), Tabs::offline(Tab::Dictionary));
    assert!(!lookup.asked && lookup.tabs.current() == Tab::Translate);
    for tab in [Tab::Dictionary, Tab::Wikipedia] {
        let p = lookup.tab_point(tab);
        tap(&mut lookup, p, &t, screen);
        assert_eq!(lookup.tabs.current(), Tab::Translate, "{tab:?} ignores taps");
    }
    let heading = lookup.heading.center();
    tap(&mut lookup, heading, &t, screen);
    assert!(!lookup.keyboard.visible);
    capture(&mut lookup, &t, screen, "long-passage");
    // Typing stops at the limit.
    let mut lookup =
        Lookup::new("fox", worker.clone(), &t, screen, None, Default::default(), Tabs::offline(Tab::Dictionary));
    cx(&t, screen, |cx| {
        lookup.start_editing();
        lookup.on_text_input(&TextInput::Insert("ж".repeat(QUERY_LIMIT)), cx);
    });
    assert_eq!(lookup.field.value().chars().count(), QUERY_LIMIT);
    assert!(!too_long(lookup.field.value()));
    worker.cancel();
    drop(lookup);
    drop(worker);
    let _ = fs::remove_dir_all(root);
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
        let mut lookup =
            Lookup::new("boxes", worker.clone(), &t, screen, None, Default::default(), Tabs::offline(Tab::Dictionary));
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
    let mut lookup =
        Lookup::new("boxes", worker.clone(), &t, screen, None, Default::default(), Tabs::offline(Tab::Dictionary));
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
        let compact = (h * 54 / 100).max(t.mm(72.0)).min(h * 60 / 100);
        let upper = Rect::new(w / 3, h / 4, t.mm(15.0), t.mm(4.0));
        let lower = Rect::new(w / 3, h * 3 / 4, t.mm(15.0), t.mm(4.0));
        for (word, top) in [(None, false), (Some(upper), false), (Some(lower), true)] {
            let mut lookup = Lookup::new(
                "word",
                worker.clone(),
                &t,
                screen,
                word,
                Default::default(),
                Tabs::offline(Tab::Dictionary),
            );
            let card = lookup.rect(&t);
            let expected = if top { Rect::new(0, 0, w, compact) } else { Rect::new(0, h - compact, w, compact) };
            assert_eq!(card, expected, "{w}×{h} {word:?}");
            assert!(word.is_none_or(|word| card.intersect(&word).is_none()), "the word stays visible");
            // Three header rows, the definition and the action strip all fit.
            assert!(lookup.body_full.h >= 3 * t.dictionary_body.line_height() as i32, "{w}×{h}");
            assert!(lookup.heading.bottom() < lookup.tabs.rect().y && lookup.tabs.rect().bottom() < lookup.switcher.y);
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
            let tall = Rect::new(0, if top { 0 } else { h - h * 4 / 5 }, w, h * 4 / 5);
            assert_eq!(lookup.rect(&t), tall, "grows from its own edge");
            lookup.place(&t);
            assert_eq!(lookup.expand.icon, Icon::Collapse);
            {
                let p = lookup.expand.rect().center();
                tap(&mut lookup, p, &t, screen)
            };
            assert_eq!(lookup.rect(&t), expected, "collapsing restores the original edge");
            // Wikipedia opens tall at the same edge; pickers and typing stay there.
            let wikipedia = lookup.tab_point(Tab::Wikipedia);
            tap(&mut lookup, wikipedia, &t, screen);
            assert_eq!(lookup.rect(&t), tall);
            let collapse = lookup.expand.rect().center();
            assert!(tap(&mut lookup, collapse, &t, screen).is_none(), "its size isn't saved");
            assert_eq!(lookup.rect(&t), expected);
            let dictionary = lookup.tab_point(Tab::Dictionary);
            tap(&mut lookup, dictionary, &t, screen);
            assert_eq!(lookup.rect(&t), expected, "Dictionary keeps its own size");
            lookup.open_picker(&t);
            assert_eq!(lookup.rect(&t), tall);
            lookup.picker = None;
            lookup.start_editing();
            let keyboard = lookup.keyboard.rect().y;
            let typing = if top { Rect::new(0, 0, w, keyboard) } else { tall };
            assert_eq!(lookup.rect(&t), typing);
            lookup.place(&t);
            assert!(lookup.body_full.bottom() <= keyboard && lookup.search.rect().bottom() < keyboard, "{w}×{h}");
            lookup.stop_typing();
            lookup.place(&t);
            capture(&mut lookup, &t, screen, if top { "compact-top" } else { "compact-bottom" });
        }
    }
    // Landscape: a side panel away from the word, already full height.
    let t = Theme::for_dpi(300);
    let screen = Rect::new(0, 0, 1696, 1272);
    let width = (1696 * 52 / 100).max(t.mm(72.0));
    for (x, right) in [(200, true), (1300, false)] {
        let word = Rect::new(x, 600, 150, 50);
        let lookup = Lookup::new(
            "word",
            worker.clone(),
            &t,
            screen,
            Some(word),
            Default::default(),
            Tabs::offline(Tab::Dictionary),
        );
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
    let mut lookup = Lookup::new(
        "boxes",
        worker.clone(),
        &t,
        screen,
        Some(word),
        Default::default(),
        Tabs::offline(Tab::Dictionary),
    );
    wait(&mut lookup, &t, screen);
    let compact = lookup.rect(&t);
    assert_eq!(compact.y, 0, "a word in the lower half puts the card at the top");
    let expanded = Rect::new(0, 0, 1272, 1696 * 4 / 5);
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
    assert_eq!(lookup.rect(&t), Rect::new(0, 0, 1272, lookup.keyboard.rect().y));
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
    // The edited word is a step of the chain: no book actions.
    assert_eq!(lookup.depth(), 1);
    lookup.place(&t);
    assert!(lookup.actions_divider.is_none());
    // A miss for the book's own word keeps the actions.
    worker.flush().unwrap();
    let mut lookup = Lookup::new(
        "nothing",
        worker.clone(),
        &t,
        screen,
        Some(word),
        Default::default(),
        Tabs::offline(Tab::Dictionary),
    );
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
    let mut lookup =
        Lookup::new("boxes", worker.clone(), &t, screen, None, Default::default(), Tabs::offline(Tab::Dictionary));
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
    assert!(lookup.body.h > compact + t.mm(30.0));
    capture(&mut lookup, &t, screen, "expanded");
    worker.flush().unwrap();
    drop(lookup);
    drop(worker);
    let _ = fs::remove_dir_all(root);
}

/// A store with one small dictionary per (word, definition).
fn words(entries: &[(&str, &str)]) -> (std::path::PathBuf, Store) {
    let root = std::env::temp_dir().join(format!(
        "verso-chain-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let store = Store::at(root.clone());
    store.save_dictionary_folders(&[]).unwrap();
    fs::create_dir_all(store.dictionary_dir()).unwrap();
    for (i, (word, definition)) in entries.iter().enumerate() {
        let mut idx = word.as_bytes().to_vec();
        idx.push(0);
        idx.extend_from_slice(&0u32.to_be_bytes());
        idx.extend_from_slice(&(definition.len() as u32).to_be_bytes());
        let path = store.dictionary_dir().join(format!("{i:02}"));
        fs::write(path.with_extension("idx"), &idx).unwrap();
        fs::write(path.with_extension("dict"), definition).unwrap();
        fs::write(path.with_extension("ifo"), format!("StarDict's dict ifo file\nversion=2.4.2\nbookname=Words {i}\nwordcount=1\nidxfilesize={}\nsametypesequence=m\n", idx.len())).unwrap();
    }
    (root, store)
}

/// Draws the lookup; returns the refresh hints it asked for.
fn draw_hints(lookup: &mut Lookup, t: &Theme, screen: Rect, text: &mut TextRenderer) -> Vec<crate::platform::Update> {
    let mut canvas = Canvas::new(screen.w as u32, screen.h as u32);
    canvas.fill(crate::ui::theme::PAPER);
    let mut hints = vec![];
    lookup.draw(&mut DrawCx::new(&mut canvas, text, &mut IconRenderer::default(), t, &mut hints));
    hints
}

/// Where `word` is on the definition's current page.
fn find_word(lookup: &mut Lookup, word: &str, text: &mut TextRenderer) -> crate::geom::Point {
    let body = lookup.body;
    let page = lookup.pager.page;
    let pages = lookup.pages.as_mut().expect("laid out");
    for y in (body.y..body.bottom()).step_by(8) {
        for x in (body.x..body.right()).step_by(8) {
            let p = crate::geom::Point::new(x, y);
            if text.hit_test_styled_page(pages, page, body, p, 0).is_some_and(|hit| hit.query == word) {
                return p;
            }
        }
    }
    panic!("{word} is not on the page");
}

#[test]
fn words_in_a_definition_look_up_on_tap_or_long_press_but_not_on_swipes() {
    let (root, store) = words(&[("box", "A container, often a crate."), ("crate", "A wooden box.")]);
    let t = Theme::for_dpi(300);
    let screen = Rect::new(0, 0, 1272, 1696);
    let worker = Rc::new(Worker::new(store, Waker::new(|| {})));
    let mut text = TextRenderer::new();
    let mut lookup =
        Lookup::new("box", worker.clone(), &t, screen, None, Default::default(), Tabs::offline(Tab::Dictionary));
    wait(&mut lookup, &t, screen);
    draw_hints(&mut lookup, &t, screen, &mut text);
    let crate_at = find_word(&mut lookup, "crate", &mut text);
    // Down shows the word inverted with a fast refresh on the next draw.
    cx(&t, screen, |cx| lookup.on_gesture(&Gesture::Down(crate_at), cx));
    let hints = draw_hints(&mut lookup, &t, screen, &mut text);
    let Some(WordPress { hit: Some(Some(hit)), .. }) = &lookup.word_press else { panic!("no pressed word") };
    assert_eq!(hit.query, "crate");
    assert!(
        hit.rects
            .iter()
            .all(|r| hints.iter().any(|h| h.mode == crate::platform::RefreshMode::Fast && h.rect.contains(r.center())))
    );
    // Sliding away lets go; the swipe turns pages, never looks up.
    let away = crate::geom::Point::new(crate_at.x + t.mm(15.0), crate_at.y);
    cx(&t, screen, |cx| {
        lookup.on_gesture(&Gesture::Move(away), cx);
        assert!(lookup.word_press.is_none());
        lookup.on_gesture(&Gesture::Swipe { dir: Direction::Left, from: crate_at, to: away }, cx);
    });
    draw_hints(&mut lookup, &t, screen, &mut text);
    assert_eq!((lookup.depth(), lookup.query.as_str()), (0, "box"));
    // A quick tap, released before any draw: the next draw finds the word.
    tap(&mut lookup, crate_at, &t, screen);
    assert_eq!(lookup.depth(), 0);
    draw_hints(&mut lookup, &t, screen, &mut text);
    assert_eq!((lookup.depth(), lookup.query.as_str()), (1, "crate"));
    wait(&mut lookup, &t, screen);
    draw_hints(&mut lookup, &t, screen, &mut text);
    // Depth 1: ← and the previous word in the header, no book actions.
    assert!(lookup.back.rect().w > 0 && lookup.heading.x > lookup.back.rect().right());
    assert!(lookup.actions_divider.is_none() && !lookup.allows_handles());
    capture(&mut lookup, &t, screen, "chained");
    // Tapping the word shown (its headword) is no new step.
    let headword = find_word(&mut lookup, "crate", &mut text);
    tap(&mut lookup, headword, &t, screen);
    draw_hints(&mut lookup, &t, screen, &mut text);
    assert_eq!(lookup.depth(), 1);
    // A long press released in place looks up too, once the word is found.
    let box_at = find_word(&mut lookup, "box", &mut text);
    cx(&t, screen, |cx| {
        lookup.on_gesture(&Gesture::Down(box_at), cx);
        lookup.on_gesture(&Gesture::LongPress(box_at), cx);
    });
    draw_hints(&mut lookup, &t, screen, &mut text);
    let jitter = crate::geom::Point::new(box_at.x + 2, box_at.y + 1);
    cx(&t, screen, |cx| {
        lookup.on_gesture(&Gesture::Move(jitter), cx);
        lookup.on_gesture(&Gesture::End(jitter), cx);
    });
    assert_eq!((lookup.depth(), lookup.query.as_str()), (2, "box"));
    assert_eq!(lookup.history.last().unwrap().query, "crate");
    // × closes straight to the book at any depth.
    let close = lookup.close.rect().center();
    assert!(matches!(tap(&mut lookup, close, &t, screen), Some(LookupEvent::Close)));
    worker.flush().unwrap();
    drop(lookup);
    drop(worker);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn back_restores_the_dictionary_and_page_at_once_or_asks_again_after_a_rescan() {
    let (root, store) = fixture();
    let t = Theme::for_dpi(300);
    let screen = Rect::new(0, 0, 1272, 1696);
    let worker = Rc::new(Worker::new(store, Waker::new(|| {})));
    let mut text = TextRenderer::new();
    let mut lookup =
        Lookup::new("boxes", worker.clone(), &t, screen, None, Default::default(), Tabs::offline(Tab::Dictionary));
    wait(&mut lookup, &t, screen);
    lookup.cycle(1);
    wait(&mut lookup, &t, screen);
    let chosen = lookup.selected.clone();
    assert!(chosen.is_some());
    draw_hints(&mut lookup, &t, screen, &mut text);
    let swipe = Gesture::Swipe { dir: Direction::Left, from: screen.center(), to: screen.center() };
    for _ in 0..2 {
        cx(&t, screen, |cx| lookup.on_gesture(&swipe, cx));
        draw_hints(&mut lookup, &t, screen, &mut text);
    }
    assert_eq!(lookup.pager.page, 2);
    let depth0_body = lookup.body_full.h;
    // Looking up the same word again, in any case, is no step.
    lookup.look_up_word("BOXES".into());
    assert_eq!(lookup.depth(), 0);
    assert!(!lookup.pending);
    // A manual edit is a step like a tapped word.
    cx(&t, screen, |cx| {
        lookup.start_editing();
        lookup.on_text_input(&TextInput::Clear, cx);
        lookup.on_text_input(&TextInput::Insert("box".into()), cx);
        lookup.on_text_input(&TextInput::Submit, cx);
    });
    assert_eq!(lookup.depth(), 1);
    wait(&mut lookup, &t, screen);
    lookup.place(&t);
    assert!(lookup.body_full.h > depth0_body + t.mm(8.0), "the hidden action strip gives the entry its room");
    assert_eq!(lookup.rect(&t).h, (1696 * 54 / 100).max(t.mm(72.0)), "the card stays compact");
    // Back with the reply kept: shown at once, same dictionary and page.
    let request = lookup.request;
    let back = lookup.back.rect().center();
    tap(&mut lookup, back, &t, screen);
    assert_eq!((lookup.depth(), lookup.query.as_str()), (0, "boxes"));
    assert!(!lookup.pending && lookup.complete && lookup.request == request, "no worker round trip");
    assert_eq!(lookup.selected, chosen);
    draw_hints(&mut lookup, &t, screen, &mut text);
    assert_eq!(lookup.pager.page, 2);
    assert!(lookup.actions_divider.is_some() && lookup.back.rect().w == 0);
    // Switching dictionaries after that asks the worker again for this word.
    lookup.cycle(1);
    wait(&mut lookup, &t, screen);
    assert_ne!(lookup.selected, chosen);
    assert!(matches!(&lookup.state, State::Definition { entries, .. } if entries[0].headword == "boxes"));
    lookup.cycle(-1);
    wait(&mut lookup, &t, screen);
    assert_eq!(lookup.selected, chosen);
    draw_hints(&mut lookup, &t, screen, &mut text);
    for _ in 0..2 {
        cx(&t, screen, |cx| lookup.on_gesture(&swipe, cx));
        draw_hints(&mut lookup, &t, screen, &mut text);
    }
    // After a dictionary change the kept reply is stale: Back asks again,
    // then restores the dictionary and the page.
    lookup.look_up_word("box".into());
    wait(&mut lookup, &t, screen);
    worker.edit(crate::dictionary::sets::Edit::MoveDictionary {
        set: crate::dictionary::sets::DEFAULT.into(),
        dictionary: "13.ifo".into(),
        position: 13,
    });
    cx(&t, screen, |cx| lookup.receive(cx));
    wait(&mut lookup, &t, screen);
    lookup.place(&t);
    let back = lookup.back.rect().center();
    tap(&mut lookup, back, &t, screen);
    assert!(lookup.pending, "asked again");
    wait(&mut lookup, &t, screen);
    assert_eq!(lookup.selected, chosen);
    draw_hints(&mut lookup, &t, screen, &mut text);
    assert_eq!(lookup.pager.page, 2);
    worker.flush().unwrap();
    drop(lookup);
    drop(worker);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn history_keeps_24_steps_and_the_newest_6_replies() {
    let root = std::env::temp_dir().join(format!("verso-history-{}", std::process::id()));
    let worker = Rc::new(Worker::new(Store::at(root.clone()), Waker::new(|| {})));
    let t = Theme::for_dpi(300);
    let mut lookup = Lookup::new(
        "w0",
        worker.clone(),
        &t,
        Rect::new(0, 0, 1272, 1696),
        None,
        Default::default(),
        Tabs::offline(Tab::Dictionary),
    );
    for i in 1..=30 {
        // Pretend each word's reply arrived.
        lookup.pending = false;
        lookup.complete = true;
        lookup.look_up_word(format!("w{i}"));
    }
    assert_eq!(lookup.depth(), 24);
    assert_eq!(lookup.history[0].query, "w6", "the oldest steps go first");
    let kept: Vec<_> = lookup.history.iter().map(|v| v.snapshot.is_some()).collect();
    assert_eq!(kept.iter().filter(|k| **k).count(), 6);
    assert!(kept[18..].iter().all(|k| *k), "the newest replies are kept");
    worker.cancel();
    drop(lookup);
    drop(worker);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn the_passage_bar_floats_beside_the_selection_on_every_screen() {
    for (w, h, dpi) in [(1272, 1696, 300), (1696, 1272, 300), (600, 800, 167), (800, 600, 167)] {
        let t = Theme::for_dpi(dpi);
        let screen = Rect::new(0, 0, w, h);
        let clearance = t.mm(4.5);
        let line = |y| Rect::new(w / 4, y, w / 2, t.mm(4.0));
        let upper = line(h / 4);
        let lower = line(h - t.mm(20.0));
        let longest = "x".repeat(QUERY_LIMIT);
        let whole = Rect::new(t.mm(4.0), t.mm(8.0), w - t.mm(8.0), h - t.mm(12.0));
        for (selection, placement) in [(upper, "below"), (lower, "above"), (whole, "docked")] {
            let mut bar = SelectionActions::new(&t, screen, selection, &longest);
            let r = bar.rect();
            assert!(r.w <= t.mm(115.0) && r.w <= w - 2 * t.margin, "{w}×{h}");
            assert!(r.x >= t.margin && r.right() <= w - t.margin && r.y >= t.margin && r.bottom() <= h - t.margin);
            match placement {
                "below" => assert_eq!(r.y, selection.bottom() + clearance),
                "above" => assert_eq!(r.bottom(), selection.y - clearance),
                _ => assert_eq!(r.bottom(), h - t.margin),
            }
            if placement != "docked" {
                assert!(r.inset(-clearance + 1).intersect(&selection).is_none(), "the handles stay clear");
                assert!((r.center().x - selection.center().x).abs() <= 1, "centred on the passage");
            }
            let actions = [
                SelectionAction::Highlight,
                SelectionAction::Note,
                SelectionAction::Search,
                SelectionAction::Dictionary,
                SelectionAction::Wikipedia,
                SelectionAction::Translate,
                SelectionAction::Extend,
            ];
            for action in actions {
                let cell = bar.cell(action);
                assert!(cell.w >= t.target && cell.h >= t.target, "{w}×{h}: touch targets");
                let p = cell.center();
                bar.on_gesture(&Gesture::Down(p));
                assert_eq!(bar.on_gesture(&Gesture::Tap(p)), Some(action));
            }
            let outside = crate::geom::Point::new(r.x + r.w / 2, if r.y > h / 2 { t.mm(2.0) } else { h - t.mm(2.0) });
            assert_eq!(bar.on_gesture(&Gesture::Down(outside)), None);
            assert_eq!(bar.on_gesture(&Gesture::Tap(outside)), Some(SelectionAction::Dismiss));
        }
        // One character more: too long to look up, so Dict and Wiki ignore taps.
        let mut bar = SelectionActions::new(&t, screen, upper, &format!("{longest}x"));
        for (action, works) in [
            (SelectionAction::Dictionary, false),
            (SelectionAction::Wikipedia, false),
            (SelectionAction::Translate, true),
        ] {
            let p = bar.cell(action).center();
            bar.on_gesture(&Gesture::Down(p));
            assert_eq!(bar.on_gesture(&Gesture::Tap(p)), works.then_some(action));
        }
        {
            let mut canvas = Canvas::new(w as u32, h as u32);
            canvas.fill(crate::ui::theme::PAPER);
            let (mut text, mut icons) = (TextRenderer::new(), IconRenderer::default());
            bar.draw(&mut DrawCx::new(&mut canvas, &mut text, &mut icons, &t, &mut vec![]));
            save_capture(&canvas, w, h, "passage-bar-long");
            let style = t.caption.weight(Weight::SemiBold);
            for (label, action) in
                [("Highlight", SelectionAction::Highlight), ("Translate", SelectionAction::Translate)]
            {
                assert!(text.width(label, style) <= (bar.cell(action).w - t.mm(1.0)) as f32, "{w}×{h}: {label} is cut");
            }
        }
        // Drawn: the primary cell is ink, with its label in paper.
        let mut bar = SelectionActions::new(&t, screen, upper, &longest);
        let mut canvas = Canvas::new(w as u32, h as u32);
        canvas.fill(crate::ui::theme::PAPER);
        let (mut text, mut icons, mut hints) = (TextRenderer::new(), IconRenderer::default(), vec![]);
        bar.draw(&mut DrawCx::new(&mut canvas, &mut text, &mut icons, &t, &mut hints));
        let gray = |canvas: &Canvas, p: crate::geom::Point| canvas.gray_data()[(p.y * w + p.x) as usize];
        let highlight = bar.cell(SelectionAction::Highlight);
        assert_eq!(
            gray(&canvas, crate::geom::Point::new(highlight.x + 3, highlight.center().y)),
            crate::ui::theme::INK
        );
        let shadow = crate::geom::Point::new(bar.rect().center().x, bar.rect().bottom() + t.mm(0.5));
        assert_eq!(gray(&canvas, shadow), crate::ui::theme::SHADOW);
        save_capture(&canvas, w, h, "passage-bar");
        let note = bar.cell(SelectionAction::Note).center();
        bar.on_gesture(&Gesture::Down(note));
        let mut pressed = vec![];
        bar.draw(&mut DrawCx::new(&mut canvas, &mut text, &mut icons, &t, &mut pressed));
        assert!(pressed.iter().any(|u| u.mode == crate::platform::RefreshMode::Fast && u.rect.contains(note)));
    }
}

fn save_capture(canvas: &Canvas, w: i32, h: i32, name: &str) {
    let Some(folder) = std::env::var_os("VERSO_DICTIONARY_CAPTURES") else { return };
    let folder = std::path::PathBuf::from(folder);
    fs::create_dir_all(&folder).unwrap();
    image::save_buffer(
        folder.join(format!("{w}x{h}-{name}.png")),
        canvas.gray_data(),
        w as u32,
        h as u32,
        image::ColorType::L8,
    )
    .unwrap();
}
