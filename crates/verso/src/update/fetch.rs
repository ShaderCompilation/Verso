//! HTTPS downloads with `ureq` and `rustls`, trusting the certificate list
//! built into Verso (`webpki-roots`) rather than the firmware's. See
//! docs/updates.md ("HTTPS client").
use std::fmt;
use std::io::ErrorKind;
use std::time::Duration;

/// Every request names the app and its version, and nothing else.
pub fn user_agent() -> String {
    format!("Verso/{}", env!("CARGO_PKG_VERSION"))
}

/// GitHub sends release downloads through one or two redirects.
const MAX_REDIRECTS: u32 = 5;

/// A configured HTTP client: one per check or download, so its timeouts
/// cover the whole job.
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

impl Client {
    /// `connect` limits each connection attempt (including DNS); `total` the
    /// whole request, body included. With `https_only`, plain `http://`
    /// addresses and redirects to them are refused.
    pub fn new(connect: Duration, total: Duration, https_only: bool) -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_connect(Some(connect))
            .timeout_global(Some(total))
            .max_redirects(MAX_REDIRECTS)
            .https_only(https_only)
            .user_agent(user_agent())
            .build()
            .into();
        Self { agent }
    }

    /// Downloads `url` into memory, following redirects, refusing bodies
    /// longer than `limit` bytes.
    pub fn get(&self, url: &str, limit: u64) -> Result<Vec<u8>, FetchError> {
        let mut response = self.agent.get(url).call().map_err(|e| classify(e, limit))?;
        response.body_mut().with_config().limit(limit).read_to_vec().map_err(|e| classify(e, limit))
    }
}

fn classify(error: ureq::Error, limit: u64) -> FetchError {
    use ureq::Error as E;
    match error {
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
        Status(u16),
        Redirect(String),
        /// Sends headers promising `len` bytes, then stalls.
        Stall(usize),
        /// Never answers.
        Silent,
    }

    /// A tiny HTTP/1.1 server on 127.0.0.1 for tests. It records each
    /// request's path and User-Agent.
    pub(crate) struct Server {
        pub base: String,
        pub requests: Arc<Mutex<Vec<(String, String)>>>,
    }

    impl Server {
        pub(crate) fn start(routes: Vec<(&str, Reply)>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let routes: Vec<(String, Reply)> = routes.into_iter().map(|(p, r)| (p.to_string(), r)).collect();
            let requests = Arc::new(Mutex::new(Vec::new()));
            let log = requests.clone();
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else { break };
                    let routes = routes.clone();
                    let log = log.clone();
                    std::thread::spawn(move || serve(stream, &routes, &log));
                }
            });
            Self { base, requests }
        }

        pub(crate) fn url(&self, path: &str) -> String {
            format!("{}{path}", self.base)
        }

        pub(crate) fn paths(&self) -> Vec<String> {
            self.requests.lock().unwrap().iter().map(|(p, _)| p.clone()).collect()
        }
    }

    fn serve(mut stream: TcpStream, routes: &[(String, Reply)], log: &Mutex<Vec<(String, String)>>) {
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        if reader.read_line(&mut line).is_err() {
            return;
        }
        let path = line.split_whitespace().nth(1).unwrap_or("").to_string();
        let mut agent = String::new();
        loop {
            let mut header = String::new();
            if reader.read_line(&mut header).unwrap_or(0) == 0 || header == "\r\n" {
                break;
            }
            if let Some((name, value)) = header.split_once(':')
                && name.eq_ignore_ascii_case("user-agent")
            {
                agent = value.trim().to_string();
            }
        }
        log.lock().unwrap().push((path.clone(), agent));
        let reply = routes.iter().find(|(p, _)| *p == path).map_or(Reply::Status(404), |(_, r)| r.clone());
        let head = |status: &str, extra: &str, len: usize| {
            format!("HTTP/1.1 {status}\r\nContent-Length: {len}\r\nConnection: close\r\n{extra}\r\n")
        };
        let _ = match reply {
            Reply::Ok(body) => {
                stream.write_all(head("200 OK", "", body.len()).as_bytes()).and_then(|()| stream.write_all(&body))
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
        let agents: Vec<String> = server.requests.lock().unwrap().iter().map(|(_, a)| a.clone()).collect();
        assert!(agents.iter().all(|a| *a == format!("Verso/{}", env!("CARGO_PKG_VERSION"))), "{agents:?}");
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

    #[test]
    fn nothing_listening_is_unreachable() {
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let result = client(1000).get(&format!("http://127.0.0.1:{port}/latest.json"), 100);
        assert!(matches!(result, Err(FetchError::Unreachable(_))), "{result:?}");
    }

    #[test]
    fn https_only_refuses_plain_http() {
        let server = Server::start(vec![("/latest.json", Reply::Ok(b"{}".to_vec()))]);
        let strict = Client::new(Duration::from_millis(500), Duration::from_secs(1), true);
        assert!(matches!(strict.get(&server.url("/latest.json"), 100), Err(FetchError::Insecure(_))));
        assert!(server.paths().is_empty());
    }
}
