//! The update check and download end to end against a local HTTP server
//! serving the signed fixtures in `crates/verso-update/testdata` (release
//! 0.3.0: a required `app` and an optional `fonts` that needs app 0.3.0).
use super::fetch::tests::{Reply, Server};
use super::*;
use std::sync::atomic::{AtomicU8, AtomicU64};
use verso_update::InstalledComponent;
use verso_update::stage::{STAGED_DIR, STAGED_FILE, Staged};

const MANIFEST: &[u8] = include_bytes!("../../../verso-update/testdata/latest.json");
const SIG: &str = include_str!("../../../verso-update/testdata/latest.json.minisig");
const SIG_OTHER: &str = include_str!("../../../verso-update/testdata/latest.json.other.minisig");
const SIG_COMMENT: &str = include_str!("../../../verso-update/testdata/latest.json.comment.minisig");
const TEST_KEY: &str = include_str!("../../../verso-update/testdata/test.pub");
const APP_ARCHIVE: &[u8] = include_bytes!("../../../verso-update/testdata/app-0.3.0.tar.gz");
const FONTS_ARCHIVE: &[u8] = include_bytes!("../../../verso-update/testdata/fonts-0.3.0.tar.gz");

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
    /// The device's keep-awake setting, and every change made to it.
    awake: Arc<Mutex<(bool, Vec<bool>)>>,
    free: Arc<AtomicU64>,
    updates: Arc<Updates>,
}

fn serve(manifest: &[u8], signature: &str) -> Server {
    Server::start(vec![
        ("/latest.json", Reply::Ok(manifest.to_vec())),
        ("/latest.json.minisig", Reply::Ok(signature.as_bytes().to_vec())),
    ])
}

/// The signed release with its archives.
fn serve_release() -> Server {
    Server::start(vec![
        ("/latest.json", Reply::Ok(MANIFEST.to_vec())),
        ("/latest.json.minisig", Reply::Ok(SIG.as_bytes().to_vec())),
        ("/app-0.3.0.tar.gz", Reply::Ranged(APP_ARCHIVE.to_vec())),
        ("/fonts-0.3.0.tar.gz", Reply::Ranged(FONTS_ARCHIVE.to_vec())),
    ])
}

fn bench(server: Server) -> Bench {
    let root = tempfile::tempdir().unwrap();
    let now = Arc::new(AtomicU64::new(START));
    let link = Arc::new(AtomicU8::new(2));
    let source = Source { url: server.url("/latest.json"), keys: vec![TEST_KEY.to_owned()] };
    let awake = Arc::new(Mutex::new((false, Vec::new())));
    let free = Arc::new(AtomicU64::new(1 << 40));
    let (clock, wifi, get, set, space) = (now.clone(), link.clone(), awake.clone(), awake.clone(), free.clone());
    let device = Device {
        link: Box::new(move || {
            Ok(match wifi.load(Ordering::Relaxed) {
                0 => Link::Off,
                1 => Link::Connecting,
                _ => Link::Connected,
            })
        }),
        clock: Box::new(move || clock.load(Ordering::Relaxed)),
        kept_awake: Box::new(move || Ok(get.lock().unwrap().0)),
        keep_awake: Box::new(move |on| {
            let mut state = set.lock().unwrap();
            state.0 = on;
            state.1.push(on);
            Ok(())
        }),
        free_space: Box::new(move |_| Ok(space.load(Ordering::Relaxed))),
    };
    let mut updates = Updates::with(Paths { root: root.path().to_owned() }, source, v("0.1.0"), device);
    let short = Duration::from_millis(500);
    updates.timeouts = Timeouts { connect: short, total: short, idle: short };
    Bench { root, server, now, link, awake, free, updates: Arc::new(updates) }
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
    assert_eq!(file_size(6_095_497), "6.1 MB");
    assert_eq!(file_size(94), "0.1 MB");
    assert_eq!(file_size(2_400_000_000), "2.4 GB");
    assert_eq!(app_version().to_string(), env!("CARGO_PKG_VERSION"));
}

impl Bench {
    /// What `download` does, on this thread.
    fn download(&self, version: &str, battery: Option<Battery>) -> DownloadState {
        {
            let mut inner = self.updates.lock();
            assert!(!inner.running, "a job is already running");
            (inner.running, inner.downloading) = (true, true);
        }
        let waker = Waker::new(|| {});
        self.updates.run_download(&v(version), battery, &waker);
        let status = self.updates.status();
        assert!(!status.cancelling);
        status.download.unwrap()
    }

    fn awake_changes(&self) -> Vec<bool> {
        self.awake.lock().unwrap().1.clone()
    }

    fn marker(&self) -> bool {
        self.root.path().join(AWAKE_MARKER).exists()
    }

    fn staged(&self) -> Option<Staged> {
        let path = self.root.path().join(STAGED_DIR).join(STAGED_FILE);
        Some(Staged::parse(&std::fs::read(path).ok()?).unwrap())
    }
}

const CHARGED: Option<Battery> = Some(Battery { percent: 80, charging: false });

#[test]
fn downloads_and_stages_the_offer_keeping_the_device_awake() {
    let b = bench(serve_release());
    b.install("0.2.0", false, &[("app", "old"), ("fonts", "old")]);
    b.check(true);
    assert_eq!(b.download("0.3.0", CHARGED), DownloadState::Ready(v("0.3.0")));
    let staged = b.staged().unwrap();
    assert_eq!(staged.components.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), ["app", "fonts"]);
    assert_eq!(staged.installed.version, v("0.3.0"));
    assert!(!staged.installed.dev);
    let app = b.root.path().join(STAGED_DIR).join("app/verso");
    assert_eq!(std::fs::read(app).unwrap().len(), 6);
    // Kept awake during the download only, and the marker is gone.
    assert_eq!(b.awake_changes(), [true, false]);
    assert!(!b.marker());
    // The release was fetched again, then the archives.
    assert_eq!(
        b.server.paths()[2..],
        ["/latest.json", "/latest.json.minisig", "/app-0.3.0.tar.gz", "/fonts-0.3.0.tar.gz"]
    );
    assert!(b.updates.begin(true), "jobs can run again");
}

#[test]
fn a_device_already_kept_awake_stays_so() {
    let b = bench(serve_release());
    b.awake.lock().unwrap().0 = true;
    assert_eq!(b.download("0.3.0", None), DownloadState::Ready(v("0.3.0")));
    assert!(b.awake_changes().is_empty());
    assert!(b.awake.lock().unwrap().0);
    assert!(!b.marker());
}

#[test]
fn failures_restore_keep_awake_and_stage_nothing() {
    // The archive is missing on the server.
    let server = Server::start(vec![
        ("/latest.json", Reply::Ok(MANIFEST.to_vec())),
        ("/latest.json.minisig", Reply::Ok(SIG.as_bytes().to_vec())),
    ]);
    let b = bench(server);
    let state = b.download("0.3.0", CHARGED);
    assert!(matches!(state, DownloadState::Failed(DownloadError::Network(FetchError::Status(404)))), "{state:?}");
    assert_eq!(b.awake_changes(), [true, false]);
    assert!(!b.marker());
    assert!(b.staged().is_none());
    assert!(!b.root.path().join(STAGED_DIR).exists());
    // Not enough space.
    let b = bench(serve_release());
    b.free.store(1000, Ordering::Relaxed);
    let state = b.download("0.3.0", CHARGED);
    assert!(matches!(state, DownloadState::Failed(DownloadError::NotEnoughSpace { .. })), "{state:?}");
    assert_eq!(b.awake_changes(), [true, false]);
    // A tampered manifest: refused before any archive.
    let tampered = String::from_utf8(MANIFEST.to_vec()).unwrap().replace("Faster", "Fastest");
    let b = bench(serve(tampered.as_bytes(), SIG));
    let state = b.download("0.3.0", CHARGED);
    assert!(matches!(state, DownloadState::Failed(DownloadError::Check(CheckError::Release(_)))), "{state:?}");
    assert_eq!(b.server.paths(), ["/latest.json", "/latest.json.minisig"]);
}

#[test]
fn battery_and_wifi_are_checked_first() {
    let b = bench(serve_release());
    let low = Some(Battery { percent: 19, charging: false });
    assert_eq!(b.download("0.3.0", low), DownloadState::Failed(DownloadError::LowBattery(19)));
    b.link.store(0, Ordering::Relaxed);
    let charging = Some(Battery { percent: 5, charging: true });
    assert_eq!(b.download("0.3.0", charging), DownloadState::Failed(DownloadError::Check(CheckError::WifiOff)));
    assert!(b.server.paths().is_empty());
    assert!(b.awake_changes().is_empty(), "never kept awake");
    b.link.store(2, Ordering::Relaxed);
    let enough = Some(Battery { percent: 20, charging: false });
    assert_eq!(b.download("0.3.0", enough), DownloadState::Ready(v("0.3.0")));
}

#[test]
fn the_release_must_still_be_the_one_agreed_to() {
    let b = bench(serve_release());
    assert_eq!(b.download("0.2.5", CHARGED), DownloadState::Failed(DownloadError::ReleaseChanged(v("0.3.0"))));
    // What the server said is saved like a check's result.
    assert_eq!(b.saved().available.unwrap().version, v("0.3.0"));
    b.install("0.3.0", false, &[]);
    b.updates.forget();
    assert_eq!(b.download("0.3.0", CHARGED), DownloadState::Failed(DownloadError::UpToDate));
    assert_eq!(b.saved().available, None);
    assert!(b.staged().is_none());
}

#[test]
fn cancel_stops_the_download_and_restores_keep_awake() {
    let b = bench(serve_release());
    {
        let mut inner = b.updates.lock();
        (inner.running, inner.downloading) = (true, true);
    }
    b.updates.cancel_download();
    assert!(b.updates.status().cancelling);
    b.updates.run_download(&v("0.3.0"), CHARGED, &Waker::new(|| {}));
    let status = b.updates.status();
    assert_eq!(status.download, Some(DownloadState::Failed(DownloadError::Cancelled)));
    assert!(!status.cancelling);
    assert!(b.server.paths().is_empty());
    // Cancel without a download does nothing.
    b.updates.cancel_download();
    assert_eq!(b.download("0.3.0", CHARGED), DownloadState::Ready(v("0.3.0")));
}

#[test]
fn checks_and_downloads_run_one_at_a_time() {
    let b = bench(serve_release());
    let waker = Waker::new(|| {});
    assert!(b.updates.begin(false));
    assert!(!b.updates.download(&v("0.3.0"), CHARGED, &waker), "a check is running");
    b.updates.run();
    {
        let mut inner = b.updates.lock();
        (inner.running, inner.downloading) = (true, true);
    }
    assert!(!b.updates.begin(true), "a download is running");
    assert!(!b.updates.status().checking, "and no check is shown");
    assert!(!b.updates.download(&v("0.3.0"), CHARGED, &waker));
}

#[test]
fn the_download_thread_reports_back() {
    let b = bench(serve_release());
    let woken = Arc::new(AtomicU64::new(0));
    let count = woken.clone();
    let waker = Waker::new(move || {
        count.fetch_add(1, Ordering::Relaxed);
    });
    assert!(b.updates.download(&v("0.3.0"), CHARGED, &waker));
    let deadline = Instant::now() + Duration::from_secs(5);
    while b.updates.status().download != Some(DownloadState::Ready(v("0.3.0"))) {
        assert!(Instant::now() < deadline, "{:?}", b.updates.status().download);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(woken.load(Ordering::Relaxed) >= 2, "woken for progress and the end");
}

#[test]
fn keep_awake_left_by_a_crash_is_restored_at_start() {
    let b = bench(serve_release());
    b.updates.restore_awake_after_crash();
    assert!(b.awake_changes().is_empty(), "no marker, nothing to do");
    let marker = b.root.path().join(AWAKE_MARKER);
    std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
    std::fs::write(&marker, "0\n").unwrap();
    b.updates.restore_awake_after_crash();
    assert_eq!(b.awake_changes(), [false]);
    assert!(!marker.exists());
}

#[test]
fn rollback_reasons_and_details_read_plainly() {
    assert_eq!(rollback_reason(&Reason::Requested), None);
    assert!(rollback_reason(&Reason::Signal(6)).unwrap().contains("internal error"));
    assert!(rollback_reason(&Reason::Signal(11)).unwrap().contains("signal 11"));
    assert!(rollback_reason(&Reason::Hang).unwrap().contains("two minutes"));
    let log: Vec<String> = [
        "2026/11/19 03:48:50.6053 INFO starting",
        "2026/11/19 03:48:50.7001 INFO library ready",
        "thread 'main' panicked at src/x.rs:1:1:",
        "index out of bounds",
    ]
    .map(String::from)
    .into();
    assert_eq!(rollback_details(&log, 8), ["thread 'main' panicked at src/x.rs:1:1:", "index out of bounds"]);
    assert_eq!(rollback_details(&log[..2], 1), ["INFO library ready"]);
    // As the Kindle wrote them for a build that panicked (Phase 6): the log
    // hook's copy of the panic is skipped, and so is the backtrace advice.
    let kindle: Vec<String> = [
        "2026-11-19 04:38:01 launcher: starting Verso",
        "[2026-11-19T00:38:01.961Z INFO  verso] screen: PaperWhite 6 1272x1696 @ 300 dpi, Portrait",
        "[2026-11-19T00:38:01.962Z ERROR verso::logging] panic: panicked at crates/verso/src/main.rs:178:5:",
        "    Phase 6 test build: this version panics on start on purpose",
        "",
        "thread 'main' (24597) panicked at crates/verso/src/main.rs:178:5:",
        "Phase 6 test build: this version panics on start on purpose",
        "note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace",
        "2026-11-19 04:38:02 launcher: Verso exited with code 134",
        "2026-11-19 04:38:02 launcher: the new version didn't start",
    ]
    .map(String::from)
    .into();
    assert_eq!(
        rollback_details(&kindle, 8),
        [
            "thread 'main' (24597) panicked at crates/verso/src/main.rs:178:5:",
            "Phase 6 test build: this version panics on start on purpose",
            "launcher: Verso exited with code 134",
            "launcher: the new version didn't start",
        ]
    );
    // A crash: the last lines, without their times.
    assert_eq!(
        rollback_details(&kindle[..2], 8),
        ["launcher: starting Verso", "INFO verso: screen: PaperWhite 6 1272x1696 @ 300 dpi, Portrait"]
    );
}

#[test]
fn download_failures_have_a_heading_and_plain_words() {
    let space = DownloadError::NotEnoughSpace { needed: 2_400_000_000, free: 0 };
    assert_eq!(space.title(), "Not enough space");
    assert!(space.plain().contains("2.4 GB"), "{}", space.plain());
    assert_eq!(DownloadError::Check(CheckError::WifiOff).title(), "Wi-Fi is off");
    assert!(DownloadError::Check(CheckError::WifiOff).plain().contains("Settings → Device"));
}
