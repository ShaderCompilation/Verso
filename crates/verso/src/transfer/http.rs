//! The HTTP/1.1 subset the web app needs (docs/web-app.md, "HTTP server"):
//! request heads, bodies with a Content-Length, and responses that always
//! carry one. Sockets are read and written with a short timeout so every
//! wait can notice a stop, and gives up after a while without progress.

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Request line and headers together.
pub const MAX_HEAD: usize = 8 * 1024;
pub const MAX_HEADERS: usize = 64;
/// The connection's read buffer, also the size of each body chunk.
pub const BUFFER: usize = 64 * 1024;
/// Every socket read and write wakes this often to check for a stop.
pub const SOCKET_TIMEOUT: Duration = Duration::from_secs(1);
/// A transfer that makes no progress for this long is given up.
pub const STALL: Duration = Duration::from_secs(30);
/// A request head must arrive this soon after its first byte.
const HEAD_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Put,
    Post,
    Delete,
}

impl Method {
    /// Whether the call changes something (and so needs X-Verso-Request).
    pub fn writes(self) -> bool {
        !matches!(self, Method::Get)
    }
}

#[derive(Debug)]
pub struct Request {
    pub method: Method,
    /// The path as sent, without the query.
    pub path: String,
    /// What follows `?`, as sent.
    pub query: String,
    /// Names in lowercase, in the order sent.
    pub headers: Vec<(String, String)>,
    pub content_length: u64,
    pub keep_alive: bool,
}

impl Request {
    /// The first header called `name` (lowercase).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }

    /// A decoded query parameter.
    pub fn param(&self, name: &str) -> Option<String> {
        self.query.split('&').find_map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (decode_component(key)? == name).then(|| decode_component(value)).flatten()
        })
    }

    /// A cookie's value, from any Cookie header.
    pub fn cookie(&self, name: &str) -> Option<&str> {
        self.headers.iter().filter(|(n, _)| n == "cookie").flat_map(|(_, v)| v.split(';')).find_map(|pair| {
            let (key, value) = pair.trim().split_once('=')?;
            (key == name).then_some(value)
        })
    }
}

/// Why a request head couldn't be read.
#[derive(Debug)]
pub enum HeadError {
    /// The client closed, went quiet between requests, or the server is
    /// stopping: close without answering.
    Closed,
    /// Answer with this status, then close.
    Status(u16, &'static str),
}

/// Percent-decodes a query component (`+` is a space, as
/// URLSearchParams writes it). None for a bad escape or invalid UTF-8.
pub fn decode_component(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                let hex = bytes.get(i + 1..i + 3)?;
                let hex = std::str::from_utf8(hex).ok()?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

fn is_tchar(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_\x60|~".contains(&b)
}

/// Parses a request head: the request line and headers, up to and
/// including the empty line.
pub fn parse_head(head: &[u8]) -> Result<Request, HeadError> {
    let bad = |why| HeadError::Status(400, why);
    let text = std::str::from_utf8(head).map_err(|_| bad("The request isn't valid text."))?;
    let mut lines = text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l));
    let request_line = lines.next().ok_or(bad("Empty request."))?;
    let mut parts = request_line.split(' ');
    let (Some(method), Some(target), Some(version), None) = (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(bad("Malformed request line."));
    };
    if method.is_empty() || !method.bytes().all(is_tchar) {
        return Err(bad("Malformed method."));
    }
    let method = match method {
        "GET" => Method::Get,
        "PUT" => Method::Put,
        "POST" => Method::Post,
        "DELETE" => Method::Delete,
        _ => return Err(HeadError::Status(501, "Method not supported.")),
    };
    if !target.starts_with('/') || target.bytes().any(|b| b <= b' ' || b == 0x7f) {
        return Err(bad("Malformed request target."));
    }
    let http11 = match version {
        "HTTP/1.1" => true,
        "HTTP/1.0" => false,
        v if v.starts_with("HTTP/") => return Err(HeadError::Status(505, "HTTP version not supported.")),
        _ => return Err(bad("Malformed request line.")),
    };
    let (path, query) = target.split_once('?').unwrap_or((target, ""));

    let mut headers: Vec<(String, String)> = Vec::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        if line.starts_with([' ', '\t']) {
            return Err(bad("Folded headers aren't supported."));
        }
        let (name, value) = line.split_once(':').ok_or(bad("Malformed header."))?;
        if name.is_empty() || !name.bytes().all(is_tchar) {
            return Err(bad("Malformed header name."));
        }
        let value = value.trim_matches([' ', '\t']);
        if value.bytes().any(|b| (b < b' ' && b != b'\t') || b == 0x7f) {
            return Err(bad("Malformed header value."));
        }
        if headers.len() == MAX_HEADERS {
            return Err(HeadError::Status(431, "Too many headers."));
        }
        headers.push((name.to_ascii_lowercase(), value.to_owned()));
    }

    if headers.iter().any(|(n, _)| n == "transfer-encoding") {
        return Err(HeadError::Status(411, "Send the request with a Content-Length."));
    }
    let mut content_length = None;
    for (_, value) in headers.iter().filter(|(n, _)| n == "content-length") {
        if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
            return Err(bad("Malformed Content-Length."));
        }
        let length: u64 = value.parse().map_err(|_| bad("Malformed Content-Length."))?;
        if content_length.is_some_and(|l| l != length) {
            return Err(bad("Conflicting Content-Length."));
        }
        content_length = Some(length);
    }
    let hosts = headers.iter().filter(|(n, _)| n == "host").count();
    if hosts > 1 || (http11 && hosts == 0) {
        return Err(bad("The request needs one Host header."));
    }
    let connection = |token: &str| {
        headers
            .iter()
            .filter(|(n, _)| n == "connection")
            .flat_map(|(_, v)| v.split(','))
            .any(|t| t.trim().eq_ignore_ascii_case(token))
    };
    let keep_alive = if http11 { !connection("close") } else { connection("keep-alive") };
    Ok(Request {
        method,
        path: path.to_owned(),
        query: query.to_owned(),
        headers,
        content_length: content_length.unwrap_or(0),
        keep_alive,
    })
}

/// Where the head ends: just past the empty line (CRLF or bare LF).
fn head_end(buf: &[u8]) -> Option<usize> {
    let mut i = 0;
    while let Some(nl) = buf[i..].iter().position(|&b| b == b'\n') {
        let at = i + nl + 1;
        match buf.get(at..) {
            Some([b'\n', ..]) => return Some(at + 1),
            Some([b'\r', b'\n', ..]) => return Some(at + 2),
            _ => i = at,
        }
    }
    None
}

fn timed_out(e: &io::Error) -> bool {
    matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut)
}

pub fn cancelled_error() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "the server is stopping")
}

/// One client connection: the socket, a read buffer and the server's stop flag.
pub struct Conn {
    stream: TcpStream,
    buf: Box<[u8]>,
    start: usize,
    end: usize,
    cancel: Arc<AtomicBool>,
    /// A transfer is given up after this long without progress.
    stall: Duration,
}

impl Conn {
    /// `stream` must have SOCKET_TIMEOUT as its read and write timeout.
    pub fn new(stream: TcpStream, cancel: Arc<AtomicBool>, stall: Duration) -> Self {
        Self { stream, buf: vec![0; BUFFER].into_boxed_slice(), start: 0, end: 0, cancel, stall }
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// Reads from the socket into the free end of the buffer, waiting
    /// through timeouts until `deadline`. Ok(0) at the end of the stream.
    fn fill(&mut self, deadline: Instant) -> io::Result<usize> {
        loop {
            if self.cancelled() {
                return Err(cancelled_error());
            }
            match self.stream.read(&mut self.buf[self.end..]) {
                Ok(n) => return Ok(n),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) if timed_out(&e) => {
                    if Instant::now() >= deadline {
                        return Err(io::Error::new(io::ErrorKind::TimedOut, "the client went quiet"));
                    }
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Reads the next request head. Waits up to `idle` for it to begin.
    pub fn read_head(&mut self, idle: Duration) -> Result<Request, HeadError> {
        let idle_deadline = Instant::now() + idle;
        let mut head_deadline = None;
        loop {
            // Blank lines before a request are allowed (RFC 9112 §2.2).
            while self.start < self.end && matches!(self.buf[self.start], b'\r' | b'\n') {
                self.start += 1;
            }
            if self.start < self.end {
                head_deadline.get_or_insert_with(|| Instant::now() + HEAD_TIMEOUT);
                let pending = &self.buf[self.start..self.end];
                if let Some(len) = head_end(&pending[..pending.len().min(MAX_HEAD)]) {
                    let request = parse_head(&self.buf[self.start..self.start + len]);
                    self.start += len;
                    return request;
                }
                if pending.len() >= MAX_HEAD {
                    return Err(HeadError::Status(431, "The request head is too large."));
                }
            }
            self.compact();
            let deadline = head_deadline.unwrap_or(idle_deadline);
            match self.fill(deadline) {
                Ok(0) if head_deadline.is_some() => return Err(HeadError::Status(400, "The request was cut short.")),
                Ok(0) => return Err(HeadError::Closed),
                Ok(n) => self.end += n,
                Err(e) if e.kind() == io::ErrorKind::TimedOut && head_deadline.is_some() => {
                    return Err(HeadError::Status(408, "The request took too long."));
                }
                Err(_) => return Err(HeadError::Closed),
            }
        }
    }

    /// Moves unread bytes to the front, so a whole head fits.
    fn compact(&mut self) {
        if self.start == self.end {
            (self.start, self.end) = (0, 0);
        } else if self.start > 0 && self.end == self.buf.len() {
            self.buf.copy_within(self.start..self.end, 0);
            (self.start, self.end) = (0, self.end - self.start);
        }
    }

    /// The request body: exactly `length` bytes.
    pub fn body(&mut self, length: u64) -> Body<'_> {
        Body { conn: self, remaining: length, failed: false }
    }

    /// Writes all of `data`, waiting through timeouts while the client
    /// keeps reading; fails after STALL without progress or on a stop.
    pub fn write_all(&mut self, mut data: &[u8]) -> io::Result<()> {
        let mut deadline = Instant::now() + self.stall;
        while !data.is_empty() {
            if self.cancelled() {
                return Err(cancelled_error());
            }
            match self.stream.write(data) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(n) => {
                    data = &data[n..];
                    deadline = Instant::now() + self.stall;
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) if timed_out(&e) => {
                    if Instant::now() >= deadline {
                        return Err(io::Error::new(io::ErrorKind::TimedOut, "the client stopped reading"));
                    }
                }
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// Sends a response. `keep_alive`: the connection stays open for the
    /// next request; otherwise the response says it closes.
    pub fn respond(&mut self, response: &mut Response, keep_alive: bool) -> io::Result<()> {
        let len = response.body.len();
        let mut head = format!("HTTP/1.1 {} {}\r\n", response.status, reason(response.status));
        for (name, value) in &response.headers {
            head.push_str(&format!("{name}: {value}\r\n"));
        }
        head.push_str(&format!("Content-Length: {len}\r\n"));
        if !keep_alive {
            head.push_str("Connection: close\r\n");
        }
        head.push_str("\r\n");
        let mut out = head.into_bytes();
        match &mut response.body {
            ResponseBody::Bytes(body) => {
                // One write for small responses, so the head and body go out together.
                if body.len() <= BUFFER {
                    out.extend_from_slice(body);
                    return self.write_all(&out);
                }
                self.write_all(&out)?;
                self.write_all(body)
            }
            ResponseBody::File { file, len } => {
                self.write_all(&out)?;
                let mut buf = vec![0u8; BUFFER];
                let mut remaining = *len;
                while remaining > 0 {
                    let want = remaining.min(buf.len() as u64) as usize;
                    let n = file.read(&mut buf[..want])?;
                    if n == 0 {
                        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "file shrank during download"));
                    }
                    self.write_all(&buf[..n])?;
                    remaining -= n as u64;
                }
                Ok(())
            }
        }
    }

    /// Closes after an answer that left part of a request body unread:
    /// stop sending, then read and drop what the client still sends for a
    /// moment, so its own stack doesn't reset the connection before the
    /// answer is read.
    pub fn linger(mut self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Write);
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut drained = 0usize;
        while drained < 4 * BUFFER && Instant::now() < deadline {
            (self.start, self.end) = (0, 0);
            match self.fill(deadline) {
                Ok(0) | Err(_) => break,
                Ok(n) => drained += n,
            }
        }
    }
}

/// A request body. Reads fail on a stop, at STALL without progress, and
/// if the client closes early.
pub struct Body<'a> {
    conn: &'a mut Conn,
    remaining: u64,
    /// A read failed: the connection can't be used for another request.
    failed: bool,
}

impl Body<'_> {
    pub fn failed(&self) -> bool {
        self.failed
    }

    /// Bytes not read yet.
    pub fn remaining(&self) -> u64 {
        self.remaining
    }

    /// Reads the whole body into memory, if it's at most `limit` bytes.
    pub fn read_small(&mut self, limit: usize) -> io::Result<Option<Vec<u8>>> {
        if self.remaining > limit as u64 {
            return Ok(None);
        }
        let mut out = Vec::with_capacity(self.remaining as usize);
        self.read_to_end(&mut out)?;
        Ok(Some(out))
    }

    /// Reads and drops what's left, if it's at most `limit` bytes.
    pub fn skip(&mut self, limit: u64) -> io::Result<bool> {
        if self.remaining > limit {
            return Ok(false);
        }
        io::copy(self, &mut io::sink())?;
        Ok(true)
    }
}

impl Read for Body<'_> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.remaining == 0 || out.is_empty() {
            return Ok(0);
        }
        let conn = &mut *self.conn;
        if conn.start == conn.end {
            (conn.start, conn.end) = (0, 0);
            let read = match conn.fill(Instant::now() + conn.stall) {
                Ok(0) => Err(io::Error::new(io::ErrorKind::UnexpectedEof, "the client closed mid-request")),
                other => other,
            };
            match read {
                Ok(n) => conn.end = n,
                Err(e) => {
                    self.failed = true;
                    return Err(e);
                }
            }
        }
        let n = out.len().min(conn.end - conn.start).min(self.remaining.min(usize::MAX as u64) as usize);
        out[..n].copy_from_slice(&conn.buf[conn.start..conn.start + n]);
        conn.start += n;
        self.remaining -= n as u64;
        Ok(n)
    }
}

/// A response body: small payloads in memory, or a file streamed in 64 KB
/// chunks so downloads keep RSS flat.
#[derive(Debug)]
pub enum ResponseBody {
    Bytes(Vec<u8>),
    File { file: std::fs::File, len: u64 },
}

impl ResponseBody {
    pub fn len(&self) -> u64 {
        match self {
            Self::Bytes(b) => b.len() as u64,
            Self::File { len, .. } => *len,
        }
    }
}

/// A complete response; the server adds Content-Length and Connection.
#[derive(Debug)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(&'static str, String)>,
    pub body: ResponseBody,
}

impl Response {
    pub fn new(status: u16) -> Self {
        Self { status, headers: Vec::new(), body: ResponseBody::Bytes(Vec::new()) }
    }

    pub fn header(mut self, name: &'static str, value: impl Into<String>) -> Self {
        self.headers.push((name, value.into()));
        self
    }

    pub fn body(mut self, content_type: &str, body: impl Into<Vec<u8>>) -> Self {
        self.body = ResponseBody::Bytes(body.into());
        self.header("Content-Type", content_type)
    }

    /// Streams `file` (`len` bytes) as a safe attachment download.
    pub fn file(file: std::fs::File, len: u64, download_name: &str) -> Self {
        Self { status: 200, headers: download_headers(download_name), body: ResponseBody::File { file, len } }
    }

    /// A short plain-text answer, used for errors.
    pub fn text(status: u16, text: &str) -> Self {
        Self::new(status).body("text/plain; charset=utf-8", text.as_bytes())
    }

    pub fn json(status: u16, value: &serde_json::Value) -> Self {
        Self::new(status).body("application/json", value.to_string())
    }
}

pub fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        409 => "Conflict",
        411 => "Length Required",
        413 => "Content Too Large",
        421 => "Misdirected Request",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        503 => "Service Unavailable",
        505 => "HTTP Version Not Supported",
        507 => "Insufficient Storage",
        _ => "Unknown",
    }
}

/// The headers every download carries (books, highlights, files): saved,
/// never shown under the page's origin, whatever the file holds.
pub fn download_headers(name: &str) -> Vec<(&'static str, String)> {
    let ascii: String = name
        .chars()
        .map(|c| if c == ' ' || (c.is_ascii_graphic() && !matches!(c, '"' | '\\' | '%' | ';')) { c } else { '_' })
        .collect();
    let mut encoded = String::new();
    for b in name.bytes() {
        if b.is_ascii_alphanumeric() || b"!#$&+-.^_\x60|~".contains(&b) {
            encoded.push(b as char);
        } else {
            encoded.push_str(&format!("%{b:02X}"));
        }
    }
    vec![
        ("Content-Type", "application/octet-stream".into()),
        ("Content-Disposition", format!("attachment; filename=\"{ascii}\"; filename*=UTF-8''{encoded}")),
        ("X-Content-Type-Options", "nosniff".into()),
        ("Content-Security-Policy", "sandbox".into()),
        ("Cache-Control", "no-store".into()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(head: &str) -> Result<Request, HeadError> {
        parse_head(head.as_bytes())
    }

    fn status(head: &str) -> u16 {
        match parse(head) {
            Err(HeadError::Status(status, _)) => status,
            other => panic!("{head:?}: expected an error status, got {other:?}"),
        }
    }

    #[test]
    fn parses_a_request() {
        let r = parse(
            "PUT /api/books?folder=%2Fmnt%2Fus%2Fdocuments&name=Anna+Karenina.epub HTTP/1.1\r\n\
             Host: verso.local\r\nContent-Length: 12\r\nCookie: a=1; verso_session=abc\r\n\
             X-Verso-Request: 1\r\n\r\n",
        )
        .unwrap();
        assert_eq!(r.method, Method::Put);
        assert_eq!(r.path, "/api/books");
        assert_eq!(r.param("folder").as_deref(), Some("/mnt/us/documents"));
        assert_eq!(r.param("name").as_deref(), Some("Anna Karenina.epub"));
        assert_eq!(r.param("missing"), None);
        assert_eq!(r.content_length, 12);
        assert_eq!(r.header("host"), Some("verso.local"));
        assert_eq!(r.header("x-verso-request"), Some("1"));
        assert_eq!(r.cookie("verso_session"), Some("abc"));
        assert!(r.keep_alive);
    }

    #[test]
    fn keep_alive_follows_the_version_and_connection_header() {
        assert!(!parse("GET / HTTP/1.1\r\nHost: a\r\nConnection: close\r\n\r\n").unwrap().keep_alive);
        assert!(!parse("GET / HTTP/1.0\r\n\r\n").unwrap().keep_alive);
        assert!(parse("GET / HTTP/1.0\r\nConnection: Keep-Alive\r\n\r\n").unwrap().keep_alive);
        // Bare LF line ends are read too.
        assert!(parse("GET / HTTP/1.1\nHost: a\n\n").unwrap().keep_alive);
    }

    #[test]
    fn refuses_malformed_heads() {
        assert_eq!(status("GET /\r\n\r\n"), 400);
        assert_eq!(status("GET  / HTTP/1.1\r\nHost: a\r\n\r\n"), 400);
        assert_eq!(status("GET http://evil/ HTTP/1.1\r\nHost: a\r\n\r\n"), 400);
        assert_eq!(status("GET * HTTP/1.1\r\nHost: a\r\n\r\n"), 400);
        assert_eq!(status("GET / FTP/1.1\r\nHost: a\r\n\r\n"), 400);
        assert_eq!(status("GET / HTTP/2.0\r\nHost: a\r\n\r\n"), 505);
        assert_eq!(status("PATCH / HTTP/1.1\r\nHost: a\r\n\r\n"), 501);
        assert_eq!(status("G(T / HTTP/1.1\r\nHost: a\r\n\r\n"), 400);
        assert_eq!(status("GET / HTTP/1.1\r\n\r\n"), 400, "no Host");
        assert_eq!(status("GET / HTTP/1.1\r\nHost: a\r\nHost: b\r\n\r\n"), 400);
        assert_eq!(status("GET / HTTP/1.1\r\nHost: a\r\nNo colon\r\n\r\n"), 400);
        assert_eq!(status("GET / HTTP/1.1\r\nHost: a\r\nBad name: 1\r\n\r\n"), 400);
        assert_eq!(status("GET / HTTP/1.1\r\nHost: a\r\nX: 1\r\n folded\r\n\r\n"), 400);
        assert_eq!(status("GET / HTTP/1.1\r\nHost: a\r\nX: a\x01b\r\n\r\n"), 400);
        assert_eq!(status("GET /\x7f HTTP/1.1\r\nHost: a\r\n\r\n"), 400);
        assert!(matches!(parse_head(b"GET /\xff HTTP/1.1\r\nHost: a\r\n\r\n"), Err(HeadError::Status(400, _))));
    }

    #[test]
    fn checks_the_body_length() {
        assert_eq!(status("PUT / HTTP/1.1\r\nHost: a\r\nTransfer-Encoding: chunked\r\n\r\n"), 411);
        assert_eq!(status("PUT / HTTP/1.1\r\nHost: a\r\nContent-Length: 5\r\nTransfer-Encoding: x\r\n\r\n"), 411);
        assert_eq!(status("PUT / HTTP/1.1\r\nHost: a\r\nContent-Length: -1\r\n\r\n"), 400);
        assert_eq!(status("PUT / HTTP/1.1\r\nHost: a\r\nContent-Length: 1e3\r\n\r\n"), 400);
        assert_eq!(status("PUT / HTTP/1.1\r\nHost: a\r\nContent-Length: 99999999999999999999\r\n\r\n"), 400);
        assert_eq!(status("PUT / HTTP/1.1\r\nHost: a\r\nContent-Length: 1\r\nContent-Length: 2\r\n\r\n"), 400);
        let same = parse("PUT / HTTP/1.1\r\nHost: a\r\nContent-Length: 2\r\nContent-Length: 2\r\n\r\n").unwrap();
        assert_eq!(same.content_length, 2);
        assert_eq!(parse("GET / HTTP/1.1\r\nHost: a\r\n\r\n").unwrap().content_length, 0);
    }

    #[test]
    fn limits_headers() {
        let mut head = String::from("GET / HTTP/1.1\r\nHost: a\r\n");
        for i in 0..MAX_HEADERS - 1 {
            head.push_str(&format!("X-{i}: 1\r\n"));
        }
        assert!(parse(&format!("{head}\r\n")).is_ok());
        assert_eq!(status(&format!("{head}X-last: 1\r\n\r\n")), 431);
    }

    #[test]
    fn decodes_components() {
        assert_eq!(decode_component("a%20b+c%2B").as_deref(), Some("a b c+"));
        assert_eq!(decode_component("%D0%90").as_deref(), Some("А"));
        assert_eq!(decode_component("%"), None);
        assert_eq!(decode_component("%zz"), None);
        assert_eq!(decode_component("%ff"), None, "invalid UTF-8");
    }

    #[test]
    fn finds_the_end_of_a_head() {
        assert_eq!(head_end(b"GET / HTTP/1.1\r\nHost: a\r\n\r\nbody"), Some(27));
        assert_eq!(head_end(b"GET / HTTP/1.1\nHost: a\n\nbody"), Some(24));
        assert_eq!(head_end(b"GET / HTTP/1.1\r\nHost: a\r\n"), None);
    }

    #[test]
    fn download_headers_always_save_the_file() {
        let headers = download_headers("Анна \"K\"; <script>.html");
        let get = |name| headers.iter().find(|(n, _)| *n == name).map(|(_, v)| v.as_str()).unwrap();
        assert_eq!(get("Content-Type"), "application/octet-stream");
        assert_eq!(get("X-Content-Type-Options"), "nosniff");
        assert_eq!(get("Content-Security-Policy"), "sandbox");
        let disposition = get("Content-Disposition");
        assert!(disposition.starts_with("attachment; filename=\"____ _K__ <script>.html\"; filename*=UTF-8''"));
        assert!(disposition.ends_with("%D0%90%D0%BD%D0%BD%D0%B0%20%22K%22%3B%20%3Cscript%3E.html"), "{disposition}");
    }
}
