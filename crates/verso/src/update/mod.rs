//! In-app updates, the app's side. The update check: a worker thread
//! fetches the signed release manifest, verifies it, works out what would
//! change and saves the result in `state.json`; Settings shows it. The
//! download: another worker fetches the changed components' archives,
//! verifies and stages them for the launcher (`download.rs`).
//! See docs/updates.md.
pub mod download;
pub mod fetch;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use verso_update::check::{self, Offer, Outcome, Refusal, State};
use verso_update::launcher::{self, Previous, Reason, Rollback};
use verso_update::stage::AWAKE_MARKER;
use verso_update::{INSTALLED_PATH, Installed, MAX_MANIFEST_BYTES, MAX_SIGNATURE_BYTES, Manifest, Version};

use crate::platform::wifi::{self, Link};
use crate::platform::{Battery, Waker, awake};
use download::{DownloadError, Step};
use fetch::{Client, FetchError};

/// The newest release's manifest: a stable GitHub link that redirects to
/// the latest release's file. `VERSO_UPDATE_URL` replaces it.
pub const DEFAULT_URL: &str = "https://github.com/ShaderCompilation/Verso/releases/latest/download/latest.json";
/// A check is two small files; give up rather than keep a thread waiting.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const CHECK_TIMEOUT: Duration = Duration::from_secs(30);
/// An archive download gives up when no data arrives for this long; a
/// retry continues where it stopped.
const IDLE_TIMEOUT: Duration = Duration::from_secs(20);

/// This build's version.
pub fn app_version() -> Version {
    env!("CARGO_PKG_VERSION").parse().expect("the crate version is a release version")
}

/// Where release files live: the storage root that component install
/// folders are relative to.
#[derive(Clone, Debug)]
pub struct Paths {
    pub root: PathBuf,
}

impl Paths {
    /// `/mnt/us` on the Kindle. The simulator uses a pretend storage root
    /// in its data folder, `<data>/device`. `VERSO_STORAGE_ROOT` replaces
    /// either.
    pub fn for_device(data: &Path) -> Self {
        let root = std::env::var_os("VERSO_STORAGE_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| if cfg!(feature = "kindle") { PathBuf::from("/mnt/us") } else { data.join("device") });
        Self { root }
    }

    fn installed(&self) -> PathBuf {
        self.root.join(INSTALLED_PATH)
    }

    fn state(&self) -> PathBuf {
        self.root.join(check::STATE_PATH)
    }

    fn awake_marker(&self) -> PathBuf {
        self.root.join(AWAKE_MARKER)
    }
}

/// Where releases come from and which keys may sign them.
#[derive(Clone, Debug)]
pub struct Source {
    /// The manifest; its signature is the same address plus `.minisig`.
    pub url: String,
    pub keys: Vec<String>,
}

impl Source {
    /// The public releases, or `VERSO_UPDATE_URL` (a manifest address, or a
    /// folder ending in `/` that holds `latest.json`). The simulator also
    /// trusts the public key file named by `VERSO_UPDATE_KEY`, for testing
    /// against a local server with a throwaway key.
    pub fn configured() -> Self {
        let mut url = std::env::var("VERSO_UPDATE_URL").ok().filter(|u| !u.trim().is_empty());
        if let Some(u) = &mut url
            && u.ends_with('/')
        {
            u.push_str("latest.json");
        }
        let keys = verso_update::release_keys().into_iter().map(str::to_owned).chain(test_key()).collect();
        Self { url: url.unwrap_or_else(|| DEFAULT_URL.to_owned()), keys }
    }

    fn signature_url(&self) -> String {
        format!("{}.minisig", self.url)
    }

    /// Plain HTTP only for a local test server someone configured.
    fn https_only(&self) -> bool {
        !self.url.starts_with("http://")
    }
}

/// How long a check may take: per connection, and per request.
#[derive(Clone, Copy, Debug)]
pub struct Timeouts {
    pub connect: Duration,
    pub total: Duration,
    /// For archives: the longest wait for more data.
    pub idle: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self { connect: CONNECT_TIMEOUT, total: CHECK_TIMEOUT, idle: IDLE_TIMEOUT }
    }
}

/// The simulator's extra trusted key (`VERSO_UPDATE_KEY`). Never on the
/// Kindle: only the built-in keys may sign what it installs.
fn test_key() -> Option<String> {
    if !cfg!(feature = "desktop") {
        return None;
    }
    let path = std::env::var_os("VERSO_UPDATE_KEY")?;
    std::fs::read_to_string(&path).map_err(|e| log::error!("VERSO_UPDATE_KEY {}: {e}", Path::new(&path).display())).ok()
}

/// Why a check found nothing to act on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CheckError {
    WifiOff,
    /// Wi-Fi is on but hasn't joined a network.
    NotConnected,
    Fetch(FetchError),
    /// The manifest or its signature was refused.
    Release(verso_update::Error),
    Refused(Refusal),
}

impl CheckError {
    /// Said in Settings: what happened and, where it helps, what to do.
    pub fn plain(&self) -> String {
        use verso_update::Error as R;
        let text = match self {
            CheckError::WifiOff => "Wi-Fi is off. Turn it on to check.",
            CheckError::NotConnected => "Wi-Fi hasn’t connected to a network yet.",
            CheckError::Fetch(FetchError::Timeout) => "The update server took too long to answer.",
            CheckError::Fetch(FetchError::Unreachable(_)) => "Couldn’t reach the update server. Is the network online?",
            CheckError::Fetch(FetchError::Tls(_)) => "Couldn’t connect securely. Check the Kindle’s date and time.",
            CheckError::Fetch(FetchError::Status(404)) => "No release was found on the update server.",
            CheckError::Fetch(FetchError::Status(code)) => {
                return format!("The update server had a problem (error {code}).");
            }
            CheckError::Fetch(FetchError::TooLarge(_)) => "The update server sent an unexpectedly large file.",
            CheckError::Fetch(FetchError::Insecure(_)) => "The update address isn’t secure (HTTPS).",
            CheckError::Fetch(FetchError::Other(_)) => "Couldn’t download the release information.",
            CheckError::Release(R::NoKeys) => "This build has no release keys, so it can’t check.",
            CheckError::Release(R::UnsupportedFormat(_)) | CheckError::Refused(Refusal::NeedsNewerApp { .. }) => {
                "This release needs a newer Verso. Install it by hand."
            }
            CheckError::Release(R::Manifest(_)) => "The release information is damaged.",
            CheckError::Release(_) => "The release isn’t signed by Verso, so it was ignored.",
            CheckError::Refused(Refusal::Stale { .. }) => {
                "The server offered an older release than before, so it was ignored."
            }
        };
        text.to_owned()
    }

    /// Wi-Fi wasn't ready, so nothing was tried.
    fn before_network(&self) -> bool {
        matches!(self, CheckError::WifiOff | CheckError::NotConnected)
    }
}

impl From<FetchError> for CheckError {
    fn from(e: FetchError) -> Self {
        CheckError::Fetch(e)
    }
}

/// Downloads and verifies the manifest: the signature is checked over the
/// raw bytes before anything parses them.
pub fn fetch_release(source: &Source, timeouts: Timeouts) -> Result<Manifest, CheckError> {
    let client = Client::new(timeouts.connect, timeouts.total, source.https_only());
    let manifest = client.get(&source.url, MAX_MANIFEST_BYTES as u64)?;
    let signature = client.get(&source.signature_url(), MAX_SIGNATURE_BYTES as u64)?;
    let signature =
        String::from_utf8(signature).map_err(|_| CheckError::Release(verso_update::Error::BadSignatureFile))?;
    let keys: Vec<&str> = source.keys.iter().map(String::as_str).collect();
    verso_update::verify_manifest(&manifest, &signature, &keys).map_err(CheckError::Release)
}

/// What Settings shows.
#[derive(Clone, Debug)]
pub struct Status {
    /// A check is talking to the server.
    pub checking: bool,
    pub state: State,
    /// The installed release (the running app's version without
    /// `installed.json`).
    pub current: Version,
    /// A development install (or none recorded): no automatic checks.
    pub dev: bool,
    /// The last check asked for in Settings couldn't start (Wi-Fi). Not
    /// saved: it's about this moment.
    pub problem: Option<CheckError>,
    /// The download asked for in this session: where it is, or how it
    /// ended. None until one is started.
    pub download: Option<DownloadState>,
    /// Cancel was pressed and the download is stopping.
    pub cancelling: bool,
    /// Changes whenever anything above does.
    pub revision: u64,
}

/// A download's progress or result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DownloadState {
    Working(Step),
    /// Staged and ready for the launcher to install.
    Ready(Version),
    Failed(DownloadError),
}

impl Status {
    /// The update Home tells about: none once it was skipped, or "Later"
    /// was chosen since the last check.
    pub fn notice(&self) -> Option<&Offer> {
        self.state.notice(&self.current)
    }

    /// An update newer than the installed version, even if skipped or
    /// postponed: the dot on the Settings icon, and Settings' Update button.
    pub fn update(&self) -> Option<&Offer> {
        self.state.update(&self.current)
    }

    /// The line under the version in Settings. `when` says when a Unix
    /// time was ("today at 14:02").
    pub fn note(&self, when: impl Fn(u64) -> String) -> String {
        if self.checking {
            return "Checking…".into();
        }
        if let Some(problem) = &self.problem {
            return problem.plain();
        }
        let s = &self.state;
        if let Some(error) = &s.error {
            let tried = s.attempted.map(|at| format!("Tried {}: ", when(at))).unwrap_or_default();
            let checked = s.checked.map(|at| format!(" Last checked {}.", when(at))).unwrap_or_default();
            return format!("{tried}{error}{checked}");
        }
        let checked = s.checked.map(|at| format!(" Checked {}.", when(at))).unwrap_or_default();
        if let Some(offer) = s.available.as_ref().filter(|o| o.version > self.current) {
            let skipped = if s.is_skipped(&offer.version) { " You skipped it." } else { "" };
            return format!(
                "Verso {} is available, a {} download.{skipped}{checked}",
                offer.version,
                file_size(offer.download)
            );
        }
        if s.checked.is_some() { format!("Up to date.{checked}") } else { "Not checked yet.".into() }
    }
}

/// `6.1 MB`, never `0.0 MB`; `2.4 GB` from a gigabyte.
pub fn file_size(bytes: u64) -> String {
    if bytes >= 1_000_000_000 {
        return format!("{:.1} GB", bytes as f64 / 1e9);
    }
    format!("{:.1} MB", (bytes as f64 / 1e6).max(0.1))
}

struct Inner {
    /// None until `installed.json` and `state.json` were read.
    installed: Option<Option<Installed>>,
    state: State,
    /// A worker thread exists (a check or a download).
    running: bool,
    /// The worker is a download.
    downloading: bool,
    download: Option<DownloadState>,
    /// Settings asked for a check (the worker skips the automatic rules).
    manual: bool,
    checking: bool,
    problem: Option<CheckError>,
    revision: u64,
}

/// What the updater asks of the device; tests replace it.
pub(crate) struct Device {
    pub link: Box<dyn Fn() -> anyhow::Result<Link> + Send + Sync>,
    pub clock: Box<dyn Fn() -> u64 + Send + Sync>,
    /// Whether the device is kept from sleeping on its idle timeout…
    pub kept_awake: Box<dyn Fn() -> anyhow::Result<bool> + Send + Sync>,
    /// …and changing it.
    pub keep_awake: Box<dyn Fn(bool) -> anyhow::Result<()> + Send + Sync>,
    /// Bytes free on the filesystem holding a path.
    pub free_space: Box<download::FreeSpace>,
}

impl Device {
    fn real() -> Self {
        Self {
            link: Box::new(wifi::link),
            clock: Box::new(unix_now),
            kept_awake: Box::new(awake::kept_awake),
            keep_awake: Box::new(awake::keep_awake),
            free_space: Box::new(download::free_space),
        }
    }
}

/// The update check and download, shared by the startup check, Settings
/// and the update screen.
pub struct Updates {
    paths: Paths,
    source: Source,
    app: Version,
    timeouts: Timeouts,
    device: Device,
    /// Set by Cancel; the download stops at its next step.
    cancel: AtomicBool,
    inner: Mutex<Inner>,
}

impl Updates {
    pub fn new(paths: Paths, source: Source) -> Self {
        Self::with(paths, source, app_version(), Device::real())
    }

    fn with(paths: Paths, source: Source, app: Version, device: Device) -> Self {
        let inner = Inner {
            installed: None,
            state: State::default(),
            running: false,
            downloading: false,
            download: None,
            manual: false,
            checking: false,
            problem: None,
            revision: 0,
        };
        let cancel = AtomicBool::new(false);
        Self { paths, source, app, timeouts: Timeouts::default(), device, cancel, inner: Mutex::new(inner) }
    }

    /// Locks the shared state, reading the files on first use.
    fn lock(&self) -> MutexGuard<'_, Inner> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.installed.is_none() {
            inner.installed = Some(read_installed(&self.paths.installed()));
            inner.state = read_state(&self.paths.state());
        }
        inner
    }

    pub fn status(&self) -> Status {
        let inner = self.lock();
        let installed = inner.installed.as_ref().and_then(Option::as_ref);
        Status {
            checking: inner.checking,
            state: inner.state.clone(),
            current: installed.map_or_else(|| self.app.clone(), |i| i.version.clone()),
            dev: installed.is_none_or(|i| i.dev),
            problem: inner.problem.clone(),
            download: inner.download.clone(),
            cancelling: inner.downloading && self.cancel.load(Ordering::Relaxed),
            revision: inner.revision,
        }
    }

    /// Changes whenever `status()` would; cheap enough to poll per frame.
    pub fn revision(&self) -> u64 {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).revision
    }

    /// "Check for updates" in Settings: checks now, in the background.
    pub fn check_now(self: &Arc<Self>, waker: &Waker) {
        if self.begin(true) {
            self.spawn(waker);
        }
    }

    /// At startup, once the first frame is on screen: checks in the
    /// background if one is due (see `check::auto_check_due`), this isn't
    /// a development install and Wi-Fi is already connected. Quiet on
    /// failure; the error shows in Settings.
    pub fn check_on_start(self: &Arc<Self>, waker: &Waker) {
        if self.begin(false) {
            self.spawn(waker);
        }
    }

    /// Records the request; true when a worker should start.
    fn begin(&self, manual: bool) -> bool {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.downloading {
            // The download fetches the release again anyway.
            return false;
        }
        if manual {
            inner.manual = true;
            inner.problem = None;
            // Shown at once, before the worker has even started.
            inner.checking = true;
            inner.revision += 1;
        }
        !std::mem::replace(&mut inner.running, true)
    }

    fn spawn(self: &Arc<Self>, waker: &Waker) {
        let this = self.clone();
        let waker = waker.clone();
        let spawned = std::thread::Builder::new().name("update-check".into()).spawn(move || {
            this.run();
            waker.wake();
        });
        if let Err(e) = spawned {
            log::error!("starting the update check: {e}");
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            (inner.running, inner.manual, inner.checking) = (false, false, false);
            inner.revision += 1;
        }
    }

    /// The worker's job. Returns whether a check actually ran. Every exit
    /// bumps `revision` and wakes the loop, so a waiting update download can
    /// start even when an automatic check was not due.
    fn run(&self) -> bool {
        let now = (self.device.clock)();
        {
            let mut inner = self.lock();
            let dev = inner.installed.as_ref().and_then(Option::as_ref).is_none_or(|i| i.dev);
            if !inner.manual && (dev || !check::auto_check_due(&inner.state, now)) {
                inner.running = false;
                inner.revision += 1;
                log::debug!("update check: not due (dev install: {dev})");
                return false;
            }
        }
        let started = Instant::now();
        let result = self.wifi_ready();
        let mut manual = self.inner.lock().unwrap_or_else(|e| e.into_inner()).manual;
        let result = result.and_then(|()| {
            if !manual {
                let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                inner.checking = true;
                inner.revision += 1;
            }
            let manifest = fetch_release(&self.source, self.timeouts)?;
            let inner = self.lock();
            let installed = inner.installed.as_ref().and_then(Option::as_ref);
            let outcome = check::evaluate(&manifest, installed, &self.app, inner.state.newest_seen.as_ref())
                .map_err(CheckError::Refused)?;
            Ok((manifest.version, outcome))
        });
        let mut inner = self.lock();
        manual |= inner.manual;
        let current = inner.installed.as_ref().and_then(Option::as_ref).map_or(&self.app, |i| &i.version).clone();
        match result {
            Err(e) if e.before_network() => {
                log::info!("update check: skipped, {e:?}");
                // Only someone who asked hears about Wi-Fi.
                inner.problem = manual.then_some(e);
            }
            Err(e) => {
                log::warn!("update check failed after {:?}: {e:?}", started.elapsed());
                inner.state.record_failure(e.plain(), now);
                self.save(&inner.state);
            }
            Ok((version, outcome)) => {
                log::info!(
                    "update check: release {version}, installed {current}: {outcome:?} in {:?}",
                    started.elapsed()
                );
                inner.state.record_success(&version, outcome, &current, now);
                self.save(&inner.state);
            }
        }
        (inner.running, inner.manual, inner.checking) = (false, false, false);
        inner.revision += 1;
        true
    }

    /// Whether Wi-Fi is connected. When its state can't be read, the
    /// request itself will tell.
    fn wifi_ready(&self) -> Result<(), CheckError> {
        match (self.device.link)() {
            Ok(Link::Off) => Err(CheckError::WifiOff),
            Ok(Link::Connecting) => Err(CheckError::NotConnected),
            Ok(Link::Connected) => Ok(()),
            Err(e) => {
                log::warn!("updates: reading Wi-Fi state: {e:#}");
                Ok(())
            }
        }
    }

    fn save(&self, state: &State) {
        if let Err(e) = crate::store::atomic(&self.paths.state(), state) {
            log::error!("saving the update check: {e:#}");
        }
    }

    /// A check is running (scripted screenshots wait for it).
    pub fn checking(&self) -> bool {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.running && !inner.downloading
    }

    /// "Later" on the update sheet: hides Home's notice until the next
    /// successful check (the next day's, for automatic checks).
    pub fn postpone(&self) {
        let now = (self.device.clock)();
        let mut inner = self.lock();
        inner.state.postpone(now);
        self.save(&inner.state);
        inner.revision += 1;
    }

    /// "Skip this version": hides Home's notice until a newer version.
    pub fn skip(&self, version: &Version) {
        let mut inner = self.lock();
        inner.state.skip(version);
        self.save(&inner.state);
        inner.revision += 1;
    }

    /// "Update": downloads and stages release `version` in the background.
    /// `battery` is the platform's reading (None: unknown, allowed). False
    /// when a check or download is already running.
    pub fn download(self: &Arc<Self>, version: &Version, battery: Option<Battery>, waker: &Waker) -> bool {
        {
            let mut inner = self.lock();
            if inner.running {
                return false;
            }
            (inner.running, inner.downloading) = (true, true);
            inner.problem = None;
            inner.download = Some(DownloadState::Working(Step::Preparing));
            inner.revision += 1;
        }
        self.cancel.store(false, Ordering::Relaxed);
        let (this, waker, version) = (self.clone(), waker.clone(), version.clone());
        let spawned = std::thread::Builder::new().name("update-download".into()).spawn(move || {
            this.run_download(&version, battery, &waker);
            waker.wake();
        });
        if let Err(e) = spawned {
            log::error!("starting the update download: {e}");
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            (inner.running, inner.downloading) = (false, false);
            inner.download = Some(DownloadState::Failed(DownloadError::Storage(e.to_string())));
            inner.revision += 1;
        }
        true
    }

    /// Cancel on the progress screen. The download stops at its next step,
    /// at most [`IDLE_TIMEOUT`] later while waiting on the network.
    pub fn cancel_download(&self) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.downloading {
            self.cancel.store(true, Ordering::Relaxed);
            inner.revision += 1;
        }
    }

    /// The download worker's job.
    fn run_download(&self, version: &Version, battery: Option<Battery>, waker: &Waker) {
        let started = Instant::now();
        let mut last_percent = None;
        let mut report = |step: Step| {
            if self.cancel.load(Ordering::Relaxed) {
                return false;
            }
            // Redraw for a new step or another whole percent, not per chunk.
            let percent = match step {
                Step::Downloading { done, total } => Some(done * 100 / total.max(1)),
                _ => None,
            };
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            let same_kind = matches!(
                (&inner.download, &step),
                (Some(DownloadState::Working(Step::Downloading { .. })), Step::Downloading { .. })
            );
            if inner.download.as_ref() != Some(&DownloadState::Working(step.clone()))
                && (!same_kind || percent != last_percent)
            {
                last_percent = percent;
                inner.download = Some(DownloadState::Working(step));
                inner.revision += 1;
                drop(inner);
                waker.wake();
            }
            true
        };
        let result = self.download_release(version, battery, &mut report);
        match &result {
            Ok(staged) => log::info!("update {} staged in {:?}", staged.version, started.elapsed()),
            Err(e) => log::warn!("update download stopped after {:?}: {} ({e:?})", started.elapsed(), e.plain()),
        }
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.download = Some(match result {
            Ok(staged) => DownloadState::Ready(staged.version),
            Err(e) => DownloadState::Failed(e),
        });
        (inner.running, inner.downloading) = (false, false);
        self.cancel.store(false, Ordering::Relaxed);
        inner.revision += 1;
    }

    fn download_release(
        &self,
        version: &Version,
        battery: Option<Battery>,
        report: &mut dyn FnMut(Step) -> bool,
    ) -> Result<verso_update::stage::Staged, DownloadError> {
        if let Some(b) = battery
            && !b.charging
            && b.percent < download::MIN_BATTERY
        {
            return Err(DownloadError::LowBattery(b.percent));
        }
        self.wifi_ready().map_err(DownloadError::Check)?;
        if !report(Step::Preparing) {
            return Err(DownloadError::Cancelled);
        }
        let _awake = self.keep_awake();
        // The release again: the person agreed to `version`, and the
        // download needs the archives' hashes from a verified manifest.
        let manifest = fetch_release(&self.source, self.timeouts).map_err(DownloadError::Check)?;
        let (components, installed) = {
            let mut inner = self.lock();
            let installed = inner.installed.clone().flatten();
            let current = installed.as_ref().map_or(&self.app, |i| &i.version).clone();
            let outcome = check::evaluate(&manifest, installed.as_ref(), &self.app, inner.state.newest_seen.as_ref())
                .map_err(|r| DownloadError::Check(CheckError::Refused(r)))?;
            let now = (self.device.clock)();
            let result = match &outcome {
                Outcome::UpToDate => Err(DownloadError::UpToDate),
                Outcome::Available(offer) if offer.version != *version => {
                    Err(DownloadError::ReleaseChanged(offer.version.clone()))
                }
                Outcome::Available(offer) => Ok(offer.components.clone()),
            };
            // What the server says now is a check result like any other.
            inner.state.record_success(&manifest.version, outcome, &current, now);
            self.save(&inner.state);
            inner.revision += 1;
            (result?, installed)
        };
        let client = Client::for_downloads(self.timeouts.connect, self.timeouts.idle, self.source.https_only());
        let job = download::Job {
            root: &self.paths.root,
            manifest: &manifest,
            manifest_url: &self.source.url,
            components: &components,
            installed: installed.as_ref(),
            client: &client,
            free_space: &*self.device.free_space,
        };
        download::run(&job, report)
    }

    /// Keeps the device awake until the guard is dropped, unless it already
    /// was. A marker file says so meanwhile: if Verso dies first, the
    /// launcher (or the next start) turns the setting back off.
    fn keep_awake(&self) -> KeepAwake<'_> {
        let mut guard = KeepAwake { updates: self, restore: false };
        match (self.device.kept_awake)() {
            Ok(true) => {}
            Ok(false) => {
                let marker = self.paths.awake_marker();
                let marked = marker
                    .parent()
                    .map_or(Ok(()), std::fs::create_dir_all)
                    .and_then(|()| std::fs::write(&marker, "0\n"));
                if let Err(e) = marked {
                    log::warn!("not keeping the device awake: writing {}: {e}", marker.display());
                    return guard;
                }
                guard.restore = true;
                if let Err(e) = (self.device.keep_awake)(true) {
                    log::warn!("keeping the device awake: {e:#}");
                }
            }
            Err(e) => log::warn!("not keeping the device awake: {e:#}"),
        }
        guard
    }

    /// At startup: a download that didn't finish (Verso was killed or
    /// crashed) may have left the device kept awake. The launcher undoes
    /// this when Verso exits; this covers an older launcher.
    pub fn restore_awake_after_crash(&self) {
        let marker = self.paths.awake_marker();
        if !marker.exists() || self.inner.lock().unwrap_or_else(|e| e.into_inner()).downloading {
            return;
        }
        log::info!("turning off keep-awake left by an unfinished update download");
        match (self.device.keep_awake)(false) {
            Ok(()) => {
                let _ = std::fs::remove_file(&marker);
            }
            Err(e) => log::warn!("turning off keep-awake: {e:#}"),
        }
    }

    /// Why the launcher went back the last time, until the person has seen
    /// it (`dismiss_rollback`). One small file read.
    pub fn rollback(&self) -> Option<Rollback> {
        Rollback::read(&self.paths.root)
    }

    /// OK on the rollback sheet: it isn't shown again.
    pub fn dismiss_rollback(&self) {
        let path = self.paths.root.join(launcher::ROLLBACK_PATH);
        if let Err(e) = std::fs::remove_file(&path)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            log::error!("removing {}: {e}", path.display());
        }
    }

    /// The installed version and the one "Go back" would restore, while
    /// the previous files are kept and the launcher can switch them back.
    /// One small file read.
    pub fn previous(&self) -> Option<(Version, Version)> {
        if !can_install() {
            return None;
        }
        let Previous { current, previous } = launcher::previous(&self.paths.root)?;
        Some((current, previous?))
    }

    /// At startup: if the launcher went back from a version (it didn't
    /// start, or the person asked), that version counts as skipped, so it
    /// isn't offered again. `rollback.json` stays for the message.
    pub fn note_rollback(&self) {
        let Some(rollback) = Rollback::read(&self.paths.root) else { return };
        let Some(version) = rollback.from else { return };
        let mut inner = self.lock();
        if inner.state.is_skipped(&version) {
            return;
        }
        log::info!("the launcher went back from Verso {version} ({:?}); skipping that version", rollback.reason);
        inner.state.skip(&version);
        self.save(&inner.state);
        inner.revision += 1;
    }
}

/// Whether the launcher running Verso installs staged updates and goes
/// back (exit codes 85 and 86). Not under an older launcher, the simulator
/// or a hand start; `VERSO_LAUNCHER_PROTOCOL=1` pretends, for captures.
pub fn can_install() -> bool {
    launcher::protocol() >= 1
}

/// Why the launcher went back, in plain words for the rollback sheet.
/// None when the person asked.
pub fn rollback_reason(reason: &Reason) -> Option<String> {
    Some(match reason {
        Reason::Requested => return None,
        Reason::Exit(code) => format!("It stopped with error {code} while starting."),
        // An abort: a Rust panic, or a failed check in a library.
        Reason::Signal(6) => "It stopped on an internal error while starting.".into(),
        Reason::Signal(signal) => format!("It crashed while starting (signal {signal})."),
        Reason::Hang => "It didn’t open within two minutes.".into(),
        Reason::Interrupted => "It was interrupted twice while starting, for example by a restart.".into(),
        Reason::Install => "Its files couldn’t be put in place.".into(),
        Reason::Other(other) => format!("The launcher gave the reason “{other}”."),
    })
}

/// The log lines worth showing from a failed start: from the panic message
/// if there is one, else the last `max` lines, without their timestamps.
pub fn rollback_details(log: &[String], max: usize) -> Vec<String> {
    // Rust's own panic message ("thread 'main' (123) panicked at …") is the
    // whole of it; the log hook's copy just before it would say it twice.
    let start = log
        .iter()
        .position(|l| l.starts_with("thread '") && l.contains("panicked at"))
        .or_else(|| log.iter().position(|l| l.contains("panicked at")))
        .unwrap_or(log.len().saturating_sub(max));
    log[start..]
        .iter()
        // Advice for developers, which someone reading the sheet can't follow.
        .filter(|l| !l.starts_with("note: run with `RUST_BACKTRACE"))
        .take(max)
        .map(|line| without_time(line))
        .collect()
}

/// A log line without its date and time, as the Kindle writes them
/// **(measured, Phase 6)**: Verso's `[2026-11-19T00:38:01.962Z ERROR verso::x]
/// message` becomes `ERROR verso::x: message`; the launcher's `2026-11-19
/// 04:38:02 launcher: …` and FBInk's `2026/11/19 03:48:50.6053 WARN …` keep
/// what follows the time.
fn without_time(line: &str) -> String {
    let is_date = |s: &str| {
        let b = s.as_bytes();
        b.len() >= 10 && b[..4].iter().all(u8::is_ascii_digit) && (b[4] == b'-' || b[4] == b'/') && b[7] == b[4]
    };
    if let Some(rest) = line.strip_prefix('[')
        && let Some((header, message)) = rest.split_once("] ")
    {
        let mut parts = header.split_whitespace();
        if let (Some(time), Some(level)) = (parts.next(), parts.next())
            && is_date(time)
            && time.contains('T')
        {
            return match parts.next() {
                Some(target) => format!("{level} {target}: {message}"),
                None => format!("{level} {message}"),
            };
        }
    }
    let mut parts = line.splitn(3, ' ');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(date), Some(time), Some(rest)) if date.len() == 10 && is_date(date) && time.contains(':') => {
            rest.to_owned()
        }
        _ => line.to_owned(),
    }
}

/// How long Verso runs after its first frame before it counts as started.
/// A version that crashes right after drawing (say, in the library scan
/// that follows) is still rolled back.
pub const STARTED_DELAY: Duration = Duration::from_secs(5);

/// Tells the launcher, after [`STARTED_DELAY`], that this version works:
/// a version on trial that exits before then is rolled back. Call once
/// the first frame is on screen. Does nothing without a launcher that asks.
pub fn mark_started_soon() {
    if std::env::var_os(launcher::STARTED_ENV).is_none() {
        return;
    }
    let spawned = std::thread::Builder::new().name("started".into()).spawn(|| {
        std::thread::sleep(STARTED_DELAY);
        mark_started();
    });
    if let Err(e) = spawned {
        log::warn!("starting the started-marker thread: {e}");
        mark_started();
    }
}

/// Creates the started marker now: after the delay, or when the person
/// quits sooner (a clean exit after the first frame).
pub fn mark_started() {
    if let Some(path) = std::env::var_os(launcher::STARTED_ENV)
        && let Err(e) = std::fs::write(&path, b"")
    {
        log::warn!("creating the started marker {}: {e}", Path::new(&path).display());
    }
}

/// Restores "keep awake" when a download ends, however it ends (short of
/// the process dying: see [`Updates::restore_awake_after_crash`]).
struct KeepAwake<'a> {
    updates: &'a Updates,
    restore: bool,
}

impl Drop for KeepAwake<'_> {
    fn drop(&mut self) {
        if !self.restore {
            return;
        }
        match (self.updates.device.keep_awake)(false) {
            Ok(()) => {
                let _ = std::fs::remove_file(self.updates.paths.awake_marker());
            }
            // The marker stays, so the launcher tries again.
            Err(e) => log::warn!("restoring keep-awake: {e:#}"),
        }
    }
}

fn read_installed(path: &Path) -> Option<Installed> {
    match std::fs::read(path) {
        Ok(bytes) => Installed::parse(&bytes).map_err(|e| log::warn!("ignoring {}: {e}", path.display())).ok(),
        Err(e) => {
            if e.kind() != std::io::ErrorKind::NotFound {
                log::warn!("reading {}: {e}", path.display());
            }
            None
        }
    }
}

fn read_state(path: &Path) -> State {
    match std::fs::read(path) {
        Ok(bytes) => State::parse(&bytes).unwrap_or_else(|e| {
            log::warn!("ignoring {}: {e}", path.display());
            State::default()
        }),
        Err(_) => State::default(),
    }
}

pub fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests;
