//! Dictionaries tab: archives in each format, loose-file batches, checks,
//! limits, clashes and delete, through the real server.

use super::*;
use crate::platform::Waker;
use crate::transfer::api::{Api, Context};
use crate::transfer::auth::Auth;
use crate::transfer::server::{Server, Timeouts};
use std::net::TcpStream;
use std::time::{Duration, Instant};

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

struct Env {
    dir: tempfile::TempDir,
    store: Store,
    api: Arc<Api>,
    server: Server,
    cookie: String,
    changed: Arc<AtomicBool>,
    activity: Arc<Recorded>,
    /// Free space reported by the first check of each test step, then later.
    free_first: Arc<AtomicU64>,
    free_later: Arc<AtomicU64>,
    calls: Arc<AtomicU64>,
}

const PLENTY: u64 = 10 * 1024 * 1024 * 1024;

impl Env {
    fn new() -> Self {
        Self::with_limits(Limits::default())
    }

    fn with_limits(limits: Limits) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path().join("data"));
        store.save_dictionary_folders(&[]).unwrap();
        let (free_first, free_later, calls) =
            (Arc::new(AtomicU64::new(PLENTY)), Arc::new(AtomicU64::new(PLENTY)), Arc::new(AtomicU64::new(0)));
        let free: FreeSpace = Arc::new({
            let (first, later, calls) = (free_first.clone(), free_later.clone(), calls.clone());
            move |_| {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                Ok(if n == 0 { first.load(Ordering::SeqCst) } else { later.load(Ordering::SeqCst) })
            }
        });
        let changed = Arc::new(AtomicBool::new(false));
        let activity = Arc::new(Recorded::default());
        let dictionaries = Dictionaries::with_limits(store.clone(), free, activity.clone(), changed.clone(), limits);
        let api = Arc::new(Api::new(
            Auth::with_code("4827".into()),
            Context {
                port: 80,
                names: Box::new(|| vec!["verso.local".into()]),
                status: Box::new(|| json!({})),
                changed: Box::new(|| {}),
                books: None,
                dictionaries: Some(dictionaries),
            },
        ));
        let server = Server::start("127.0.0.1:0".parse().unwrap(), api.clone(), Timeouts::default()).unwrap();
        let mut env =
            Self { dir, store, api, server, cookie: String::new(), changed, activity, free_first, free_later, calls };
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
        // The server may answer early (a refused archive) and close.
        let _ = stream.write_all(body);
        let mut raw = Vec::new();
        let _ = stream.read_to_end(&mut raw);
        let split = raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        let head = String::from_utf8(raw[..split].to_vec()).unwrap();
        (head[9..12].parse().unwrap(), head, raw[split + 4..].to_vec())
    }

    fn json(&self, method: &str, target: &str, body: &[u8]) -> (u16, Value) {
        let head = format!(
            "{method} {target} HTTP/1.1\r\nHost: verso.local\r\nCookie: {}\r\nX-Verso-Request: 1\r\n",
            self.cookie
        );
        let (status, _, body) = self.raw(&head, body);
        (
            status,
            serde_json::from_slice(&body)
                .unwrap_or_else(|e| panic!("{status} {:?}: {e}", String::from_utf8_lossy(&body))),
        )
    }

    fn upload(&self, name: &str, bytes: &[u8]) -> (u16, Value) {
        self.calls.store(0, Ordering::SeqCst);
        self.json("PUT", &format!("/api/dictionaries?name={}", encode(name)), bytes)
    }

    /// Opens a batch, sends each file, commits; the commit's answer.
    fn batch(&self, files: &[(String, Vec<u8>)]) -> (u16, Value) {
        let (status, batch) = self.json("POST", "/api/dictionaries/batch", b"");
        assert_eq!(status, 201);
        let id = batch["id"].as_str().unwrap().to_owned();
        for (name, bytes) in files {
            self.calls.store(0, Ordering::SeqCst);
            let (status, body) =
                self.json("PUT", &format!("/api/dictionaries/batch/{id}?name={}", encode(name)), bytes);
            assert_eq!(status, 200, "{name}: {body}");
        }
        self.calls.store(0, Ordering::SeqCst);
        self.json("POST", &format!("/api/dictionaries/batch/{id}/commit"), b"")
    }

    fn installed(&self) -> Vec<String> {
        let mut names: Vec<String> = match fs::read_dir(self.store.dictionary_dir()) {
            Ok(entries) => entries.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect(),
            Err(_) => Vec::new(),
        };
        names.sort();
        names
    }

    /// Nothing is left in staging and nothing escaped the data folder.
    fn assert_clean(&self) {
        let staging = self.store.root().join("dictionary-staging");
        let left: Vec<_> = fs::read_dir(&staging).map(|r| r.flatten().map(|e| e.path()).collect()).unwrap_or_default();
        assert!(left.is_empty(), "staging left behind: {left:?}");
        for escaped in [self.dir.path().join("evil.ifo"), self.store.root().join("evil.ifo"), PathBuf::from("/abs.ifo")]
        {
            assert!(!escaped.exists(), "{} escaped", escaped.display());
        }
    }
}

fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

#[derive(Clone, Copy, PartialEq)]
enum Syn {
    None,
    Plain,
    Dz,
    /// The `.ifo` counts synonyms, but no `.syn` is sent.
    Missing,
}

fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    e.write_all(bytes).unwrap();
    e.finish().unwrap()
}

/// One StarDict dictionary: `apple` → `fruit`, with the synonym `pomme`.
fn dict_files(bookname: &str, base: &str, syn: Syn) -> Vec<(String, Vec<u8>)> {
    let mut idx = b"apple\0".to_vec();
    idx.extend(0u32.to_be_bytes());
    idx.extend(5u32.to_be_bytes());
    let syn_count = if syn == Syn::None { "" } else { "synwordcount=1\n" };
    let ifo = format!(
        "StarDict's dict ifo file\nversion=2.4.2\nbookname={bookname}\nwordcount=1\n{syn_count}idxfilesize={}\nsametypesequence=m\n",
        idx.len()
    );
    let mut files = vec![
        (format!("{base}.ifo"), ifo.into_bytes()),
        (format!("{base}.idx"), idx),
        (format!("{base}.dict"), b"fruit".to_vec()),
    ];
    let mut syn_bytes = b"pomme\0".to_vec();
    syn_bytes.extend(0u32.to_be_bytes());
    match syn {
        Syn::Plain => files.push((format!("{base}.syn"), syn_bytes)),
        Syn::Dz => files.push((format!("{base}.syn.dz"), gzip(&syn_bytes))),
        Syn::None | Syn::Missing => {}
    }
    files
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Zip,
    Tgz,
    Tbz2,
}

impl Kind {
    const ALL: [Kind; 3] = [Kind::Zip, Kind::Tgz, Kind::Tbz2];

    fn name(self) -> &'static str {
        match self {
            Kind::Zip => "dict.zip",
            Kind::Tgz => "dict.tar.gz",
            Kind::Tbz2 => "dict.tar.bz2",
        }
    }
}

enum E<'a> {
    File(&'a str, &'a [u8]),
    Dir(&'a str),
    Link(&'a str),
}

fn archive(kind: Kind, entries: &[E]) -> Vec<u8> {
    if kind == Kind::Zip {
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        for e in entries {
            match e {
                E::File(name, data) => {
                    w.start_file(*name, options).unwrap();
                    w.write_all(data).unwrap();
                }
                E::Dir(name) => w.add_directory(*name, options).unwrap(),
                E::Link(name) => w.add_symlink(*name, "x.dict", options).unwrap(),
            }
        }
        return w.finish().unwrap().into_inner();
    }
    let mut b = tar::Builder::new(Vec::new());
    for e in entries {
        let (name, data, kind): (&str, &[u8], tar::EntryType) = match e {
            E::File(n, d) => (n, d, tar::EntryType::Regular),
            E::Dir(n) => (n, b"", tar::EntryType::Directory),
            E::Link(n) => (n, b"", tar::EntryType::Symlink),
        };
        // Names are written raw, so `..` and absolute paths reach the reader.
        let mut h = tar::Header::new_gnu();
        let gnu = h.as_gnu_mut().unwrap();
        gnu.name = [0; 100];
        gnu.name[..name.len()].copy_from_slice(name.as_bytes());
        h.set_entry_type(kind);
        h.set_size(data.len() as u64);
        h.set_mode(if kind == tar::EntryType::Directory { 0o755 } else { 0o644 });
        if kind == tar::EntryType::Symlink {
            h.set_link_name("x.dict").unwrap();
        }
        h.set_cksum();
        b.append(&h, data).unwrap();
    }
    let tar = b.into_inner().unwrap();
    match kind {
        Kind::Tgz => gzip(&tar),
        Kind::Tbz2 => {
            let mut e = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::default());
            e.write_all(&tar).unwrap();
            e.finish().unwrap()
        }
        Kind::Zip => unreachable!(),
    }
}

/// An archive holding `files` inside `folder/`.
fn archive_of(kind: Kind, folder: &str, files: &[(String, Vec<u8>)]) -> Vec<u8> {
    let names: Vec<String> = files.iter().map(|(n, _)| format!("{folder}/{n}")).collect();
    let dir = format!("{folder}/");
    let mut entries = vec![E::Dir(&dir)];
    entries.extend(files.iter().zip(&names).map(|((_, data), name)| E::File(name, data)));
    archive(kind, &entries)
}

fn lookup(path: &Path, cache: &Path, word: &str) -> Vec<String> {
    let mut backend = dictionary::Backend::open(path, cache, &|| true).unwrap();
    backend.lookup(word).unwrap().into_iter().map(|e| e.headword).collect()
}

#[test]
fn installs_every_archive_format_unpacking_syn_dz_with_clash_free_folders() {
    let env = Env::new();
    for (i, kind) in Kind::ALL.into_iter().enumerate() {
        let bytes = archive_of(kind, "oxford", &dict_files("Oxford: English", "ox", Syn::Dz));
        let (status, body) = env.upload(kind.name(), &bytes);
        assert_eq!(status, 201, "{kind:?}: {body}");
        let folder = if i == 0 { "Oxford_ English".to_owned() } else { format!("Oxford_ English ({})", i + 1) };
        assert_eq!(body["installed"][0]["folder"], folder.as_str());
        assert_eq!(body["installed"][0]["word_count"], 1);
        let dest = env.store.dictionary_dir().join(&folder);
        assert!(dest.join("ox.syn").is_file() && !dest.join("ox.syn.dz").exists(), "{kind:?}: .syn.dz unpacked");
        // What installs also opens for lookups, synonyms included.
        assert_eq!(lookup(&dest.join("ox.ifo"), &env.store.dictionary_cache(), "pomme"), ["apple"]);
    }
    env.assert_clean();
    assert!(env.changed.load(Ordering::SeqCst), "Done rescans the catalog");
    let names: Vec<_> = env
        .activity
        .events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|a| match a {
            Activity::Dictionary { name } => Some(name.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(names.len(), 3);

    let (status, list) = env.json("GET", "/api/dictionaries", b"");
    assert_eq!(status, 200);
    let list = list["dictionaries"].as_array().unwrap();
    assert_eq!(list.len(), 3);
    assert!(list.iter().all(|d| d["word_count"] == 1 && d["native"] == true && d["location"] == "Verso"));
}

#[test]
fn installs_several_dictionaries_from_one_archive_sharing_res() {
    let env = Env::new();
    let mut files = dict_files("First", "a", Syn::None);
    files.extend(dict_files("Second", "b", Syn::Plain));
    files.push(("res/pic.png".into(), b"png".to_vec()));
    let (status, body) = env.upload("two.zip", &archive_of(Kind::Zip, "pack", &files));
    assert_eq!(status, 201, "{body}");
    assert_eq!(env.installed(), ["First", "Second"]);
    for name in ["First", "Second"] {
        assert!(env.store.dictionary_dir().join(name).join("res/pic.png").is_file());
    }
    env.assert_clean();
}

#[test]
fn refuses_path_escapes_and_links_in_every_format() {
    let env = Env::new();
    let files = dict_files("Escape", "x", Syn::None);
    for kind in Kind::ALL {
        let cases: [(&str, Vec<E>); 3] = [
            ("parent", vec![E::File("../evil.ifo", &files[0].1), E::File("x.idx", &files[1].1)]),
            ("absolute", vec![E::File("/abs.ifo", &files[0].1)]),
            ("link", vec![E::File("x.ifo", &files[0].1), E::File("x.idx", &files[1].1), E::Link("x.dict")]),
        ];
        for (case, entries) in cases {
            let (status, body) = env.upload(kind.name(), &archive(kind, &entries));
            assert_eq!(status, 400, "{kind:?} {case}: {body}");
            let error = body["error"].as_str().unwrap();
            let expected = match case {
                "parent" => "path escape",
                "absolute" => "absolute path",
                _ => "links",
            };
            assert!(error.contains(expected), "{kind:?} {case}: {error}");
            assert!(env.installed().is_empty());
            env.assert_clean();
        }
    }
    assert!(!env.changed.load(Ordering::SeqCst));
}

#[test]
fn enforces_entry_and_unpacked_size_limits() {
    let env = Env::with_limits(Limits { max_bytes: 4096, max_entries: 5 });
    let many: Vec<(String, Vec<u8>)> = (0..6).map(|i| (format!("f{i}.txt"), b"x".to_vec())).collect();
    let big = vec![(String::from("big.dict"), vec![0u8; 8192])];
    for kind in Kind::ALL {
        let (status, body) = env.upload(kind.name(), &archive_of(kind, "d", &many));
        assert_eq!(status, 413, "{kind:?} entries: {body}");
        assert!(body["error"].as_str().unwrap().contains("too many entries"));
        // Compresses to a few dozen bytes, unpacks past the limit.
        let (status, body) = env.upload(kind.name(), &archive_of(kind, "d", &big));
        assert_eq!(status, 413, "{kind:?} size: {body}");
        assert!(body["error"].as_str().unwrap().contains("2 GB"));
        env.assert_clean();
    }
    // Loose files count against the same limits.
    let (status, batch) = env.json("POST", "/api/dictionaries/batch", b"");
    assert_eq!(status, 201);
    let id = batch["id"].as_str().unwrap();
    let (status, _) = env.json("PUT", &format!("/api/dictionaries/batch/{id}?name=x.dict"), &vec![0u8; 8192]);
    assert_eq!(status, 413);
    assert!(env.installed().is_empty());
}

#[test]
fn checks_free_space_before_and_while_unpacking() {
    let env = Env::new();
    let bytes = archive_of(Kind::Tgz, "d", &dict_files("Space", "s", Syn::None));
    env.free_first.store(FREE_SPACE_RESERVE, Ordering::SeqCst);
    let (status, _) = env.upload("d.tar.gz", &bytes);
    assert_eq!(status, 507, "the upload itself doesn't fit");
    // Room for the archive, but storage fills up while unpacking.
    env.free_first.store(PLENTY, Ordering::SeqCst);
    env.free_later.store(FREE_SPACE_RESERVE - 1, Ordering::SeqCst);
    for kind in Kind::ALL {
        let (status, body) = env.upload(kind.name(), &archive_of(kind, "d", &dict_files("Space", "s", Syn::None)));
        assert_eq!(status, 507, "{kind:?}: {body}");
        env.assert_clean();
    }
    assert!(env.installed().is_empty());
}

#[test]
fn refuses_incomplete_sets_naming_the_missing_part() {
    let env = Env::new();
    let full = dict_files("Part", "p", Syn::None);
    let without = |suffix: &str| -> Vec<(String, Vec<u8>)> {
        full.iter().filter(|(n, _)| !n.ends_with(suffix)).cloned().collect()
    };
    for (files, missing) in [
        (without(".idx"), ".idx"),
        (without(".dict"), ".dict"),
        (without(".ifo"), ".ifo"),
        (dict_files("Part", "p", Syn::Missing), "synonym"),
    ] {
        let (status, body) = env.batch(&files);
        assert_eq!(status, 400, "{missing}: {body}");
        assert!(body["error"].as_str().unwrap().contains(missing), "{missing}: {body}");
        // The same set in an archive is refused the same way.
        let (status, body) = env.upload("p.zip", &archive_of(Kind::Zip, "p", &files));
        assert_eq!(status, 400, "{missing}: {body}");
        assert!(body["error"].as_str().unwrap().contains(missing), "{missing}: {body}");
    }
    assert!(env.installed().is_empty());
    env.assert_clean();
}

#[test]
fn skips_macos_leftovers_in_a_zip_made_on_a_mac() {
    let env = Env::new();
    let files = dict_files("Mac", "m", Syn::Plain);
    let mut entries: Vec<E> = vec![E::Dir("Mac/"), E::Dir("__MACOSX/"), E::Dir("__MACOSX/Mac/")];
    let names: Vec<String> = files.iter().map(|(n, _)| format!("Mac/{n}")).collect();
    let junk: Vec<String> = files.iter().map(|(n, _)| format!("__MACOSX/Mac/._{n}")).collect();
    for (i, (_, data)) in files.iter().enumerate() {
        entries.push(E::File(&names[i], data));
        // AppleDouble files named like dictionary files but holding junk.
        entries.push(E::File(&junk[i], b"\x00\x05\x16\x07junk"));
    }
    entries.push(E::File("Mac/.DS_Store", b"junk"));
    entries.push(E::File("Mac/._m.ifo", b"junk"));
    let (status, body) = env.upload("Mac.zip", &archive(Kind::Zip, &entries));
    assert_eq!(status, 201, "{body}");
    let mut installed: Vec<_> = fs::read_dir(env.store.dictionary_dir().join("Mac"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    installed.sort();
    assert_eq!(installed, ["m.dict", "m.idx", "m.ifo", "m.syn"]);
    env.assert_clean();
}

#[test]
fn loose_files_install_through_a_batch_with_safe_names_and_clashes() {
    let env = Env::new();
    let files = dict_files("A/B: C?", "loose", Syn::Dz);
    let (status, body) = env.batch(&files);
    assert_eq!(status, 201, "{body}");
    let (status, body) = env.batch(&files);
    assert_eq!(status, 201, "{body}");
    assert_eq!(body["installed"][0]["folder"], "A_B_ C_ (2)");
    assert_eq!(env.installed(), ["A_B_ C_", "A_B_ C_ (2)"]);
    assert!(env.store.dictionary_dir().join("A_B_ C_/loose.syn").is_file());
    env.assert_clean();

    // Names other than StarDict files, and path tricks, are refused.
    let (_, batch) = env.json("POST", "/api/dictionaries/batch", b"");
    let id = batch["id"].as_str().unwrap();
    for name in ["notes.txt", "../x.ifo", ".hidden.ifo", "a/b.ifo"] {
        let (status, _) = env.json("PUT", &format!("/api/dictionaries/batch/{id}?name={}", encode(name)), b"x");
        assert_eq!(status, 400, "{name}");
    }
    assert_eq!(env.json("POST", "/api/dictionaries/batch/nope/commit", b"").0, 404);
}

#[test]
fn abandoned_batches_are_deleted_on_stop_and_at_the_next_start() {
    let env = Env::new();
    let (_, batch) = env.json("POST", "/api/dictionaries/batch", b"");
    let id = batch["id"].as_str().unwrap().to_owned();
    let (name, bytes) = &dict_files("Left", "l", Syn::None)[0];
    assert_eq!(env.json("PUT", &format!("/api/dictionaries/batch/{id}?name={name}"), bytes).0, 200);
    let staging = env.store.root().join("dictionary-staging");
    assert!(staging.join(format!("batch-{id}")).join(name).is_file());
    // The session's stop cleans up batches never committed.
    env.api.cleanup_batches();
    assert!(!staging.exists());
    assert_eq!(env.json("POST", &format!("/api/dictionaries/batch/{id}/commit"), b"").0, 404);

    // A crash leaves staging behind; the next start removes it.
    let leftover = env.store.root().join("dictionary-staging/batch-b7");
    fs::create_dir_all(&leftover).unwrap();
    fs::write(leftover.join("x.ifo"), b"x").unwrap();
    let free: FreeSpace = Arc::new(|_| Ok(PLENTY));
    let _fresh = Dictionaries::new(env.store.clone(), free, env.activity.clone(), Arc::new(AtomicBool::new(false)));
    assert!(!staging.exists());
}

#[test]
fn refuses_other_archive_formats_and_names() {
    let env = Env::new();
    for name in ["d.7z", "d.rar", "d.tar.xz"] {
        let (status, body) = env.upload(name, b"whatever");
        assert_eq!(status, 400);
        assert!(body["error"].as_str().unwrap().contains("Unpack it first"), "{name}: {body}");
    }
    assert_eq!(env.upload("d.txt", b"x").0, 400);
    assert_eq!(env.upload("d.zip", b"not a zip").0, 400);
    env.assert_clean();
}

#[test]
fn delete_closes_files_removes_cache_and_only_touches_verso_folder() {
    let env = Env::new();
    let (status, _) = env.batch(&dict_files("Gone", "g", Syn::Plain));
    assert_eq!(status, 201);
    // A lookup opens the dictionary in the shared service and builds its cache.
    let worker = crate::dictionary::worker::Worker::new(env.store.clone(), Waker::new(|| {}));
    worker.lookup("apple".into(), true);
    let started = Instant::now();
    loop {
        if worker.receive_wait(Duration::from_millis(200)).is_some_and(|r| r.complete) {
            break;
        }
        assert!(started.elapsed() < Duration::from_secs(10), "lookup timed out");
    }
    let indexes = || {
        fs::read_dir(env.store.dictionary_cache())
            .map(|r| r.flatten().filter(|e| e.path().extension().is_some_and(|x| x == "index")).count())
            .unwrap_or(0)
    };
    assert_eq!(indexes(), 1);

    // An external folder's dictionary is listed but can't be deleted here.
    let external = env.dir.path().join("koreader-dict");
    fs::create_dir_all(&external).unwrap();
    for (name, bytes) in dict_files("Outside", "o", Syn::None) {
        fs::write(external.join(name), bytes).unwrap();
    }
    env.store.save_dictionary_folders(std::slice::from_ref(&external)).unwrap();
    let (_, list) = env.json("GET", "/api/dictionaries", b"");
    let list = list["dictionaries"].as_array().unwrap().clone();
    let native = list.iter().find(|d| d["name"] == "Gone").unwrap();
    let outside = list.iter().find(|d| d["name"] == "Outside").unwrap();
    assert_eq!((native["native"].clone(), outside["native"].clone()), (json!(true), json!(false)));
    let id = |d: &Value| encode(d["id"].as_str().unwrap());
    assert_eq!(env.json("DELETE", &format!("/api/dictionaries?id={}", id(outside)), b"").0, 403);
    assert!(external.join("o.ifo").is_file());
    assert_eq!(env.json("DELETE", "/api/dictionaries?id=missing.ifo", b"").0, 404);

    env.changed.store(false, Ordering::SeqCst);
    let (status, body) = env.json("DELETE", &format!("/api/dictionaries?id={}", id(native)), b"");
    assert_eq!(status, 200, "{body}");
    assert!(env.installed().is_empty(), "files and the emptied folder are gone");
    assert_eq!(indexes(), 0, "its cache file went with it");
    assert!(env.changed.load(Ordering::SeqCst));
    assert!(env.activity.events.lock().unwrap().iter().any(|a| *a == Activity::Deleted { name: "Gone".into() }));
}

#[test]
fn archive_paths_are_cleaned_or_refused() {
    assert_eq!(validate_archive_path("./a/./b.ifo").ok(), Some(PathBuf::from("a/b.ifo")));
    for bad in ["../a", "a/../../b", "/etc/passwd", "C:/x", "a\\..\\b", "a\0b"] {
        assert!(validate_archive_path(bad).is_err(), "{bad}");
    }
    assert_eq!(safe_folder_name(" ..Ozhegov: Толковый*  словарь.. "), "Ozhegov_ Толковый_ словарь");
    assert_eq!(safe_folder_name("..."), "Dictionary");
    assert_eq!(safe_folder_name("notes.sdr"), "notes.sdr_");
}
