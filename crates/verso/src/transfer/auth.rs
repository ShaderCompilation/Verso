//! Who may use the web app a 4-digit code shown on the Kindle, a session cookie for a
//! browser that entered it, and the Host check against DNS rebinding.

use std::io::Read;
use std::time::{Duration, Instant};

/// The session cookie's name.
pub const COOKIE: &str = "verso_session";
/// Wrong codes in a row before the Kindle shows a new one.
pub const ATTEMPTS: u32 = 5;
/// At most one login attempt per this long, from anyone.
pub const THROTTLE: Duration = Duration::from_secs(1);
/// Browsers signed in at once; the oldest is forgotten beyond this.
const SESSIONS: usize = 32;

#[derive(Debug, PartialEq, Eq)]
pub enum Login {
    /// Signed in: the session token for the cookie.
    Ok(String),
    Wrong,
    /// Too many wrong codes: the Kindle shows a new one now.
    NewCode,
    /// Another attempt came less than THROTTLE ago.
    Throttled,
}

pub struct Auth {
    code: String,
    failures: u32,
    last_attempt: Option<Instant>,
    sessions: Vec<String>,
    /// Counts code changes, so the screen knows to redraw it.
    revision: u64,
}

impl Auth {
    pub fn with_code(code: String) -> Self {
        Self { code, failures: 0, last_attempt: None, sessions: Vec::new(), revision: 0 }
    }

    pub fn code(&self) -> &str {
        &self.code
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn login(&mut self, attempt: &str, now: Instant) -> Login {
        if self.last_attempt.is_some_and(|last| now.saturating_duration_since(last) < THROTTLE) {
            return Login::Throttled;
        }
        self.last_attempt = Some(now);
        if same(attempt.trim().as_bytes(), self.code.as_bytes()) {
            self.failures = 0;
            let token = random_hex(32);
            if self.sessions.len() == SESSIONS {
                self.sessions.remove(0);
            }
            self.sessions.push(token.clone());
            return Login::Ok(token);
        }
        self.failures += 1;
        if self.failures < ATTEMPTS {
            return Login::Wrong;
        }
        // Browsers already signed in stay signed in.
        self.failures = 0;
        let old = std::mem::take(&mut self.code);
        self.code = loop {
            let code = random_code();
            if code != old {
                break code;
            }
        };
        self.revision += 1;
        log::info!("web app: {ATTEMPTS} wrong codes; showing a new one");
        Login::NewCode
    }

    pub fn signed_in(&self, token: Option<&str>) -> bool {
        token.is_some_and(|token| self.sessions.iter().any(|s| same(s.as_bytes(), token.as_bytes())))
    }
}

/// Compares without stopping at the first difference.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |diff, (x, y)| diff | (x ^ y)) == 0
}

fn random_bytes(out: &mut [u8]) {
    let read = std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(out));
    // /dev/urandom is always there on Linux; never hand out a guessable code.
    read.expect("reading /dev/urandom");
}

/// Four digits, each equally likely.
pub fn random_code() -> String {
    loop {
        let mut bytes = [0u8; 2];
        random_bytes(&mut bytes);
        let n = u16::from_le_bytes(bytes);
        if n < 60_000 {
            return format!("{:04}", n % 10_000);
        }
    }
}

fn random_hex(len: usize) -> String {
    let mut bytes = vec![0u8; len];
    random_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Whether a Host header names this server: one of `names` (lowercase:
/// the IP and the advertised name), any case, with an optional trailing
/// dot, and the server's port, which may be left out when it's 80.
pub fn host_allowed(host: &str, names: &[String], port: u16) -> bool {
    if host.starts_with('[') {
        return false; // IPv6 literals: the server only listens on IPv4.
    }
    let (name, given) = match host.rsplit_once(':') {
        Some((name, p)) => {
            // Plain digits only: no sign, no leading zero.
            if p.is_empty() || p.starts_with('0') || !p.bytes().all(|b| b.is_ascii_digit()) {
                return false;
            }
            match p.parse::<u16>() {
                Ok(p) => (name, Some(p)),
                Err(_) => return false,
            }
        }
        None => (host, None),
    };
    if given.unwrap_or(80) != port {
        return false;
    }
    let name = name.strip_suffix('.').unwrap_or(name);
    !name.is_empty() && names.iter().any(|n| n.eq_ignore_ascii_case(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signs_in_with_the_code_and_throttles_attempts() {
        let start = Instant::now();
        let mut auth = Auth::with_code("4827".into());
        assert!(!auth.signed_in(None));
        assert_eq!(auth.login("1111", start), Login::Wrong);
        assert_eq!(auth.login("4827", start + Duration::from_millis(500)), Login::Throttled);
        let Login::Ok(token) = auth.login(" 4827 ", start + Duration::from_secs(1)) else { panic!() };
        assert_eq!(token.len(), 64);
        assert!(auth.signed_in(Some(&token)));
        assert!(!auth.signed_in(Some("0".repeat(64).as_str())));
        assert!(!auth.signed_in(Some(&token[..63])));
        let Login::Ok(second) = auth.login("4827", start + Duration::from_secs(2)) else { panic!() };
        assert_ne!(token, second);
        assert!(auth.signed_in(Some(&token)) && auth.signed_in(Some(&second)));
    }

    #[test]
    fn five_wrong_codes_bring_a_new_one_and_keep_sessions() {
        let start = Instant::now();
        let mut auth = Auth::with_code("4827".into());
        let Login::Ok(token) = auth.login("4827", start) else { panic!() };
        let at = |s| start + Duration::from_secs(s);
        for s in 1..ATTEMPTS as u64 {
            assert_eq!(auth.login("0000", at(s)), Login::Wrong);
        }
        // A throttled try doesn't count.
        assert_eq!(auth.login("0000", at(4) + Duration::from_millis(10)), Login::Throttled);
        assert_eq!(auth.login("0000", at(5)), Login::NewCode);
        assert_ne!(auth.code(), "4827");
        assert_eq!(auth.revision(), 1);
        assert_eq!(auth.login("4827", at(6)), Login::Wrong, "the old code no longer works");
        assert!(auth.signed_in(Some(&token)));
        let code = auth.code().to_owned();
        assert!(matches!(auth.login(&code, at(7)), Login::Ok(_)));
    }

    #[test]
    fn codes_are_four_digits() {
        for _ in 0..200 {
            let code = random_code();
            assert!(code.len() == 4 && code.bytes().all(|b| b.is_ascii_digit()), "{code}");
        }
    }

    #[test]
    fn checks_the_host() {
        let names = vec!["192.168.1.42".to_owned(), "verso-2.local".to_owned()];
        for ok in
            ["192.168.1.42", "192.168.1.42:80", "verso-2.local", "VERSO-2.Local", "verso-2.local.", "verso-2.local:80"]
        {
            assert!(host_allowed(ok, &names, 80), "{ok}");
        }
        for bad in [
            "",
            ":80",
            "verso.local",
            "evil.example",
            "192.168.1.42:8080",
            "192.168.1.42:",
            "192.168.1.42:080",
            "192.168.1.42:+80",
            "verso-2.local:80:80",
            "[::1]",
            "192.168.1.43",
            "192.168.1.42.evil.example",
        ] {
            assert!(!host_allowed(bad, &names, 80), "{bad}");
        }
        let local = vec!["127.0.0.1".to_owned(), "localhost".to_owned()];
        assert!(host_allowed("localhost:8080", &local, 8080));
        assert!(!host_allowed("localhost", &local, 8080), "the simulator's port must be given");
    }
}
