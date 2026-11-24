//! Files tab: every permission class, path rules, names, uploads with and
//! without overwrite, rename, move, delete and downloads, through the real
//! server.

use super::*;
use crate::transfer::api::{Api, Context};
use crate::transfer::auth::Auth;
use crate::transfer::server::{Server, Timeouts};
use std::net::TcpStream;
use std::sync::Mutex;
use std::time::Duration;

#[derive(Default)]
struct Recorded {
    events: Mutex<Vec<Activity>>,
}

impl ActivitySink for Recorded {
    fn begin_receiving(&self, _name: String) -> u64 {
        1
    }
    fn finish_receiving(&self, _id: u64, received: Option<Activity>) {
        if let Some(a) = received {
            self.events.lock().unwrap().push(a);
        }
    }
    fn push(&self, activity: Activity) {
        self.events.lock().unwrap().push(activity);
    }
}

/// A Kindle-like storage root: `documents` is the library, Verso's data is
/// in `verso/data`, and there are KOReader, KUAL, jailbreak and system
/// folders.
struct Env {
    dir: tempfile::TempDir,
    root: PathBuf,
    store: Store,
    server: Server,
    cookie: String,
    library_changed: Arc<AtomicBool>,
    dictionaries_changed: Arc<AtomicBool>,
    activity: Arc<Recorded>,
}

fn s(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|p| (*p).to_owned()).collect()
}

impl Env {
    fn new() -> Self {
        Self::with_developer(false)
    }

    fn with_developer(developer: bool) -> Self {
        Self::build(developer, list_names)
    }

    fn with_listing(list: fn(&Path) -> io::Result<Vec<String>>) -> Self {
        Self::build(false, list)
    }

    fn build(developer: bool, list: fn(&Path) -> io::Result<Vec<String>>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("us");
        for d in [
            "documents/Russian",
            "fonts",
            "koreader/data/dict",
            "extensions/koreader",
            "extensions/verso",
            "verso/bin",
            "verso/cache",
            "verso/data/books",
            "verso/data/annotations",
            "verso/data/covers",
            "verso/data/dictionaries/Ozhegov",
            "verso/data/exports",
            "libkh",
            "system",
            "lost+found",
        ] {
            fs::create_dir_all(root.join(d)).unwrap();
        }
        for (f, body) in [
            ("documents/notes.txt", "notes"),
            ("documents/.hidden-note", "dot"),
            ("verso/data/settings.json", "{}"),
            ("verso/verso.sh", "#!/bin/sh"),
            ("verso/data/exports/Book.md", "# Book"),
            ("verso/data/dictionaries/Ozhegov/ozhegov.ifo", "StarDict's dict ifo file"),
            ("extensions/verso/menu.json", "{}"),
            ("libkh/hotfix", "x"),
        ] {
            fs::write(root.join(f), body).unwrap();
        }
        let store = Store::at(root.join("verso/data"));
        store.save_library_folders(vec![root.join("documents")]).unwrap();
        store.save_dictionary_folders(&[]).unwrap();
        let library_changed = Arc::new(AtomicBool::new(false));
        let dictionaries_changed = Arc::new(AtomicBool::new(false));
        let activity = Arc::new(Recorded::default());
        let files = Files::new(
            root.clone(),
            store.clone(),
            Arc::new(|_| Ok(10 * 1024 * 1024 * 1024)),
            activity.clone(),
            developer,
            library_changed.clone(),
            dictionaries_changed.clone(),
        )
        .listing_with(list);
        let api = Arc::new(Api::new(
            Auth::with_code("4827".into()),
            Context {
                port: 80,
                names: Box::new(|| vec!["verso.local".into()]),
                status: Box::new(|| json!({})),
                changed: Box::new(|| {}),
                books: None,
                dictionaries: None,
                files: Some(files),
            },
        ));
        let server = Server::start("127.0.0.1:0".parse().unwrap(), api, Timeouts::default()).unwrap();
        let mut env =
            Self { dir, root, store, server, cookie: String::new(), library_changed, dictionaries_changed, activity };
        let (status, head, _) =
            env.raw("POST /api/login HTTP/1.1\r\nHost: verso.local\r\nX-Verso-Request: 1\r\n", br#"{"code":"4827"}"#);
        assert_eq!(status, 200);
        env.cookie =
            head.lines().find_map(|l| l.strip_prefix("Set-Cookie: ")).unwrap().split(';').next().unwrap().to_owned();
        env
    }

    fn raw(&self, head: &str, body: &[u8]) -> (u16, String, Vec<u8>) {
        let mut stream = TcpStream::connect(self.server.addr()).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let req = format!("{head}Content-Length: {}\r\nConnection: close\r\n\r\n", body.len());
        stream.write_all(req.as_bytes()).unwrap();
        stream.write_all(body).unwrap();
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).unwrap();
        let split = raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        let head = String::from_utf8(raw[..split].to_vec()).unwrap();
        (head[9..12].parse().unwrap(), head, raw[split + 4..].to_vec())
    }

    fn req(&self, method: &str, target: &str, body: &[u8]) -> (u16, String, Vec<u8>) {
        let head = format!(
            "{method} {target} HTTP/1.1\r\nHost: verso.local\r\nCookie: {}\r\nX-Verso-Request: 1\r\n",
            self.cookie
        );
        self.raw(&head, body)
    }

    fn json(&self, method: &str, target: &str, body: &[u8]) -> (u16, Value) {
        let (status, _, body) = self.req(method, target, body);
        let value = serde_json::from_slice(&body)
            .unwrap_or_else(|e| panic!("status {status}, body {:?}: {e}", String::from_utf8_lossy(&body)));
        (status, value)
    }

    fn list(&self, path: &str) -> (u16, Value) {
        self.json("GET", &format!("/api/files?path={}", encode(path)), b"")
    }

    fn names(&self, path: &str, hidden: bool) -> Vec<String> {
        let (status, list) = self.json(
            "GET",
            &format!("/api/files?path={}{}", encode(path), if hidden { "&hidden=1" } else { "" }),
            b"",
        );
        assert_eq!(status, 200, "{list}");
        list["entries"].as_array().unwrap().iter().map(|e| e["name"].as_str().unwrap().to_owned()).collect()
    }

    fn entry(&self, folder: &str, name: &str) -> Value {
        let (_, list) = self.list(folder);
        list["entries"].as_array().unwrap().iter().find(|e| e["name"] == name).cloned().unwrap()
    }

    fn put(&self, path: &str, overwrite: bool, body: &[u8]) -> (u16, Value) {
        let flag = if overwrite { "&overwrite=1" } else { "" };
        self.json("PUT", &format!("/api/files?path={}{flag}", encode(path)), body)
    }

    fn post(&self, what: &str, body: Value) -> (u16, Value) {
        self.json("POST", &format!("/api/files/{what}"), body.to_string().as_bytes())
    }

    fn delete(&self, path: &str, confirm: bool) -> (u16, Value) {
        let flag = if confirm { "&confirm=2" } else { "" };
        self.json("DELETE", &format!("/api/files?path={}{flag}", encode(path)), b"")
    }

    fn no_part_files(&self) {
        let mut stack = vec![self.root.clone()];
        while let Some(dir) = stack.pop() {
            for entry in fs::read_dir(&dir).unwrap().flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                assert!(!name.ends_with(".part"), "part file left: {}", entry.path().display());
                if entry.file_type().unwrap().is_dir() {
                    stack.push(entry.path());
                }
            }
        }
    }
}

fn encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b'/') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[test]
fn classifies_every_class_ignoring_case() {
    let cases: &[(&[&str], Class)] = &[
        (&[], Class::User),
        (&["documents", "Book.epub"], Class::User),
        (&["My Library"], Class::User),
        (&["fonts", "Literata.ttf"], Class::User),
        (&["koreader", "settings.reader.lua"], Class::User),
        (&["extensions", "koreader", "menu.json"], Class::User),
        (&["extensions"], Class::User),
        (&["verso", "data", "dictionaries"], Class::User),
        (&["verso", "data", "dictionaries", "Ozhegov", "o.ifo"], Class::User),
        (&["verso", "data", "exports", "Book.md"], Class::User),
        (&["verso", "data", "settings.json"], Class::LiveData),
        (&["verso", "data", "library.json"], Class::LiveData),
        (&["verso", "data", "books", "abc.json"], Class::LiveData),
        (&["verso", "data", "annotations"], Class::LiveData),
        (&["verso"], Class::App),
        (&["verso", "verso"], Class::App),
        (&["verso", "verso.sh"], Class::App),
        (&["verso", "verso.log"], Class::App),
        (&["verso", "installed.json"], Class::App),
        (&["verso", "data"], Class::App),
        (&["verso", "data", "sub", "x.json"], Class::App),
        (&["extensions", "verso", "menu.json"], Class::App),
        (&["verso", "cache", "x"], Class::Cache),
        (&["verso", "data", "covers"], Class::Cache),
        (&["verso", "data", "cre", "a.cache"], Class::Cache),
        (&["verso", "data", "dictionary-cache"], Class::Cache),
        (&["libkh"], Class::System),
        (&["mrpackages", "x.bin"], Class::System),
        (&["system", "fonts"], Class::System),
        (&[".active_content_sandbox"], Class::System),
        (&[".KFPM"], Class::System),
        (&["lost+found"], Class::System),
        // New paths under verso stay read-only without changing the table.
        (&["verso", "update"], Class::App),
        (&["verso", "data", "new-folder"], Class::App),
        (&["verso", "data", "staging", "a.json"], Class::App),
        // Case doesn't matter: on FAT storage these reach the same files.
        (&["VERSO", "data", "settings.json"], Class::LiveData),
        (&["Verso", "Data", "Books", "x"], Class::LiveData),
        (&["VERSO", "DATA", "Dictionaries", "x"], Class::User),
        (&["Verso", "Data", "COVERS"], Class::Cache),
        (&["EXTENSIONS", "Verso"], Class::App),
        (&["System"], Class::System),
        (&["LIBKH"], Class::System),
    ];
    for (rel, class) in cases {
        assert_eq!(classify(&s(rel)), *class, "{rel:?}");
    }
    for rel in
        [&[][..], &["verso"], &["extensions"], &["extensions", "verso"], &["Verso", "data", "Exports"], &["system"]]
    {
        assert!(holds_protected(&s(rel)), "{rel:?}");
    }
    for rel in
        [&["documents"][..], &["extensions", "koreader"], &["verso", "data", "exports", "Book.md"], &["koreader"]]
    {
        assert!(!holds_protected(&s(rel)), "{rel:?}");
    }
}

#[test]
fn lists_with_permissions_shortcuts_and_hidden_files() {
    let env = Env::new();
    let (status, root) = env.list("");
    assert_eq!(status, 200);
    assert_eq!(root["writable"], true);
    let names: Vec<_> = root["entries"].as_array().unwrap().iter().map(|e| e["name"].as_str().unwrap()).collect();
    // Folders, sorted; jailbreak and system folders hidden for everyone.
    assert_eq!(names, ["documents", "extensions", "fonts", "koreader", "verso"]);
    let entry = |name: &str| root["entries"].as_array().unwrap().iter().find(|e| e["name"] == name).unwrap().clone();
    assert_eq!(entry("documents")["writable"], true);
    assert_eq!(entry("documents")["twice"], true, "the library folder asks twice");
    assert_eq!(entry("koreader")["twice"], true);
    assert_eq!(entry("verso")["writable"], false);
    assert_eq!(entry("extensions")["writable"], false, "it holds extensions/verso");
    let labels: Vec<_> = root["shortcuts"].as_array().unwrap().iter().map(|s| s["label"].as_str().unwrap()).collect();
    assert_eq!(labels, ["Books", "Fonts", "Dictionaries", "Exports", "Everything"]);
    assert_eq!(root["shortcuts"][0]["path"], "documents");

    // Caches never show; app files and live data show read-only.
    assert_eq!(env.names("verso", false), ["bin", "data", "verso.sh"]);
    assert_eq!(
        env.names("verso/data", false),
        ["annotations", "books", "dictionaries", "exports", "dictionary-folders.json", "settings.json"]
    );
    let (_, data) = env.list("verso/data");
    assert_eq!(data["writable"], false);
    assert_eq!(env.entry("verso/data", "settings.json")["writable"], false);
    assert_eq!(env.entry("verso/data", "dictionaries")["writable"], false, "pinned");
    assert_eq!(env.entry("verso/data", "dictionaries")["readonly"], false, "but files go in");
    assert_eq!(env.entry("verso/data", "settings.json")["readonly"], true);
    let (_, dicts) = env.list("verso/data/dictionaries");
    assert_eq!(dicts["writable"], true);
    assert_eq!(env.entry("verso/data/dictionaries", "Ozhegov")["writable"], true);
    assert_eq!(env.list("verso/cache").0, 404);
    assert_eq!(env.list("verso/data/covers").0, 404);
    assert_eq!(env.list("libkh").0, 404);
    assert_eq!(env.json("GET", "/api/files/download?path=libkh/hotfix", b"").0, 404);

    // Dotfiles only with the switch.
    assert_eq!(env.names("documents", false), ["Russian", "notes.txt"]);
    assert_eq!(env.names("documents", true), ["Russian", ".hidden-note", "notes.txt"]);
    assert_eq!(env.list("documents/notes.txt").0, 400, "a file isn't a folder");
    assert_eq!(env.list("documents/missing").0, 404);

    let (_, crumbs) = env.list("documents/Russian");
    assert_eq!(
        crumbs["crumbs"],
        json!([
            { "name": "Kindle", "path": "" },
            { "name": "documents", "path": "documents" },
            { "name": "Russian", "path": "documents/Russian" },
        ])
    );
}

#[test]
fn reports_have_a_shortcut_and_download_but_stay_read_only() {
    let env = Env::new();
    let shortcut = || {
        let (_, root) = env.list("");
        root["shortcuts"].as_array().unwrap().iter().find(|s| s["label"] == "Reports").cloned()
    };
    assert_eq!(shortcut(), None, "no reports yet");
    let report = "verso/reports/crash-20261122-123000.txt";
    fs::create_dir_all(env.root.join("verso/reports")).unwrap();
    fs::write(env.root.join(report), "report").unwrap();
    assert_eq!(shortcut().unwrap()["path"], "verso/reports");

    assert_eq!(env.list("verso/reports").1["writable"], false);
    assert_eq!(env.entry("verso/reports", "crash-20261122-123000.txt")["writable"], false);
    let (status, _, body) = env.req("GET", &format!("/api/files/download?path={report}"), b"");
    assert_eq!((status, body.as_slice()), (200, &b"report"[..]));
    assert_eq!(env.put(report, true, b"x").0, 403);
    assert_eq!(env.put("verso/reports/new.txt", false, b"x").0, 403);
    assert_eq!(env.delete(report, false).0, 403);
    assert_eq!(fs::read(env.root.join(report)).unwrap(), b"report");
}

#[test]
fn developers_see_jailbreak_and_system_folders_read_only() {
    let env = Env::with_developer(true);
    assert_eq!(
        env.names("", false),
        ["documents", "extensions", "fonts", "koreader", "libkh", "lost+found", "system", "verso"]
    );
    assert_eq!(env.entry("", "libkh")["writable"], false);
    let (status, _, body) = env.req("GET", "/api/files/download?path=libkh/hotfix", b"");
    assert_eq!((status, body.as_slice()), (200, &b"x"[..]));
    assert_eq!(env.put("libkh/new", false, b"x").0, 403);
    assert_eq!(env.delete("libkh/hotfix", false).0, 403);
    assert_eq!(env.post("rename", json!({ "path": "system", "name": "old" })).0, 403);
    // Caches stay hidden even for developers.
    assert_eq!(env.list("verso/cache").0, 404);
}

#[test]
fn developers_write_app_files_at_once_and_live_data_for_a_restart() {
    let env = Env::with_developer(true);
    // App files: written like user content, nothing to restart for.
    assert_eq!(env.entry("verso", "verso.sh")["writable"], true);
    assert_eq!(env.entry("verso", "verso.sh")["readonly"], false);
    assert_eq!(env.put("verso/verso.sh", true, b"#!/bin/sh\necho new").0, 201);
    assert_eq!(env.put("extensions/verso/menu.json", true, b"{\"new\":1}").0, 201);
    assert_eq!(env.post("mkdir", json!({ "path": "verso/bin", "name": "tools" })).0, 201);
    assert_eq!(env.post("rename", json!({ "path": "verso/bin/tools", "name": "more" })).0, 200);
    assert_eq!(env.delete("verso/bin/more", false).0, 200);
    let (_, data) = env.list("verso/data");
    assert_eq!((data["writable"].clone(), data["live"].clone()), (json!(true), json!(false)));
    assert!(!env.store.live_data_edited(), "app files need no restart");

    // Verso's own folders stay where they are, caches stay hidden, and the
    // jailbreak stays read-only.
    for path in ["verso", "verso/data", "extensions/verso", "verso/data/dictionaries", "verso/data/exports"] {
        assert_eq!(env.post("rename", json!({ "path": path, "name": "renamed" })).0, 403, "rename {path}");
        assert_eq!(env.post("move", json!({ "path": path, "to": "documents" })).0, 403, "move {path}");
        assert_eq!(env.delete(path, true).0, 403, "delete {path}");
    }
    assert_eq!(env.put("verso/data/covers/x.png", false, b"x").0, 404);
    assert_eq!(env.post("mkdir", json!({ "path": "verso/data", "name": "covers" })).0, 403);
    assert_eq!(env.post("mkdir", json!({ "path": "verso", "name": "cache" })).0, 403);
    assert_eq!(env.put("libkh/new", false, b"x").0, 403);
    assert!(!env.store.live_data_edited());

    // Live data: written, and noted for Restart to apply.
    let (_, books) = env.list("verso/data/books");
    assert_eq!((books["writable"].clone(), books["live"].clone()), (json!(true), json!(true)));
    assert_eq!(env.entry("verso/data", "settings.json")["readonly"], false);
    assert_eq!(env.put("verso/data/settings.json", false, b"{}").0, 409, "replacing still asks");
    assert_eq!(env.put("verso/data/settings.json", true, b"{\"developer\":true}").0, 201);
    assert!(env.store.live_data_edited());
    assert_eq!(fs::read_to_string(env.root.join("verso/data/settings.json")).unwrap(), "{\"developer\":true}");
}

#[test]
fn every_write_into_live_data_is_noted() {
    type Op = fn(&Env) -> u16;
    let ops: [(&str, Op); 7] = [
        ("upload", |env| env.put("verso/data/books/new.json", false, b"{}").0),
        ("new folder", |env| env.post("mkdir", json!({ "path": "verso/data/annotations", "name": "x" })).0),
        ("rename", |env| env.post("rename", json!({ "path": "verso/data/books/a.json", "name": "b.json" })).0),
        ("move out", |env| env.post("move", json!({ "path": "verso/data/books/a.json", "to": "documents" })).0),
        ("move in", |env| env.post("move", json!({ "path": "documents/notes.txt", "to": "verso/data/books" })).0),
        ("delete", |env| env.delete("verso/data/books/a.json", false).0),
        ("delete a folder", |env| env.delete("verso/data/annotations", false).0),
    ];
    for (name, op) in ops {
        let env = Env::with_developer(true);
        fs::write(env.root.join("verso/data/books/a.json"), "{}").unwrap();
        // A link from user content counts as live data too.
        std::os::unix::fs::symlink(env.root.join("verso/data/books"), env.root.join("documents/states")).unwrap();
        assert!(!env.store.live_data_edited());
        let status = op(&env);
        assert!((200..300).contains(&status), "{name}: {status}");
        assert!(env.store.live_data_edited(), "{name}");
    }
    let env = Env::with_developer(true);
    std::os::unix::fs::symlink(env.root.join("verso/data/books"), env.root.join("documents/states")).unwrap();
    assert_eq!(env.put("documents/states/linked.json", false, b"{}").0, 201);
    assert!(env.store.live_data_edited(), "through a link");
    // Without developer mode, nothing gets that far.
    let env = Env::new();
    assert_eq!(env.put("verso/data/settings.json", true, b"{}").0, 403);
    assert!(!env.store.live_data_edited());
}

#[test]
fn developers_cant_reach_live_data_or_caches_by_other_fat_spellings() {
    let env = Env::build(true, fat_listing);
    for d in ["verso/data.", "verso/data/covers.", "verso/DATA~1"] {
        fs::create_dir_all(env.root.join(d)).unwrap();
    }
    fs::write(env.root.join("verso/data./settings.json"), "{}").unwrap();
    assert_eq!(env.put("verso/data./settings.json", true, b"x").0, 400);
    assert_eq!(env.put("verso/data/covers./x.png", false, b"x").0, 400);
    assert_eq!(env.put("verso/DATA~1/settings.json", false, b"x").0, 400);
    assert_eq!(env.post("mkdir", json!({ "path": "verso/data", "name": "covers." })).0, 409);
    assert_eq!(fs::read_to_string(env.root.join("verso/data./settings.json")).unwrap(), "{}");
    // Deeper names are only names.
    assert_eq!(env.put("verso/data/books/a~1.json", false, b"{}").0, 201);
}

#[test]
fn uploads_refuse_overwrites_without_the_flag() {
    let env = Env::new();
    let (status, saved) = env.put("documents/Russian/Пушкин.txt", false, "стихи".as_bytes());
    assert_eq!(status, 201, "{saved}");
    assert_eq!(saved["path"], "documents/Russian/Пушкин.txt");
    assert_eq!(fs::read_to_string(env.root.join("documents/Russian/Пушкин.txt")).unwrap(), "стихи");
    assert!(env.library_changed.load(Ordering::SeqCst), "a library folder changed");
    assert!(!env.dictionaries_changed.load(Ordering::SeqCst));

    let (status, body) = env.put("documents/notes.txt", false, b"new");
    assert_eq!((status, body["exists"].clone()), (409, json!(true)));
    assert_eq!(fs::read_to_string(env.root.join("documents/notes.txt")).unwrap(), "notes");
    assert_eq!(env.put("documents/notes.txt", true, b"new").0, 201);
    assert_eq!(fs::read_to_string(env.root.join("documents/notes.txt")).unwrap(), "new");
    // A folder is never replaced by a file.
    assert_eq!(env.put("documents/Russian", true, b"x").0, 409);
    assert!(env.root.join("documents/Russian").is_dir());

    // User content inside verso: dictionaries and exports.
    assert_eq!(env.put("verso/data/dictionaries/Ozhegov/ozhegov.idx", false, b"idx").0, 201);
    assert!(env.dictionaries_changed.load(Ordering::SeqCst));
    assert_eq!(env.put("verso/data/exports/Other.md", false, b"# Other").0, 201);
    assert_eq!(env.put("fonts/Literata.ttf", false, b"font").0, 201);
    assert_eq!(env.put("missing/a.txt", false, b"x").0, 404);
    assert_eq!(env.put("documents/notes.txt/a", false, b"x").0, 400, "the parent is a file");
    env.no_part_files();
    let events = env.activity.events.lock().unwrap();
    assert!(events.contains(&Activity::Received { name: "Пушкин.txt".into(), folder: "Russian".into() }));
}

#[test]
fn new_paths_under_verso_stay_read_only() {
    let env = Env::new();
    let settings = fs::read(env.root.join("verso/data/settings.json")).unwrap();
    for path in [
        "verso/new.txt",
        "verso/update/package.bin",
        "verso/data/settings.json",
        "verso/data/new.json",
        "verso/data/books/abc.json",
        "verso/bin/verso",
        "extensions/verso/menu.json",
        "verso.txt/../verso/x",
    ] {
        let (status, body) = env.put(path, true, b"x");
        assert!(matches!(status, 400 | 403 | 404), "{path}: {status} {body}");
    }
    assert_eq!(fs::read(env.root.join("verso/data/settings.json")).unwrap(), settings);
    assert!(!env.root.join("verso/new.txt").exists());
    // A new folder under verso, or one named like Verso's in the root.
    assert_eq!(env.post("mkdir", json!({ "path": "verso", "name": "update" })).0, 403);
    assert_eq!(env.post("mkdir", json!({ "path": "verso/data", "name": "mine" })).0, 403);
    assert_eq!(env.post("mkdir", json!({ "path": "", "name": "VERSO" })).0, 403);
    assert_eq!(env.post("mkdir", json!({ "path": "extensions", "name": "Verso" })).0, 403);
    assert_eq!(env.post("mkdir", json!({ "path": "", "name": "system" })).0, 403);
    assert_eq!(env.post("mkdir", json!({ "path": "verso/data/exports", "name": "2026" })).0, 201);
    assert_eq!(env.post("mkdir", json!({ "path": "documents", "name": "Russian" })).0, 409);
    assert!(!env.root.join("verso/update").exists());
    env.no_part_files();
}

#[test]
fn mixed_case_paths_reach_the_same_class() {
    let env = Env::new();
    // As on FAT storage, where VERSO/data/settings.json is Verso's settings.
    fs::create_dir_all(env.root.join("VERSO/data")).unwrap();
    fs::write(env.root.join("VERSO/data/settings.json"), "{}").unwrap();
    fs::create_dir_all(env.root.join("Verso/Data/Covers")).unwrap();
    assert_eq!(env.put("VERSO/data/settings.json", true, b"{\"x\":1}").0, 403);
    assert_eq!(env.put("VERSO/data/new.txt", false, b"x").0, 403);
    assert_eq!(fs::read_to_string(env.root.join("VERSO/data/settings.json")).unwrap(), "{}");
    assert_eq!(env.delete("VERSO/data/settings.json", false).0, 403);
    assert_eq!(env.delete("VERSO", false).0, 403);
    assert_eq!(env.post("rename", json!({ "path": "VERSO", "name": "mine" })).0, 403);
    assert_eq!(env.list("Verso/Data/Covers").0, 404, "a cache, hidden");
    assert_eq!(env.entry("", "VERSO")["writable"], false);
    assert!(env.root.join("VERSO/data/settings.json").exists());
}

/// Lists a folder the way FAT storage would if the names ending in `.` or
/// holding `~` were other spellings of listed entries (`verso.` of `verso`,
/// `EXTENS~1` of `extensions`). The test storage can't make them the same
/// folder, but they resolve without being listed, which is what the checks see.
fn fat_listing(dir: &Path) -> io::Result<Vec<String>> {
    Ok(list_names(dir)?.into_iter().filter(|n| !n.ends_with('.') && !n.contains('~')).collect())
}

#[test]
fn other_fat_spellings_of_protected_folders_are_refused() {
    let env = Env::with_listing(fat_listing);
    for d in ["verso./data", "EXTENS~1/verso", "extensions/verso.", "LOST_F~1", "documents/Old~1"] {
        fs::create_dir_all(env.root.join(d)).unwrap();
    }
    for f in ["verso./data/settings.json", "EXTENS~1/verso/menu.json", "voice."] {
        fs::write(env.root.join(f), "{}").unwrap();
    }
    // Existing paths through those spellings.
    assert_eq!(env.put("verso./data/settings.json", true, b"x").0, 400);
    assert_eq!(env.put("EXTENS~1/verso/menu.json", true, b"x").0, 400);
    assert_eq!(env.put("extensions/verso./verso.sh", false, b"x").0, 400);
    assert_eq!(env.list("verso./data").0, 400);
    assert_eq!(env.delete("EXTENS~1", true).0, 400);
    assert_eq!(env.post("rename", json!({ "path": "verso.", "name": "mine" })).0, 400);
    assert_eq!(env.post("mkdir", json!({ "path": "LOST_F~1", "name": "x" })).0, 400);
    assert_eq!(env.post("move", json!({ "path": "documents/notes.txt", "to": "EXTENS~1/verso" })).0, 400);
    // And where a link leads.
    std::os::unix::fs::symlink(env.root.join("verso./data"), env.root.join("documents/data")).unwrap();
    assert_eq!(env.list("documents/data").0, 400);
    assert_eq!(env.put("documents/data/settings.json", true, b"x").0, 400);
    // New names that would reach something already there.
    assert_eq!(env.put("voice.", true, b"x").0, 409);
    assert_eq!(env.post("mkdir", json!({ "path": "", "name": "verso." })).0, 409);
    assert_eq!(env.post("rename", json!({ "path": "fonts", "name": "EXTENS~1" })).0, 409);
    assert_eq!(env.post("move", json!({ "path": "documents/Old~1", "to": "" })).0, 200, "not there yet");
    for f in ["verso./data/settings.json", "EXTENS~1/verso/menu.json", "voice."] {
        assert_eq!(fs::read_to_string(env.root.join(f)).unwrap(), "{}", "{f}");
    }
    assert!(env.root.join("fonts").is_dir() && env.root.join("EXTENS~1").is_dir());
    // Only the names classes depend on are checked.
    assert_eq!(env.put("documents/notes~1.txt", false, b"x").0, 201);
    assert_eq!(env.put("verso/data/exports/Book.", false, b"x").0, 201);
}

#[test]
fn names_that_are_only_names_elsewhere_stay_user_content() {
    // On storage that lists `verso.`, it is a folder of its own.
    let env = Env::new();
    fs::create_dir_all(env.root.join("verso.")).unwrap();
    assert_eq!(env.put("verso./notes.txt", false, b"x").0, 201);
    assert_eq!(env.post("rename", json!({ "path": "verso.", "name": "mine" })).0, 200);
    assert!(env.root.join("mine/notes.txt").exists());
}

#[test]
fn verso_extensions_and_pinned_folders_cant_be_renamed_moved_or_deleted() {
    let env = Env::new();
    for path in
        ["verso", "extensions", "extensions/verso", "verso/data", "verso/data/dictionaries", "verso/data/exports"]
    {
        assert_eq!(env.post("rename", json!({ "path": path, "name": "renamed" })).0, 403, "rename {path}");
        assert_eq!(env.post("move", json!({ "path": path, "to": "documents" })).0, 403, "move {path}");
        assert_eq!(env.delete(path, true).0, 403, "delete {path}");
    }
    assert_eq!(env.delete("", true).0, 403, "the root");
    assert!(env.root.join("verso/data/dictionaries/Ozhegov").is_dir());
    assert!(env.root.join("extensions/verso/menu.json").exists());
    // What's inside the pinned folders and other extensions is the user's.
    assert_eq!(env.post("rename", json!({ "path": "verso/data/exports/Book.md", "name": "Old.md" })).0, 200);
    assert!(env.root.join("verso/data/exports/Old.md").exists());
    assert_eq!(env.delete("extensions/koreader", true).0, 200);
    assert!(!env.root.join("extensions/koreader").exists());
    // A user folder can't be renamed to a protected name.
    assert_eq!(env.post("rename", json!({ "path": "fonts", "name": "verso" })).0, 403);
    assert_eq!(env.post("rename", json!({ "path": "fonts", "name": "LIBKH" })).0, 403);
    assert_eq!(env.post("rename", json!({ "path": "fonts", "name": "Fonts" })).0, 200);
    assert!(env.root.join("Fonts").is_dir());
    assert_eq!(env.post("rename", json!({ "path": "documents/notes.txt", "name": "Russian" })).0, 409);
}

#[test]
fn moves_check_the_source_and_destination() {
    let env = Env::new();
    for to in ["verso", "verso/data", "verso/data/books", "extensions/verso", "libkh", "verso/cache"] {
        let (status, body) = env.post("move", json!({ "path": "documents/notes.txt", "to": to }));
        assert!(matches!(status, 403 | 404), "{to}: {status} {body}");
    }
    assert!(env.root.join("documents/notes.txt").exists());
    assert_eq!(env.post("move", json!({ "path": "documents", "to": "documents/Russian" })).0, 400, "into itself");
    assert_eq!(env.post("move", json!({ "path": "documents/notes.txt", "to": "documents" })).0, 400);
    let (status, moved) = env.post("move", json!({ "path": "documents/notes.txt", "to": "verso/data/exports" }));
    assert_eq!(status, 200, "{moved}");
    assert_eq!(moved["path"], "verso/data/exports/notes.txt");
    assert!(env.library_changed.load(Ordering::SeqCst));
    fs::write(env.root.join("documents/notes.txt"), "again").unwrap();
    assert_eq!(env.post("move", json!({ "path": "documents/notes.txt", "to": "verso/data/exports" })).0, 409);
    assert_eq!(fs::read_to_string(env.root.join("verso/data/exports/notes.txt")).unwrap(), "notes");
    // Out of a pinned folder is fine: it's user content.
    assert_eq!(
        env.post("move", json!({ "path": "verso/data/dictionaries/Ozhegov", "to": "koreader/data/dict" })).0,
        200
    );
    assert!(env.dictionaries_changed.load(Ordering::SeqCst));
    // Verso's files can't be moved out either.
    assert_eq!(env.post("move", json!({ "path": "verso/verso.sh", "to": "documents" })).0, 403);
    assert_eq!(env.post("move", json!({ "path": "verso/data/settings.json", "to": "documents" })).0, 403);
}

#[test]
fn symlinks_are_checked_where_they_lead() {
    let env = Env::new();
    let outside = env.dir.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("secret"), "secret").unwrap();
    std::os::unix::fs::symlink(&outside, env.root.join("documents/escape")).unwrap();
    std::os::unix::fs::symlink(outside.join("secret"), env.root.join("documents/secret")).unwrap();
    std::os::unix::fs::symlink(env.root.join("verso/data"), env.root.join("documents/data")).unwrap();
    std::os::unix::fs::symlink(env.root.join("verso/cache"), env.root.join("documents/cache")).unwrap();
    std::os::unix::fs::symlink(env.root.join("fonts"), env.root.join("documents/fonts")).unwrap();
    std::os::unix::fs::symlink(env.root.join("missing"), env.root.join("documents/broken")).unwrap();

    // Links out of the storage, broken links and links into caches aren't listed.
    assert_eq!(env.names("documents", false), ["data", "fonts", "Russian", "notes.txt"]);
    assert_eq!(env.list("documents/escape").0, 403);
    assert_eq!(env.json("GET", "/api/files/download?path=documents/secret", b"").0, 403);
    assert_eq!(env.put("documents/escape/new", false, b"x").0, 403);
    assert!(!outside.join("new").exists());
    assert_eq!(env.delete("documents/escape/secret", false).0, 403);
    assert_eq!(env.post("move", json!({ "path": "documents/notes.txt", "to": "documents/escape" })).0, 403);
    assert_eq!(env.list("documents/cache").0, 404);

    // A link into Verso's live data reads as live data: read-only.
    let settings = fs::read(env.root.join("verso/data/settings.json")).unwrap();
    let (status, _, body) = env.req("GET", "/api/files/download?path=documents/data/settings.json", b"");
    assert_eq!((status, &body), (200, &settings));
    assert_eq!(env.put("documents/data/settings.json", true, b"{\"x\":1}").0, 403);
    assert_eq!(env.put("documents/data/new.txt", false, b"x").0, 403);
    assert_eq!(env.entry("documents", "data")["writable"], false);
    assert_eq!(env.delete("documents/data", true).0, 403);
    assert_eq!(fs::read(env.root.join("verso/data/settings.json")).unwrap(), settings);

    // A link to a user folder works like the folder; deleting it removes only the link.
    assert_eq!(env.put("documents/fonts/a.ttf", false, b"x").0, 201);
    assert!(env.root.join("fonts/a.ttf").exists());
    assert_eq!(env.delete("documents/fonts", false).0, 200);
    assert!(!env.root.join("documents/fonts").exists());
    assert!(env.root.join("fonts/a.ttf").exists(), "the target stays");
}

#[test]
fn refuses_dot_dot_and_bad_names() {
    let env = Env::new();
    for path in ["../outside", "documents/../verso/data/settings.json", "documents/./notes.txt", "a\0b"] {
        assert_eq!(env.list(path).0, 400, "{path:?}");
        assert_eq!(env.put(&format!("{path}/x"), false, b"x").0, 400, "{path:?}");
    }
    assert_eq!(env.post("rename", json!({ "path": "documents/notes.txt", "name": "../notes.txt" })).0, 400);
    for name in
        ["a:b.txt", "a*b", "a?b", "a\"b", "a<b", "a>b", "a|b", "a\\b", "a\u{7}b", "..", ".", "  ", &"x".repeat(256)]
    {
        assert_eq!(env.post("mkdir", json!({ "path": "documents", "name": name })).0, 400, "{name:?}");
        assert_eq!(env.post("rename", json!({ "path": "documents/notes.txt", "name": name })).0, 400, "{name:?}");
    }
    // Dotfiles, .sdr folders and 255-byte names are fine here, unlike in the library.
    assert_eq!(env.post("mkdir", json!({ "path": "documents", "name": "Book.sdr" })).0, 201);
    assert_eq!(env.post("rename", json!({ "path": "documents/notes.txt", "name": ".notes" })).0, 200);
    let long = "я".repeat(127) + "x";
    assert_eq!(long.len(), 255);
    assert_eq!(env.put(&format!("documents/{long}"), false, b"x").0, 201);
    env.no_part_files();
}

#[test]
fn koreader_and_library_folders_ask_twice() {
    let env = Env::new();
    for path in ["koreader", "documents", "extensions/koreader"] {
        let (status, head, body) = env.req("DELETE", &format!("/api/files?path={path}"), b"");
        assert!(head.starts_with("HTTP/1.1 428 Precondition Required"), "{head}");
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!((status, body["twice"].clone()), (428, json!(true)), "{path}");
        assert!(env.root.join(path).exists());
    }
    let (status, info) = env.json("GET", "/api/files/info?path=documents", b"");
    assert_eq!(status, 200);
    assert_eq!(
        (info["files"].clone(), info["folders"].clone(), info["twice"].clone()),
        (json!(2), json!(1), json!(true))
    );
    assert_eq!(env.delete("koreader", true).0, 200);
    assert!(!env.root.join("koreader").exists());
    assert_eq!(env.delete("documents", true).0, 200);
    assert!(env.library_changed.load(Ordering::SeqCst));
    // Other folders and files ask once.
    fs::create_dir_all(env.root.join("screenshots/2026")).unwrap();
    assert_eq!(env.delete("screenshots", false).0, 200);
    assert_eq!(env.delete("verso/data/exports/Book.md", false).0, 200);
    let events = env.activity.events.lock().unwrap();
    assert!(events.contains(&Activity::Deleted { name: "koreader".into() }));
}

#[test]
fn downloads_are_always_attachments() {
    let env = Env::new();
    fs::write(env.root.join("documents/page.html"), "<script>alert(1)</script>").unwrap();
    fs::write(env.root.join("documents/image.svg"), "<svg onload=\"alert(1)\"/>").unwrap();
    for (name, body) in [("page.html", "<script>alert(1)</script>"), ("image.svg", "<svg onload=\"alert(1)\"/>")] {
        let (status, head, got) = env.req("GET", &format!("/api/files/download?path=documents/{name}"), b"");
        assert_eq!(status, 200);
        assert_eq!(got, body.as_bytes());
        assert!(head.contains("Content-Type: application/octet-stream"), "{head}");
        assert!(head.contains(&format!("Content-Disposition: attachment; filename=\"{name}\"")), "{head}");
        assert!(head.contains("Content-Security-Policy: sandbox"), "{head}");
        assert!(head.contains("X-Content-Type-Options: nosniff"), "{head}");
    }
    assert_eq!(env.json("GET", "/api/files/download?path=documents", b"").0, 400);
}

#[test]
fn writes_need_the_session_and_header() {
    let env = Env::new();
    let bare = "PUT /api/files?path=documents/x HTTP/1.1\r\nHost: verso.local\r\nX-Verso-Request: 1\r\n";
    assert_eq!(env.raw(bare, b"x").0, 401);
    let no_header = format!(
        "DELETE /api/files?path=documents/notes.txt HTTP/1.1\r\nHost: verso.local\r\nCookie: {}\r\n",
        env.cookie
    );
    assert_eq!(env.raw(&no_header, b"").0, 403);
    assert!(env.root.join("documents/notes.txt").exists());
}

#[test]
fn entries_say_who_needs_a_folder() {
    let env = Env::new();
    std::os::unix::fs::symlink(env.root.join("koreader"), env.root.join("documents/KOReader link")).unwrap();
    let needs = |folder: &str, name: &str| env.entry(folder, name)["needs"].clone();
    assert_eq!(needs("", "documents"), json!(["library", "documents"]));
    assert_eq!(needs("", "koreader"), json!(["koreader"]));
    assert_eq!(needs("extensions", "koreader"), json!(["koreader"]));
    assert_eq!(needs("", "fonts"), json!([]));
    assert_eq!(needs("documents", "Russian"), json!([]));
    assert_eq!(needs("documents", "notes.txt"), json!([]));
    // A link: renaming, moving or deleting it leaves KOReader alone.
    let link = env.entry("documents", "KOReader link");
    assert_eq!((link["needs"].clone(), link["twice"].clone()), (json!([]), json!(false)));
    assert_eq!(env.delete("documents/KOReader link", false).0, 200);
    assert!(env.root.join("koreader").is_dir());
    // A folder holding the library folder.
    env.store.save_library_folders(vec![env.root.join("documents/Russian")]).unwrap();
    assert_eq!(needs("", "documents"), json!(["library", "documents"]));
    assert_eq!(needs("documents", "Russian"), json!(["library"]));
}

#[test]
fn upload_checks_answer_before_any_body_is_sent() {
    let env = Env::new();
    let check = |path: &str, size: u64, overwrite: bool| {
        let flag = if overwrite { "&overwrite=1" } else { "" };
        env.json("GET", &format!("/api/files/check?path={}&size={size}{flag}", encode(path)), b"")
    };
    assert_eq!(check("documents/New.epub", 1000, false).0, 200);
    let (status, body) = check("documents/notes.txt", 1000, false);
    assert_eq!((status, body["exists"].clone()), (409, json!(true)));
    assert_eq!(check("documents/notes.txt", 1000, true).0, 200);
    let (status, body) = check("documents/Russian", 1000, true);
    assert_eq!((status, body["exists"].clone()), (409, Value::Null), "a folder can't be replaced");
    assert_eq!(check("verso/bin/tool", 1000, false).0, 403);
    assert_eq!(check("VERSO", 1000, false).0, 403);
    assert_eq!(check("documents/a:b.txt", 1000, false).0, 400);
    assert_eq!(check("documents/Big.epub", 20 * 1024 * 1024 * 1024, false).0, 507);
    assert_eq!(env.json("GET", "/api/files/check?path=documents/x.txt", b"").0, 400, "the size is required");
    // A check writes nothing.
    assert!(!env.root.join("documents/New.epub").exists());
    env.no_part_files();
}

#[test]
fn sweeping_a_folder_removes_only_stale_parts() {
    let env = Env::new();
    let old = std::time::SystemTime::now() - Duration::from_secs(3600);
    let stale = env.root.join("documents/.verso-upload.part");
    File::create(&stale).unwrap().set_modified(old).unwrap();
    let fresh = env.root.join("documents/.verso-upload.2.part");
    File::create(&fresh).unwrap();
    super::sweep_stale_parts(&env.root.join("documents"));
    assert!(!stale.exists());
    assert!(fresh.exists());
}

#[test]
fn listings_remove_parts_left_by_unfinished_uploads() {
    let env = Env::new();
    let old = std::time::SystemTime::now() - Duration::from_secs(3600);
    let part = |path: &str, modified: Option<std::time::SystemTime>| {
        let file = File::create(env.root.join(path)).unwrap();
        if let Some(time) = modified {
            file.set_modified(time).unwrap();
        }
    };
    part("documents/.verso-upload.part", Some(old));
    part("documents/.verso-upload.3.part", None);
    part("verso/bin/.verso-upload.part", Some(old));
    fs::write(env.root.join("documents/.verso-upload.txt"), "mine").unwrap();
    let names = env.names("documents", true);
    assert!(names.iter().all(|n| !n.ends_with(".part")), "{names:?}");
    assert!(names.contains(&".verso-upload.txt".to_owned()));
    // Only the stale one goes: the fresh one may be an upload still running.
    assert!(!env.root.join("documents/.verso-upload.part").exists());
    assert!(env.root.join("documents/.verso-upload.3.part").exists());
    // Read-only folders are never changed.
    env.names("verso/bin", true);
    assert!(env.root.join("verso/bin/.verso-upload.part").exists());
    assert!(env.root.join("documents/.verso-upload.txt").exists());
}

#[test]
fn upload_part_names_are_refused() {
    let env = Env::new();
    for name in [".verso-upload.part", ".verso-upload.2.part", ".VERSO-UPLOAD.PART"] {
        assert_eq!(env.put(&format!("documents/{name}"), false, b"x").0, 400, "{name}");
        assert_eq!(env.post("rename", json!({ "path": "documents/notes.txt", "name": name })).0, 400, "{name}");
        assert_eq!(env.post("mkdir", json!({ "path": "documents", "name": name })).0, 400, "{name}");
    }
    // Non-part dotfiles remain allowed.
    assert_eq!(env.put("documents/.verso-upload.txt", false, b"x").0, 201);
}

#[cfg(unix)]
#[test]
fn downloading_a_fifo_returns_400_without_blocking() {
    let env = Env::new();
    let fifo = env.root.join("documents/pipe.txt");
    let c_path = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
    let (status, _, _) = env.req("GET", "/api/files/download?path=documents/pipe.txt", b"");
    assert_eq!(status, 400);
}
