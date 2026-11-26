//! HTTPS with `ureq` and `rustls`, trusting the certificate list built into
//! Verso (`webpki-roots`) rather than the firmware's. Updates and online
//! lookup share it. Calls block, so they belong on worker threads, and can't
//! be aborted: timeouts bound them.
use std::fmt;
use std::fs::OpenOptions;
use std::io::{ErrorKind, Read, Write};
use std::path::Path;
use std::time::Duration;

/// What requests say unless a client is told otherwise: the app and its
/// version, and nothing else.
pub fn user_agent() -> String {
    format!("Verso/{}", env!("CARGO_PKG_VERSION"))
}

/// GitHub sends release downloads through one or two redirects.
const MAX_REDIRECTS: u32 = 5;

/// A configured HTTP client: one per check, download or lookup. Responses may
/// arrive compressed (gzip), except saved files.
pub struct Client {
    agent: ureq::Agent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchError {
    /// The connection or the whole request took longer than allowed.
    Timeout,
    /// The name didn't resolve, or nothing answered: usually no internet.
    Unreachable(String),
    /// The secure connection failed: a bad certificate, or the device's
    /// clock is far off.
    Tls(String),
    /// The server answered with an error status.
    Status(u16),
    /// The response was larger than the caller allows.
    TooLarge(u64),
    /// A plain `http://` address where HTTPS is required (also in redirects).
    Insecure(String),
    /// Anything else: protocol errors, too many redirects, a bad address.
    Other(String),
}

impl fmt::Display for FetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FetchError::Timeout => write!(f, "timed out"),
            FetchError::Unreachable(e) => write!(f, "unreachable: {e}"),
            FetchError::Tls(e) => write!(f, "TLS: {e}"),
            FetchError::Status(code) => write!(f, "HTTP {code}"),
            FetchError::TooLarge(limit) => write!(f, "larger than {limit} bytes"),
            FetchError::Insecure(url) => write!(f, "not HTTPS: {url}"),
            FetchError::Other(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for FetchError {}

/// Why saving a download to a file stopped.
#[derive(Debug)]
pub enum SaveError {
    Fetch(FetchError),
    /// Writing the file failed.
    Io(std::io::Error),
    Cancelled,
}

impl Client {
    /// `connect` limits each connection attempt (including DNS); `total` the
    /// whole request, body included. With `https_only`, plain `http://`
    /// addresses and redirects to them are refused.
    pub fn new(connect: Duration, total: Duration, https_only: bool) -> Self {
        Self::build(connect, Some(total), None, https_only, &user_agent())
    }

    /// Like `new`, for a server that wants to know more than [`user_agent`].
    pub fn named(user_agent: &str, connect: Duration, total: Duration, https_only: bool) -> Self {
        Self::build(connect, Some(total), None, https_only, user_agent)
    }

    /// A client for archives: no limit on a whole transfer, which depends on
    /// the network, but every read must arrive within `idle`.
    pub fn for_downloads(connect: Duration, idle: Duration, https_only: bool) -> Self {
        Self::build(connect, None, Some(idle), https_only, &user_agent())
    }

    fn build(
        connect: Duration,
        total: Option<Duration>,
        idle: Option<Duration>,
        https_only: bool,
        user_agent: &str,
    ) -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_connect(Some(connect))
            .timeout_global(total)
            .timeout_per_read(idle)
            .max_redirects(MAX_REDIRECTS)
            .https_only(https_only)
            .user_agent(user_agent)
            .build()
            .into();
        Self { agent }
    }

    /// Downloads `url` into the file at `path`, which should end up `size`
    /// bytes long. What the file already holds is kept and only the rest is
    /// asked for (an HTTP range); a server that ignores ranges sends it all
    /// again. `progress` hears the file's length after every chunk and
    /// returns false to stop. The file is synced when complete; after a
    /// failure it keeps what arrived, for the next try.
    pub fn save(
        &self,
        url: &str,
        path: &Path,
        size: u64,
        progress: &mut dyn FnMut(u64) -> bool,
    ) -> Result<(), SaveError> {
        let mut have = std::fs::metadata(path).map_or(0, |m| m.len());
        if have > size {
            have = 0;
        }
        if have == size && size > 0 {
            return Ok(());
        }
        // Sizes and ranges are the file's own, never a compressed copy's.
        let mut request = self.agent.get(url).header("Accept-Encoding", "identity");
        if have > 0 {
            request = request.header("Range", format!("bytes={have}-"));
        }
        let mut response = match request.call() {
            // The part we have is no longer valid (the file changed): start over.
            Err(ureq::Error::StatusCode(416)) if have > 0 => {
                std::fs::remove_file(path).map_err(SaveError::Io)?;
                return self.save(url, path, size, progress);
            }
            other => other.map_err(|e| SaveError::Fetch(classify(e, size)))?,
        };
        let start = match response.status().as_u16() {
            206 => {
                let range = response.headers().get("content-range").and_then(|v| v.to_str().ok()).unwrap_or("");
                if !range.starts_with(&format!("bytes {have}-")) {
                    // Not the part asked for: forget the old part.
                    let _ = std::fs::remove_file(path);
                    return Err(SaveError::Fetch(FetchError::Other(format!("unexpected range {range:?}"))));
                }
                have
            }
            _ => 0,
        };
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .append(start > 0)
            .truncate(start == 0)
            .open(path)
            .map_err(SaveError::Io)?;
        let remaining = size - start;
        let mut body = response.body_mut().with_config().limit(remaining).reader();
        let mut buf = vec![0; 64 * 1024];
        let mut length = start;
        loop {
            let n = match body.read(&mut buf) {
                Ok(n) => n,
                Err(e) => return Err(SaveError::Fetch(classify(ureq::Error::from(e), remaining))),
            };
            if n == 0 {
                break;
            }
            file.write_all(&buf[..n]).map_err(SaveError::Io)?;
            length += n as u64;
            if !progress(length) {
                return Err(SaveError::Cancelled);
            }
        }
        if length != size {
            return Err(SaveError::Fetch(FetchError::Other(format!(
                "the download ended after {length} of {size} bytes"
            ))));
        }
        file.sync_all().map_err(SaveError::Io)
    }

    /// Downloads `url` into memory, following redirects, refusing bodies
    /// longer than `limit` bytes (once decompressed).
    pub fn get(&self, url: &str, limit: u64) -> Result<Vec<u8>, FetchError> {
        self.get_query(url, &[], limit)
    }

    /// `get` with query parameters, which it percent-encodes.
    pub fn get_query(&self, url: &str, query: &[(&str, &str)], limit: u64) -> Result<Vec<u8>, FetchError> {
        read(self.agent.get(url).query_pairs(query.iter().copied()).call(), limit)
    }

    /// Posts `form` (URL-encoded) and downloads the answer like `get`. For
    /// text too long for an address.
    pub fn post_form(&self, url: &str, form: &[(&str, &str)], limit: u64) -> Result<Vec<u8>, FetchError> {
        self.post_form_headers(url, &[], form, limit)
    }

    /// Like `post_form`, with extra request headers (such as `Authorization`).
    pub fn post_form_headers(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        form: &[(&str, &str)],
        limit: u64,
    ) -> Result<Vec<u8>, FetchError> {
        let mut request = self.agent.post(url);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        read(request.send_form(form.iter().copied()), limit)
    }
}

fn read(response: Result<ureq::http::Response<ureq::Body>, ureq::Error>, limit: u64) -> Result<Vec<u8>, FetchError> {
    let mut response = response.map_err(|e| classify(e, limit))?;
    response.body_mut().with_config().limit(limit).read_to_vec().map_err(|e| classify(e, limit))
}

/// Whether `e` carries the error `variant` holds: rustls reports a failed
/// handshake through `io::Error`. Why this shape: it names rustls' error type
/// without a dependency on rustls, whose version must stay ureq's.
fn wraps<T: std::error::Error + 'static>(e: &std::io::Error, _variant: fn(T) -> ureq::Error) -> bool {
    e.get_ref().is_some_and(|inner| inner.is::<T>())
}

fn classify(error: ureq::Error, limit: u64) -> FetchError {
    use ureq::Error as E;
    match error {
        E::Io(e) if wraps(&e, E::Rustls) => FetchError::Tls(e.to_string()),
        E::Timeout(_) => FetchError::Timeout,
        E::StatusCode(code) => FetchError::Status(code),
        E::BodyExceedsLimit(_) => FetchError::TooLarge(limit),
        E::HostNotFound | E::ConnectionFailed => FetchError::Unreachable(error.to_string()),
        E::Io(e) => match e.kind() {
            ErrorKind::TimedOut | ErrorKind::WouldBlock => FetchError::Timeout,
            ErrorKind::ConnectionRefused
            | ErrorKind::ConnectionReset
            | ErrorKind::ConnectionAborted
            | ErrorKind::NotConnected
            | ErrorKind::HostUnreachable
            | ErrorKind::NetworkUnreachable
            | ErrorKind::NetworkDown
            | ErrorKind::AddrNotAvailable => FetchError::Unreachable(e.to_string()),
            _ => FetchError::Other(e.to_string()),
        },
        E::Rustls(e) => FetchError::Tls(e.to_string()),
        E::Tls(e) => FetchError::Tls(e.to_string()),
        E::RequireHttpsOnly(url) => FetchError::Insecure(url),
        other => FetchError::Other(other.to_string()),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};
    use std::time::Instant;

    /// What the test server does with a request for a path.
    #[derive(Clone)]
    pub(crate) enum Reply {
        Ok(Vec<u8>),
        /// Like `Ok`, compressed.
        Gzip(Vec<u8>),
        Status(u16),
        Redirect(String),
        /// Sends headers promising `len` bytes, then stalls.
        Stall(usize),
        /// Never answers.
        Silent,
        /// Like `Ok`, and answers `Range: bytes=N-` with 206.
        Ranged(Vec<u8>),
        /// Promises the whole body, sends the first `n` bytes (of the part
        /// asked for, honouring ranges) and closes the connection.
        Cut(Vec<u8>, usize),
    }

    /// A request as the test server got it.
    #[derive(Clone, Debug, Default)]
    pub(crate) struct Request {
        pub method: String,
        /// With the query, as sent.
        pub path: String,
        pub user_agent: String,
        pub authorization: String,
        pub accept_encoding: String,
        pub content_type: String,
        pub body: String,
    }

    impl Request {
        /// A decoded parameter of the query or the posted form.
        pub(crate) fn param(&self, name: &str) -> Option<String> {
            let query = self.path.split_once('?').map_or("", |(_, q)| q);
            let mut pairs = query.split('&').chain(self.body.split('&')).filter_map(|p| p.split_once('='));
            pairs.find(|(n, _)| *n == name).map(|(_, value)| crate::transfer::http::decode_component(value).unwrap())
        }
    }

    type Handler = dyn Fn(&Request) -> Reply + Send + Sync;

    /// A tiny HTTP/1.1 server on 127.0.0.1 for tests. It records every
    /// request.
    pub(crate) struct Server {
        pub base: String,
        pub requests: Arc<Mutex<Vec<Request>>>,
        /// Each request's path and Range header, if it had one.
        pub ranges: Arc<RangeLog>,
    }

    type RangeLog = Mutex<Vec<(String, Option<String>)>>;

    impl Server {
        pub(crate) fn start(routes: Vec<(&str, Reply)>) -> Self {
            let routes: Vec<(String, Reply)> = routes.into_iter().map(|(p, r)| (p.to_string(), r)).collect();
            Self::answering(move |request| {
                routes.iter().find(|(p, _)| *p == request.path).map_or(Reply::Status(404), |(_, r)| r.clone())
            })
        }

        /// A server that works out each reply from the request.
        pub(crate) fn answering(handler: impl Fn(&Request) -> Reply + Send + Sync + 'static) -> Self {
            let handler: Arc<Handler> = Arc::new(handler);
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let requests = Arc::new(Mutex::new(Vec::new()));
            let ranges = Arc::new(Mutex::new(Vec::new()));
            let (log, range_log) = (requests.clone(), ranges.clone());
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else { break };
                    let handler = handler.clone();
                    let (log, range_log) = (log.clone(), range_log.clone());
                    std::thread::spawn(move || serve(stream, &*handler, &log, &range_log));
                }
            });
            Self { base, requests, ranges }
        }

        pub(crate) fn url(&self, path: &str) -> String {
            format!("{}{path}", self.base)
        }

        pub(crate) fn paths(&self) -> Vec<String> {
            self.requests.lock().unwrap().iter().map(|r| r.path.clone()).collect()
        }

        pub(crate) fn request(&self, index: usize) -> Request {
            self.requests.lock().unwrap()[index].clone()
        }
    }

    fn serve(mut stream: TcpStream, handler: &Handler, log: &Mutex<Vec<Request>>, ranges: &RangeLog) {
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        if reader.read_line(&mut line).is_err() {
            return;
        }
        let mut words = line.split_whitespace().map(str::to_string);
        let mut request = Request { method: words.next().unwrap_or_default(), ..Request::default() };
        request.path = words.next().unwrap_or_default();
        let mut range = None;
        let mut length = 0;
        loop {
            let mut header = String::new();
            if reader.read_line(&mut header).unwrap_or(0) == 0 || header == "\r\n" {
                break;
            }
            if let Some((name, value)) = header.split_once(':') {
                let value = value.trim().to_string();
                match name.to_ascii_lowercase().as_str() {
                    "user-agent" => request.user_agent = value,
                    "authorization" => request.authorization = value,
                    "accept-encoding" => request.accept_encoding = value,
                    "content-type" => request.content_type = value,
                    "content-length" => length = value.parse().unwrap(),
                    "range" => range = Some(value),
                    _ => {}
                }
            }
        }
        let mut body = vec![0; length];
        if reader.read_exact(&mut body).is_err() {
            return;
        }
        request.body = String::from_utf8(body).unwrap();
        ranges.lock().unwrap().push((request.path.clone(), range.clone()));
        log.lock().unwrap().push(request.clone());
        // The offset a `bytes=N-` range asks for.
        let from = range.as_deref().and_then(|r| r.strip_prefix("bytes=")?.strip_suffix('-')?.parse::<usize>().ok());
        let reply = handler(&request);
        let head = |status: &str, extra: &str, len: usize| {
            format!("HTTP/1.1 {status}\r\nContent-Length: {len}\r\nConnection: close\r\n{extra}\r\n")
        };
        let _ = match reply {
            Reply::Ok(body) => {
                stream.write_all(head("200 OK", "", body.len()).as_bytes()).and_then(|()| stream.write_all(&body))
            }
            Reply::Gzip(body) => {
                let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
                encoder.write_all(&body).unwrap();
                let body = encoder.finish().unwrap();
                let header = head("200 OK", "Content-Encoding: gzip\r\n", body.len());
                stream.write_all(header.as_bytes()).and_then(|()| stream.write_all(&body))
            }
            Reply::Status(code) => stream.write_all(head(&format!("{code} Error"), "", 0).as_bytes()),
            Reply::Redirect(to) => stream.write_all(head("302 Found", &format!("Location: {to}\r\n"), 0).as_bytes()),
            Reply::Stall(len) => {
                let _ = stream.write_all(head("200 OK", "", len).as_bytes()).and_then(|()| stream.write_all(b"x"));
                std::thread::sleep(Duration::from_secs(5));
                Ok(())
            }
            Reply::Silent => {
                std::thread::sleep(Duration::from_secs(5));
                Ok(())
            }
            Reply::Ranged(body) | Reply::Cut(body, _) if from.is_some_and(|n| n >= body.len()) => {
                let extra = format!("Content-Range: bytes */{}\r\n", body.len());
                stream.write_all(head("416 Range Not Satisfiable", &extra, 0).as_bytes())
            }
            Reply::Ranged(ref body) | Reply::Cut(ref body, _) => {
                let start = from.unwrap_or(0);
                let part = &body[start..];
                let send = match reply {
                    Reply::Cut(_, n) => &part[..n.min(part.len())],
                    _ => part,
                };
                let header = match from {
                    Some(n) => {
                        let extra = format!("Content-Range: bytes {n}-{}/{}\r\n", body.len() - 1, body.len());
                        head("206 Partial Content", &extra, part.len())
                    }
                    None => head("200 OK", "", part.len()),
                };
                stream.write_all(header.as_bytes()).and_then(|()| stream.write_all(send))
            }
        };
    }

    fn client(total_ms: u64) -> Client {
        Client::new(Duration::from_millis(500), Duration::from_millis(total_ms), false)
    }

    #[test]
    fn downloads_with_the_user_agent_and_follows_redirects() {
        let server = Server::start(vec![
            ("/latest.json", Reply::Redirect("/download/latest.json".into())),
            ("/download/latest.json", Reply::Ok(b"{}".to_vec())),
        ]);
        assert_eq!(client(2000).get(&server.url("/latest.json"), 100).unwrap(), b"{}");
        assert_eq!(server.paths(), ["/latest.json", "/download/latest.json"]);
        let agents: Vec<String> = server.requests.lock().unwrap().iter().map(|r| r.user_agent.clone()).collect();
        assert!(agents.iter().all(|a| *a == format!("Verso/{}", env!("CARGO_PKG_VERSION"))), "{agents:?}");
    }

    #[test]
    fn sends_queries_and_forms_under_a_chosen_name() {
        let server = Server::answering(|_| Reply::Ok(b"ok".to_vec()));
        let client = Client::named("Verso/1 (contact)", Duration::from_secs(1), Duration::from_secs(2), false);
        let text = "naïve & “Köln” 100%+1=2";
        client.get_query(&server.url("/api?a=1"), &[("q", text)], 100).unwrap();
        client.post_form(&server.url("/form"), &[("x", "1"), ("q", text)], 100).unwrap();
        let (get, post) = (server.request(0), server.request(1));
        assert_eq!((get.method.as_str(), get.param("a"), get.param("q")), ("GET", Some("1".into()), Some(text.into())));
        assert_eq!((post.method.as_str(), post.path.as_str()), ("POST", "/form"), "nothing in the address");
        assert_eq!(post.content_type, "application/x-www-form-urlencoded");
        assert_eq!((post.param("x"), post.param("q")), (Some("1".into()), Some(text.into())));
        assert_eq!([get.user_agent, post.user_agent], ["Verso/1 (contact)", "Verso/1 (contact)"]);
    }

    #[test]
    fn decompresses_within_the_limit() {
        let server = Server::start(vec![("/a", Reply::Gzip(vec![b'x'; 10_000]))]);
        assert_eq!(client(2000).get(&server.url("/a"), 10_000).unwrap(), vec![b'x'; 10_000]);
        assert!(server.request(0).accept_encoding.contains("gzip"));
        // The limit counts what comes out, however small the transfer.
        assert_eq!(client(2000).get(&server.url("/a"), 9_999), Err(FetchError::TooLarge(9_999)));
    }

    #[test]
    fn refuses_large_bodies_and_error_statuses() {
        let server = Server::start(vec![("/big", Reply::Ok(vec![b'x'; 101])), ("/gone", Reply::Status(503))]);
        assert_eq!(client(2000).get(&server.url("/big"), 100), Err(FetchError::TooLarge(100)));
        assert_eq!(client(2000).get(&server.url("/big"), 101).unwrap().len(), 101);
        assert_eq!(client(2000).get(&server.url("/gone"), 100), Err(FetchError::Status(503)));
        assert_eq!(client(2000).get(&server.url("/missing"), 100), Err(FetchError::Status(404)));
    }

    #[test]
    fn redirect_loops_end() {
        let server = Server::start(vec![("/a", Reply::Redirect("/b".into())), ("/b", Reply::Redirect("/a".into()))]);
        assert!(matches!(client(2000).get(&server.url("/a"), 100), Err(FetchError::Other(_))));
        assert_eq!(server.paths().len(), MAX_REDIRECTS as usize + 1);
    }

    #[test]
    fn times_out_on_silent_or_stalled_servers() {
        let server = Server::start(vec![("/silent", Reply::Silent), ("/stall", Reply::Stall(1000))]);
        for path in ["/silent", "/stall"] {
            let started = Instant::now();
            assert_eq!(client(300).get(&server.url(path), 10_000), Err(FetchError::Timeout), "{path}");
            assert!(started.elapsed() < Duration::from_secs(2), "{path}: {:?}", started.elapsed());
        }
    }

    /// A port that refuses connections: a socket is bound to it but never
    /// listens. A released port could go to another test's server meanwhile.
    fn refusing_port() -> (std::os::fd::OwnedFd, u16) {
        use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
        let error = std::io::Error::last_os_error;
        // SAFETY: socket calls on a descriptor this function opens and owns.
        unsafe {
            let fd = libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0);
            assert!(fd >= 0, "socket: {}", error());
            let fd = OwnedFd::from_raw_fd(fd);
            let mut addr: libc::sockaddr_in = std::mem::zeroed();
            addr.sin_family = libc::AF_INET as libc::sa_family_t;
            addr.sin_addr.s_addr = u32::from(std::net::Ipv4Addr::LOCALHOST).to_be();
            let mut len = std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t;
            let ptr = &mut addr as *mut libc::sockaddr_in as *mut libc::sockaddr;
            assert_eq!(libc::bind(fd.as_raw_fd(), ptr, len), 0, "bind: {}", error());
            assert_eq!(libc::getsockname(fd.as_raw_fd(), ptr, &mut len), 0, "getsockname: {}", error());
            (fd, u16::from_be(addr.sin_port))
        }
    }

    #[test]
    fn nothing_listening_is_unreachable() {
        let (_reserved, port) = refusing_port();
        let result = client(1000).get(&format!("http://127.0.0.1:{port}/latest.json"), 100);
        assert!(matches!(result, Err(FetchError::Unreachable(_))), "{result:?}");
    }

    #[test]
    fn a_failed_handshake_is_a_tls_error() {
        // Answers the handshake with something that isn't TLS.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("https://{}/latest.json", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let _ = stream.read(&mut [0; 4096]);
                let _ = stream.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n");
            }
        });
        let result = client(2000).get(&url, 100);
        assert!(matches!(result, Err(FetchError::Tls(_))), "{result:?}");
    }

    fn downloader() -> Client {
        Client::for_downloads(Duration::from_millis(500), Duration::from_millis(500), false)
    }

    fn body() -> Vec<u8> {
        (0..200_000u32).map(|i| (i % 251) as u8).collect()
    }

    #[test]
    fn saves_and_resumes_with_a_range() {
        let data = body();
        let server = Server::start(vec![
            ("/cut", Reply::Cut(data.clone(), 70_000)),
            ("/a.tar.gz", Reply::Redirect("/download/a.tar.gz".into())),
            ("/download/a.tar.gz", Reply::Ranged(data.clone())),
        ]);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.part");
        let size = data.len() as u64;
        // The connection drops: what arrived is kept.
        let result = downloader().save(&server.url("/cut"), &path, size, &mut |_| true);
        assert!(matches!(result, Err(SaveError::Fetch(_))), "{result:?}");
        assert_eq!(std::fs::read(&path).unwrap(), data[..70_000]);
        // The next try asks only for the rest, through a redirect.
        let mut seen = Vec::new();
        downloader()
            .save(&server.url("/a.tar.gz"), &path, size, &mut |n| {
                seen.push(n);
                true
            })
            .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), data);
        assert!(seen.first().is_some_and(|n| *n > 70_000) && seen.last() == Some(&size), "{seen:?}");
        let ranges = server.ranges.lock().unwrap().clone();
        assert_eq!(
            ranges[1..],
            [
                ("/a.tar.gz".to_string(), Some("bytes=70000-".to_string())),
                ("/download/a.tar.gz".to_string(), Some("bytes=70000-".to_string())),
            ]
        );
        assert_eq!(server.request(0).accept_encoding, "identity", "ranges count the file's own bytes");
        // Already complete: no request at all.
        downloader().save(&server.url("/a.tar.gz"), &path, size, &mut |_| true).unwrap();
        assert_eq!(server.paths().len(), 3);
    }

    #[test]
    fn servers_without_ranges_send_it_all_again() {
        let data = body();
        let server = Server::start(vec![("/a", Reply::Ok(data.clone()))]);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.part");
        std::fs::write(&path, &data[..1000]).unwrap();
        downloader().save(&server.url("/a"), &path, data.len() as u64, &mut |_| true).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), data);
    }

    #[test]
    fn an_oversized_part_starts_over() {
        let data = body();
        let server = Server::start(vec![("/a", Reply::Ranged(data.clone()))]);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.part");
        std::fs::write(&path, vec![0; data.len() + 5]).unwrap();
        downloader().save(&server.url("/a"), &path, data.len() as u64, &mut |_| true).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), data);
        assert_eq!(server.ranges.lock().unwrap()[0].1, None, "no range asked for");
    }

    #[test]
    fn range_not_satisfiable_restarts() {
        let data = body();
        // The server's file is shorter than our part claims to continue.
        let server = Server::start(vec![("/a", Reply::Ranged(data[..1000].to_vec()))]);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.part");
        std::fs::write(&path, vec![9; 1000]).unwrap();
        // 1000 bytes kept of a 2000-byte file: asks for 1000-, gets 416, starts over.
        let result = downloader().save(&server.url("/a"), &path, 2000, &mut |_| true);
        assert!(matches!(result, Err(SaveError::Fetch(FetchError::Other(_)))), "{result:?}");
        let ranges: Vec<_> = server.ranges.lock().unwrap().iter().map(|(_, r)| r.clone()).collect();
        assert_eq!(ranges, [Some("bytes=1000-".to_string()), None]);
        assert_eq!(std::fs::read(&path).unwrap(), data[..1000], "the new start is kept");
    }

    #[test]
    fn cancels_between_chunks_and_keeps_the_part() {
        let data = body();
        let server = Server::start(vec![("/a", Reply::Ranged(data.clone()))]);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.part");
        let result = downloader().save(&server.url("/a"), &path, data.len() as u64, &mut |_| false);
        assert!(matches!(result, Err(SaveError::Cancelled)), "{result:?}");
        let kept = std::fs::metadata(&path).unwrap().len();
        assert!(kept > 0 && kept < data.len() as u64, "{kept}");
    }

    #[test]
    fn a_larger_body_than_expected_is_refused() {
        let data = body();
        let server = Server::start(vec![("/a", Reply::Ok(data.clone()))]);
        let dir = tempfile::tempdir().unwrap();
        let result = downloader().save(&server.url("/a"), &dir.path().join("a"), 1000, &mut |_| true);
        assert!(matches!(result, Err(SaveError::Fetch(FetchError::TooLarge(_)))), "{result:?}");
    }

    #[test]
    fn https_only_refuses_plain_http() {
        let server = Server::start(vec![("/latest.json", Reply::Ok(b"{}".to_vec()))]);
        let strict = Client::new(Duration::from_millis(500), Duration::from_secs(1), true);
        assert!(matches!(strict.get(&server.url("/latest.json"), 100), Err(FetchError::Insecure(_))));
        assert!(server.paths().is_empty());
    }
}
