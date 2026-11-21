//! The web app's server (docs/web-app.md, "HTTP server"): one accept
//! thread and one thread per connection, at most MAX_CONNECTIONS at once;
//! more wait in the listen backlog. Stopping closes every connection, which
//! wakes any read or write in progress, and joins every thread before it
//! returns, so whatever a handler was writing is cleaned up by then.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::http::{Body, Conn, HeadError, Request, Response, SOCKET_TIMEOUT, STALL};

/// Browsers open up to 6 connections to a host.
pub const MAX_CONNECTIONS: usize = 8;
/// An open connection with no request in progress is closed after this.
const KEEP_ALIVE: Duration = Duration::from_secs(5);
/// Connection threads need little stack: bodies go through the
/// connection's heap buffer.
const STACK: usize = 256 * 1024;
/// What's left of a body nobody read is read and dropped up to this size,
/// so the connection can serve the next request; beyond it, it's closed.
const DRAIN: u64 = 64 * 1024;

/// Answers requests. Runs on connection threads.
pub trait Handler: Send + Sync + 'static {
    fn handle(&self, request: &Request, body: &mut Body<'_>) -> Response;
}

/// How long the server waits for a client: tests shorten these.
#[derive(Clone, Copy)]
pub struct Timeouts {
    pub keep_alive: Duration,
    pub stall: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self { keep_alive: KEEP_ALIVE, stall: STALL }
    }
}

pub struct Server {
    shared: Arc<Shared>,
    listener: Arc<TcpListener>,
    accept: Option<JoinHandle<()>>,
}

struct Shared {
    cancel: Arc<AtomicBool>,
    conns: Mutex<Conns>,
    /// A connection closed, or the server is stopping.
    changed: Condvar,
    in_flight: AtomicUsize,
    last_activity: Mutex<Instant>,
    timeouts: Timeouts,
}

#[derive(Default)]
struct Conns {
    next: u64,
    /// A second handle on each open socket, to shut it down on stop.
    open: HashMap<u64, TcpStream>,
    threads: Vec<JoinHandle<()>>,
}

impl Shared {
    fn conns(&self) -> MutexGuard<'_, Conns> {
        self.conns.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

/// Counts a request as in progress until dropped.
struct Active<'a>(&'a Shared);

impl<'a> Active<'a> {
    fn begin(shared: &'a Shared) -> Self {
        shared.in_flight.fetch_add(1, Ordering::SeqCst);
        Self(shared)
    }
}

impl Drop for Active<'_> {
    fn drop(&mut self) {
        *self.0.last_activity.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
        self.0.in_flight.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Server {
    /// Listens on `addr` and serves until stopped (or dropped).
    pub fn start(addr: SocketAddr, handler: Arc<dyn Handler>, timeouts: Timeouts) -> io::Result<Server> {
        let listener = Arc::new(TcpListener::bind(addr)?);
        let addr = listener.local_addr()?;
        let shared = Arc::new(Shared {
            cancel: Arc::default(),
            conns: Mutex::default(),
            changed: Condvar::new(),
            in_flight: AtomicUsize::new(0),
            last_activity: Mutex::new(Instant::now()),
            timeouts,
        });
        let accept = {
            let (shared, listener) = (shared.clone(), listener.clone());
            std::thread::Builder::new()
                .name("web-accept".into())
                .stack_size(STACK)
                .spawn(move || accept_loop(&listener, &shared, &handler))?
        };
        log::info!("web app: serving on {addr}");
        Ok(Server { shared, listener, accept: Some(accept) })
    }

    #[cfg(test)]
    pub fn addr(&self) -> SocketAddr {
        self.listener.local_addr().unwrap()
    }

    /// When the last request ended, while none is in progress.
    pub fn idle_since(&self) -> Option<Instant> {
        if self.shared.in_flight.load(Ordering::SeqCst) > 0 {
            return None;
        }
        Some(*self.shared.last_activity.lock().unwrap_or_else(|e| e.into_inner()))
    }

    /// Closes every connection and waits for all threads to finish.
    pub fn stop(&mut self) {
        let Some(accept) = self.accept.take() else { return };
        let started = Instant::now();
        self.shared.cancel.store(true, Ordering::SeqCst);
        // Wakes the blocked accept (Linux returns EINVAL from it).
        // SAFETY: shutdown(2) on the listener's open descriptor.
        unsafe { libc::shutdown(self.listener.as_raw_fd(), libc::SHUT_RDWR) };
        self.shared.changed.notify_all();
        let _ = accept.join();
        let (open, threads) = {
            let mut conns = self.shared.conns();
            (std::mem::take(&mut conns.open), std::mem::take(&mut conns.threads))
        };
        for stream in open.values() {
            let _ = stream.shutdown(Shutdown::Both);
        }
        let count = threads.len();
        for thread in threads {
            let _ = thread.join();
        }
        log::info!("web app: stopped in {:?} ({count} connection thread(s) joined)", started.elapsed());
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop();
    }
}

fn accept_loop(listener: &TcpListener, shared: &Arc<Shared>, handler: &Arc<dyn Handler>) {
    loop {
        {
            let mut conns = shared.conns();
            while conns.open.len() >= MAX_CONNECTIONS && !shared.cancelled() {
                conns = shared.changed.wait(conns).unwrap_or_else(|e| e.into_inner());
            }
        }
        if shared.cancelled() {
            return;
        }
        let (stream, peer) = match listener.accept() {
            Ok(accepted) => accepted,
            Err(_) if shared.cancelled() => return,
            Err(e) => {
                // Out of descriptors or memory: wait instead of spinning.
                log::warn!("web app: accept: {e}");
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
        };
        if shared.cancelled() {
            return;
        }
        let ready = stream
            .set_read_timeout(Some(SOCKET_TIMEOUT))
            .and_then(|()| stream.set_write_timeout(Some(SOCKET_TIMEOUT)))
            .and_then(|()| stream.set_nodelay(true))
            .and_then(|()| stream.try_clone());
        let handle = match ready {
            Ok(handle) => handle,
            Err(e) => {
                log::warn!("web app: preparing a connection from {peer}: {e}");
                continue;
            }
        };
        let mut conns = shared.conns();
        conns.threads.retain(|t| !t.is_finished());
        let id = conns.next;
        conns.next += 1;
        conns.open.insert(id, handle);
        let spawned = {
            let (shared, handler) = (shared.clone(), handler.clone());
            std::thread::Builder::new().name("web-conn".into()).stack_size(STACK).spawn(move || {
                serve(stream, &shared, &*handler);
                shared.conns().open.remove(&id);
                shared.changed.notify_all();
            })
        };
        match spawned {
            Ok(thread) => conns.threads.push(thread),
            Err(e) => {
                log::warn!("web app: starting a connection thread: {e}");
                conns.open.remove(&id);
            }
        }
    }
}

/// Serves requests on one connection until it closes, goes quiet or the
/// server stops.
fn serve(stream: TcpStream, shared: &Shared, handler: &dyn Handler) {
    let mut conn = Conn::new(stream, shared.cancel.clone(), shared.timeouts.stall);
    loop {
        let request = match conn.read_head(shared.timeouts.keep_alive) {
            Ok(request) => request,
            Err(HeadError::Closed) => return,
            Err(HeadError::Status(status, why)) => {
                log::debug!("web app: refused a request: {status} {why}");
                let _active = Active::begin(shared);
                let mut response = Response::text(status, why);
                if conn.respond(&mut response, false).is_ok() {
                    conn.linger();
                }
                return;
            }
        };
        let active = Active::begin(shared);
        let mut body = conn.body(request.content_length, request.expect_continue);
        let mut response = handler.handle(&request, &mut body);
        log::debug!("web app: {:?} {} → {}", request.method, request.path, response.status);
        // An unread body is dropped if it's small; a broken one ends the connection.
        let finished = !body.failed() && (body.remaining() == 0 || body.skip(DRAIN).unwrap_or(false));
        let broken = body.failed();
        let keep_alive = request.keep_alive && finished && !shared.cancelled();
        let sent = conn.respond(&mut response, keep_alive);
        drop(active);
        if sent.is_err() || !keep_alive {
            if sent.is_ok() && !finished && !broken {
                conn.linger();
            }
            return;
        }
    }
}

/// A file being received: written under a temporary name, and deleted
/// unless it's finished, however its handler ends (an error, a broken
/// connection, the server stopping).
pub struct PartFile {
    path: PathBuf,
    file: Option<File>,
}

impl PartFile {
    /// Creates `path`, which must not exist yet.
    pub fn create(path: PathBuf) -> io::Result<Self> {
        let file = File::options().write(true).create_new(true).open(&path)?;
        Ok(Self { path, file: Some(file) })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Flushes and closes the write handle while keeping the part file
    /// guarded for cleanup until renamed.
    pub fn sync_and_close(&mut self) -> io::Result<()> {
        if let Some(mut file) = self.file.take() {
            file.flush()?;
            file.sync_all()?;
        }
        Ok(())
    }

    /// Flushes the file to storage and renames it to `to`.
    pub fn finish(mut self, to: &Path) -> io::Result<()> {
        self.sync_and_close()?;
        std::fs::rename(&self.path, to)?;
        self.path = PathBuf::new();
        Ok(())
    }

    /// Renames to `to` without overwriting an existing file. On error, keeps
    /// the part file path so a caller can retry on `AlreadyExists` or let Drop
    /// delete it.
    pub fn try_finish_noreplace(&mut self, to: &Path) -> io::Result<()> {
        self.sync_and_close()?;
        crate::library::files::rename_free(&self.path, to)?;
        self.path = PathBuf::new();
        Ok(())
    }
}

impl Write for PartFile {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.file.as_mut().ok_or(io::ErrorKind::NotConnected)?.write(data)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.as_mut().map_or(Ok(()), |f| f.flush())
    }
}

impl Drop for PartFile {
    fn drop(&mut self) {
        drop(self.file.take());
        if !self.path.as_os_str().is_empty()
            && let Err(e) = std::fs::remove_file(&self.path)
            && e.kind() != io::ErrorKind::NotFound
        {
            log::warn!("web app: deleting {}: {e}", self.path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    /// Answers "ok" to GET; PUT /upload streams the body into a part file
    /// in the folder and keeps it as "upload" when complete.
    struct Test {
        dir: PathBuf,
        started: AtomicUsize,
    }

    impl Handler for Test {
        fn handle(&self, request: &Request, body: &mut Body<'_>) -> Response {
            self.started.fetch_add(1, Ordering::SeqCst);
            if request.path == "/upload" {
                let mut part = PartFile::create(self.dir.join("upload.part")).unwrap();
                return match io::copy(body, &mut part) {
                    Ok(_) => {
                        part.finish(&self.dir.join("upload")).unwrap();
                        Response::text(201, "kept")
                    }
                    Err(e) => Response::text(400, &e.to_string()),
                };
            }
            Response::text(200, &format!("ok {}", request.path))
        }
    }

    fn server(timeouts: Timeouts) -> (Server, Arc<Test>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let handler = Arc::new(Test { dir: dir.path().to_owned(), started: AtomicUsize::new(0) });
        let server = Server::start("127.0.0.1:0".parse().unwrap(), handler.clone(), timeouts).unwrap();
        (server, handler, dir)
    }

    fn connect(server: &Server) -> TcpStream {
        let stream = TcpStream::connect(server.addr()).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        stream
    }

    /// Reads one response; its status and body.
    fn response(stream: &mut TcpStream) -> (u16, String) {
        let mut head = Vec::new();
        let mut byte = [0];
        while !head.ends_with(b"\r\n\r\n") {
            assert_eq!(stream.read(&mut byte).unwrap(), 1, "connection closed: {:?}", String::from_utf8_lossy(&head));
            head.push(byte[0]);
        }
        let head = String::from_utf8(head).unwrap();
        let status = head[9..12].parse().unwrap();
        let length: usize =
            head.lines().find_map(|l| l.strip_prefix("Content-Length: ")).map_or(0, |v| v.trim().parse().unwrap());
        let mut body = vec![0; length];
        stream.read_exact(&mut body).unwrap();
        (status, String::from_utf8(body).unwrap())
    }

    fn closed(stream: &mut TcpStream) -> bool {
        let mut byte = [0];
        matches!(stream.read(&mut byte), Ok(0) | Err(_))
    }

    #[test]
    fn serves_requests_on_a_kept_alive_connection() {
        let (server, handler, _dir) = server(Timeouts::default());
        let mut c = connect(&server);
        c.write_all(b"GET /a HTTP/1.1\r\nHost: x\r\n\r\nGET /b HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        assert_eq!(response(&mut c), (200, "ok /a".into()));
        assert_eq!(response(&mut c), (200, "ok /b".into()));
        // A small body nobody reads is dropped; the connection goes on.
        c.write_all(b"POST /c HTTP/1.1\r\nHost: x\r\nContent-Length: 3\r\n\r\nabcGET /d HTTP/1.1\r\nHost: x\r\n\r\n")
            .unwrap();
        assert_eq!(response(&mut c), (200, "ok /c".into()));
        assert_eq!(response(&mut c), (200, "ok /d".into()));
        c.write_all(b"GET /e HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").unwrap();
        assert_eq!(response(&mut c), (200, "ok /e".into()));
        assert!(closed(&mut c));
        assert_eq!(handler.started.load(Ordering::SeqCst), 5);
    }

    #[test]
    fn refuses_malformed_truncated_and_oversized_requests() {
        let (server, handler, _dir) = server(Timeouts { keep_alive: Duration::from_millis(300), stall: STALL });
        let mut c = connect(&server);
        c.write_all(b"GET / HTTP/1.1\r\nBroken header\r\n\r\n").unwrap();
        assert_eq!(response(&mut c).0, 400);
        assert!(closed(&mut c));

        let mut c = connect(&server);
        c.write_all(&[b'a'; 9000]).unwrap();
        assert_eq!(response(&mut c).0, 431);
        assert!(closed(&mut c));

        let mut c = connect(&server);
        let mut head = b"GET / HTTP/1.1\r\nHost: x\r\nX-Long: ".to_vec();
        head.extend([b'a'; 8200]);
        head.extend(b"\r\n\r\n");
        c.write_all(&head).unwrap();
        assert_eq!(response(&mut c).0, 431);

        // Cut off in the head: closed without an answer to read.
        let mut c = connect(&server);
        c.write_all(b"GET / HTTP/1.1\r\nHo").unwrap();
        c.shutdown(Shutdown::Write).unwrap();
        let mut rest = String::new();
        let _ = c.read_to_string(&mut rest);
        assert!(rest.is_empty() || rest.starts_with("HTTP/1.1 400"), "{rest}");

        // Cut off in the body: the upload is dropped with its part file.
        let mut c = connect(&server);
        c.write_all(b"PUT /upload HTTP/1.1\r\nHost: x\r\nContent-Length: 100\r\n\r\nshort").unwrap();
        c.shutdown(Shutdown::Write).unwrap();
        assert_eq!(response(&mut c).0, 400);
        assert!(!handler.dir.join("upload.part").exists() && !handler.dir.join("upload").exists());

        // An idle keep-alive connection is closed.
        let mut c = connect(&server);
        let started = Instant::now();
        assert!(closed(&mut c));
        assert!(started.elapsed() < Duration::from_secs(3));
        assert_eq!(handler.started.load(Ordering::SeqCst), 1, "only the upload reached the handler");
    }

    #[test]
    fn answers_expect_continue_when_the_body_is_first_read() {
        let (server, handler, _dir) = server(Timeouts::default());
        let mut c = connect(&server);
        c.write_all(b"PUT /upload HTTP/1.1\r\nHost: x\r\nContent-Length: 4\r\nExpect: 100-continue\r\n\r\n").unwrap();
        // The client waits for the interim response before sending anything.
        assert_eq!(response(&mut c).0, 100);
        c.write_all(b"data").unwrap();
        assert_eq!(response(&mut c), (201, "kept".into()));
        assert_eq!(std::fs::read(handler.dir.join("upload")).unwrap(), b"data");

        // A request answered without reading the body isn't invited to send it.
        c.write_all(b"POST /x HTTP/1.1\r\nHost: x\r\nContent-Length: 4\r\nExpect: 100-continue\r\n\r\n").unwrap();
        assert_eq!(response(&mut c), (200, "ok /x".into()));
        assert!(closed(&mut c));

        let mut c = connect(&server);
        c.write_all(b"PUT /upload HTTP/1.1\r\nHost: x\r\nContent-Length: 4\r\nExpect: 200-ok\r\n\r\n").unwrap();
        assert_eq!(response(&mut c).0, 417);
        assert!(closed(&mut c));
    }

    #[test]
    fn keeps_a_complete_upload() {
        let (server, handler, _dir) = server(Timeouts::default());
        let mut c = connect(&server);
        let data = vec![7u8; 300_000];
        c.write_all(format!("PUT /upload HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n", data.len()).as_bytes())
            .unwrap();
        c.write_all(&data).unwrap();
        assert_eq!(response(&mut c), (201, "kept".into()));
        assert_eq!(std::fs::read(handler.dir.join("upload")).unwrap(), data);
        assert!(!handler.dir.join("upload.part").exists());
    }

    #[test]
    fn stopping_during_a_stalled_upload_deletes_the_part_file_and_joins() {
        let (mut server, handler, _dir) = server(Timeouts::default());
        let mut c = connect(&server);
        c.write_all(b"PUT /upload HTTP/1.1\r\nHost: x\r\nContent-Length: 1000000\r\n\r\n").unwrap();
        c.write_all(&[1u8; 100_000]).unwrap();
        let part = handler.dir.join("upload.part");
        let started = Instant::now();
        while std::fs::metadata(&part).map_or(0, |m| m.len()) < 100_000 {
            assert!(started.elapsed() < Duration::from_secs(5), "the upload didn't start");
            std::thread::sleep(Duration::from_millis(10));
        }
        // Idle connections and the accept thread are stopped too.
        let _idle = connect(&server);
        let started = Instant::now();
        server.stop();
        assert!(started.elapsed() < Duration::from_secs(2), "stop took {:?}", started.elapsed());
        assert!(!part.exists(), "the part file is gone once stop returns");
        assert!(!handler.dir.join("upload").exists());
        assert!(TcpStream::connect(server.addr()).is_err(), "nothing listens after stop");
    }

    #[test]
    fn a_client_that_stops_sending_is_given_up() {
        let stall = Duration::from_millis(500);
        let (server, handler, _dir) = server(Timeouts { keep_alive: KEEP_ALIVE, stall });
        let mut c = connect(&server);
        c.write_all(b"PUT /upload HTTP/1.1\r\nHost: x\r\nContent-Length: 1000\r\n\r\nabc").unwrap();
        let (status, _) = response(&mut c);
        assert_eq!(status, 400);
        assert!(closed(&mut c));
        assert!(!handler.dir.join("upload.part").exists());
    }

    #[test]
    fn limits_connections_and_tracks_activity() {
        let (server, _handler, _dir) = server(Timeouts::default());
        let idle = (0..MAX_CONNECTIONS).map(|_| connect(&server)).collect::<Vec<_>>();
        std::thread::sleep(Duration::from_millis(100));
        let mut waiting = connect(&server);
        waiting.set_read_timeout(Some(Duration::from_millis(300))).unwrap();
        waiting.write_all(b"GET /late HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        let mut byte = [0];
        assert!(waiting.read(&mut byte).is_err(), "served beyond the limit");
        let before = server.idle_since().unwrap();
        drop(idle);
        waiting.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        assert_eq!(response(&mut waiting), (200, "ok /late".into()));
        // The response reaches the client before the serving thread finishes
        // the request's bookkeeping, so the idle stamp can still be pending;
        // wait for it instead of racing it.
        let deadline = Instant::now() + Duration::from_secs(5);
        while server.idle_since().is_none_or(|at| at <= before) {
            assert!(Instant::now() < deadline, "a finished request restarts the idle time");
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}
