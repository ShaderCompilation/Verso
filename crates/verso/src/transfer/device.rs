//! What Wi-Fi transfer needs from the device: Wi-Fi, staying awake, the
//! firewall, the address and the `verso.local` responder. The Kindle uses the
//! platform's; the simulator and tests fake them.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;

use super::{Activity, IDLE, NETWORK_WAIT, POSTPONE_SLEEP, Phase};

type Hook<T> = Box<dyn Fn() -> T + Send + Sync>;
type Set<T> = Box<dyn Fn(T) -> Result<()> + Send + Sync>;
type Do<T> = Box<dyn Fn(T) + Send + Sync>;
type StartResponder = Box<dyn Fn(&str) -> Option<i32> + Send + Sync>;
pub type FreeSpace = Arc<dyn Fn(&Path) -> std::io::Result<u64> + Send + Sync>;

pub struct Device {
    pub wifi_enabled: Hook<Result<bool>>,
    pub set_wifi: Set<bool>,
    /// Restarts the idle countdown before the Kindle would sleep; called
    /// every `postpone_every` while serving (platform::awake).
    pub postpone_sleep: Hook<Result<()>>,
    pub postpone_every: Duration,
    /// The address other devices reach us at (wlan0's on the Kindle).
    pub address: Hook<Option<Ipv4Addr>>,
    /// Opens (true) or closes the server's port in the firewall.
    pub firewall: Do<bool>,
    /// Starts a responder advertising `<label>.local` for as long as Verso
    /// runs, unless one runs already; the PID of the one it started.
    pub start_responder: StartResponder,
    /// Whether the responder `start_responder` returned is still running.
    pub responder_alive: Box<dyn Fn(i32) -> bool + Send + Sync>,
    pub stop_responder: Do<i32>,
    /// The name a running responder won, without `.local`.
    pub hostname: Hook<Option<String>>,
    pub listen: SocketAddr,
    /// Host names always accepted (the simulator's localhost).
    pub local_names: Vec<String>,
    /// Where the crash markers go.
    pub markers: PathBuf,
    /// Free space is reported for this folder.
    pub storage: PathBuf,
    /// The Files tab's root: `/mnt/us` on the Kindle, a test folder in the
    /// simulator (never `/`).
    pub files_root: PathBuf,
    /// Reads available bytes on the filesystem containing the given folder.
    pub free_space: FreeSpace,
    pub network_wait: Duration,
    pub idle: Duration,
    /// Simulator captures: the port shown instead of the real one, a fixed
    /// code, and a phase shown without starting anything.
    pub display_port: Option<u16>,
    pub code: Option<String>,
    pub forced: Option<Phase>,
    /// The activity list to start with.
    pub activity: Vec<Activity>,
    /// Simulator captures: Verso's data counts as edited from the start, so
    /// Done shows the Restart sheet.
    pub edited: bool,
}

impl Device {
    /// This device: the Kindle, or the desktop simulator.
    pub fn current(storage: PathBuf) -> Device {
        #[cfg(feature = "kindle")]
        {
            let _ = storage;
            kindle()
        }
        #[cfg(not(feature = "kindle"))]
        simulator(storage, &std::env::var("VERSO_SIM_TRANSFER").unwrap_or_default())
    }
}

#[cfg(feature = "kindle")]
fn kindle() -> Device {
    use crate::platform::{awake, firewall, mdns, wifi};
    const PORT: u16 = 80;
    Device {
        wifi_enabled: Box::new(wifi::enabled),
        set_wifi: Box::new(wifi::set_enabled),
        postpone_sleep: Box::new(awake::postpone_sleep),
        postpone_every: POSTPONE_SLEEP,
        address: Box::new(wifi::address),
        firewall: Box::new(|open| firewall::set(PORT, open)),
        start_responder: Box::new(mdns::start_for_verso),
        responder_alive: Box::new(mdns::running),
        stop_responder: Box::new(mdns::stop),
        hostname: Box::new(mdns::hostname),
        listen: SocketAddr::from((Ipv4Addr::UNSPECIFIED, PORT)),
        local_names: Vec::new(),
        markers: PathBuf::from("/tmp"),
        storage: PathBuf::from("/mnt/us"),
        files_root: PathBuf::from("/mnt/us"),
        free_space: Arc::new(crate::update::download::free_space),
        network_wait: NETWORK_WAIT,
        idle: IDLE,
        display_port: None,
        code: None,
        forced: None,
        activity: Vec::new(),
        edited: false,
    }
}

/// The simulator serves on 127.0.0.1:8080 (VERSO_TRANSFER_PORT), with no
/// Wi-Fi or firewall. VERSO_SIM_TRANSFER, a comma-separated list, sets it
/// up for captures: `kindle` shows a Kindle's address and name, `code=NNNN`
/// fixes the code, `activity` fills the list, `wifi-off` starts with Wi-Fi
/// off, `edited` counts Verso's data as edited (the Restart sheet), and
/// `phase=starting|waiting|no-network|idle|slept|no-wifi|port` shows that
/// state without starting anything.
#[cfg(not(feature = "kindle"))]
pub fn simulator(storage: PathBuf, options: &str) -> Device {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let has = |name: &str| options.split(',').any(|o| o.trim() == name);
    let value = |name: &str| options.split(',').find_map(|o| o.trim().strip_prefix(name)?.strip_prefix('='));
    let kindle_like = has("kindle");
    let port = std::env::var("VERSO_TRANSFER_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(8080);
    let wifi = Arc::new(AtomicBool::new(!has("wifi-off")));
    let forced = value("phase").and_then(|p| match p {
        "starting" => Some(Phase::Starting),
        "waiting" => Some(Phase::Waiting),
        "no-network" => Some(Phase::NoNetwork),
        "idle" => Some(Phase::Stopped(super::Stop::Idle)),
        "slept" => Some(Phase::Stopped(super::Stop::Slept)),
        "no-wifi" => Some(Phase::Failed(super::Failure::Wifi)),
        "port" => Some(Phase::Failed(super::Failure::PortInUse)),
        _ => None,
    });
    Device {
        wifi_enabled: Box::new({
            let wifi = wifi.clone();
            move || Ok(wifi.load(Ordering::Relaxed))
        }),
        set_wifi: Box::new(move |on| {
            log::info!("simulator: Wi-Fi {}", if on { "on" } else { "off" });
            wifi.store(on, Ordering::Relaxed);
            Ok(())
        }),
        postpone_sleep: Box::new(crate::platform::awake::postpone_sleep),
        postpone_every: POSTPONE_SLEEP,
        address: Box::new(move || Some(if kindle_like { Ipv4Addr::new(192, 168, 1, 42) } else { Ipv4Addr::LOCALHOST })),
        firewall: Box::new(|open| log::info!("simulator: port {} {}", 80, if open { "opened" } else { "closed" })),
        start_responder: Box::new(|_| None),
        responder_alive: Box::new(|_| false),
        stop_responder: Box::new(|_| {}),
        hostname: Box::new(move || kindle_like.then(|| verso_mdns::DEFAULT_LABEL.to_owned())),
        listen: SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
        local_names: vec!["127.0.0.1".into(), "localhost".into()],
        markers: std::env::temp_dir().join("verso-simulator"),
        storage,
        files_root: super::files::simulator_root(),
        free_space: Arc::new(crate::update::download::free_space),
        network_wait: NETWORK_WAIT,
        idle: IDLE,
        display_port: kindle_like.then_some(80),
        code: value("code").map(str::to_owned),
        forced,
        activity: if has("activity") { sample_activity() } else { Vec::new() },
        edited: has("edited"),
    }
}

/// What a few minutes of use look like, for captures.
#[cfg(not(feature = "kindle"))]
fn sample_activity() -> Vec<Activity> {
    let received = |name: &str, folder: &str| Activity::Received { name: name.into(), folder: folder.into() };
    vec![
        Activity::Receiving { name: "The Idiot".into() },
        received("Anna Karenina", "Russian"),
        Activity::Dictionary { name: "Ozhegov".into() },
        received("Middlemarch", "Literature"),
        Activity::Deleted { name: "Old Book".into() },
    ]
}

/// A fake device for tests: everything it's asked to do is written to
/// `calls`; the address and the responder's name come from `net`.
#[cfg(test)]
pub mod fake {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// A port nothing listens on, for the session to bind. It comes from below
    /// Linux's ephemeral range (32768 up), where binding port 0 never lands, so
    /// another test's server can't take it before the session binds it. Each
    /// process starts at its own spot, for suites running side by side.
    pub fn free_port() -> u16 {
        static NEXT: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(0);
        let base = 10_000 + (std::process::id() % 1_000) as u16 * 10;
        loop {
            let port = base + NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % 10_000;
            if std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_ok() {
                return port;
            }
        }
    }

    #[derive(Default)]
    pub struct Net {
        pub wifi: bool,
        /// How often the idle countdown was restarted (not in `calls`: it
        /// repeats while serving).
        pub postponed: u32,
        pub address: Option<Ipv4Addr>,
        /// Another responder (SSH's) already has a name.
        pub other_responder: Option<String>,
        pub ours: Option<i32>,
        pub wifi_fails: bool,
        pub calls: Vec<String>,
    }

    pub fn device(net: &Arc<Mutex<Net>>, markers: PathBuf) -> Device {
        let n = || net.clone();
        Device {
            wifi_enabled: Box::new({
                let n = n();
                move || Ok(n.lock().unwrap().wifi)
            }),
            set_wifi: Box::new({
                let n = n();
                move |on| {
                    let mut net = n.lock().unwrap();
                    net.calls.push(format!("wifi {on}"));
                    anyhow::ensure!(!net.wifi_fails, "wifid said no");
                    net.wifi = on;
                    Ok(())
                }
            }),
            postpone_sleep: Box::new({
                let n = n();
                move || {
                    n.lock().unwrap().postponed += 1;
                    Ok(())
                }
            }),
            postpone_every: Duration::from_millis(30),
            address: Box::new({
                let n = n();
                move || n.lock().unwrap().address
            }),
            firewall: Box::new({
                let n = n();
                move |open| n.lock().unwrap().calls.push(format!("firewall {open}"))
            }),
            start_responder: Box::new({
                let n = n();
                move |label| {
                    let mut net = n.lock().unwrap();
                    if net.other_responder.is_some() || net.ours.is_some() {
                        return None;
                    }
                    net.calls.push(format!("responder {label}"));
                    net.ours = Some(4242);
                    Some(4242)
                }
            }),
            responder_alive: Box::new({
                let n = n();
                move |pid| n.lock().unwrap().ours == Some(pid)
            }),
            stop_responder: Box::new({
                let n = n();
                move |pid| {
                    let mut net = n.lock().unwrap();
                    net.calls.push(format!("stop responder {pid}"));
                    net.ours = None;
                }
            }),
            hostname: Box::new({
                let n = n();
                move || {
                    let net = n.lock().unwrap();
                    net.other_responder.clone().or(net.ours.map(|_| "verso".to_owned()))
                }
            }),
            listen: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
            local_names: vec!["127.0.0.1".into()],
            markers: markers.clone(),
            storage: std::env::temp_dir(),
            files_root: markers.clone(),
            free_space: Arc::new(crate::update::download::free_space),
            network_wait: Duration::from_millis(300),
            idle: Duration::from_millis(400),
            display_port: None,
            code: None,
            forced: None,
            activity: Vec::new(),
            edited: false,
        }
    }
}
