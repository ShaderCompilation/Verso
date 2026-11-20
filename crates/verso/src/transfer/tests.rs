//! The session's lifecycle against a fake device (device::fake).

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::device::fake::{Net, device};
use super::*;

const IP: Ipv4Addr = Ipv4Addr::new(192, 168, 1, 42);

struct Fixture {
    net: Arc<Mutex<Net>>,
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new(net: Net) -> Self {
        Self { net: Arc::new(Mutex::new(net)), dir: tempfile::tempdir().unwrap() }
    }

    fn device(&self) -> Device {
        let mut device = device(&self.net, self.dir.path().to_owned());
        device.listen = SocketAddr::from((Ipv4Addr::LOCALHOST, free_port()));
        device
    }

    fn start(&self, device: Device) -> Session {
        Session::start(Arc::new(device), Waker::new(|| {}), "verso".into(), false)
    }

    fn calls(&self) -> Vec<String> {
        std::mem::take(&mut self.net.lock().unwrap().calls)
    }

    fn markers(&self) -> [bool; 2] {
        marker_paths(self.dir.path()).map(|p| p.exists())
    }
}

/// A port nothing listens on (bound and released, so it's free for a moment).
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn wait_for(session: &Session, what: &str, done: impl Fn(&Snapshot) -> bool) -> Snapshot {
    let start = Instant::now();
    loop {
        let snapshot = session.snapshot();
        if done(&snapshot) {
            return snapshot;
        }
        assert!(start.elapsed() < Duration::from_secs(5), "waiting for {what}: {snapshot:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// GET /api/status (signed out: 401) on the session's port.
fn request(port: u16) -> std::io::Result<String> {
    let mut stream = TcpStream::connect(("127.0.0.1", port))?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let request = format!("GET /api/status HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes())?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    Ok(response)
}

#[test]
fn starts_everything_and_puts_it_all_back() {
    let f = Fixture::new(Net { address: Some(IP), ..Net::default() });
    let device = f.device();
    let port = device.listen.port();
    let mut session = f.start(device);
    let ready = wait_for(&session, "ready", |s| s.phase == Phase::Ready && s.hostname.is_some());
    assert_eq!(ready.address, Some(IP));
    assert_eq!(ready.name_url().as_deref(), Some(&*format!("http://verso.local:{port}")));
    assert_eq!(ready.qr_url(), Some(format!("http://192.168.1.42:{port}/#code={}", ready.code)));
    assert!(ready.code.len() == 4);
    assert_eq!(f.calls(), ["wifi true", "awake true", "firewall true", "responder verso"]);
    assert_eq!(f.markers(), [true, true]);
    assert!(request(port).unwrap().starts_with("HTTP/1.1 401"), "the server answers");

    session.close();
    assert_eq!(f.calls(), ["firewall false", "stop responder 4242", "awake false", "wifi false"]);
    assert_eq!(f.markers(), [false, false]);
    assert!(request(port).is_err(), "nothing listens after closing");
    session.close();
    assert!(f.calls().is_empty(), "closing twice does nothing more");
}

#[test]
fn leaves_wifi_awake_and_another_responder_alone() {
    let f = Fixture::new(Net {
        wifi: true,
        awake: true,
        address: Some(IP),
        other_responder: Some("verso-2".into()),
        ..Net::default()
    });
    let mut session = f.start(f.device());
    let ready = wait_for(&session, "ready", |s| s.phase == Phase::Ready && s.hostname.is_some());
    assert_eq!(ready.hostname.as_deref(), Some("verso-2"));
    session.close();
    assert_eq!(f.calls(), ["firewall true", "firewall false"]);
    assert_eq!(f.markers(), [false, false]);
}

#[test]
fn waits_for_wifi_then_offers_try_again() {
    let f = Fixture::new(Net::default());
    let mut session = f.start(f.device());
    wait_for(&session, "waiting", |s| s.phase == Phase::Waiting);
    wait_for(&session, "no network", |s| s.phase == Phase::NoNetwork);
    assert!(f.calls().iter().all(|c| !c.starts_with("responder")), "no responder without a network");
    session.retry();
    wait_for(&session, "waiting again", |s| s.phase == Phase::Waiting);
    f.net.lock().unwrap().address = Some(IP);
    wait_for(&session, "connected", |s| s.phase == Phase::Ready && s.address == Some(IP));
    // Wi-Fi drops: back to waiting.
    f.net.lock().unwrap().address = None;
    wait_for(&session, "waiting after a drop", |s| s.phase == Phase::Waiting);
    session.close();
}

#[test]
fn reports_wifi_and_port_failures() {
    let f = Fixture::new(Net { wifi_fails: true, ..Net::default() });
    let mut session = f.start(f.device());
    wait_for(&session, "no Wi-Fi", |s| s.phase == Phase::Failed(Failure::Wifi));
    session.close();
    assert_eq!(f.calls(), ["wifi true", "wifi false"], "Wi-Fi is put back even after a failure");

    let taken = TcpListener::bind("127.0.0.1:0").unwrap();
    let f = Fixture::new(Net { wifi: true, address: Some(IP), ..Net::default() });
    let mut device = f.device();
    device.listen = taken.local_addr().unwrap();
    let mut session = f.start(device);
    wait_for(&session, "port in use", |s| s.phase == Phase::Failed(Failure::PortInUse));
    assert_eq!(f.calls(), ["awake true", "awake false"], "keep-awake is given back at once");
    assert_eq!(f.markers(), [false, false]);
    drop(taken);
    session.restart();
    wait_for(&session, "ready after trying again", |s| s.phase == Phase::Ready);
    session.close();
}

#[test]
fn sleep_stops_everything_but_wifi_and_a_new_start_has_a_new_code() {
    let f = Fixture::new(Net { address: Some(IP), ..Net::default() });
    let device = f.device();
    let port = device.listen.port();
    let mut session = f.start(device);
    let first = wait_for(&session, "ready", |s| s.phase == Phase::Ready && s.hostname.is_some());
    f.calls();
    session.stop(Stop::Slept);
    assert_eq!(session.snapshot().phase, Phase::Stopped(Stop::Slept));
    assert_eq!(f.calls(), ["firewall false", "stop responder 4242", "awake false"]);
    assert_eq!(f.markers(), [false, false]);
    assert!(request(port).is_err());
    assert!(session.idle_deadline().is_none());

    session.restart();
    let second = wait_for(&session, "ready again", |s| s.phase == Phase::Ready && s.hostname.is_some());
    assert_ne!(first.code, second.code);
    assert_eq!(f.calls(), ["awake true", "firewall true", "responder verso"], "Wi-Fi is still on");
    session.close();
    assert_eq!(f.calls(), ["firewall false", "stop responder 4242", "awake false", "wifi false"]);
}

#[test]
fn idle_deadline_follows_requests() {
    let f = Fixture::new(Net { wifi: true, address: Some(IP), ..Net::default() });
    let device = f.device();
    let port = device.listen.port();
    let idle = device.idle;
    let mut session = f.start(device);
    wait_for(&session, "ready", |s| s.phase == Phase::Ready);
    let first = session.idle_deadline().unwrap();
    assert!(first <= Instant::now() + idle);
    std::thread::sleep(Duration::from_millis(50));
    request(port).unwrap();
    let after = session.idle_deadline().unwrap();
    assert!(after > first, "a request restarts the idle time");
    session.stop(Stop::Idle);
    assert_eq!(session.snapshot().phase, Phase::Stopped(Stop::Idle));
    session.close();
}

#[test]
fn cleans_up_after_a_crash() {
    let f = Fixture::new(Net { awake: true, ..Net::default() });
    let device = f.device();
    cleanup_after_crash(&device);
    assert!(f.calls().is_empty(), "nothing to do without markers");
    for path in marker_paths(f.dir.path()) {
        std::fs::write(path, "1\n").unwrap();
    }
    cleanup_after_crash(&device);
    assert_eq!(f.calls(), ["awake false", "firewall false"]);
    assert_eq!(f.markers(), [false, false]);
}

#[test]
fn dropping_the_session_closes_it() {
    let f = Fixture::new(Net { address: Some(IP), ..Net::default() });
    let session = f.start(f.device());
    wait_for(&session, "ready", |s| s.phase == Phase::Ready);
    drop(session);
    let calls = f.calls();
    assert_eq!(calls.last().map(String::as_str), Some("wifi false"), "{calls:?}");
    assert_eq!(f.markers(), [false, false]);
}
