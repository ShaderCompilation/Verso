//! The update check end to end against a local HTTP server serving the
//! signed fixtures in `crates/verso-update/testdata` (release 0.3.0: a
//! required `app` and an optional `fonts` that needs app 0.3.0).
use super::fetch::tests::{Reply, Server};
use super::*;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use verso_update::InstalledComponent;

const MANIFEST: &[u8] = include_bytes!("../../../verso-update/testdata/latest.json");
const SIG: &str = include_str!("../../../verso-update/testdata/latest.json.minisig");
const SIG_OTHER: &str = include_str!("../../../verso-update/testdata/latest.json.other.minisig");
const SIG_COMMENT: &str = include_str!("../../../verso-update/testdata/latest.json.comment.minisig");
const TEST_KEY: &str = include_str!("../../../verso-update/testdata/test.pub");

const START: u64 = 1_800_000_000;
const HOUR: u64 = 3600;

fn v(s: &str) -> Version {
    s.parse().unwrap()
}

/// A test bench: a temporary storage root, a server, a settable clock and
/// Wi-Fi link.
struct Bench {
    root: tempfile::TempDir,
    server: Server,
    now: Arc<AtomicU64>,
    /// 0 off, 1 connecting, 2 connected.
    link: Arc<AtomicU8>,
    updates: Updates,
}

fn serve(manifest: &[u8], signature: &str) -> Server {
    Server::start(vec![
        ("/latest.json", Reply::Ok(manifest.to_vec())),
        ("/latest.json.minisig", Reply::Ok(signature.as_bytes().to_vec())),
    ])
}

fn bench(server: Server) -> Bench {
    let root = tempfile::tempdir().unwrap();
    let now = Arc::new(AtomicU64::new(START));
    let link = Arc::new(AtomicU8::new(2));
    let source = Source { url: server.url("/latest.json"), keys: vec![TEST_KEY.to_owned()] };
    let (clock, wifi) = (now.clone(), link.clone());
    let mut updates = Updates::with(
        Paths { root: root.path().to_owned() },
        source,
        v("0.1.0"),
        Box::new(move || {
            Ok(match wifi.load(Ordering::Relaxed) {
                0 => Link::Off,
                1 => Link::Connecting,
                _ => Link::Connected,
            })
        }),
        Box::new(move || clock.load(Ordering::Relaxed)),
    );
    updates.timeouts = Timeouts { connect: Duration::from_millis(500), total: Duration::from_millis(500) };
    Bench { root, server, now, link, updates }
}

impl Bench {
    fn install(&self, version: &str, dev: bool, components: &[(&str, &str)]) {
        let installed = Installed {
            format: verso_update::FORMAT,
            version: v(version),
            dev,
            components: components
                .iter()
                .map(|(id, hash)| InstalledComponent {
                    id: id.to_string(),
                    hash: hash.to_string(),
                    install: "verso".into(),
                    files: vec!["x".into()],
                })
                .collect(),
        };
        let path = self.root.path().join(INSTALLED_PATH);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, serde_json::to_vec(&installed).unwrap()).unwrap();
    }

    /// What `check_now` / `check_on_start` do, on this thread.
    fn check(&self, manual: bool) -> bool {
        assert!(self.updates.begin(manual), "a check is already running");
        self.updates.run()
    }

    fn saved(&self) -> State {
        State::parse(&std::fs::read(self.root.path().join(check::STATE_PATH)).unwrap()).unwrap()
    }

    fn note(&self) -> String {
        self.updates.status().note(|at| format!("at {}", at - START))
    }
}

impl Updates {
    /// Reads the files again on next use.
    fn forget(&self) {
        self.inner.lock().unwrap().installed = None;
    }
}

fn component_hash(id: &str) -> String {
    Manifest::parse(MANIFEST).unwrap().component(id).unwrap().hash.clone()
}

#[test]
fn finds_an_update_and_saves_it() {
    let b = bench(serve(MANIFEST, SIG));
    assert_eq!(b.note(), "Not checked yet.");
    assert!(b.check(true));
    assert_eq!(b.server.paths(), ["/latest.json", "/latest.json.minisig"]);
    let status = b.updates.status();
    assert!(!status.checking && status.problem.is_none());
    let offer = status.state.available.clone().unwrap();
    // No installed.json: the required app only; the optional fonts aren't installed.
    assert_eq!((offer.version.clone(), offer.components.clone(), offer.download), (v("0.3.0"), vec!["app".into()], 94));
    assert_eq!(offer.notes, "Faster page turns.");
    assert_eq!(b.saved(), status.state);
    assert_eq!(b.saved().checked, Some(START));
    assert_eq!(b.note(), "Verso 0.3.0 is available, a 0.1 MB download. Checked at 0.");
}

#[test]
fn automatic_checks_skip_dev_installs_and_run_once_a_day() {
    let b = bench(serve(MANIFEST, SIG));
    // No installed.json, then a development install: never automatic.
    assert!(!b.check(false));
    b.install("0.1.0", true, &[]);
    b.updates.forget();
    assert!(!b.check(false));
    assert!(b.server.paths().is_empty());
    assert!(b.root.path().join(check::STATE_PATH).metadata().is_err(), "nothing saved");
    // A release install checks, then not again for a day.
    b.install("0.1.0", false, &[]);
    b.updates.forget();
    assert!(b.check(false));
    assert_eq!(b.server.paths().len(), 2);
    b.now.store(START + 23 * HOUR, Ordering::Relaxed);
    assert!(!b.check(false));
    assert_eq!(b.server.paths().len(), 2);
    // Settings can always check.
    assert!(b.check(true));
    assert_eq!(b.server.paths().len(), 4);
    b.now.store(START + 47 * HOUR + 1, Ordering::Relaxed);
    assert!(b.check(false));
    assert_eq!(b.server.paths().len(), 6);
}

#[test]
fn wifi_off_is_quiet_unless_asked() {
    let b = bench(serve(MANIFEST, SIG));
    b.install("0.1.0", false, &[]);
    for (link, problem) in [(0, CheckError::WifiOff), (1, CheckError::NotConnected)] {
        b.link.store(link, Ordering::Relaxed);
        b.check(false);
        assert_eq!(b.updates.status().problem, None);
        assert_eq!(b.updates.status().state, State::default(), "an automatic check without Wi-Fi isn't an attempt");
        b.check(true);
        assert_eq!(b.updates.status().problem, Some(problem.clone()));
        assert_eq!(b.note(), problem.plain());
    }
    assert!(b.server.paths().is_empty(), "never touched the network");
    assert!(b.root.path().join(check::STATE_PATH).metadata().is_err());
    // Asking again clears the problem at once.
    b.link.store(2, Ordering::Relaxed);
    assert!(b.updates.begin(true));
    assert_eq!(b.updates.status().problem, None);
    assert_eq!(b.note(), "Checking…");
    b.updates.run();
    assert!(b.updates.status().state.available.is_some());
}

#[test]
fn bad_signatures_are_refused_and_keep_the_last_offer() {
    let good = bench(serve(MANIFEST, SIG));
    good.check(true);
    let offer = good.updates.status().state.available.clone();
    assert!(offer.is_some());
    let tampered = String::from_utf8(MANIFEST.to_vec()).unwrap().replace("Faster", "Fastest");
    for (manifest, signature) in [(MANIFEST, SIG_OTHER), (MANIFEST, SIG_COMMENT), (tampered.as_bytes(), SIG)] {
        let b = bench(serve(manifest, signature));
        // Start from the good check's saved state.
        let path = b.root.path().join(check::STATE_PATH);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::copy(good.root.path().join(check::STATE_PATH), &path).unwrap();
        b.now.store(START + HOUR, Ordering::Relaxed);
        b.check(true);
        let state = b.saved();
        assert_eq!(state.error.as_deref(), Some("The release isn’t signed by Verso, so it was ignored."));
        assert_eq!((state.checked, state.attempted), (Some(START), Some(START + HOUR)));
        assert_eq!(state.available, offer, "a refused manifest changes nothing");
        assert_eq!(b.note(), "Tried at 3600: The release isn’t signed by Verso, so it was ignored. Last checked at 0.");
    }
}

#[test]
fn downgrades_and_replays_are_not_offered() {
    // The same or an older release than installed: up to date.
    for installed in ["0.3.0", "0.4.0"] {
        let b = bench(serve(MANIFEST, SIG));
        b.install(installed, false, &[]);
        b.check(true);
        assert_eq!(b.saved().available, None);
        assert_eq!(b.note(), "Up to date. Checked at 0.");
    }
    // Newer than installed, but older than a release seen before.
    let b = bench(serve(MANIFEST, SIG));
    let path = b.root.path().join(check::STATE_PATH);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, br#"{"newest_seen":"0.4.0"}"#).unwrap();
    b.check(true);
    assert_eq!(b.saved().error.as_deref(), Some("The server offered an older release than before, so it was ignored."));
    assert_eq!(b.saved().available, None);
}

#[test]
fn minimum_app_version_and_skipped_versions() {
    // The app unchanged but the installed fonts changed: the new fonts
    // need app 0.3.0, and the app would stay 0.1.0.
    let b = bench(serve(MANIFEST, SIG));
    b.install("0.2.0", false, &[("app", &component_hash("app")), ("fonts", "old")]);
    b.check(true);
    assert_eq!(b.saved().error.as_deref(), Some("This release needs a newer Verso. Install it by hand."));
    // With the app changing too, both are offered together.
    let b = bench(serve(MANIFEST, SIG));
    b.install("0.2.0", false, &[("app", "old"), ("fonts", "old")]);
    let path = b.root.path().join(check::STATE_PATH);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, br#"{"skipped":["0.3.0","0.1.0"]}"#).unwrap();
    b.check(true);
    let state = b.saved();
    assert_eq!(state.available.unwrap().components, ["app", "fonts"]);
    assert_eq!(state.skipped, [v("0.3.0")], "skips at or below the installed release are dropped");
    assert_eq!(b.note(), "Verso 0.3.0 is available, a 0.1 MB download. You skipped it. Checked at 0.");
}

#[test]
fn network_failures_are_saved_in_plain_words() {
    let silent = Server::start(vec![("/latest.json", Reply::Silent)]);
    let b = bench(silent);
    let started = Instant::now();
    b.check(true);
    assert!(started.elapsed() < Duration::from_secs(3), "{:?}", started.elapsed());
    assert_eq!(b.saved().error.as_deref(), Some("The update server took too long to answer."));
    assert_eq!(b.saved().checked, None);
    assert_eq!(b.note(), "Tried at 0: The update server took too long to answer.");

    let b = bench(Server::start(vec![]));
    b.check(true);
    assert_eq!(b.saved().error.as_deref(), Some("No release was found on the update server."));

    let b = bench(Server::start(vec![("/latest.json", Reply::Ok(vec![b' '; MAX_MANIFEST_BYTES + 1]))]));
    b.check(true);
    assert_eq!(b.saved().error.as_deref(), Some("The update server sent an unexpectedly large file."));
    assert_eq!(b.server.paths(), ["/latest.json"], "no signature fetched after a failure");
}

#[test]
fn one_check_at_a_time() {
    let b = bench(serve(MANIFEST, SIG));
    assert!(b.updates.begin(false));
    assert!(!b.updates.begin(true), "joins the running check");
    assert_eq!(b.note(), "Checking…");
    // The running automatic check now counts as asked for: it runs even
    // though this is a development install.
    assert!(b.updates.run());
    assert!(b.updates.status().state.available.is_some());
    assert!(b.updates.begin(false));
}

#[test]
fn sources_and_sizes() {
    let s = Source { url: "http://localhost:8000/latest.json".into(), keys: vec![] };
    assert!(!s.https_only());
    assert_eq!(s.signature_url(), "http://localhost:8000/latest.json.minisig");
    assert!(Source { url: DEFAULT_URL.into(), keys: vec![] }.https_only());
    assert_eq!(megabytes(6_095_497), "6.1 MB");
    assert_eq!(megabytes(94), "0.1 MB");
    assert_eq!(app_version().to_string(), env!("CARGO_PKG_VERSION"));
}
