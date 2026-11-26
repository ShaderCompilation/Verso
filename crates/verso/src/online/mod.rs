//! Online lookup: Wikipedia articles and translations for the lookup card.
//! Nothing here runs unless the reader asks, and nothing is kept.
//!
//! [`Service`] runs each lookup on a short-lived thread and checks Wi-Fi there
//! before opening a socket; a [`Worker`] is one tab's view of it (`worker.rs`).
//! `wikipedia.rs` and `translate.rs` know the servers' formats.
//!
//! What is looked up comes from someone's book, so it never goes into the log.
pub mod translate;
pub mod wikipedia;
mod worker;

pub use worker::{Answer, Reply, Request, Service, Worker};

use crate::net::FetchError;

/// The most characters Wikipedia's search takes; the lookup card doesn't send
/// more.
pub const QUERY_LIMIT: usize = 300;

/// The largest reply read, decompressed. The longest articles are under half
/// of this as plain text.
const MAX_REPLY_BYTES: u64 = 512 * 1024;

/// Why a lookup has no answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    WifiOff,
    /// Wi-Fi is on but hasn't joined a network.
    Connecting,
    Fetch(FetchError),
    /// The server answered with its own error code.
    Refused(String),
    /// The answer isn't in the expected format.
    Unreadable,
    /// The article is gone.
    NoArticle,
    /// More than [`QUERY_LIMIT`] characters.
    QueryTooLong,
    /// More than [`translate::TEXT_LIMIT`] characters.
    TextTooLong,
    /// The language picked by hand is the one to translate into.
    SameLanguage,
    /// The service doesn't know this language.
    Language(String),
    /// The service rejected the configured API key (HTTP 403).
    Unauthorized,
    /// The API key's monthly character quota is used up (HTTP 456).
    QuotaExceeded,
    /// Nobody waits for the answer any more. Never delivered.
    Obsolete,
}

impl From<FetchError> for Error {
    fn from(e: FetchError) -> Self {
        Error::Fetch(e)
    }
}

impl Error {
    /// The card's heading; `service` is "Wikipedia", "Google Translate" or "DeepL".
    pub fn title(&self, service: &str) -> String {
        use FetchError as F;
        match self {
            Error::WifiOff => "Wi-Fi is off".into(),
            Error::Connecting => "Wi-Fi is still connecting".into(),
            Error::Fetch(F::Tls(_)) => "Couldn’t make a secure connection".into(),
            Error::Fetch(F::Timeout) => format!("{service} took too long to answer"),
            Error::Fetch(F::Unreachable(_)) => format!("Couldn’t reach {service}"),
            Error::Fetch(F::Status(429)) => format!("{service} is busy"),
            Error::Unauthorized => format!("{service} rejected the API key"),
            Error::QuotaExceeded => format!("{service} quota exceeded"),
            Error::NoArticle => "No article found".into(),
            Error::QueryTooLong => "Too long to look up".into(),
            Error::TextTooLong => "Too long to translate".into(),
            Error::SameLanguage => "Both languages are the same".into(),
            Error::Language(_) => format!("{service} doesn’t have this language"),
            Error::Fetch(_) | Error::Refused(_) | Error::Unreadable | Error::Obsolete => {
                format!("{service} had a problem")
            }
        }
    }

    /// Under the heading: what to do about it.
    pub fn plain(&self) -> String {
        use FetchError as F;
        match self {
            Error::WifiOff => "Turn it on from the top drawer or Kindle settings, then retry.".into(),
            Error::Connecting => "Try again in a moment.".into(),
            Error::Fetch(F::Tls(_)) => "Check that the Kindle’s date and time are right, then retry.".into(),
            Error::Fetch(F::Timeout) => "The connection may be slow.".into(),
            Error::Fetch(F::Unreachable(_)) => "This network may have no internet access.".into(),
            Error::Fetch(F::Status(429)) => "Too many requests from this network. Wait a minute, then retry.".into(),
            Error::Unauthorized => {
                "Check your DeepL API key in Settings → Advanced → Translate, or paste it in Wi-Fi transfer.".into()
            }
            Error::QuotaExceeded => {
                "You’ve reached your monthly character limit. Switch to Google in Settings, or try again later.".into()
            }
            Error::Fetch(F::Status(code)) => format!("It answered with error {code}. Try again later."),
            Error::Fetch(F::TooLarge(_)) => "The answer is too large to show.".into(),
            Error::Refused(code) => format!("It answered “{code}”. Try again later."),
            Error::Fetch(_) | Error::Unreadable | Error::Obsolete => "The answer couldn’t be read.".into(),
            Error::NoArticle => "It may have been moved or deleted.".into(),
            Error::QueryTooLong => "Look up a few words at a time.".into(),
            Error::TextTooLong => "Select up to 4,000 characters at a time.".into(),
            Error::SameLanguage | Error::Language(_) => "Choose another language.".into(),
        }
    }

    /// Whether the same request may work later.
    pub fn retryable(&self) -> bool {
        match self {
            Error::WifiOff | Error::Connecting | Error::Refused(_) | Error::Unreadable => true,
            Error::Fetch(e) => !matches!(e, FetchError::TooLarge(_)),
            Error::Unauthorized | Error::QuotaExceeded => false,
            Error::NoArticle | Error::QueryTooLong | Error::TextTooLong => false,
            Error::SameLanguage | Error::Language(_) | Error::Obsolete => false,
        }
    }

    /// For the log: never an address, which holds what was looked up.
    fn kind(&self) -> String {
        match self {
            Error::Fetch(FetchError::Insecure(_)) => "not HTTPS".into(),
            Error::Fetch(FetchError::Other(_)) => "bad response".into(),
            Error::Fetch(e) => e.to_string(),
            other => format!("{other:?}"),
        }
    }
}

#[cfg(test)]
mod tests;
