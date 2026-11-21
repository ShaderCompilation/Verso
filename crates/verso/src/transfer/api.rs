//! The web app's requests (docs/web-app.md, "API"): the page, sign-in and
//! status. Every request must name the Kindle in its Host header; every API
//! call needs the session cookie, and every write X-Verso-Request too.

use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Instant;

use serde_json::json;
use sha2::{Digest, Sha256};

use super::auth::{self, Auth, Login};
use super::books::Books;
use super::dictionaries::Dictionaries;
use super::files::Files;
use super::http::{Body, Method, Request, Response};
use super::server::Handler;

const PAGE: &str = include_str!("page.html");

/// What the API needs from the rest of Verso; set up by the session.
pub struct Context {
    /// The server's port (80 on the Kindle).
    pub port: u16,
    /// The names a Host header may use (lowercase), read again when one
    /// doesn't match: the IP and name can change while the server runs.
    pub names: Box<dyn Fn() -> Vec<String> + Send + Sync>,
    /// The `/api/status` answer.
    pub status: Box<dyn Fn() -> serde_json::Value + Send + Sync>,
    /// Something the Kindle screen shows changed (a new code).
    pub changed: Box<dyn Fn() + Send + Sync>,
    /// Books & Highlights tab handlers; None only in minimal auth/routing unit tests.
    pub books: Option<Books>,
    /// Dictionaries tab handlers; None only in minimal unit tests.
    pub dictionaries: Option<Dictionaries>,
    /// Files tab handlers; None only in minimal unit tests.
    pub files: Option<Files>,
}

pub struct Api {
    auth: Mutex<Auth>,
    names: Mutex<Vec<String>>,
    cx: Context,
}

impl Api {
    pub fn new(auth: Auth, cx: Context) -> Self {
        let names = (cx.names)();
        Self { auth: Mutex::new(auth), names: Mutex::new(names), cx }
    }

    fn auth(&self) -> MutexGuard<'_, Auth> {
        self.auth.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The code the Kindle shows, and how often it has changed.
    pub fn code(&self) -> (String, u64) {
        let auth = self.auth();
        (auth.code().to_owned(), auth.revision())
    }

    /// Removes any uncommitted loose-file dictionary staging batches.
    pub fn cleanup_batches(&self) {
        if let Some(dicts) = &self.cx.dictionaries {
            dicts.cleanup_batches();
        }
    }

    fn host_ok(&self, request: &Request) -> bool {
        let Some(host) = request.header("host") else { return false };
        let mut names = self.names.lock().unwrap_or_else(|e| e.into_inner());
        if auth::host_allowed(host, &names, self.cx.port) {
            return true;
        }
        *names = (self.cx.names)();
        auth::host_allowed(host, &names, self.cx.port)
    }

    fn route(&self, request: &Request, body: &mut Body<'_>) -> Response {
        if !self.host_ok(request) {
            return Response::text(403, "This address doesn't name this Kindle.");
        }
        let method = request.method;
        let path = request.path.as_str();
        if !path.starts_with("/api/") {
            return match (method, path) {
                (Method::Get, "/") => page(),
                _ => Response::text(404, "Not found."),
            };
        }
        // Another site's page can't add this header without a CORS
        // preflight, which this server never grants.
        if method.writes() && request.header("x-verso-request") != Some("1") {
            return Response::text(403, "Missing X-Verso-Request.");
        }
        if (method, path) == (Method::Post, "/api/login") {
            return self.login(body);
        }
        if !self.auth().signed_in(request.cookie(auth::COOKIE)) {
            return Response::json(401, &json!({ "error": "signin" }));
        }
        match (method, path) {
            (Method::Get, "/api/status") => Response::json(200, &(self.cx.status)()),
            (Method::Get, "/api/books") => self.books().map_or_else(not_found, |b| b.list(request)),
            (Method::Get, "/api/books/check") => self.books().map_or_else(not_found, |b| b.check_upload(request)),
            (Method::Put, "/api/books") => self.books().map_or_else(not_found, |b| b.upload(request, body)),
            (Method::Post, "/api/books/mkdir") => self.books().map_or_else(not_found, |b| b.mkdir(body)),
            (Method::Get, p) if book_id_segment(p).is_some() => {
                let id = book_id_segment(p).unwrap();
                self.books().map_or_else(not_found, |b| b.download(id))
            }
            (Method::Delete, p) if book_id_segment(p).is_some() => {
                let id = book_id_segment(p).unwrap();
                self.books().map_or_else(not_found, |b| b.delete(id))
            }
            (Method::Get, "/api/highlights.zip") => self.books().map_or_else(not_found, |b| b.highlights_zip()),
            (Method::Get, p) if highlight_id_segment(p).is_some() => {
                let id = highlight_id_segment(p).unwrap();
                self.books().map_or_else(not_found, |b| b.highlights(id))
            }
            (Method::Get, "/api/dictionaries") => self.dictionaries().map_or_else(not_found, |d| d.list()),
            (Method::Get, "/api/dictionaries/check") => {
                self.dictionaries().map_or_else(not_found, |d| d.check_upload(request))
            }
            (Method::Put, "/api/dictionaries") => {
                self.dictionaries().map_or_else(not_found, |d| d.upload_archive(request, body))
            }
            (Method::Delete, "/api/dictionaries") => self.dictionaries().map_or_else(not_found, |d| d.delete(request)),
            (Method::Post, "/api/dictionaries/batch") => {
                self.dictionaries().map_or_else(not_found, |d| d.create_batch())
            }
            (Method::Put, p) if batch_id_segment(p).is_some() => {
                let id = batch_id_segment(p).unwrap();
                self.dictionaries().map_or_else(not_found, |d| d.upload_batch_file(id, request, body))
            }
            (Method::Delete, p) if batch_id_segment(p).is_some() => {
                let id = batch_id_segment(p).unwrap();
                self.dictionaries().map_or_else(not_found, |d| d.cancel_batch(id))
            }
            (Method::Post, p) if batch_commit_segment(p).is_some() => {
                let id = batch_commit_segment(p).unwrap();
                self.dictionaries().map_or_else(not_found, |d| d.commit_batch(id))
            }
            (Method::Get, "/api/files") => self.files().map_or_else(not_found, |f| f.list(request)),
            (Method::Get, "/api/files/download") => self.files().map_or_else(not_found, |f| f.download(request)),
            (Method::Get, "/api/files/info") => self.files().map_or_else(not_found, |f| f.info(request)),
            (Method::Get, "/api/files/check") => self.files().map_or_else(not_found, |f| f.check_upload(request)),
            (Method::Put, "/api/files") => self.files().map_or_else(not_found, |f| f.upload(request, body)),
            (Method::Delete, "/api/files") => self.files().map_or_else(not_found, |f| f.delete(request)),
            (Method::Post, "/api/files/mkdir") => self.files().map_or_else(not_found, |f| f.mkdir(body)),
            (Method::Post, "/api/files/rename") => self.files().map_or_else(not_found, |f| f.rename(body)),
            (Method::Post, "/api/files/move") => self.files().map_or_else(not_found, |f| f.move_to(body)),
            _ => not_found(),
        }
    }

    fn files(&self) -> Option<&Files> {
        self.cx.files.as_ref()
    }

    fn books(&self) -> Option<&Books> {
        self.cx.books.as_ref()
    }

    fn dictionaries(&self) -> Option<&Dictionaries> {
        self.cx.dictionaries.as_ref()
    }

    fn login(&self, body: &mut Body<'_>) -> Response {
        let Ok(Some(bytes)) = body.read_small(256) else {
            return Response::text(413, "Too large.");
        };
        #[derive(serde::Deserialize)]
        struct Attempt {
            code: String,
        }
        let Ok(attempt) = serde_json::from_slice::<Attempt>(&bytes) else {
            return Response::text(400, "Expected {\"code\": \"…\"}.");
        };
        let result = self.auth().login(&attempt.code, Instant::now());
        match result {
            Login::Ok(token) => Response::json(200, &json!({ "ok": true }))
                .header("Set-Cookie", format!("{}={token}; Path=/; HttpOnly; SameSite=Strict", auth::COOKIE)),
            Login::Wrong => Response::json(401, &json!({ "error": "wrong" })),
            Login::NewCode => {
                (self.cx.changed)();
                Response::json(401, &json!({ "error": "new-code" }))
            }
            Login::Throttled => Response::json(429, &json!({ "error": "busy" })).header("Retry-After", "1"),
        }
    }
}

impl Handler for Api {
    fn handle(&self, request: &Request, body: &mut Body<'_>) -> Response {
        let mut response = self.route(request, body);
        for (name, value) in
            [("X-Content-Type-Options", "nosniff"), ("Referrer-Policy", "no-referrer"), ("Cache-Control", "no-store")]
        {
            if !response.headers.iter().any(|(n, _)| *n == name) {
                response.headers.push((name, value.into()));
            }
        }
        response
    }
}

fn not_found() -> Response {
    Response::json(404, &json!({ "error": "not-found" }))
}

fn book_id_segment(path: &str) -> Option<&str> {
    let rest = path.strip_prefix("/api/books/")?;
    (!rest.is_empty() && !rest.contains('/')).then_some(rest)
}

fn highlight_id_segment(path: &str) -> Option<&str> {
    let rest = path.strip_prefix("/api/highlights/")?;
    (!rest.is_empty() && !rest.contains('/')).then_some(rest)
}

fn batch_id_segment(path: &str) -> Option<&str> {
    let rest = path.strip_prefix("/api/dictionaries/batch/")?;
    (!rest.is_empty() && !rest.contains('/')).then_some(rest)
}

fn batch_commit_segment(path: &str) -> Option<&str> {
    let rest = path.strip_prefix("/api/dictionaries/batch/")?.strip_suffix("/commit")?;
    (!rest.is_empty() && !rest.contains('/')).then_some(rest)
}

fn page() -> Response {
    Response::new(200)
        .body("text/html; charset=utf-8", PAGE.as_bytes())
        .header("Content-Security-Policy", page_policy())
        .header("X-Frame-Options", "DENY")
}

/// The page's Content-Security-Policy: nothing from anywhere but this
/// server, and only its own inline script and style, allowed by hash.
fn page_policy() -> &'static str {
    static POLICY: OnceLock<String> = OnceLock::new();
    POLICY.get_or_init(|| {
        let hash = |tag: &str| {
            let open = format!("<{tag}>");
            let start = PAGE.find(&open).expect("the page has its inline block") + open.len();
            let end = start + PAGE[start..].find(&format!("</{tag}>")).expect("the inline block ends");
            format!("'sha256-{}'", base64(&Sha256::digest(&PAGE.as_bytes()[start..end])))
        };
        format!(
            "default-src 'none'; script-src {}; style-src {}; img-src data:; connect-src 'self'; \
             base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
            hash("script"),
            hash("style")
        )
    })
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(TABLE[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transfer::server::{Server, Timeouts};
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    #[test]
    fn encodes_base64() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn the_page_makes_no_requests_elsewhere() {
        let policy = page_policy();
        assert!(policy.starts_with("default-src 'none'; script-src 'sha256-"), "{policy}");
        assert!(policy.contains("connect-src 'self'") && policy.contains("frame-ancestors 'none'"));
        for outside in ["http://", "https://", "//fonts", "src=\"/", "@import", "url("] {
            assert!(!PAGE.contains(outside), "the page refers to {outside}");
        }
        assert!(!PAGE.contains("innerHTML"), "lists are built with textContent");
    }

    struct Client {
        server: Server,
        api: Arc<Api>,
        changes: Arc<AtomicUsize>,
    }

    fn client() -> Client {
        let changes = Arc::new(AtomicUsize::new(0));
        let api = Arc::new(Api::new(
            Auth::with_code("4827".into()),
            Context {
                port: 80,
                names: Box::new(|| vec!["192.168.1.42".into(), "verso.local".into()]),
                status: Box::new(|| json!({ "name": "verso.local", "free": 7_800_000_000u64 })),
                changed: Box::new({
                    let changes = changes.clone();
                    move || {
                        changes.fetch_add(1, Ordering::SeqCst);
                    }
                }),
                books: None,
                dictionaries: None,
                files: None,
            },
        ));
        let server = Server::start("127.0.0.1:0".parse().unwrap(), api.clone(), Timeouts::default()).unwrap();
        Client { server, api, changes }
    }

    /// Sends one request on a new connection; the status, head and body.
    fn send(c: &Client, head: &str, body: &str) -> (u16, String, String) {
        let mut stream = TcpStream::connect(c.server.addr()).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let request = format!("{head}Content-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        stream.write_all(request.as_bytes()).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        let (head, body) = response.split_once("\r\n\r\n").unwrap();
        (head[9..12].parse().unwrap(), head.to_owned(), body.to_owned())
    }

    const LOGIN: &str = "POST /api/login HTTP/1.1\r\nHost: verso.local\r\nX-Verso-Request: 1\r\n";

    fn login(c: &Client, code: &str) -> (u16, String, String) {
        send(c, LOGIN, &format!("{{\"code\":\"{code}\"}}"))
    }

    fn cookie(head: &str) -> String {
        let line = head.lines().find_map(|l| l.strip_prefix("Set-Cookie: ")).unwrap();
        assert!(line.ends_with("; Path=/; HttpOnly; SameSite=Strict"), "{line}");
        line.split(';').next().unwrap().to_owned()
    }

    #[test]
    fn serves_the_page_with_its_policy() {
        let c = client();
        let (status, head, body) = send(&c, "GET / HTTP/1.1\r\nHost: verso.local\r\n", "");
        assert_eq!(status, 200);
        assert!(head.contains("Content-Type: text/html; charset=utf-8"));
        assert!(head.contains(&format!("Content-Security-Policy: {}", page_policy())));
        assert!(head.contains("X-Content-Type-Options: nosniff") && head.contains("X-Frame-Options: DENY"));
        assert!(!head.to_ascii_lowercase().contains("access-control"), "no CORS headers");
        assert_eq!(body, PAGE);
        assert_eq!(send(&c, "GET /missing HTTP/1.1\r\nHost: verso.local\r\n", "").0, 404);
    }

    #[test]
    fn refuses_other_hosts() {
        let c = client();
        for host in ["evil.example", "verso.local:8080", "192.168.1.43"] {
            let (status, ..) = send(&c, &format!("GET / HTTP/1.1\r\nHost: {host}\r\n"), "");
            assert_eq!(status, 403, "{host}");
        }
        assert_eq!(send(&c, "GET / HTTP/1.1\r\nHost: VERSO.LOCAL:80\r\n", "").0, 200);
        assert_eq!(send(&c, "GET / HTTP/1.1\r\nHost: 192.168.1.42\r\n", "").0, 200);
        // A rebinding attack's login is refused before the code is checked.
        let rebound = LOGIN.replace("verso.local", "evil.example");
        assert_eq!(send(&c, &rebound, "{\"code\":\"4827\"}").0, 403);
    }

    #[test]
    fn signs_in_and_checks_the_session_and_header() {
        let c = client();
        let status = "GET /api/status HTTP/1.1\r\nHost: verso.local\r\n";
        assert_eq!(send(&c, status, "").0, 401);
        // Without X-Verso-Request, a login is refused whatever the code.
        let bare = LOGIN.replace("X-Verso-Request: 1\r\n", "");
        assert_eq!(send(&c, &bare, "{\"code\":\"4827\"}").0, 403);
        let (code, head, _) = login(&c, "4827");
        assert_eq!(code, 200);
        let cookie = cookie(&head);
        let (code, _, body) = send(&c, &format!("{status}Cookie: {cookie}\r\n"), "");
        assert_eq!(code, 200);
        assert_eq!(serde_json::from_str::<serde_json::Value>(&body).unwrap()["name"], "verso.local");
        let forged = format!("{status}Cookie: {}=00\r\n", auth::COOKIE);
        assert_eq!(send(&c, &forged, "").0, 401);
        // Writes need the header even when signed in.
        let write = format!("DELETE /api/status HTTP/1.1\r\nHost: verso.local\r\nCookie: {cookie}\r\n");
        assert_eq!(send(&c, &write, "").0, 403);
        let write = format!("{write}X-Verso-Request: 1\r\n");
        assert_eq!(send(&c, &write, "").0, 404);
    }

    #[test]
    fn throttles_logins_and_changes_the_code() {
        let c = client();
        assert_eq!(login(&c, "0000").0, 401);
        let (status, head, _) = login(&c, "4827");
        assert_eq!(status, 429, "a second attempt within the second");
        assert!(head.contains("Retry-After: 1"));
        for _ in 0..3 {
            std::thread::sleep(auth::THROTTLE);
            let (status, _, body) = login(&c, "0000");
            assert_eq!((status, body.as_str()), (401, "{\"error\":\"wrong\"}"));
        }
        assert_eq!(c.changes.load(Ordering::SeqCst), 0);
        std::thread::sleep(auth::THROTTLE);
        let (status, _, body) = login(&c, "0000");
        assert_eq!((status, body.as_str()), (401, "{\"error\":\"new-code\"}"));
        assert_eq!(c.changes.load(Ordering::SeqCst), 1, "the Kindle is told to show the new code");
        let (code, revision) = c.api.code();
        assert_ne!(code, "4827");
        assert_eq!(revision, 1);
        std::thread::sleep(auth::THROTTLE);
        assert_eq!(login(&c, &code).0, 200);
        assert_eq!(send(&c, LOGIN, &"x".repeat(300)).0, 413);
    }
}
