//! Where lookups run. A request can't be aborted once it's in a socket, so one
//! thread would make a retry wait behind a hung connection; each request gets
//! its own short-lived thread instead, at most [`MAX_THREADS`] at a time. A
//! request beyond that waits for one to end, unless it's obsolete by then.
use std::collections::VecDeque;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use super::Error;
use super::translate::{self, DeepL, GoogleGtx, Provider, Translation, Translator};
use super::wikipedia::{Article, Edition, Found, Wikipedia};
use crate::net::{self, Client, FetchError};
use crate::platform::Waker;
use crate::platform::wifi::{self, Link};

/// Bounds sockets and memory.
const MAX_THREADS: usize = 3;
/// Per request; looking up an article takes two.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
const TOTAL_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone, PartialEq, Eq)]
pub enum Request {
    /// The best article for a query, or the hits to choose from.
    Wikipedia { edition: Edition, query: String },
    /// The article with a hit's title.
    Article { edition: Edition, title: String },
    /// `source`: None to detect it.
    Translate { text: String, source: Option<String>, target: String, provider: Provider, api_key: Option<String> },
}

impl fmt::Debug for Request {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Request::Wikipedia { edition, query } => {
                f.debug_struct("Wikipedia").field("edition", edition).field("query", query).finish()
            }
            Request::Article { edition, title } => {
                f.debug_struct("Article").field("edition", edition).field("title", title).finish()
            }
            Request::Translate { text, source, target, provider, api_key } => f
                .debug_struct("Translate")
                .field("text", text)
                .field("source", source)
                .field("target", target)
                .field("provider", provider)
                .field("api_key", &api_key.as_ref().map(|_| "<redacted>"))
                .finish(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    Found(Found),
    Article(Article),
    Translation(Translation),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reply {
    /// The request's ID, from [`Worker::request`].
    pub id: u64,
    pub result: Result<Answer, Error>,
}

/// Where the services are.
struct Endpoints {
    /// `{lang}` becomes the edition's code.
    wikipedia: String,
    translate: String,
    /// Optional local-server override for DeepL (`{tier}` → `free`/`pro`).
    deepl: Option<String>,
}

impl Endpoints {
    /// The public services, or `VERSO_WIKIPEDIA_URL`, `VERSO_TRANSLATE_URL` and
    /// `VERSO_DEEPL_URL` (`{tier}` → `free`/`pro`) for a local test server.
    fn configured() -> Self {
        let or = |name: &str, default: &str| {
            std::env::var(name).ok().filter(|u| !u.trim().is_empty()).unwrap_or_else(|| default.to_owned())
        };
        let deepl = std::env::var("VERSO_DEEPL_URL").ok().filter(|u| !u.trim().is_empty());
        Self {
            wikipedia: or("VERSO_WIKIPEDIA_URL", "https://{lang}.wikipedia.org/w/api.php"),
            translate: or("VERSO_TRANSLATE_URL", "https://translate.googleapis.com/translate_a/single"),
            deepl,
        }
    }

    fn deepl_url(&self, api_key: &str) -> String {
        match &self.deepl {
            Some(template) => template.replace("{tier}", translate::deepl_tier(api_key).as_str()),
            None => translate::deepl_endpoint(api_key).to_owned(),
        }
    }
}

/// Wikimedia blocks clients that don't say how to reach their authors.
fn wikipedia_user_agent() -> String {
    format!("{} ({})", net::user_agent(), env!("CARGO_PKG_REPOSITORY"))
}

type ReadLink = dyn Fn() -> anyhow::Result<Link> + Send + Sync;

/// Runs lookups for every [`Worker`].
pub struct Service {
    endpoints: Endpoints,
    connect: Duration,
    total: Duration,
    link: Box<ReadLink>,
    queue: Mutex<Queue>,
}

#[derive(Default)]
struct Queue {
    threads: usize,
    waiting: VecDeque<Job>,
}

struct Job {
    id: u64,
    request: Request,
    mailbox: Arc<Mailbox>,
}

struct Mailbox {
    /// One lock for both, so an obsolete reply can't slip in.
    inner: Mutex<Inbox>,
    waker: Waker,
}

#[derive(Default)]
struct Inbox {
    latest: u64,
    reply: Option<Reply>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

impl Job {
    fn is_current(&self) -> bool {
        lock(&self.mailbox.inner).latest == self.id
    }

    fn finish(&self, result: Result<Answer, Error>) {
        {
            let mut inbox = lock(&self.mailbox.inner);
            if inbox.latest != self.id {
                return;
            }
            inbox.reply = Some(Reply { id: self.id, result });
        }
        self.mailbox.waker.wake();
    }
}

impl Service {
    pub fn new() -> Arc<Self> {
        Self::with(Endpoints::configured(), CONNECT_TIMEOUT, TOTAL_TIMEOUT, Box::new(wifi::link))
    }

    fn with(endpoints: Endpoints, connect: Duration, total: Duration, link: Box<ReadLink>) -> Arc<Self> {
        Arc::new(Self { endpoints, connect, total, link, queue: Mutex::default() })
    }

    fn submit(self: &Arc<Self>, job: Job) {
        let mut queue = lock(&self.queue);
        // Each worker waits for one request at most, so this stays short.
        queue.waiting.retain(Job::is_current);
        queue.waiting.push_back(job);
        if queue.threads == MAX_THREADS {
            return;
        }
        queue.threads += 1;
        let this = self.clone();
        let spawned = std::thread::Builder::new().name("online-lookup".into()).spawn(move || this.run());
        if let Err(e) = spawned {
            log::error!("starting an online lookup: {e}");
            queue.threads -= 1;
            // Other threads, if there are any, get to it.
            if queue.threads == 0 {
                let jobs = std::mem::take(&mut queue.waiting);
                drop(queue);
                for job in jobs {
                    job.finish(Err(Error::Fetch(FetchError::Other("no thread".into()))));
                }
            }
        }
    }

    fn run(&self) {
        loop {
            let job = {
                let mut queue = lock(&self.queue);
                queue.waiting.retain(Job::is_current);
                let Some(job) = queue.waiting.pop_front() else {
                    // Under the same lock, so a new job sees a free thread.
                    queue.threads -= 1;
                    return;
                };
                job
            };
            let started = Instant::now();
            let result = self.answer(&job);
            let what = match job.request {
                Request::Wikipedia { .. } => "Wikipedia lookup",
                Request::Article { .. } => "Wikipedia article",
                Request::Translate { .. } => "translation",
            };
            match &result {
                Ok(_) => log::info!("online: {what} in {:?}", started.elapsed()),
                Err(e) => log::info!("online: {what}: {} after {:?}", e.kind(), started.elapsed()),
            }
            job.finish(result);
        }
    }

    fn answer(&self, job: &Job) -> Result<Answer, Error> {
        if let Request::Translate { text, source, target, .. } = &job.request {
            translate::check(text, source.as_deref(), target)?;
        }
        match (self.link)() {
            Ok(Link::Off) => return Err(Error::WifiOff),
            Ok(Link::Connecting) => return Err(Error::Connecting),
            Ok(Link::Connected) => {}
            Err(e) => log::warn!("online: reading Wi-Fi state: {e:#}"),
        }
        // Reading the link takes a moment on the Kindle.
        if !job.is_current() {
            return Err(Error::Obsolete);
        }
        match &job.request {
            Request::Wikipedia { edition, query } => {
                self.wikipedia(*edition, |w| w.lookup(query, &|| job.is_current())).map(Answer::Found)
            }
            Request::Article { edition, title } => self.wikipedia(*edition, |w| w.article(title)).map(Answer::Article),
            Request::Translate { text, source, target, provider, api_key } => {
                match translate::effective_provider(*provider, api_key.as_deref(), source.as_deref(), target) {
                    Provider::DeepL => {
                        let key = api_key.as_deref().unwrap_or("").trim();
                        let url = self.endpoints.deepl_url(key);
                        let client = self.client(&url, &net::user_agent());
                        let translator = DeepL { client: &client, url: &url, api_key: key };
                        translator.translate(text, source.as_deref(), target).map(Answer::Translation)
                    }
                    Provider::Google => {
                        let url = &self.endpoints.translate;
                        let client = self.client(url, &net::user_agent());
                        let translator = GoogleGtx { client: &client, url };
                        translator.translate(text, source.as_deref(), target).map(Answer::Translation)
                    }
                }
            }
        }
    }

    fn wikipedia<T>(&self, edition: Edition, f: impl FnOnce(&Wikipedia) -> Result<T, Error>) -> Result<T, Error> {
        let url = self.endpoints.wikipedia.replace("{lang}", edition.code());
        let client = self.client(&url, &wikipedia_user_agent());
        f(&Wikipedia { client: &client, url: &url, edition })
    }

    /// Plain HTTP only for a local test server someone configured.
    fn client(&self, url: &str, user_agent: &str) -> Client {
        Client::named(user_agent, self.connect, self.total, !url.starts_with("http://"))
    }
}

/// One tab's requests. Only the newest one's reply arrives; [`cancel`] (or
/// dropping the worker) forgets that too, though its thread may run on until
/// its timeout.
///
/// [`cancel`]: Worker::cancel
pub struct Worker {
    service: Arc<Service>,
    mailbox: Arc<Mailbox>,
}

impl Worker {
    /// `waker` is called when a reply is ready to [`receive`].
    ///
    /// [`receive`]: Worker::receive
    pub fn new(service: Arc<Service>, waker: Waker) -> Self {
        Self { service, mailbox: Arc::new(Mailbox { inner: Mutex::default(), waker }) }
    }

    /// Starts a request, replacing the one before. Returns its ID; IDs only
    /// grow.
    pub fn request(&self, request: Request) -> u64 {
        let id = self.advance();
        self.service.submit(Job { id, request, mailbox: self.mailbox.clone() });
        id
    }

    /// The newest request's reply, once.
    pub fn receive(&self) -> Option<Reply> {
        lock(&self.mailbox.inner).reply.take()
    }

    /// Drops whatever was asked for.
    pub fn cancel(&self) {
        self.advance();
    }

    fn advance(&self) -> u64 {
        let mut inbox = lock(&self.mailbox.inner);
        inbox.latest += 1;
        inbox.reply = None;
        inbox.latest
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[cfg(test)]
impl Service {
    /// A service that talks to `base` (`/{lang}/api`, `/translate` and
    /// `/deepl/{free|pro}`).
    pub(crate) fn for_test(base: &str, total: Duration, link: Box<ReadLink>) -> Arc<Self> {
        let endpoints = Endpoints {
            wikipedia: format!("{base}/{{lang}}/api"),
            translate: format!("{base}/translate"),
            deepl: Some(format!("{base}/deepl/{{tier}}")),
        };
        Self::with(endpoints, Duration::from_millis(500), total, link)
    }

    pub(crate) fn threads(&self) -> usize {
        lock(&self.queue).threads
    }
}
