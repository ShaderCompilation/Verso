//! Wi-Fi transfer: a small web app served by the Kindle while its screen
//! (screens/transfer.rs) is open. See docs/web-app.md.
//!
//! A session turns Wi-Fi on if needed, opens port 80, starts the server and
//! a `verso.local` responder, then watches the address and keeps the Kindle
//! awake by restarting its idle countdown every minute, which leaves the
//! power button working. Stopping undoes all of it in reverse; leaving the
//! screen also turns Wi-Fi back off if the session turned it on. The
//! firewall rules are marked with a file in /tmp first, so if Verso dies (it
//! builds with panic = "abort") the launcher's restore() and the next start
//! undo them (`cleanup_after_crash`).

mod api;
mod auth;
mod books;
mod device;
mod dictionaries;
mod files;
mod http;
mod server;

use std::net::Ipv4Addr;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::library::worker::Worker as LibraryWorker;
use crate::platform::{Battery, Waker};
use crate::store::Store;
use api::{Api, Context};
use auth::Auth;
use books::{ActivitySink, Books};
use dictionaries::Dictionaries;
use files::Files;
use server::{Server, Timeouts};

pub use device::Device;
#[cfg(test)]
pub(crate) use device::fake;

/// How long the screen waits for Wi-Fi to connect before offering Try again
/// (the same bound as the SSH row's re-reads).
pub const NETWORK_WAIT: Duration = Duration::from_secs(90);
/// The server stops after this long without a request.
pub const IDLE: Duration = Duration::from_secs(10 * 60);
/// How often the Kindle's idle countdown is restarted while serving: well
/// under the shortest sleep timeout the Kindle offers.
pub const POSTPONE_SLEEP: Duration = Duration::from_secs(60);
/// The marker file, in the device's marker folder (/tmp on the Kindle). The
/// launcher (dist/kindle/verso/verso.sh) uses the same name.
const FIREWALL_MARKER: &str = "verso-transfer-firewall";
/// How often the address and name are read again: often while waiting for
/// Wi-Fi to connect, rarely once it has.
/// How long to wait before starting the name responder again after it failed.
const RESPONDER_RETRY: Duration = Duration::from_secs(30);
const POLL_WAITING: Duration = Duration::from_secs(1);
const POLL_READY: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Turning Wi-Fi on, opening the port.
    Starting,
    /// Serving, but Wi-Fi hasn't connected yet.
    Waiting,
    /// Still not connected after NETWORK_WAIT: Try again.
    NoNetwork,
    Ready,
    Stopped(Stop),
    Failed(Failure),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    Idle,
    Slept,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    Wifi,
    PortInUse,
    Server,
}

/// A line in the screen's activity list, newest first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Activity {
    Receiving { name: String },
    Received { name: String, folder: String },
    Deleted { name: String },
    Dictionary { name: String },
}

/// What the screen shows.
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub phase: Phase,
    pub address: Option<Ipv4Addr>,
    /// The name the responder won, without `.local`.
    pub hostname: Option<String>,
    /// The port in the addresses shown (80 on the Kindle).
    pub port: u16,
    pub code: String,
    pub activity: Vec<Activity>,
}

impl Snapshot {
    fn origin(&self, host: &str) -> String {
        if self.port == 80 { format!("http://{host}") } else { format!("http://{host}:{}", self.port) }
    }

    /// `http://verso.local`, once the name is known.
    pub fn name_url(&self) -> Option<String> {
        Some(self.origin(&format!("{}.local", self.hostname.as_ref()?)))
    }

    /// `http://192.168.1.42`, once there's an address.
    pub fn ip_url(&self) -> Option<String> {
        Some(self.origin(&self.address?.to_string()))
    }

    /// What the QR code holds: the IP, and the code in the fragment, which
    /// browsers don't send to the server.
    pub fn qr_url(&self) -> Option<String> {
        Some(format!("{}/#code={}", self.ip_url()?, self.code))
    }
}

pub struct Session {
    device: Arc<Device>,
    store: Store,
    library: LibraryWorker,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
    waker: Waker,
    /// The name to advertise (`Settings::hostname`).
    label: String,
    developer: bool,
    /// The last code shown: a new start never shows the same one.
    last_code: String,
    /// Set when any dictionary is installed or deleted during this session, so
    /// Done triggers a catalog rescan.
    dictionaries_changed: Arc<AtomicBool>,
    /// Set when the Files tab changed something in a library folder, so Done
    /// runs one incremental rescan.
    library_changed: Arc<AtomicBool>,
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
    battery: Mutex<Option<Battery>>,
}

struct State {
    phase: Phase,
    address: Option<Ipv4Addr>,
    hostname: Option<String>,
    /// `(id, activity)` newest first; `id` matches a `Receiving` row to its finish.
    activity: Vec<(u64, Activity)>,
    next_activity_id: u64,
    /// Active uploads in progress (used to confirm Done on the Kindle screen).
    receiving: usize,
    /// Counts changes the screen should draw.
    revision: u64,
    stop: bool,
    retry: bool,
    api: Option<Arc<Api>>,
    running: Running,
    /// Wi-Fi was off and the session turned it on; leaving puts it back.
    wifi_turned_on: bool,
}

struct SessionActivity {
    shared: Weak<Shared>,
    waker: Waker,
}

impl ActivitySink for SessionActivity {
    fn begin_receiving(&self, name: String) -> u64 {
        let Some(shared) = self.shared.upgrade() else { return 0 };
        let mut state = shared.lock();
        state.next_activity_id += 1;
        let id = state.next_activity_id;
        state.receiving += 1;
        state.activity.insert(0, (id, Activity::Receiving { name }));
        state.activity.truncate(50);
        state.revision += 1;
        drop(state);
        self.waker.wake();
        id
    }

    fn finish_receiving(&self, id: u64, received: Option<Activity>) {
        let Some(shared) = self.shared.upgrade() else { return };
        let mut state = shared.lock();
        state.receiving = state.receiving.saturating_sub(1);
        state.activity.retain(|(entry_id, _)| *entry_id != id);
        if let Some(activity) = received {
            state.activity.insert(0, (id, activity));
            state.activity.truncate(50);
        }
        state.revision += 1;
        drop(state);
        self.waker.wake();
    }

    fn push(&self, activity: Activity) {
        let Some(shared) = self.shared.upgrade() else { return };
        let mut state = shared.lock();
        state.next_activity_id += 1;
        let id = state.next_activity_id;
        state.activity.insert(0, (id, activity));
        state.activity.truncate(50);
        state.revision += 1;
        drop(state);
        self.waker.wake();
    }
}

/// What a running session holds, to give back when it stops.
#[derive(Default)]
struct Running {
    server: Option<Server>,
    firewall: bool,
    responder: Option<i32>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Session {
    /// Starts serving; the screen follows along through `snapshot`, and
    /// `waker` is called whenever it changes.
    pub fn start(
        device: Arc<Device>,
        store: Store,
        library: LibraryWorker,
        waker: Waker,
        label: String,
        developer: bool,
    ) -> Self {
        let initial_activity: Vec<(u64, Activity)> =
            device.activity.iter().cloned().enumerate().map(|(i, a)| (i as u64 + 1, a)).collect();
        let next_activity_id = initial_activity.len() as u64;
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                phase: Phase::Starting,
                address: None,
                hostname: None,
                activity: initial_activity,
                next_activity_id,
                receiving: 0,
                revision: 0,
                stop: false,
                retry: false,
                api: None,
                running: Running::default(),
                wifi_turned_on: false,
            }),
            wake: Condvar::new(),
            battery: Mutex::new(None),
        });
        let mut session = Self {
            device,
            store,
            library,
            shared,
            thread: None,
            waker,
            label,
            developer,
            last_code: String::new(),
            dictionaries_changed: Arc::new(AtomicBool::new(false)),
            library_changed: Arc::new(AtomicBool::new(false)),
        };
        session.spawn();
        session
    }

    fn spawn(&mut self) {
        let code = match &self.device.code {
            Some(code) => code.clone(),
            None => loop {
                let code = auth::random_code();
                if code != self.last_code {
                    break code;
                }
            },
        };
        self.last_code = code.clone();
        // Made before locking: it reads the names, which takes the lock.
        let api = Arc::new(Api::new(Auth::with_code(code), self.context()));
        {
            let mut state = self.shared.lock();
            state.stop = false;
            state.retry = false;
            state.phase = Phase::Starting;
            state.revision += 1;
            state.api = Some(api);
            if let Some(phase) = self.device.forced {
                // Simulator captures of states that are hard to reach.
                state.phase = phase;
                return;
            }
        }
        let (device, shared, waker, label) =
            (self.device.clone(), self.shared.clone(), self.waker.clone(), self.label.clone());
        let thread = std::thread::Builder::new().name("transfer".into()).spawn(move || {
            run(&device, &shared, &waker, &label);
        });
        match thread {
            Ok(thread) => self.thread = Some(thread),
            Err(e) => {
                log::error!("Wi-Fi transfer: starting its thread: {e}");
                self.shared.lock().phase = Phase::Failed(Failure::Server);
            }
        }
    }

    /// The API's view of the device; it holds the session weakly, so the
    /// session (which owns the API) can still be dropped.
    fn context(&self) -> Context {
        let (device, weak) = (self.device.clone(), Arc::downgrade(&self.shared));
        let names = {
            let (device, weak) = (device.clone(), weak.clone());
            move || {
                let mut names = device.local_names.clone();
                let state = weak.upgrade().map(|s| {
                    let state = s.lock();
                    (state.address, state.hostname.clone())
                });
                let (cached_ip, cached_name) = state.unwrap_or_default();
                for ip in [cached_ip, (device.address)()].into_iter().flatten() {
                    names.push(ip.to_string());
                }
                for name in [cached_name, (device.hostname)()].into_iter().flatten() {
                    names.push(format!("{name}.local"));
                }
                names
            }
        };
        let status = {
            let (device, weak, developer, store) = (device.clone(), weak.clone(), self.developer, self.store.clone());
            move || status(&device, &weak, developer, &store)
        };
        let changed = {
            let (weak, waker) = (weak.clone(), self.waker.clone());
            move || {
                if let Some(shared) = weak.upgrade() {
                    shared.lock().revision += 1;
                    waker.wake();
                }
            }
        };
        let activity: Arc<dyn ActivitySink> = Arc::new(SessionActivity { shared: weak, waker: self.waker.clone() });
        let books =
            Books::new(self.store.clone(), self.library.clone(), self.device.free_space.clone(), activity.clone());
        let dictionaries = Dictionaries::new(
            self.store.clone(),
            self.device.free_space.clone(),
            activity.clone(),
            self.dictionaries_changed.clone(),
        );
        let files = Files::new(
            self.device.files_root.clone(),
            self.store.clone(),
            self.device.free_space.clone(),
            activity,
            self.developer,
            self.library_changed.clone(),
            self.dictionaries_changed.clone(),
        );
        Context {
            port: self.device.listen.port(),
            names: Box::new(names),
            status: Box::new(status),
            changed: Box::new(changed),
            books: Some(books),
            dictionaries: Some(dictionaries),
            files: Some(files),
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        let state = self.shared.lock();
        let code = state.api.as_ref().map(|api| api.code().0).unwrap_or_default();
        Snapshot {
            phase: state.phase,
            address: state.address,
            hostname: state.hostname.clone(),
            port: self.device.display_port.unwrap_or(self.device.listen.port()),
            code,
            activity: state.activity.iter().map(|(_, a)| a.clone()).collect(),
        }
    }

    /// True while a file upload is currently being received.
    pub fn receiving(&self) -> bool {
        self.shared.lock().receiving > 0
    }

    /// True if any dictionary was installed or deleted during this session.
    pub fn dictionaries_changed(&self) -> bool {
        self.dictionaries_changed.load(Ordering::SeqCst)
    }

    /// True if the Files tab changed something in a library folder.
    pub fn library_changed(&self) -> bool {
        self.library_changed.load(Ordering::SeqCst)
    }

    /// Changes whenever the snapshot may have.
    pub fn revision(&self) -> u64 {
        let state = self.shared.lock();
        state.revision + state.api.as_ref().map_or(0, |api| api.code().1)
    }

    /// For `/api/status`; the screen passes on each reading.
    pub fn set_battery(&self, battery: Option<Battery>) {
        *self.shared.battery.lock().unwrap_or_else(|e| e.into_inner()) = battery;
    }

    /// Wait for Wi-Fi again, after NETWORK_WAIT ran out.
    pub fn retry(&self) {
        let mut state = self.shared.lock();
        state.retry = true;
        self.shared.wake.notify_all();
    }

    /// Starts again after a stop or a failure, with a new code.
    pub fn restart(&mut self) {
        self.halt();
        self.spawn();
    }

    /// When the idle stop is due: IDLE after the last request ended. None
    /// while a request is in progress or nothing is served.
    pub fn idle_deadline(&self) -> Option<Instant> {
        let state = self.shared.lock();
        let since = state.running.server.as_ref()?.idle_since()?;
        Some(since + self.device.idle)
    }

    /// Stops serving and gives everything back except Wi-Fi, which stays as
    /// it is until the screen closes (`close`).
    pub fn stop(&mut self, why: Stop) {
        self.halt();
        let mut state = self.shared.lock();
        // Judged after `halt`: the session's thread may fail while it ends.
        if !matches!(state.phase, Phase::Stopped(_) | Phase::Failed(_)) {
            log::info!("Wi-Fi transfer: stopped ({why:?})");
            state.phase = Phase::Stopped(why);
            state.revision += 1;
        }
    }

    /// Ends the session: everything given back, Wi-Fi as it was before.
    pub fn close(&mut self) {
        self.halt();
        let turned_on = std::mem::take(&mut self.shared.lock().wifi_turned_on);
        if turned_on {
            log::info!("Wi-Fi transfer: turning Wi-Fi back off");
            if let Err(e) = (self.device.set_wifi)(false) {
                log::warn!("Wi-Fi transfer: turning Wi-Fi off: {e:#}");
            }
        }
    }

    /// Stops the session's thread and gives back what it holds.
    fn halt(&mut self) {
        self.shared.lock().stop = true;
        self.shared.wake.notify_all();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let (running, api) = {
            let mut state = self.shared.lock();
            (std::mem::take(&mut state.running), state.api.clone())
        };
        running.release(&self.device);
        if let Some(api) = api {
            api.cleanup_batches();
        }
    }

    /// Adds a line to the activity list (newest first) and redraws.
    #[allow(dead_code)] // Dictionaries tab (Phase 3)
    pub fn push_activity(&self, activity: Activity) {
        let mut state = self.shared.lock();
        state.next_activity_id += 1;
        let id = state.next_activity_id;
        state.activity.insert(0, (id, activity));
        state.activity.truncate(50);
        state.revision += 1;
        drop(state);
        self.waker.wake();
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.close();
    }
}

impl Running {
    /// Gives everything back, newest first: the server, the port, the name.
    fn release(mut self, device: &Device) {
        if let Some(mut server) = self.server.take() {
            server.stop();
        }
        if std::mem::take(&mut self.firewall) {
            (device.firewall)(false);
            remove_marker(&device.markers, FIREWALL_MARKER);
        }
        if let Some(pid) = self.responder.take() {
            (device.stop_responder)(pid);
        }
    }
}

fn remove_marker(dir: &Path, name: &str) {
    let path = dir.join(name);
    if let Err(e) = std::fs::remove_file(&path)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        log::warn!("removing {}: {e}", path.display());
    }
}

fn write_marker(dir: &Path, name: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    std::fs::write(
        dir.join(name),
        "1
",
    )
}

/// `/api/status`. `restart`: Verso's live data was edited, so the Books
/// tab is read-only until Restart to apply.
fn status(device: &Device, shared: &Weak<Shared>, developer: bool, store: &Store) -> serde_json::Value {
    let (name, battery) = match shared.upgrade() {
        Some(shared) => {
            let state = shared.lock();
            let name = state.hostname.as_ref().map(|h| format!("{h}.local")).or(state.address.map(|a| a.to_string()));
            (name, *shared.battery.lock().unwrap_or_else(|e| e.into_inner()))
        }
        None => (None, None),
    };
    let free = (device.free_space)(&device.storage).ok();
    serde_json::json!({
        "name": name,
        "free": free,
        "battery": battery.map(|b| serde_json::json!({ "percent": b.percent, "charging": b.charging })),
        "developer": developer,
        "restart": store.live_data_edited(),
        "version": env!("CARGO_PKG_VERSION"),
    })
}

/// The session's thread: starts everything, then follows the address until
/// told to stop.
fn run(device: &Device, shared: &Shared, waker: &Waker, label: &str) {
    let stopping = || shared.lock().stop;
    let fail = |failure: Failure| {
        let running = {
            let mut state = shared.lock();
            state.phase = Phase::Failed(failure);
            state.revision += 1;
            std::mem::take(&mut state.running)
        };
        running.release(device);
        waker.wake();
    };

    match (device.wifi_enabled)() {
        Ok(true) => {}
        Ok(false) => {
            log::info!("Wi-Fi transfer: turning Wi-Fi on");
            shared.lock().wifi_turned_on = true;
            if let Err(e) = (device.set_wifi)(true) {
                log::error!("Wi-Fi transfer: turning Wi-Fi on: {e:#}");
                return fail(Failure::Wifi);
            }
        }
        Err(e) => {
            log::error!("Wi-Fi transfer: reading Wi-Fi state: {e:#}");
            return fail(Failure::Wifi);
        }
    }
    if stopping() {
        return;
    }

    // Keeping awake by restarting the idle countdown, not powerd's
    // preventScreenSaver, which also makes it ignore the power button.
    let mut awake = Awake::new(device);

    let Some(api) = shared.lock().api.clone() else { return };
    let idle_waker = waker.clone();
    match Server::start_with_idle_hook(
        device.listen,
        api,
        Timeouts::default(),
        Some(Box::new(move || idle_waker.wake())),
    ) {
        Ok(server) => shared.lock().running.server = Some(server),
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            log::error!("Wi-Fi transfer: port {} is in use", device.listen.port());
            return fail(Failure::PortInUse);
        }
        Err(e) => {
            log::error!("Wi-Fi transfer: starting the server on {}: {e}", device.listen);
            return fail(Failure::Server);
        }
    }
    if let Err(e) = write_marker(&device.markers, FIREWALL_MARKER) {
        log::warn!("Wi-Fi transfer: writing the firewall marker: {e}");
    }
    shared.lock().running.firewall = true;
    (device.firewall)(true);

    let mut waiting_since = Instant::now();
    // A responder that won't start is retried now and then, not every loop.
    let mut responder_retry_at = Instant::now();
    loop {
        awake.tick();
        let address = (device.address)();
        let dead = {
            let state = shared.lock();
            state.running.responder.filter(|&pid| !(device.responder_alive)(pid))
        };
        if let Some(pid) = dead {
            log::warn!("Wi-Fi transfer: mDNS responder (pid {pid}) exited; will restart");
            let mut state = shared.lock();
            if state.running.responder == Some(pid) {
                state.running.responder = None;
            }
        }
        let responder_needed = address.is_some() && {
            let state = shared.lock();
            state.running.responder.is_none() && !state.stop
        };
        // An SSH server's responder already advertises the name.
        if responder_needed && Instant::now() >= responder_retry_at && (device.hostname)().is_none() {
            responder_retry_at = Instant::now() + RESPONDER_RETRY;
            if let Some(pid) = (device.start_responder)(label) {
                shared.lock().running.responder = Some(pid);
            }
        }
        let hostname = (device.hostname)();
        let mut state = shared.lock();
        if state.stop {
            return;
        }
        let before = (state.phase, state.address, state.hostname.clone());
        if std::mem::take(&mut state.retry) && state.phase == Phase::NoNetwork {
            state.phase = Phase::Waiting;
            waiting_since = Instant::now();
        }
        state.phase = match (state.phase, address) {
            (_, Some(_)) => Phase::Ready,
            (Phase::Ready | Phase::Starting, None) => {
                waiting_since = Instant::now();
                Phase::Waiting
            }
            (Phase::Waiting, None) if waiting_since.elapsed() >= device.network_wait => Phase::NoNetwork,
            (phase, None) => phase,
        };
        state.address = address;
        state.hostname = hostname;
        if (state.phase, state.address, state.hostname.clone()) != before {
            if state.phase != before.0 {
                log::info!("Wi-Fi transfer: {:?} at {:?} as {:?}", state.phase, state.address, state.hostname);
            }
            state.revision += 1;
            waker.wake();
        }
        let poll = if state.phase == Phase::Ready && state.hostname.is_some() {
            POLL_READY
        } else {
            POLL_WAITING.min(device.network_wait.max(Duration::from_millis(20)))
        }
        .min(awake.due_in());
        drop(shared.wake.wait_timeout_while(state, poll, |s| !s.stop && !s.retry));
    }
}

/// Restarts the Kindle's idle countdown every `postpone_every`, from the
/// session's thread; it stops with the thread, so the Kindle's own sleep
/// timeout runs again once the server stops.
struct Awake<'a> {
    device: &'a Device,
    last: Instant,
    warned: bool,
}

impl<'a> Awake<'a> {
    fn new(device: &'a Device) -> Self {
        let mut awake = Self { device, last: Instant::now(), warned: false };
        awake.postpone();
        awake
    }

    fn tick(&mut self) {
        if self.last.elapsed() >= self.device.postpone_every {
            self.postpone();
        }
    }

    fn due_in(&self) -> Duration {
        self.device.postpone_every.saturating_sub(self.last.elapsed())
    }

    fn postpone(&mut self) {
        self.last = Instant::now();
        // Fails while the Kindle is asleep (the screen stops the session
        // then); once in the log is enough.
        if let Err(e) = (self.device.postpone_sleep)()
            && !std::mem::replace(&mut self.warned, true)
        {
            log::warn!("Wi-Fi transfer: keeping the Kindle awake: {e:#}");
        }
    }
}

/// At startup: a Wi-Fi transfer cut short by a crash may have left port 80
/// open (the launcher's restore() undoes this too; this covers a restart
/// inside the launcher's loop).
pub fn cleanup_after_crash(device: &Device) {
    if device.markers.join(FIREWALL_MARKER).exists() {
        log::info!("closing port {} left open by Wi-Fi transfer", device.listen.port());
        (device.firewall)(false);
        remove_marker(&device.markers, FIREWALL_MARKER);
    }
}

/// Where the marker goes, for tests.
#[cfg(test)]
fn marker_path(dir: &Path) -> std::path::PathBuf {
    dir.join(FIREWALL_MARKER)
}

#[cfg(test)]
mod tests;
