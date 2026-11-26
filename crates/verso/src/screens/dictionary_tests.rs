use super::*;
use crate::{
    dictionary::{Preference, sets::Preferences},
    gfx::Icon,
    gfx::{Canvas, IconRenderer},
    net::tests::{Reply as Http, Server},
    online::Service,
    platform::{Waker, wifi::Link},
    store::Store,
    ui::gesture::Direction,
};
use std::{
    fs,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
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
/// Two Wikipedias: "usa" redirects to an article, "mercury" has 10 matches,
/// "nothing" none, and opening "Gone" fails.
fn wikipedias() -> Server {
    Server::answering(|request| {
        if let Some(query) = request.param("srsearch") {
            let hits = match query.as_str() {
                "usa" => vec![
                    r#"{"ns":0,"title":"United States","redirecttitle":"USA","snippet":"A country"}"#.to_string(),
                    r#"{"ns":0,"title":"Gone","snippet":""}"#.to_string(),
                    r#"{"ns":0,"title":"USA Today","snippet":"A <b>newspaper</b>"}"#.to_string(),
                ],
                "mercury" => {
                    (0..10).map(|i| format!(r#"{{"ns":0,"title":"Mercury ({i})","snippet":"No. {i}"}}"#)).collect()
                }
                _ => vec![],
            };
            return Http::Ok(format!(r#"{{"query":{{"search":[{}]}}}}"#, hits.join(",")).into_bytes());
        }
        let title = request.param("titles").unwrap();
        if title == "Gone" {
            return Http::Ok(br#"{"query":{"pages":[{"title":"Gone","missing":true}]}}"#.to_vec());
        }
        let language = &request.path[1..3];
        let text = "A paragraph long enough to take several lines of the card. ".repeat(40);
        let extract = format!("About {title}. {text}\\n\\n\\n== History ==\\n{text}\\n=== Origins ===\\n{text}");
        let url = format!("https://{language}.wikipedia.org/wiki/{}", title.replace(' ', "_"));
        Http::Ok(
            format!(r#"{{"query":{{"pages":[{{"title":"{title}","extract":"{extract}","canonicalurl":"{url}"}}]}}}}"#)
                .into_bytes(),
        )
    })
}
/// A card on Wikipedia for a French book whose reader chose English.
fn wikipedia_card(query: &str, server: &Server, link: &Arc<AtomicU64>, t: &Theme, screen: Rect) -> Lookup {
    let languages = Languages::for_test(Some("fr"), None);
    online_card(Tab::Wikipedia, query, server, link, Duration::from_secs(5), languages, t, screen)
}
/// A card on Translate for a French book.
fn translate_card(
    query: &str,
    server: &Server,
    link: &Arc<AtomicU64>,
    target: Option<&'static str>,
    t: &Theme,
    screen: Rect,
) -> Lookup {
    let languages = Languages::for_test(Some("fr"), target);
    // "slow" waits this long.
    online_card(Tab::Translate, query, server, link, Duration::from_secs(1), languages, t, screen)
}
/// With the link `state` (0 off, 1 connecting, 2 connected).
#[allow(clippy::too_many_arguments)]
fn online_card(
    tab: Tab,
    query: &str,
    server: &Server,
    state: &Arc<AtomicU64>,
    timeout: Duration,
    languages: Languages,
    t: &Theme,
    screen: Rect,
) -> Lookup {
    let state = state.clone();
    let link = move || {
        Ok(match state.load(Ordering::Relaxed) {
            0 => Link::Off,
            1 => Link::Connecting,
            _ => Link::Connected,
        })
    };
    let service = Service::for_test(&server.base, timeout, Box::new(link));
    let tabs = Tabs::new(tab, &service, Waker::new(|| {}), languages);
    let store = Store::at(std::env::temp_dir().join(format!("verso-online-card-{}", std::process::id())));
    Lookup::new(query, Rc::new(Worker::new(store, Waker::new(|| {}))), t, screen, None, Default::default(), tabs)
}
fn submit(lookup: &mut Lookup, query: &str, t: &Theme, screen: Rect) {
    cx(t, screen, |cx| {
        lookup.start_editing();
        lookup.on_text_input(&TextInput::Clear, cx);
        lookup.on_text_input(&TextInput::Insert(query.into()), cx);
        lookup.on_text_input(&TextInput::Submit, cx);
    });
}
/// The open chooser's rows and the notes beside them.
fn choices(lookup: &Lookup) -> Vec<(String, String)> {
    let rows = lookup.chooser.as_ref().unwrap().1.rows();
    rows.iter().map(|r| (r.label.clone(), r.value.clone().unwrap_or_default())).collect()
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
    if let Some(hits) = lookup.tabs.wikipedia().listing().filter(|_| lookup.on_wikipedia()) {
        return hits.iter().map(|h| h.title.as_str()).collect::<Vec<_>>().join("\n");
    }
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
    let tabs = |tab, translate| Tabs::new(tab, &service, Waker::new(|| {}), Languages::for_test(None, translate));
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
    draw_hints(&mut lookup, &t, screen, &mut text);
    assert_eq!(lookup.list.as_ref().unwrap().rows[0].label, "English");
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
fn wikipedia_opens_a_clear_match_and_switches_matches_and_languages() {
    let t = Theme::for_dpi(300);
    let screen = Rect::new(0, 0, 1272, 1696);
    let (server, connected) = (wikipedias(), Arc::new(AtomicU64::new(2)));
    let hits = || server.requests.lock().unwrap().len();
    let mut text = TextRenderer::new();
    let mut lookup = wikipedia_card("usa", &server, &connected, &t, screen);
    answered(&mut lookup, &t, screen);
    let ends = |lookup: &Lookup, language: &str, code: &str, title: &str| {
        let end = format!("\nhttps://{code}.wikipedia.org/wiki/{title}\nWikipedia · {language} · CC BY-SA 4.0");
        online_text(lookup, &t).ends_with(&end)
    };
    assert!(online_text(&lookup, &t).starts_with("United States\nRedirected from “USA”\nAbout United States."));
    assert!(ends(&lookup, "English", "en", "United_States"));
    // Headings keep with their text, and the rest is laid out while idle.
    let kept = lookup.tabs.content(&t).iter().filter(|b| matches!(b, StyledBlock::Paragraph(p) if p.keep)).count();
    assert_eq!(kept, 3, "two headings, and the address with the license");
    draw_hints(&mut lookup, &t, screen, &mut text);
    assert!(lookup.pager.incomplete && lookup.paginating());
    while lookup.paginating() {
        lookup.on_idle(&mut text);
    }
    assert!(lookup.pager.pages > 2 && !lookup.busy());
    capture(&mut lookup, &t, screen, "wikipedia-article");
    // The article's words can't be looked up.
    let body = lookup.body;
    tap(&mut lookup, crate::geom::Point::new(body.x + t.mm(3.0), body.y + t.mm(2.0)), &t, screen);
    draw_hints(&mut lookup, &t, screen, &mut text);
    assert_eq!((lookup.depth(), hits()), (0, 2));
    // Another match opens on its first page; one seen before isn't asked for again.
    let swipe = Gesture::Swipe { dir: Direction::Left, from: screen.center(), to: screen.center() };
    cx(&t, screen, |cx| lookup.on_gesture(&swipe, cx));
    let other = |lookup: &mut Lookup, row: usize| {
        let p = lookup.other_matches.rect().center();
        tap(lookup, p, &t, screen);
        let shown = choices(lookup);
        capture(lookup, &t, screen, "wikipedia-matches");
        let p = lookup.choice_point(row);
        tap(lookup, p, &t, screen);
        assert!(lookup.chooser.is_none());
        answered(lookup, &t, screen);
        draw_hints(lookup, &t, screen, &mut TextRenderer::new());
        shown
    };
    let row = |label: &str, note: &str| (label.to_string(), note.to_string());
    let rows = other(&mut lookup, 2);
    assert_eq!(rows, [row("United States", "Current"), row("Gone", ""), row("USA Today", "")]);
    assert_eq!((hits(), lookup.pager.page), (3, 0));
    assert!(ends(&lookup, "English", "en", "USA_Today") && !online_text(&lookup, &t).contains("Redirected"));
    assert_eq!(other(&mut lookup, 0)[2], row("USA Today", "Current"));
    assert!(hits() == 3 && ends(&lookup, "English", "en", "United_States"));
    // Nothing to retry for an article that's gone; the others are still there.
    other(&mut lookup, 1);
    assert!(online_text(&lookup, &t).starts_with("No article found"));
    assert!(!lookup.retry_shown());
    other(&mut lookup, 0);
    // Another language looks the words up again and is the card's from now on.
    submit(&mut lookup, "mercury", &t, screen);
    answered(&mut lookup, &t, screen);
    let before = hits();
    let p = lookup.edition_point();
    tap(&mut lookup, p, &t, screen);
    assert_eq!(choices(&lookup)[..3], [row("French", "This book"), row("English", "Chosen"), row("Afrikaans", "")]);
    capture(&mut lookup, &t, screen, "wikipedia-languages");
    let p = lookup.choice_point(0);
    let event = tap(&mut lookup, p, &t, screen);
    assert!(matches!(event, Some(LookupEvent::Edition(e)) if e.code() == "fr"));
    answered(&mut lookup, &t, screen);
    assert_eq!(hits(), before + 1);
    assert!(server.requests.lock().unwrap().last().unwrap().path.starts_with("/fr/"));
    // The chain's earlier article was English.
    let back = lookup.back.rect().center();
    tap(&mut lookup, back, &t, screen);
    answered(&mut lookup, &t, screen);
    assert!(ends(&lookup, "French", "fr", "United_States"));
}

#[test]
fn wikipedia_lists_matches_and_reports_misses_and_errors() {
    let (server, link) = (wikipedias(), Arc::new(AtomicU64::new(0)));
    let hits = || server.requests.lock().unwrap().len();
    for (w, h, dpi) in [(1272, 1696, 300), (800, 600, 167)] {
        let (t, screen) = (Theme::for_dpi(dpi), Rect::new(0, 0, w, h));
        let sent = hits();
        let retry = |lookup: &mut Lookup| {
            let p = lookup.retry.rect().center();
            tap(lookup, p, &t, screen);
            answered(lookup, &t, screen);
            draw_hints(lookup, &t, screen, &mut TextRenderer::new());
        };
        link.store(0, Ordering::Relaxed);
        let mut lookup = wikipedia_card("mercury", &server, &link, &t, screen);
        answered(&mut lookup, &t, screen);
        draw_hints(&mut lookup, &t, screen, &mut TextRenderer::new());
        assert!(online_text(&lookup, &t).starts_with("Wi-Fi is off") && lookup.retry_shown());
        assert!(
            lookup
                .body
                .intersect(&lookup.retry.rect())
                .is_none_or(|r| r.y >= lookup.body.y + lookup.text_height.unwrap())
        );
        assert!(lookup.sheet.rect().contains_rect(&lookup.retry.rect()) && !lookup.busy());
        capture(&mut lookup, &t, screen, "wikipedia-offline");
        link.store(1, Ordering::Relaxed);
        retry(&mut lookup);
        assert!(online_text(&lookup, &t).starts_with("Wi-Fi is still connecting") && lookup.retry_shown());
        assert_eq!(hits(), sent, "nothing is sent without Wi-Fi");
        link.store(2, Ordering::Relaxed);
        // Submitting the same words is a retry too.
        submit(&mut lookup, "mercury", &t, screen);
        answered(&mut lookup, &t, screen);
        draw_hints(&mut lookup, &t, screen, &mut TextRenderer::new());
        assert_eq!((hits(), lookup.depth()), (sent + 1, 0));
        assert!(!lookup.retry_shown() && !lookup.busy());
        let list = lookup.list.as_ref().unwrap();
        let per_page = list.paging().range(0).len();
        assert!(per_page >= 2 && lookup.paged && lookup.body.contains_rect(&list.row_rect(per_page - 1)));
        capture(&mut lookup, &t, screen, "wikipedia-list");
        // The list keeps its page while another tab shows.
        let swipe = Gesture::Swipe { dir: Direction::Left, from: screen.center(), to: screen.center() };
        cx(&t, screen, |cx| lookup.on_gesture(&swipe, cx));
        for tab in [Tab::Translate, Tab::Wikipedia] {
            let p = lookup.tab_point(tab);
            tap(&mut lookup, p, &t, screen);
            draw_hints(&mut lookup, &t, screen, &mut TextRenderer::new());
        }
        assert_eq!(lookup.pager.page, 1);
        let p = lookup.list_point(0);
        tap(&mut lookup, p, &t, screen);
        answered(&mut lookup, &t, screen);
        draw_hints(&mut lookup, &t, screen, &mut TextRenderer::new());
        assert!(online_text(&lookup, &t).starts_with(&format!("Mercury ({per_page})\nAbout Mercury")));
        assert_eq!((hits(), lookup.pager.page), (sent + 2, 0));
        // Back from it to the matches.
        let p = lookup.other_matches.rect().center();
        tap(&mut lookup, p, &t, screen);
        assert_eq!(choices(&lookup)[per_page].1, "Current");
        let divider = lookup.divider;
        let p = lookup.choice_point(0);
        tap(&mut lookup, p, &t, screen);
        answered(&mut lookup, &t, screen);
        assert!(online_text(&lookup, &t).starts_with("Mercury (0)"));
        // Nothing found: edit or pick another language, but no Retry.
        submit(&mut lookup, "nothing", &t, screen);
        answered(&mut lookup, &t, screen);
        draw_hints(&mut lookup, &t, screen, &mut TextRenderer::new());
        assert!(online_text(&lookup, &t).starts_with("No article found"));
        assert!(lookup.edit_row() && !lookup.retry_shown());
        capture(&mut lookup, &t, screen, "wikipedia-miss");
        // The languages take the query field's place.
        let p = lookup.edition_point();
        tap(&mut lookup, p, &t, screen);
        assert!(lookup.chooser.is_some() && lookup.divider == divider, "{} != {divider}", lookup.divider);
    }
}

#[test]
fn a_late_wikipedia_reply_never_replaces_newer_words_or_languages() {
    let t = Theme::for_dpi(300);
    let screen = Rect::new(0, 0, 1272, 1696);
    let (server, link) = (wikipedias(), Arc::new(AtomicU64::new(2)));
    let searched = || {
        let requests = server.requests.lock().unwrap();
        requests.iter().filter(|r| r.param("srsearch").is_some()).count()
    };
    let arrived = |count| {
        let deadline = Instant::now() + Duration::from_secs(5);
        while searched() < count {
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        // It's in the card's mailbox, or about to be.
        std::thread::sleep(Duration::from_millis(50));
    };
    let mut lookup = wikipedia_card("usa", &server, &link, &t, screen);
    arrived(1);
    submit(&mut lookup, "mercury", &t, screen);
    arrived(2);
    answered(&mut lookup, &t, screen);
    assert!(online_text(&lookup, &t).starts_with("Mercury (0)\n"), "the list, not the article");
    // Back asks again: the first reply never counted.
    let back = lookup.back.rect().center();
    tap(&mut lookup, back, &t, screen);
    arrived(3);
    let p = lookup.edition_point();
    tap(&mut lookup, p, &t, screen);
    let p = lookup.choice_point(0);
    tap(&mut lookup, p, &t, screen);
    answered(&mut lookup, &t, screen);
    assert!(online_text(&lookup, &t).ends_with("Wikipedia · French · CC BY-SA 4.0"));
}

/// A translator that finds everything to be French and answers with the text
/// and the language asked for. "busy" is refused and "slow" never answered
/// the first time; "long" gets pages. DeepL answers as itself.
fn translator() -> Server {
    let failed = AtomicU64::new(0);
    Server::answering(move |request| {
        if request.path.starts_with("/deepl/") {
            let (text, target) = (request.param("text").unwrap(), request.param("target_lang").unwrap());
            let reply = serde_json::json!({"translations": [{"detected_source_language": "FR", "text": format!("{text} in {target} by DeepL.")}]});
            return Http::Ok(reply.to_string().into_bytes());
        }
        let (text, target) = (request.param("q").unwrap(), request.param("tl").unwrap());
        let bit = match text.as_str() {
            "busy" => 1,
            "slow" => 2,
            _ => 0,
        };
        if bit != 0 && failed.fetch_xor(bit, Ordering::Relaxed) & bit == 0 {
            return if bit == 1 { Http::Status(429) } else { Http::Silent };
        }
        let sentence = format!("{text} in {target}. ");
        let answer = if text == "long" { format!("{}\n", sentence.repeat(40)).repeat(6) } else { sentence };
        Http::Ok(serde_json::json!([[[answer, text]], null, "fr"]).to_string().into_bytes())
    })
}
/// The languages of the last request, from and into.
fn asked_between(server: &Server) -> (String, String) {
    let requests = server.requests.lock().unwrap();
    let last = requests.last().unwrap();
    (last.param("sl").unwrap(), last.param("tl").unwrap())
}
/// Opens the source or target picker and taps a visible row.
fn pick(lookup: &mut Lookup, source: bool, row: usize, t: &Theme, screen: Rect) -> Option<LookupEvent> {
    let (from, into) = lookup.language_points();
    tap(lookup, if source { from } else { into }, t, screen);
    let p = lookup.choice_point(row);
    tap(lookup, p, t, screen)
}

#[test]
fn translate_waits_for_a_language_and_translates_between_the_chosen_ones() {
    let t = Theme::for_dpi(300);
    let screen = Rect::new(0, 0, 1272, 1696);
    let (server, link) = (translator(), Arc::new(AtomicU64::new(2)));
    let hits = || server.requests.lock().unwrap().len();
    let between = |from: &str, into: &str| assert_eq!(asked_between(&server), (from.into(), into.into()));
    let mut text = TextRenderer::new();
    // First use: the choice is in the body, and nothing is sent until it's made.
    let mut lookup = translate_card("renard", &server, &link, None, &t, screen);
    let compact = 1696 * 54 / 100;
    draw_hints(&mut lookup, &t, screen, &mut text);
    assert!(!lookup.busy() && lookup.rect(&t).h == 1696 * 4 / 5);
    submit(&mut lookup, "un renard", &t, screen);
    draw_hints(&mut lookup, &t, screen, &mut text);
    let rows = &lookup.list.as_ref().unwrap().rows;
    let first: Vec<_> = rows[..3].iter().map(|r| (r.label.as_str(), r.value.as_deref().unwrap_or(""))).collect();
    assert_eq!(first, [("English", ""), ("French", "This book"), ("Afrikaans", "")]);
    // One Norwegian: no Nynorsk, and Bokmål isn't another.
    let norwegian: Vec<_> = rows.iter().map(|r| r.label.as_str()).filter(|l| l.starts_with("Norwegian")).collect();
    assert_eq!(norwegian, ["Norwegian"]);
    let bokmal = Languages::for_test(Some("nb"), None);
    assert_eq!(translate::sources(&bokmal, Some("no"))[..3], [None, Some("no"), Some("af")]);
    assert_eq!(hits(), 0);
    capture(&mut lookup, &t, screen, "translate-first");
    let p = lookup.list_point(0);
    assert!(matches!(tap(&mut lookup, p, &t, screen), Some(LookupEvent::Target("en"))));
    answered(&mut lookup, &t, screen);
    draw_hints(&mut lookup, &t, screen, &mut text);
    assert_eq!((hits(), lookup.rect(&t).h), (1, compact));
    between("auto", "en");
    assert_eq!(online_text(&lookup, &t), "un renard in en.\nGoogle Translate");
    let label = |lookup: &Lookup| lookup.tabs.translate().source_label(lookup.tabs.languages());
    assert_eq!(label(&lookup), "Auto (French)");
    // Another source is only this card's; another target is every book's.
    let p = lookup.language_points().0;
    tap(&mut lookup, p, &t, screen);
    let row = |label: &str, note: &str| (label.to_string(), note.to_string());
    let rows = choices(&lookup);
    assert_eq!(rows[..3], [row("Detect automatically", "Chosen"), row("French", "This book"), row("Afrikaans", "")]);
    capture(&mut lookup, &t, screen, "translate-sources");
    let p = lookup.choice_point(1);
    assert!(tap(&mut lookup, p, &t, screen).is_none());
    assert_eq!(label(&lookup), "French");
    answered(&mut lookup, &t, screen);
    between("fr", "en");
    assert!(matches!(pick(&mut lookup, false, 2, &t, screen), Some(LookupEvent::Target("af"))));
    answered(&mut lookup, &t, screen);
    between("fr", "af");
    assert!(pick(&mut lookup, false, 0, &t, screen).is_none(), "the one in use");
    assert_eq!(hits(), 3);
    // Into the language picked by hand: nothing is sent, another is one tap away.
    assert!(matches!(pick(&mut lookup, false, 2, &t, screen), Some(LookupEvent::Target("fr"))));
    draw_hints(&mut lookup, &t, screen, &mut text);
    assert!(online_text(&lookup, &t).starts_with("Both languages are the same"));
    assert!(lookup.another_language_shown() && !lookup.retry_shown() && !lookup.busy());
    assert_eq!(hits(), 3);
    // Detected, it's known only from the reply.
    pick(&mut lookup, true, 0, &t, screen);
    answered(&mut lookup, &t, screen);
    draw_hints(&mut lookup, &t, screen, &mut text);
    between("auto", "fr");
    assert!(online_text(&lookup, &t).starts_with("This is already French\n"));
    capture(&mut lookup, &t, screen, "translate-same");
    let p = lookup.another_language.rect().center();
    tap(&mut lookup, p, &t, screen);
    assert_eq!(choices(&lookup)[..2], [row("French", "Chosen"), row("English", "")]);
    let p = lookup.choice_point(1);
    tap(&mut lookup, p, &t, screen);
    answered(&mut lookup, &t, screen);
    between("auto", "en");
    // The words edited earlier weren't translated into this language yet.
    let sent = hits();
    let back = lookup.back.rect().center();
    tap(&mut lookup, back, &t, screen);
    answered(&mut lookup, &t, screen);
    assert_eq!((hits(), online_text(&lookup, &t).as_str()), (sent + 1, "renard in en.\nGoogle Translate"));
    submit(&mut lookup, "chat", &t, screen);
    answered(&mut lookup, &t, screen);
    pick(&mut lookup, false, 2, &t, screen);
    answered(&mut lookup, &t, screen);
    tap(&mut lookup, back, &t, screen);
    answered(&mut lookup, &t, screen);
    assert_eq!(online_text(&lookup, &t), "renard in af.\nGoogle Translate");
}

#[test]
fn translate_pages_long_answers_and_retries_errors() {
    let (server, link) = (translator(), Arc::new(AtomicU64::new(2)));
    let hits = || server.requests.lock().unwrap().len();
    for (w, h, dpi) in [(1272, 1696, 300), (800, 600, 167)] {
        let (t, screen) = (Theme::for_dpi(dpi), Rect::new(0, 0, w, h));
        let mut text = TextRenderer::new();
        // Over 4,000 characters, nothing is sent and there's nothing to retry.
        let sent = hits();
        let passage = "x".repeat(crate::online::translate::TEXT_LIMIT + 1);
        let mut lookup = translate_card(&passage, &server, &link, Some("de"), &t, screen);
        draw_hints(&mut lookup, &t, screen, &mut text);
        assert!(!lookup.busy() && !lookup.retry_shown());
        assert_eq!(online_text(&lookup, &t), "Too long to translate\nSelect up to 4,000 characters at a time.");
        // A long answer is laid out while idle, paragraph by paragraph.
        let mut lookup = translate_card("long", &server, &link, Some("de"), &t, screen);
        answered(&mut lookup, &t, screen);
        draw_hints(&mut lookup, &t, screen, &mut text);
        assert_eq!(online_text(&lookup, &t).lines().count(), 7);
        while lookup.paginating() {
            lookup.on_idle(&mut text);
        }
        assert!(lookup.paged && lookup.pager.pages > 2 && !lookup.busy());
        assert_eq!(hits(), sent + 1);
        capture(&mut lookup, &t, screen, "translate-long");
        let retry = |lookup: &mut Lookup| {
            draw_hints(lookup, &t, screen, &mut TextRenderer::new());
            assert!(lookup.retry_shown() && lookup.sheet.rect().contains_rect(&lookup.retry.rect()));
            let p = lookup.retry.rect().center();
            tap(lookup, p, &t, screen);
            answered(lookup, &t, screen);
        };
        let mut sent = hits();
        for (query, error) in [("busy", "Google Translate is busy\n"), ("slow", "Google Translate took too long")] {
            submit(&mut lookup, query, &t, screen);
            answered(&mut lookup, &t, screen);
            assert!(online_text(&lookup, &t).starts_with(error), "{query}");
            capture(&mut lookup, &t, screen, &format!("translate-{query}"));
            retry(&mut lookup);
            assert!(online_text(&lookup, &t).starts_with(&format!("{query} in de.")));
            assert!(!lookup.retry_shown());
            sent += 2;
            assert_eq!(hits(), sent);
        }
        for (state, error) in [(0, "Wi-Fi is off\n"), (1, "Wi-Fi is still connecting\n")] {
            link.store(state, Ordering::Relaxed);
            submit(&mut lookup, error, &t, screen);
            answered(&mut lookup, &t, screen);
            assert!(online_text(&lookup, &t).starts_with(error));
            assert_eq!(hits(), sent);
            link.store(2, Ordering::Relaxed);
            retry(&mut lookup);
            sent += 1;
            assert_eq!(hits(), sent);
        }
    }
}

#[test]
fn the_service_switches_on_the_card_once_a_deepl_key_is_saved() {
    let t = Theme::for_dpi(300);
    let (server, link) = (translator(), Arc::new(AtomicU64::new(2)));
    // Without a key there is nothing to switch to.
    for (screen, key) in [(Rect::new(0, 0, 1272, 1696), None), (Rect::new(0, 0, 1696, 1272), Some("k:fx"))] {
        let mut lookup = translate_card("renard", &server, &link, Some("en"), &t, screen);
        lookup.tabs.set_languages(Languages {
            deepl_api_key: key.map(Into::into),
            ..Languages::for_test(Some("fr"), Some("en"))
        });
        answered(&mut lookup, &t, screen);
        draw_hints(&mut lookup, &t, screen, &mut TextRenderer::new());
        let Some(p) = lookup.service_point() else {
            assert!(key.is_none());
            continue;
        };
        tap(&mut lookup, p, &t, screen);
        let row = |label: &str, note: &str| (label.to_string(), note.to_string());
        assert_eq!(choices(&lookup), [row("Google Translate", "Chosen"), row("DeepL Free", "")]);
        capture(&mut lookup, &t, screen, "translate-services");
        // Chosen, it translates again at once and is saved for every book.
        let p = lookup.choice_point(1);
        assert!(matches!(tap(&mut lookup, p, &t, screen), Some(LookupEvent::Provider(Provider::DeepL))));
        answered(&mut lookup, &t, screen);
        assert_eq!(online_text(&lookup, &t), "renard in EN-US by DeepL.\nDeepL");
        assert!(server.requests.lock().unwrap().last().unwrap().path.starts_with("/deepl/free"));
        capture(&mut lookup, &t, screen, "translate-deepl");
        // Its row says beforehand when Google answers instead.
        lookup.tabs.set_target("hy");
        let p = lookup.service_point().unwrap();
        tap(&mut lookup, p, &t, screen);
        let sub = lookup.chooser.as_ref().unwrap().1.rows()[1].sub.clone();
        assert_eq!(sub.as_deref(), Some("Doesn’t translate into Armenian, so Google answers"));
    }
}

#[test]
fn paragraphs_are_translated_apart_but_edited_as_one_line() {
    let t = Theme::for_dpi(300);
    let screen = Rect::new(0, 0, 1272, 1696);
    let (server, link) = (translator(), Arc::new(AtomicU64::new(2)));
    let mut lookup = translate_card("Un renard.\nUn chat.", &server, &link, Some("de"), &t, screen);
    answered(&mut lookup, &t, screen);
    assert_eq!(server.requests.lock().unwrap()[0].param("q").as_deref(), Some("Un renard.\nUn chat."));
    assert_eq!(lookup.field.value(), "Un renard. Un chat.");
    // In a book of unknown language no source is the book's.
    let source = Some("fr");
    lookup.tabs.set_languages(Languages { source, ..Languages::for_test(None, Some("de")) });
    lookup.choose_source();
    assert_eq!(choices(&lookup)[0], ("Detect automatically".into(), String::new()));
    lookup.chooser = None;
    // Submitted as it is, it's the same passage.
    cx(&t, screen, |cx| {
        lookup.start_editing();
        lookup.on_text_input(&TextInput::Submit, cx);
    });
    assert_eq!((lookup.depth(), lookup.tabs.waiting()), (0, false));
    submit(&mut lookup, "chat", &t, screen);
    let back = lookup.back.rect().center();
    tap(&mut lookup, back, &t, screen);
    assert_eq!(lookup.field.value(), "Un renard. Un chat.");
}

#[test]
fn a_late_translation_never_replaces_newer_words_or_languages() {
    let t = Theme::for_dpi(300);
    let screen = Rect::new(0, 0, 1272, 1696);
    let (server, link) = (translator(), Arc::new(AtomicU64::new(2)));
    let arrived = |count| {
        let deadline = Instant::now() + Duration::from_secs(5);
        while server.requests.lock().unwrap().len() < count {
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        // It's in the card's mailbox, or about to be.
        std::thread::sleep(Duration::from_millis(50));
    };
    let shows = |lookup: &mut Lookup, text: &str| {
        answered(lookup, &t, screen);
        assert_eq!(online_text(lookup, &t), format!("{text}\nGoogle Translate"));
    };
    let mut lookup = translate_card("renard", &server, &link, Some("de"), &t, screen);
    arrived(1);
    submit(&mut lookup, "chat", &t, screen);
    arrived(2);
    pick(&mut lookup, false, 1, &t, screen);
    shows(&mut lookup, "chat in en.");
    // Back asks: the first reply never counted.
    let back = lookup.back.rect().center();
    tap(&mut lookup, back, &t, screen);
    arrived(4);
    // Nor does the one for a source changed meanwhile.
    pick(&mut lookup, true, 1, &t, screen);
    assert!(lookup.tabs.waiting());
    shows(&mut lookup, "renard in en.");
    assert_eq!(asked_between(&server).0, "fr");
}

#[test]
fn long_text_reaches_the_dictionaries_and_is_never_cut() {
    let (root, store) = fixture();
    // Paclitaxel, with a space: over the length a long press takes for a word.
    let chemical = "(2α,4α,5β,7β,10β,13α)-4,10-Bis(acetyloxy)-13-{[(2R,3S)-3-(benzoylamino)-2-hydroxy-3-phenylpropanoyl]oxy}\
                    -1,7-dihydroxy-9-oxo-5,20-epoxytax-11-en-2-yl benzoate";
    let longest = format!("{chemical} {}", "x".repeat(QUERY_LIMIT - chemical.chars().count() - 1));
    let long = format!("{longest}x");
    let path = store.dictionary_dir().join("02");
    let mut idx = Vec::new();
    for word in [chemical, &long] {
        idx.extend_from_slice(word.as_bytes());
        idx.push(0);
        idx.extend_from_slice(&0u32.to_be_bytes());
        idx.extend_from_slice(&7u32.to_be_bytes());
    }
    fs::write(path.with_extension("idx"), &idx).unwrap();
    fs::write(path.with_extension("dict"), "Meaning").unwrap();
    let ifo = format!(
        "StarDict's dict ifo file\nversion=2.4.2\nbookname=Chemistry\nwordcount=2\nidxfilesize={}\nsametypesequence=m\n",
        idx.len()
    );
    fs::write(path.with_extension("ifo"), ifo).unwrap();
    let worker = Rc::new(Worker::new(store, Waker::new(|| {})));
    let t = Theme::for_dpi(300);
    let screen = Rect::new(0, 0, 1272, 1696);
    let card = |query: &str| {
        let mut lookup =
            Lookup::new(query, worker.clone(), &t, screen, None, Default::default(), Tabs::offline(Tab::Dictionary));
        wait(&mut lookup, &t, screen);
        lookup
    };
    let go = |lookup: &mut Lookup, tab| {
        lookup.place(&t);
        let p = lookup.tab_point(tab);
        tap(lookup, p, &t, screen);
        lookup.tabs.current()
    };
    let found = |lookup: &Lookup| matches!(lookup.state, State::Definition { .. });
    // From the book, and from a definition; only Wikipedia has a limit.
    for (word, wikipedia) in [(chemical, Tab::Wikipedia), (long.as_str(), Tab::Dictionary)] {
        let mut chained = card("boxes");
        chained.look_up_word(word.into());
        wait(&mut chained, &t, screen);
        for mut lookup in [card(word), chained] {
            assert!(found(&lookup) && lookup.on_dictionary(), "{word}");
            assert_eq!(go(&mut lookup, Tab::Wikipedia), wikipedia, "{word}");
            assert_eq!(go(&mut lookup, Tab::Translate), Tab::Translate);
            assert_eq!(go(&mut lookup, Tab::Wikipedia) == Tab::Wikipedia, wikipedia == Tab::Wikipedia, "{word}");
            assert_eq!(go(&mut lookup, Tab::Dictionary), Tab::Dictionary);
            assert!(found(&lookup));
        }
    }
    // A miss offers the field only for text that fits it whole.
    let mut lookup = card(&longest);
    assert!(!found(&lookup) && lookup.edit_row());
    assert_eq!(lookup.field.value(), longest);
    cx(&t, screen, |cx| {
        lookup.start_editing();
        lookup.on_text_input(&TextInput::Insert("x".into()), cx);
    });
    assert_eq!(lookup.field.value(), longest, "typing stops at the limit");
    let mut lookup = card(&format!("{long}x"));
    assert!(!found(&lookup) && !lookup.edit_row());
    let heading = lookup.heading.center();
    tap(&mut lookup, heading, &t, screen);
    assert!(!lookup.keyboard.visible);
    capture(&mut lookup, &t, screen, "long-miss");
    go(&mut lookup, Tab::Translate);
    capture(&mut lookup, &t, screen, "long-passage");
    worker.flush().unwrap();
    drop(lookup);
    drop(worker);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn long_text_is_sent_nowhere_unless_its_tab_was_chosen() {
    let t = Theme::for_dpi(300);
    let screen = Rect::new(0, 0, 1272, 1696);
    let long = "字".repeat(QUERY_LIMIT + 1);
    for (tab, sent) in [(Tab::Dictionary, 0), (Tab::Wikipedia, 0), (Tab::Translate, 1)] {
        let server = online();
        let link = Arc::new(AtomicU64::new(2));
        let languages = Languages::for_test(None, Some("de"));
        let mut lookup = online_card(tab, &long, &server, &link, Duration::from_secs(5), languages, &t, screen);
        answered(&mut lookup, &t, screen);
        assert_eq!((lookup.tabs.current(), server.requests.lock().unwrap().len()), (tab, sent));
    }
}

#[test]
fn a_folded_wikipedia_grows_for_a_long_error_without_expanding_the_card() {
    let (t, screen) = (Theme::for_dpi(167), Rect::new(0, 0, 600, 800));
    let error = r#"{"error":{"code":"internal_api_error_Wikimedia\\RequestTimeout\\RequestTimeoutException"}}"#;
    let server = Server::answering(move |_| Http::Ok(error.into()));
    let mut lookup = wikipedia_card("usa", &server, &Arc::new(AtomicU64::new(2)), &t, screen);
    lookup.place(&t);
    let fold = lookup.expand_point();
    tap(&mut lookup, fold, &t, screen);
    answered(&mut lookup, &t, screen);
    draw_hints(&mut lookup, &t, screen, &mut TextRenderer::new());
    assert!(lookup.grew && !lookup.folded && !lookup.expanded);
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
    let blocks = content(&lookup.state, false, false, &["Broken dictionary".into()], &t);
    assert!(blocks.iter().all(
        |b| !matches!(b, StyledBlock::Paragraph(p) if p.text.contains("Broken") || p.text.contains("incomplete"))
    ));
    let blocks = content(&State::NoResult, false, false, &["Broken dictionary".into()], &t);
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
            assert!(
                lookup.heading.bottom() < lookup.tabs.rect().y
                    && lookup.tabs.rect().bottom() < lookup.switcher.rect().y
            );
            assert!(lookup.heading.y >= card.y && lookup.actions[3].rect().bottom() <= card.bottom());
            assert!(
                lookup.switcher.rect().bottom() < lookup.body_full.y
                    && lookup.body_full.bottom() < lookup.actions[0].rect().y
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
        let p = lookup.switcher.rect().center();
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
    let long = "A wooden box. ".repeat(200);
    let (root, store) = words(&[("box", "A container, often a crate."), ("crate", &long)]);
    let t = Theme::for_dpi(300);
    let screen = Rect::new(0, 0, 1272, 1696);
    let worker = Rc::new(Worker::new(store, Waker::new(|| {})));
    let mut text = TextRenderer::new();
    let mut lookup =
        Lookup::new("box", worker.clone(), &t, screen, None, Default::default(), Tabs::offline(Tab::Dictionary));
    wait(&mut lookup, &t, screen);
    draw_hints(&mut lookup, &t, screen, &mut text);
    let actions = lookup.actions.each_ref().map(Button::rect);
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
    // Taps where they were at depth 0 reach the pager, with › and ‹ at its ends.
    let [first, .., last] = actions;
    let mut taps = actions.map(|r| (r.center(), 1)).to_vec();
    taps.insert(0, (crate::geom::Point::new(last.right() - t.target / 2, last.center().y), 1));
    taps.push((crate::geom::Point::new(first.x + t.target / 2, first.center().y), 0));
    for (p, page) in taps {
        assert!(tap(&mut lookup, p, &t, screen).is_none(), "a hidden action took the tap at {p:?}");
        draw_hints(&mut lookup, &t, screen, &mut text);
        assert_eq!((lookup.depth(), lookup.pager.page), (1, page), "{p:?}");
    }
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
fn a_button_hidden_under_the_finger_lets_go() {
    let (root, store) = fixture();
    let t = Theme::for_dpi(300);
    let screen = Rect::new(0, 0, 1272, 1696);
    let worker = Rc::new(Worker::new(store, Waker::new(|| {})));
    let mut lookup =
        Lookup::new("boxes", worker.clone(), &t, screen, None, Default::default(), Tabs::offline(Tab::Dictionary));
    lookup.place(&t);
    let manage = lookup.manage.rect().center();
    cx(&t, screen, |cx| lookup.on_gesture(&Gesture::Down(manage), cx));
    assert!(lookup.manage.is_pressed());
    // The definition takes its place before the finger lifts.
    wait(&mut lookup, &t, screen);
    assert!(cx(&t, screen, |cx| lookup.on_gesture(&Gesture::Tap(manage), cx)).is_none());
    // A word is down when the dictionaries change, and the button is back.
    draw_hints(&mut lookup, &t, screen, &mut TextRenderer::new());
    let word = lookup.body.center();
    cx(&t, screen, |cx| lookup.on_gesture(&Gesture::Down(word), cx));
    worker.edit(crate::dictionary::sets::Edit::MoveDictionary {
        set: crate::dictionary::sets::DEFAULT.into(),
        dictionary: "13.ifo".into(),
        position: 13,
    });
    cx(&t, screen, |cx| lookup.receive(cx));
    assert!(lookup.manage_shown() && !lookup.manage.is_pressed());
    assert!(cx(&t, screen, |cx| lookup.on_gesture(&Gesture::Tap(word), cx)).is_none());
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
        // One character more: too long for Wikipedia, so Wiki ignores taps.
        let mut bar = SelectionActions::new(&t, screen, upper, &format!("{longest}x"));
        for (action, works) in [
            (SelectionAction::Dictionary, true),
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
