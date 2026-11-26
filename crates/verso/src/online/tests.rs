//! Lookups end to end against a local HTTP server.
use super::wikipedia::tests::hit;
use super::wikipedia::{Article, Block, Edition, Found};
use super::worker::Reply;
use super::*;
use crate::net::tests::{Reply as Http, Request as HttpRequest, Server};
use crate::platform::Waker;
use crate::platform::wifi::Link;
use serde_json::json;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

struct Bench {
    server: Server,
    service: Arc<Service>,
    /// 0 off, 1 connecting, 2 connected.
    link: Arc<AtomicU8>,
    woken: Arc<AtomicUsize>,
}

fn bench_within(total: Duration, handler: impl Fn(&HttpRequest) -> Http + Send + Sync + 'static) -> Bench {
    let server = Server::answering(handler);
    let link = Arc::new(AtomicU8::new(2));
    let state = link.clone();
    let read = move || {
        Ok(match state.load(Ordering::Relaxed) {
            0 => Link::Off,
            1 => Link::Connecting,
            _ => Link::Connected,
        })
    };
    let service = Service::for_test(&server.base, total, Box::new(read));
    Bench { server, service, link, woken: Arc::default() }
}

fn bench(handler: impl Fn(&HttpRequest) -> Http + Send + Sync + 'static) -> Bench {
    bench_within(Duration::from_secs(5), handler)
}

/// Waits for what another thread is about to do.
fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let started = Instant::now();
    while !done() {
        assert!(started.elapsed() < Duration::from_secs(5), "{what}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

impl Bench {
    fn worker(&self) -> Worker {
        let woken = self.woken.clone();
        let wake = move || {
            woken.fetch_add(1, Ordering::Relaxed);
        };
        Worker::new(self.service.clone(), Waker::new(wake))
    }

    fn ask(&self, request: Request) -> Result<Answer, Error> {
        let worker = self.worker();
        let id = worker.request(request);
        let reply = receive(&worker);
        assert_eq!(reply.id, id);
        reply.result
    }

    /// Every thread has ended.
    fn settle(&self) {
        wait_until("threads end", || self.service.threads() == 0);
    }

    fn hits(&self) -> usize {
        self.server.paths().len()
    }
}

fn receive(worker: &Worker) -> Reply {
    let mut reply = None;
    wait_until("a reply", || {
        reply = worker.receive();
        reply.is_some()
    });
    reply.unwrap()
}

fn lookup(query: &str) -> Request {
    Request::Wikipedia { edition: Edition::ENGLISH, query: query.into() }
}

fn translate(text: &str, source: Option<&str>, target: &str) -> Request {
    Request::Translate {
        text: text.into(),
        source: source.map(Into::into),
        target: target.into(),
        provider: translate::Provider::Google,
        api_key: None,
    }
}

fn deepl_request(text: &str, source: Option<&str>, target: &str, api_key: Option<&str>) -> Request {
    Request::Translate {
        text: text.into(),
        source: source.map(Into::into),
        target: target.into(),
        provider: translate::Provider::DeepL,
        api_key: api_key.map(Into::into),
    }
}

fn body(value: serde_json::Value) -> Http {
    Http::Gzip(value.to_string().into_bytes())
}

fn found(titles: &[(&str, Option<&str>)]) -> Http {
    let hits: Vec<_> = titles
        .iter()
        .map(|(title, redirect)| match redirect {
            Some(r) => json!({"ns": 0, "title": title, "snippet": "", "redirecttitle": r}),
            None => json!({"ns": 0, "title": title, "snippet": ""}),
        })
        .collect();
    body(json!({"batchcomplete": true, "query": {"searchinfo": {"totalhits": hits.len()}, "search": hits}}))
}

fn page(title: &str, extract: &str) -> serde_json::Value {
    json!({"pageid": 1, "ns": 0, "title": title, "extract": extract,
        "canonicalurl": format!("https://en.wikipedia.org/wiki/{}", title.replace(' ', "_"))})
}

fn pages(page: serde_json::Value) -> Http {
    body(json!({"batchcomplete": true, "query": {"pages": [page]}}))
}

/// Searches get `hits`, articles `article`.
fn wikipedia(hits: &[(&str, Option<&str>)], article: Http) -> Bench {
    let hits = found(hits);
    bench(move |r| if r.param("list").is_some() { hits.clone() } else { article.clone() })
}

#[test]
fn without_wifi_nothing_is_sent() {
    let b = bench(|_| Http::Status(500));
    for (link, error) in [(0, Error::WifiOff), (1, Error::Connecting)] {
        b.link.store(link, Ordering::Relaxed);
        assert_eq!(b.ask(lookup("fox")), Err(error.clone()));
        assert_eq!(b.ask(translate("fox", None, "de")), Err(error));
    }
    // Nor for what can't be answered.
    b.link.store(2, Ordering::Relaxed);
    assert_eq!(b.ask(lookup(&"x".repeat(QUERY_LIMIT + 1))), Err(Error::QueryTooLong));
    assert_eq!(b.ask(lookup("?")), Ok(Answer::Found(Found::Hits(Vec::new()))));
    let long = "x".repeat(translate::TEXT_LIMIT + 1);
    assert_eq!(b.ask(self::translate(&long, None, "de")), Err(Error::TextTooLong));
    assert_eq!(b.ask(self::translate("fox", Some("de"), "de")), Err(Error::SameLanguage));
    assert_eq!(b.ask(self::translate("fox", None, "und")), Err(Error::Language("und".into())));
    assert_eq!(b.hits(), 0);
}

#[test]
fn a_clear_hit_opens_its_article() {
    let article = pages(page("United States", "A country.\n\n\n== History ==\nLong."));
    let b = wikipedia(&[("United States", Some("USA")), ("USA Today", None)], article);
    let answer = b.ask(Request::Wikipedia { edition: Edition::for_language("nb").unwrap(), query: "“usa”".into() });
    let expected = Article {
        edition: Edition::for_language("no").unwrap(),
        title: "United States".into(),
        redirected_from: Some("USA".into()),
        url: "https://en.wikipedia.org/wiki/United_States".into(),
        disambiguation: false,
        blocks: vec![
            Block::Paragraph("A country.".into()),
            Block::Heading { level: 2, text: "History".into() },
            Block::Paragraph("Long.".into()),
        ],
    };
    assert_eq!(answer, Ok(Answer::Found(Found::Article { article: expected, others: vec![hit("USA Today", None)] })));
    let (search, article) = (b.server.request(0), b.server.request(1));
    assert_eq!(search.param("srsearch").as_deref(), Some("usa"));
    assert_eq!(article.param("titles").as_deref(), Some("United States"));
    for request in [search, article] {
        assert!(request.path.starts_with("/no/api?"), "{}", request.path);
        assert!(request.user_agent.ends_with("(https://github.com/ShaderCompilation/Verso)"), "{}", request.user_agent);
    }
}

#[test]
fn other_lookups_list_their_hits() {
    let text = || page("Mercury", "Mercury may refer to:\n\n\nMercury (planet)");
    let mut disambiguation = text();
    disambiguation["pageprops"] = json!({"disambiguation": ""});
    let disambiguation = || pages(disambiguation.clone());
    let gone = || pages(json!({"ns": 0, "title": "Mercury", "missing": true}));
    let mercury = [("Mercury", None), ("Freddie Mercury", None)];
    let ask = |hits: &[(&str, Option<&str>)], article: Http| {
        let b = wikipedia(hits, article);
        let Ok(Answer::Found(found)) = b.ask(lookup("mercury")) else { panic!() };
        (found, b.hits())
    };
    let freddie = || Found::Hits(vec![hit("Freddie Mercury", None)]);
    assert_eq!(ask(&mercury, disambiguation()), (freddie(), 2), "its meanings aren't an article");
    assert_eq!(ask(&mercury, gone()), (freddie(), 2));
    assert_eq!(ask(&mercury, pages(page("Mercury", ""))), (freddie(), 2), "no text");
    // Nothing else to offer: the meanings themselves.
    assert!(matches!(ask(&mercury[..1], disambiguation()).0, Found::Article { article, .. } if article.disambiguation));
    // No clear match, or none at all: one request.
    let unclear = [("Mercury Records", None), ("Freddie Mercury", None)];
    assert!(matches!(ask(&unclear, pages(text())), (Found::Hits(hits), 1) if hits.len() == 2));
    assert_eq!(ask(&[], pages(text())), (Found::Hits(Vec::new()), 1));
}

#[test]
fn a_chosen_article_names_its_redirect() {
    let b = bench(|r| match r.param("titles").unwrap().as_str() {
        "WWII" => body(json!({"query": {"redirects": [{"from": "WWII", "to": "World War II"}],
            "pages": [page("World War II", "A war.")]}})),
        _ => pages(json!({"title": "Gone", "missing": true})),
    });
    let open = |title: &str| b.ask(Request::Article { edition: Edition::ENGLISH, title: title.into() });
    let Ok(Answer::Article(article)) = open("WWII") else { panic!() };
    assert_eq!((article.title.as_str(), article.redirected_from.as_deref()), ("World War II", Some("WWII")));
    assert_eq!(open("Gone"), Err(Error::NoArticle));
}

#[test]
fn translations_are_posted() {
    let b = bench(|_| body(json!([[["The fox. ", "Der Fuchs. "], ["He jumps.", "Er springt."]], null, "de"])));
    // Any address would be too long for this.
    let text = "Der Fuchs. ".repeat(translate::TEXT_LIMIT / 11);
    let Ok(Answer::Translation(translation)) = b.ask(self::translate(&text, None, "en")) else { panic!() };
    assert_eq!((translation.text.as_str(), translation.detected.as_deref()), ("The fox. He jumps.", Some("de")));
    let request = b.server.request(0);
    assert_eq!((request.method.as_str(), request.path.as_str()), ("POST", "/translate"));
    assert_eq!(request.param("q"), Some(text));
    assert_eq!([request.param("sl"), request.param("tl")], [Some("auto".into()), Some("en".into())]);
    assert_eq!(request.user_agent, crate::net::user_agent());
    b.ask(self::translate("Fuchs", Some("de"), "zh")).unwrap();
    let request = b.server.request(1);
    assert_eq!([request.param("sl"), request.param("tl")], [Some("de".into()), Some("zh-CN".into())]);
}

#[test]
fn deepl_selects_free_or_pro_sends_auth_handles_errors_and_falls_back() {
    let b = bench(|r| match r.path.as_str() {
        "/deepl/free" => match r.authorization.as_str() {
            "DeepL-Auth-Key bad-key:fx" => Http::Status(403),
            "DeepL-Auth-Key quota-key:fx" => Http::Status(456),
            _ => body(json!({"translations": [{"detected_source_language": "NB", "text": "The fox."}]})),
        },
        "/deepl/pro" => body(json!({"translations": [{"detected_source_language": "FR", "text": "Fox pro."}]})),
        "/translate" => body(json!([[["Google fallback.", "orig"]], null, "fr"])),
        _ => Http::Status(404),
    });

    // Free key (:fx) → /deepl/free with DeepL-Auth-Key header and mapped codes.
    let free_key = "12345678-1234-1234-1234-1234567890ab:fx";
    let req = deepl_request("Reven.", None, "en", Some(free_key));
    assert!(!format!("{req:?}").contains("12345678"), "Debug must never leak the API key");
    let Ok(Answer::Translation(tr)) = b.ask(req) else { panic!() };
    assert_eq!(
        (tr.text.as_str(), tr.detected.as_deref(), tr.provider),
        ("The fox.", Some("no"), translate::Provider::DeepL)
    );
    let sent = b.server.request(0);
    assert_eq!((sent.method.as_str(), sent.path.as_str()), ("POST", "/deepl/free"));
    assert_eq!(sent.authorization, format!("DeepL-Auth-Key {free_key}"));
    assert_eq!(
        (sent.param("text").as_deref(), sent.param("source_lang"), sent.param("target_lang").as_deref()),
        (Some("Reven."), None, Some("EN-US"),)
    );

    // Pro key (no :fx) → /deepl/pro with explicit source_lang.
    let pro_key = "pro-key-without-suffix";
    let Ok(Answer::Translation(tr)) = b.ask(deepl_request("Renard.", Some("fr"), "pt", Some(pro_key))) else {
        panic!()
    };
    assert_eq!((tr.text.as_str(), tr.provider), ("Fox pro.", translate::Provider::DeepL));
    let sent = b.server.request(1);
    assert_eq!(sent.path, "/deepl/pro");
    assert_eq!(sent.authorization, format!("DeepL-Auth-Key {pro_key}"));
    assert_eq!(
        (sent.param("source_lang").as_deref(), sent.param("target_lang").as_deref()),
        (Some("FR"), Some("PT-BR"))
    );

    // 403 unauthorized and 456 quota exceeded map to dedicated non-retryable errors.
    assert_eq!(b.ask(deepl_request("x", None, "en", Some("bad-key:fx"))), Err(Error::Unauthorized));
    assert_eq!(b.ask(deepl_request("x", None, "en", Some("quota-key:fx"))), Err(Error::QuotaExceeded));

    // Empty / missing DeepL key falls back cleanly to Google Translate.
    for empty in [None, Some(""), Some("   ")] {
        let Ok(Answer::Translation(tr)) = b.ask(deepl_request("Renard.", None, "en", empty)) else { panic!() };
        assert_eq!((tr.text.as_str(), tr.provider), ("Google fallback.", translate::Provider::Google));
    }

    // Unsupported target or source language falls back cleanly to Google Translate.
    let Ok(Answer::Translation(tr)) = b.ask(deepl_request("Renard.", None, "ka", Some(free_key))) else { panic!() };
    assert_eq!((tr.text.as_str(), tr.provider), ("Google fallback.", translate::Provider::Google));
    let Ok(Answer::Translation(tr)) = b.ask(deepl_request("Vulpes.", Some("la"), "en", Some(free_key))) else {
        panic!()
    };
    assert_eq!((tr.text.as_str(), tr.provider), ("Google fallback.", translate::Provider::Google));
    let last = b.server.request(b.hits() - 1);
    assert_eq!(last.path, "/translate");
    assert!(last.authorization.is_empty(), "DeepL key is never sent to Google");
}

#[test]
fn failures_keep_their_reason() {
    use FetchError as F;
    let big = vec![b' '; MAX_REPLY_BYTES as usize + 1];
    for (reply, error) in [
        (Http::Silent, Error::Fetch(F::Timeout)),
        (Http::Stall(1000), Error::Fetch(F::Timeout)),
        (Http::Status(429), Error::Fetch(F::Status(429))),
        (Http::Status(503), Error::Fetch(F::Status(503))),
        (Http::Gzip(big), Error::Fetch(F::TooLarge(MAX_REPLY_BYTES))),
        (Http::Ok(b"<html>".to_vec()), Error::Unreadable),
    ] {
        let b = bench_within(Duration::from_millis(300), move |_| reply.clone());
        assert_eq!(b.ask(lookup("fox")), Err(error.clone()));
        assert_eq!(b.ask(self::translate("fox", None, "de")), Err(error));
    }
    let b = bench(|_| body(json!({"error": {"code": "readonly", "info": "…"}})));
    assert_eq!(b.ask(lookup("fox")), Err(Error::Refused("readonly".into())));
}

#[test]
fn errors_say_what_to_do() {
    use FetchError as F;
    for (error, title, retry) in [
        (Error::WifiOff, "Wi-Fi is off", true),
        (Error::Connecting, "Wi-Fi is still connecting", true),
        (Error::Fetch(F::Tls("expired".into())), "Couldn’t make a secure connection", true),
        (Error::Fetch(F::Timeout), "Wikipedia took too long to answer", true),
        (Error::Fetch(F::Unreachable("dns".into())), "Couldn’t reach Wikipedia", true),
        (Error::Fetch(F::Status(429)), "Wikipedia is busy", true),
        (Error::Fetch(F::Status(503)), "Wikipedia had a problem", true),
        (Error::Fetch(F::TooLarge(1)), "Wikipedia had a problem", false),
        (Error::Unauthorized, "Wikipedia rejected the API key", false),
        (Error::QuotaExceeded, "Wikipedia quota exceeded", false),
        (Error::TextTooLong, "Too long to translate", false),
    ] {
        assert_eq!((error.title("Wikipedia").as_str(), error.retryable()), (title, retry), "{error:?}");
        assert!(!error.plain().is_empty());
    }
    // Addresses hold what was looked up.
    for error in [F::Insecure("http://x/?q=secret".into()), F::Other("bad uri: secret".into())] {
        assert!(!Error::Fetch(error).kind().contains("secret"));
    }
}

/// Holds replies back until opened.
#[derive(Clone, Default)]
struct Gate(Arc<(Mutex<bool>, Condvar)>);

impl Gate {
    fn pass(&self) {
        let open = self.0.0.lock().unwrap();
        drop(self.0.1.wait_timeout_while(open, Duration::from_secs(5), |open| !*open).unwrap());
    }

    fn open(&self) {
        *self.0.0.lock().unwrap() = true;
        self.0.1.notify_all();
    }
}

#[test]
fn obsolete_requests_are_dropped() {
    let gate = Gate::default();
    let held = gate.clone();
    let b = bench(move |r| match r.param("srsearch").as_deref() {
        Some("slow") => {
            held.pass();
            found(&[("Slow", None)])
        }
        Some(_) => found(&[]),
        None => pages(page("Slow", "Text.")),
    });
    let (worker, cancelled, closed) = (b.worker(), b.worker(), b.worker());
    let slow = worker.request(lookup("slow"));
    cancelled.request(lookup("slow"));
    closed.request(lookup("slow"));
    wait_until("three searches", || b.hits() == 3);
    cancelled.cancel();
    drop(closed);
    // The newest request's reply arrives, whatever the older one does.
    let newer = worker.request(lookup("other"));
    assert!(newer > slow);
    gate.open();
    let reply = receive(&worker);
    assert_eq!((reply.id, reply.result), (newer, Ok(Answer::Found(Found::Hits(Vec::new())))));
    b.settle();
    assert_eq!((worker.receive(), cancelled.receive()), (None, None));
    assert_eq!(b.woken.load(Ordering::Relaxed), 1);
    assert_eq!(b.hits(), 4, "an obsolete lookup never fetches its article");
    // A reply that already arrived is forgotten too.
    worker.request(lookup("other"));
    wait_until("the reply", || b.woken.load(Ordering::Relaxed) == 2);
    worker.cancel();
    assert_eq!(worker.receive(), None);
}

#[test]
fn at_most_three_requests_run_at_once() {
    let gate = Gate::default();
    let held = gate.clone();
    let b = bench(move |_| {
        held.pass();
        found(&[])
    });
    let workers: Vec<Worker> = (0..4).map(|_| b.worker()).collect();
    for worker in &workers {
        worker.request(lookup("held"));
    }
    wait_until("three searches", || b.hits() == 3);
    // The waiting request is replaced, not run.
    workers[3].request(lookup("last"));
    assert_eq!(b.service.threads(), 3);
    gate.open();
    for worker in &workers {
        assert!(receive(worker).result.is_ok());
    }
    b.settle();
    let mut searches: Vec<String> = (0..b.hits()).map(|i| b.server.request(i).param("srsearch").unwrap()).collect();
    assert_eq!(searches.pop().as_deref(), Some("last"));
    assert_eq!(searches, ["held"; 3]);
}
