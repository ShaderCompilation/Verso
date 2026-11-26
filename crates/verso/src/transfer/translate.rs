//! The Translate tab API: read and update the translation provider and DeepL
//! API key over Wi-Fi so readers can paste a 39-character key from a phone or
//! computer instead of typing it on e-ink. The stored key is never written to
//! logs, and responses carry only its last four characters (`key_hint`) so the
//! page can show which key is saved.

use std::sync::Arc;

use serde_json::{Value, json};

use super::Activity;
use super::books::ActivitySink;
use super::http::{Body, Response};
use crate::online::translate::{deepl_tier, parse_deepl_key};
use crate::store::{Settings, Store, TranslateProvider};

const MAX_BODY: usize = 1024;

pub struct Translate {
    store: Store,
    activity: Arc<dyn ActivitySink>,
}

impl Translate {
    pub fn new(store: Store, activity: Arc<dyn ActivitySink>) -> Self {
        Self { store, activity }
    }

    /// `GET /api/translate`
    pub fn get(&self) -> Response {
        Response::json(200, &self.state(&self.store.settings()))
    }

    /// `PUT /api/translate`
    pub fn update(&self, body: &mut Body<'_>) -> Response {
        if self.store.live_data_edited() {
            return Response::json(
                409,
                &json!({ "error": "Restart Verso to apply your edits first: tap Done on the Kindle.", "restart": true }),
            );
        }
        let Ok(Some(bytes)) = body.read_small(MAX_BODY) else {
            return Response::json(413, &json!({ "error": "Request body is too large." }));
        };
        #[derive(serde::Deserialize)]
        struct Update {
            provider: Option<TranslateProvider>,
            api_key: Option<String>,
        }
        let Ok(update) = serde_json::from_slice::<Update>(&bytes) else {
            return Response::json(400, &json!({ "error": "Invalid JSON body." }));
        };
        let mut settings = self.store.settings();
        let mut activity_label = None;
        if let Some(raw_key) = update.api_key {
            match parse_deepl_key(&raw_key) {
                Ok(parsed) => {
                    activity_label = Some(if parsed.is_some() {
                        "DeepL API key saved".to_owned()
                    } else {
                        "DeepL API key removed".to_owned()
                    });
                    settings.deepl_api_key = parsed;
                }
                Err(msg) => return Response::json(400, &json!({ "error": msg })),
            }
        }
        if let Some(provider) = update.provider {
            if activity_label.is_none() && settings.translate_provider != provider {
                activity_label = Some(format!("Translate: {}", provider.service_name()));
            }
            settings.translate_provider = provider;
        }
        if let Err(e) = self.store.save_settings(&settings) {
            log::error!("web app: saving translation settings: {e:#}");
            return Response::json(500, &json!({ "error": "Couldn't save translation settings." }));
        }
        log::info!(
            "web app: updated translation settings (provider={:?}, key_set={})",
            settings.translate_provider,
            settings.deepl_api_key.is_some()
        );
        if let Some(label) = activity_label {
            self.activity.push(Activity::Setting { label });
        }
        Response::json(200, &self.state(&settings))
    }

    fn state(&self, settings: &Settings) -> Value {
        let key_tier = settings.deepl_api_key.as_deref().map(|k| deepl_tier(k).as_str());
        json!({
            "provider": settings.translate_provider,
            "has_key": settings.deepl_api_key.is_some(),
            "key_tier": key_tier,
            "key_hint": settings.deepl_api_key.as_deref().map(key_hint),
            "restart": self.store.live_data_edited(),
        })
    }
}

/// The last four characters of a key, keeping a Free key's `:fx` suffix, so
/// readers can recognize the saved key. Too short a key reveals nothing.
fn key_hint(key: &str) -> String {
    let (body, suffix) = key.strip_suffix(":fx").map_or((key, ""), |b| (b, ":fx"));
    let chars: Vec<char> = body.chars().collect();
    if chars.len() <= 8 {
        return suffix.to_owned();
    }
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("{tail}{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transfer::api::{Api, Context};
    use crate::transfer::auth::Auth;
    use crate::transfer::server::{Server, Timeouts};
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::sync::Mutex;
    use std::time::Duration;

    #[derive(Default)]
    struct Recorded(Mutex<Vec<Activity>>);

    impl ActivitySink for Recorded {
        fn begin_receiving(&self, _name: String) -> u64 {
            0
        }
        fn finish_receiving(&self, _id: u64, _received: Option<Activity>) {}
        fn push(&self, activity: Activity) {
            self.0.lock().unwrap().push(activity);
        }
    }

    struct Env {
        _dir: tempfile::TempDir,
        store: Store,
        server: Server,
        cookie: String,
        activity: Arc<Recorded>,
    }

    impl Env {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let store = Store::at(dir.path().join("data"));
            let activity = Arc::new(Recorded::default());
            let translate = Translate::new(store.clone(), activity.clone());
            let api = Arc::new(Api::new(
                Auth::with_code("4827".into()),
                Context {
                    port: 80,
                    names: Box::new(|| vec!["verso.local".into()]),
                    status: Box::new(|| json!({})),
                    changed: Box::new(|| {}),
                    books: None,
                    dictionaries: None,
                    files: None,
                    translate: Some(translate),
                },
            ));
            let server = Server::start("127.0.0.1:0".parse().unwrap(), api, Timeouts::default()).unwrap();
            let mut env = Self { _dir: dir, store, server, cookie: String::new(), activity };
            let (status, head, _) = env
                .raw("POST /api/login HTTP/1.1\r\nHost: verso.local\r\nX-Verso-Request: 1\r\n", r#"{"code":"4827"}"#);
            assert_eq!(status, 200);
            env.cookie =
                head.lines().find_map(|l| l.strip_prefix("Set-Cookie: ")).unwrap().split(';').next().unwrap().into();
            env
        }

        fn raw(&self, head: &str, body: &str) -> (u16, String, String) {
            let mut stream = TcpStream::connect(self.server.addr()).unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let req = format!("{head}Content-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            stream.write_all(req.as_bytes()).unwrap();
            let mut resp = String::new();
            stream.read_to_string(&mut resp).unwrap();
            let (h, b) = resp.split_once("\r\n\r\n").unwrap();
            (h[9..12].parse().unwrap(), h.to_owned(), b.to_owned())
        }

        fn get(&self) -> (u16, Value) {
            let head = format!("GET /api/translate HTTP/1.1\r\nHost: verso.local\r\nCookie: {}\r\n", self.cookie);
            let (code, _, body) = self.raw(&head, "");
            (code, serde_json::from_str(&body).unwrap())
        }

        fn put(&self, payload: Value) -> (u16, Value) {
            let head = format!(
                "PUT /api/translate HTTP/1.1\r\nHost: verso.local\r\nCookie: {}\r\nX-Verso-Request: 1\r\n",
                self.cookie
            );
            let (code, _, body) = self.raw(&head, &payload.to_string());
            (code, serde_json::from_str(&body).unwrap())
        }
    }

    #[test]
    fn saves_validates_clears_and_echoes_only_a_hint_of_the_deepl_key() {
        let env = Env::new();
        let (code, body) = env.get();
        assert_eq!(code, 200);
        assert_eq!(
            body,
            json!({"provider": "google", "has_key": false, "key_tier": null, "key_hint": null, "restart": false})
        );

        // Save a Free key (with whitespace trimmed) and switch to DeepL.
        let key = "12345678-1234-1234-1234-1234567890ab:fx";
        let (code, body) = env.put(json!({"provider": "deepl", "api_key": format!("  {key}\n")}));
        assert_eq!(code, 200);
        assert_eq!(
            body,
            json!({"provider": "deepl", "has_key": true, "key_tier": "free", "key_hint": "90ab:fx", "restart": false})
        );
        assert!(!body.to_string().contains("12345678"), "GET/PUT must never echo the raw key");
        assert_eq!(env.store.settings().deepl_api_key.as_deref(), Some(key));
        assert_eq!(env.store.settings().translate_provider, TranslateProvider::DeepL);

        // GET reports Pro tier for a Pro key and echoes only its last four
        // characters; a short key echoes none.
        let (code, body) = env.put(json!({"api_key": "pro-key-12345678"}));
        assert_eq!(code, 200);
        assert_eq!(body["key_tier"], "pro");
        let (code, body) = env.get();
        assert_eq!(code, 200);
        assert_eq!(body["key_hint"], "5678");
        assert!(!body.to_string().contains("pro-key"));
        assert_eq!(env.put(json!({"api_key": "short:fx"})).1["key_hint"], ":fx");

        // Rejects control chars / whitespace inside a key.
        assert_eq!(env.put(json!({"api_key": "bad key\r\nX: 1"})).0, 400);

        // Clearing with empty string removes the key cleanly.
        let (code, body) = env.put(json!({"api_key": ""}));
        assert_eq!(code, 200);
        assert_eq!(body["has_key"], false);
        assert_eq!(env.store.settings().deepl_api_key, None);
        assert!(!env.activity.0.lock().unwrap().is_empty());

        // Live-data edits lock writes with 409.
        env.store.note_live_data_edit();
        assert_eq!(env.put(json!({"provider": "google"})).0, 409);
    }
}
