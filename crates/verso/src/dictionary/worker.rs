//! Latest-query cancellation; preference writes remain ordered even on close.
use super::{Backend, Dictionary, Preference, discover_current, merged_preferences};
use crate::{
    platform::Waker,
    store::{Store, atomic},
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
    mpsc,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    Definition(String),
    Missing,
    Disabled,
    NoResult,
    Error(String),
}
pub struct Reply {
    pub id: u64,
    pub dictionaries: Vec<Dictionary>,
    pub selected: Option<String>,
    pub state: State,
    pub save_error: Option<String>,
}
enum Command {
    Lookup { id: u64, query: String, selected: Option<String>, rescan: bool },
    Change { id: String, up: bool },
    Cancel,
    Flush(mpsc::Sender<Result<(), String>>),
    Stop,
}
pub struct Worker {
    tx: mpsc::Sender<Command>,
    result: Arc<Mutex<Option<Reply>>>,
    latest: Arc<AtomicU64>,
}
impl Worker {
    pub fn new(store: Store, waker: Waker) -> Self {
        let (tx, commands) = mpsc::channel();
        let latest = Arc::new(AtomicU64::new(0));
        let result = Arc::new(Mutex::new(None));
        let generation = latest.clone();
        let output = result.clone();
        std::thread::Builder::new()
            .name("verso-dictionary".into())
            .spawn(move || {
                let prefs_path = store.dictionary_preferences();
                let mut preferences: Vec<Preference> = super::bounded_read(&prefs_path, 1024 * 1024)
                    .ok()
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or_default();
                let mut dictionaries = Vec::<Dictionary>::new();
                let mut backend: Option<(String, Backend)> = None;
                let mut save_error = None;
                let mut discovered = false;
                while let Ok(command) = commands.recv() {
                    match command {
                        Command::Stop => break,
                        Command::Cancel => {
                            backend = None;
                        }
                        Command::Flush(done) => {
                            if save_error.is_some() {
                                save_error = atomic(&prefs_path, &preferences).err().map(|e| format!("{e:#}"));
                            }
                            let _ = done.send(save_error.clone().map_or(Ok(()), Err));
                        }
                        Command::Change { id, up } => {
                            if let Some(i) = dictionaries.iter().position(|d| d.id == id) {
                                if up {
                                    if i > 0 {
                                        dictionaries.swap(i, i - 1);
                                    }
                                } else {
                                    dictionaries[i].disabled = !dictionaries[i].disabled;
                                }
                                // Keep preferences for temporarily removed installations too.
                                preferences = merged_preferences(&dictionaries, preferences);
                                save_error = atomic(&prefs_path, &preferences).err().map(|e| format!("{e:#}"));
                            }
                        }
                        Command::Lookup { id, query, selected, rescan } => {
                            let current = || generation.load(Ordering::Relaxed) == id;
                            if !current() {
                                continue;
                            }
                            let mut discovery_error = None;
                            if !discovered || rescan {
                                backend = None;
                                match discover_current(&store.dictionary_dir(), &preferences, &current) {
                                    Ok(found) => dictionaries = found,
                                    Err(e) => discovery_error = Some(format!("{e:#}")),
                                }
                                discovered = discovery_error.is_none();
                            }
                            let mut chosen = None;
                            let state = if let Some(e) = discovery_error {
                                State::Error(e)
                            } else if dictionaries.is_empty() {
                                State::Missing
                            } else if dictionaries.iter().all(|d| d.disabled) {
                                State::Disabled
                            } else {
                                let requested =
                                    selected.filter(|s| dictionaries.iter().any(|d| d.id == *s && !d.disabled));
                                let mut fallback = None;
                                let mut answer = None;
                                for dictionary in dictionaries
                                    .iter_mut()
                                    .filter(|d| !d.disabled && requested.as_ref().is_none_or(|s| d.id == *s))
                                {
                                    if !current() {
                                        break;
                                    }
                                    let lookup = (|| -> anyhow::Result<String> {
                                        if backend.as_ref().is_none_or(|(name, _)| name != &dictionary.id) {
                                            // Release the old index and eight-block cache before opening another.
                                            backend = None;
                                            let opened =
                                                Backend::open(&dictionary.path, &store.dictionary_cache(), &current)?;
                                            backend = Some((dictionary.id.clone(), opened));
                                        }
                                        backend.as_mut().unwrap().1.lookup(&query)
                                    })();
                                    let state = match lookup {
                                        Ok(text) => {
                                            dictionary.error = None;
                                            if text.is_empty() { State::NoResult } else { State::Definition(text) }
                                        }
                                        Err(e) => {
                                            if !current() {
                                                break;
                                            }
                                            let error = format!("{e:#}");
                                            dictionary.error = Some(error.clone());
                                            State::Error(error)
                                        }
                                    };
                                    if fallback.is_none()
                                        || matches!(&fallback, Some((_, State::Error(_)))) && state == State::NoResult
                                    {
                                        fallback = Some((dictionary.id.clone(), state.clone()));
                                    }
                                    if matches!(state, State::Definition(_)) {
                                        answer = Some((dictionary.id.clone(), state));
                                        break;
                                    }
                                }
                                if let Some((name, state)) = answer.or(fallback) {
                                    chosen = Some(name);
                                    state
                                } else {
                                    State::NoResult
                                }
                            };
                            if current() {
                                *output.lock().unwrap() = Some(Reply {
                                    id,
                                    dictionaries: dictionaries.clone(),
                                    selected: chosen,
                                    state,
                                    save_error: save_error.clone(),
                                });
                                waker.wake();
                            }
                        }
                    }
                }
            })
            .expect("spawn dictionary worker");
        Self { tx, result, latest }
    }
    pub fn lookup(&self, query: String, selected: Option<String>, rescan: bool) -> u64 {
        let id = self.latest.fetch_add(1, Ordering::Relaxed) + 1;
        let _ = self.tx.send(Command::Lookup { id, query, selected, rescan });
        id
    }
    pub fn change(&self, id: String, up: bool) {
        let _ = self.tx.send(Command::Change { id, up });
    }
    pub fn receive(&self) -> Option<Reply> {
        self.result.lock().unwrap().take()
    }
    #[cfg(test)]
    pub fn result_for_test(&self, reply: Reply) {
        *self.result.lock().unwrap() = Some(reply);
    }
    pub fn cancel(&self) {
        self.latest.fetch_add(1, Ordering::Relaxed);
        let _ = self.tx.send(Command::Cancel);
    }
    /// Close/exit barrier, after cancelling any obsolete lookup work.
    pub fn flush(&self) -> anyhow::Result<()> {
        self.cancel();
        let (tx, rx) = mpsc::channel();
        self.tx.send(Command::Flush(tx))?;
        rx.recv()?.map_err(|e| anyhow::anyhow!("Couldn't save dictionary preferences: {e}"))
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.latest.fetch_add(1, Ordering::Relaxed);
        let _ = self.tx.send(Command::Stop);
        // No UI-thread join: a slow removable-storage read may still be running.
        // Cancellation bounds further work; queued preference writes run before Stop.
    }
}
