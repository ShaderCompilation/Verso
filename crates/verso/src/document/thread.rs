//! The only thread that touches crengine. Results carry a book/layout generation.
use super::{
    CreDocument, CreEngine, Document, FontFace, Layout, Metadata, Paths, Position, SearchBatch, SearchStatus,
    Selection, SelectionQuery, TocEntry, Typography,
};
use crate::platform::Waker;
use anyhow::Result;
use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Engine time per search step before the queue is checked again.
const SEARCH_SLICE: Duration = Duration::from_millis(60);
/// Hitless progress replies are sent at most this often.
const SEARCH_PROGRESS: Duration = Duration::from_millis(300);

#[derive(Clone, Debug)]
pub struct Page {
    pub generation: u64,
    pub number: usize,
    pub pixels: Vec<u8>,
    pub position: Position,
    pub progress: u32,
    pub images: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelectionRequest {
    pub generation: u64,
    pub page: usize,
    pub id: u64,
}
/// A search session on one book layout; IDs are never reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchRequest {
    pub generation: u64,
    pub id: u64,
}
pub enum Command {
    Select {
        request: SelectionRequest,
        query: SelectionQuery,
    },
    /// Replaces any running search. Steps interleave with other commands.
    Search {
        request: SearchRequest,
        query: String,
    },
    CancelSearch,
    Open {
        generation: u64,
        path: PathBuf,
        layout: Layout,
        typography: Typography,
        footer: bool,
        position: Position,
    },
    Layout {
        generation: u64,
        typography: Typography,
        footer: bool,
        position: Position,
    },
    Page {
        generation: u64,
        number: usize,
    },
    /// Replies with `Jumped` carrying `jump`, whatever page holds the position.
    Position {
        generation: u64,
        position: Position,
        jump: u64,
    },
    Close,
    Shutdown,
}
pub enum Reply {
    Selection { request: SelectionRequest, result: Result<Option<Selection>, String> },
    Fonts(Vec<FontFace>),
    Opened { generation: u64, metadata: Metadata, toc: Vec<TocEntry>, pages: usize, current: usize },
    Page(Arc<Page>),
    Jumped { jump: u64, page: Arc<Page> },
    Search { request: SearchRequest, result: Result<SearchBatch, String> },
    Error { generation: u64, message: String },
}
pub struct Worker {
    waker: Waker,
    tx: mpsc::Sender<Command>,
    rx: RefCell<mpsc::Receiver<Reply>>,
    join: Option<JoinHandle<()>>,
    fonts: RefCell<Vec<FontFace>>,
}
impl Worker {
    pub fn new(paths: Paths, dpi: u32, waker: Waker) -> Self {
        let (tx, commands) = mpsc::channel();
        let (results, rx) = mpsc::channel();
        let thread_waker = waker.clone();
        let join = std::thread::Builder::new()
            .name("verso-book".into())
            .spawn(move || {
                let send = |r| {
                    let _ = results.send(r);
                    thread_waker.wake();
                };
                let engine = CreEngine::new(paths, dpi);
                let engine = match engine {
                    Ok(e) => e,
                    Err(e) => {
                        send(Reply::Error { generation: 0, message: format!("Book engine: {e:#}") });
                        return;
                    }
                };
                send(Reply::Fonts(engine.fonts()));
                let mut doc: Option<CreDocument> = None;
                let mut generation = 0;
                let mut dimensions = (0, 0);
                let mut pending = None;
                // (request, last reply, started, hits so far)
                let mut search: Option<(SearchRequest, Instant, Instant, usize)> = None;
                loop {
                    let command = match pending.take() {
                        Some(command) => command,
                        // A running search steps only while no command waits.
                        None if search.is_some() => match commands.try_recv() {
                            Ok(command) => command,
                            Err(mpsc::TryRecvError::Empty) => {
                                if let (Some((request, sent, started, found)), Some(d)) = (&mut search, &mut doc) {
                                    let result = d.search_step(SEARCH_SLICE).map_err(|e| format!("{e:#}"));
                                    let running = matches!(&result, Ok(b) if b.status == SearchStatus::Searching);
                                    let hits = result.as_ref().map_or(0, |b| b.hits.len());
                                    *found += hits;
                                    if !running {
                                        log::info!(
                                            "search finished: {} hits over {} pages in {:?}",
                                            found,
                                            d.page_count(),
                                            started.elapsed()
                                        );
                                    }
                                    if !running || hits > 0 || sent.elapsed() >= SEARCH_PROGRESS {
                                        *sent = Instant::now();
                                        send(Reply::Search { request: *request, result });
                                    }
                                    if !running {
                                        search = None;
                                    }
                                } else {
                                    search = None;
                                }
                                continue;
                            }
                            Err(mpsc::TryRecvError::Disconnected) => break,
                        },
                        None => match commands.recv() {
                            Ok(command) => command,
                            Err(_) => break,
                        },
                    };
                    // Skip intermediate layouts already queued, preserving ordering of other commands.
                    let command = coalesce(command, &commands, &mut pending);
                    if matches!(command, Command::Shutdown) {
                        break;
                    }
                    if matches!(command, Command::Open { .. } | Command::Layout { .. } | Command::Close) {
                        search = None;
                    }
                    let result: Result<()> = (|| {
                        match command {
                            Command::Search { request, query } => {
                                search = None;
                                if request.generation == generation
                                    && let Some(d) = &mut doc
                                {
                                    match d.search_start(&query) {
                                        Ok(()) => search = Some((request, Instant::now(), Instant::now(), 0)),
                                        Err(e) => send(Reply::Search { request, result: Err(format!("{e:#}")) }),
                                    }
                                }
                                return Ok(());
                            }
                            Command::CancelSearch => {
                                search = None;
                                return Ok(());
                            }
                            Command::Select { request, query } => {
                                if request.generation == generation
                                    && let Some(d) = &mut doc
                                {
                                    let result = d.select(request.page, &query).map_err(|e| format!("{e:#}"));
                                    send(Reply::Selection { request, result });
                                }
                                return Ok(());
                            }
                            Command::Open { generation: g, path, layout, typography, footer, position } => {
                                generation = g;
                                doc = None;
                                dimensions = (layout.width, layout.height);
                                let mut d = engine.open(&path, &layout, Some((&typography, footer)))?;
                                if !position.0.is_empty()
                                    && let Err(e) = d.go_to_position(&position)
                                {
                                    log::warn!("{e}");
                                }
                                doc = Some(d);
                            }
                            Command::Layout { generation: g, typography, footer, position } => {
                                generation = g;
                                if let Some(d) = &mut doc {
                                    d.go_to_position(&position)?;
                                    d.set_typography(&typography, dpi, footer)?;
                                }
                            }
                            Command::Page { generation: g, number } => {
                                if g == generation
                                    && let Some(d) = &mut doc
                                {
                                    send(Reply::Page(render(d, g, number, dimensions)?));
                                }
                                return Ok(());
                            }
                            Command::Position { generation: g, position, jump } => {
                                if g == generation
                                    && let Some(d) = &mut doc
                                {
                                    d.go_to_position(&position)?;
                                    let number = d.current_page();
                                    send(Reply::Jumped { jump, page: render(d, g, number, dimensions)? });
                                }
                                return Ok(());
                            }
                            Command::Close => {
                                doc = None;
                                return Ok(());
                            }
                            Command::Shutdown => return Ok(()),
                        }
                        if let Some(d) = &mut doc {
                            let pages = d.page_count();
                            let current = d.current_page();
                            let metadata = d.metadata();
                            log::info!(
                                "book {:?}, authors {:?}, language {:?}",
                                metadata.title,
                                metadata.authors,
                                metadata.language
                            );
                            send(Reply::Opened { generation, metadata, toc: d.toc()?, pages, current });
                            send(Reply::Page(render(d, generation, current, dimensions)?));
                        }
                        Ok(())
                    })();
                    if let Err(e) = result {
                        send(Reply::Error { generation, message: format!("{e:#}") });
                    }
                }
            })
            .expect("start book thread");
        Self { waker, tx, rx: RefCell::new(rx), join: Some(join), fonts: RefCell::new(Vec::new()) }
    }
    #[cfg(test)]
    pub fn stub() -> Self {
        Self::recording_stub().0
    }
    #[cfg(test)]
    pub fn recording_stub() -> (Self, mpsc::Receiver<Command>) {
        let (tx, commands) = mpsc::channel();
        let (_, rx) = mpsc::channel();
        (
            Self { waker: Waker::new(|| {}), tx, rx: RefCell::new(rx), join: None, fonts: RefCell::new(Vec::new()) },
            commands,
        )
    }
    pub fn waker(&self) -> Waker {
        self.waker.clone()
    }
    pub fn send(&self, c: Command) {
        if self.tx.send(c).is_err() {
            log::error!("book thread stopped");
        }
    }
    pub fn fonts(&self) -> Vec<FontFace> {
        self.fonts.borrow().clone()
    }
    pub fn drain(&self) -> Vec<Reply> {
        let replies: Vec<_> = self.rx.borrow_mut().try_iter().collect();
        for reply in &replies {
            if let Reply::Fonts(fonts) = reply {
                *self.fonts.borrow_mut() = fonts.clone();
            }
        }
        replies
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.tx.send(Command::Shutdown);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}
fn coalesce(mut c: Command, rx: &mpsc::Receiver<Command>, pending: &mut Option<Command>) -> Command {
    while matches!(c, Command::Layout { .. }) {
        match rx.try_recv() {
            Ok(next @ Command::Layout { .. }) => c = next,
            Ok(next) => {
                *pending = Some(next);
                break;
            }
            Err(_) => break,
        }
    }
    c
}
fn render(d: &mut CreDocument, generation: u64, number: usize, (w, h): (u32, u32)) -> Result<Arc<Page>> {
    let started = std::time::Instant::now();
    let last = d.page_count().saturating_sub(1);
    d.go_to_page(number.min(last));
    let mut pixels = vec![255; w as usize * h as usize];
    d.draw(&mut pixels, w as usize)?;
    let page = Page {
        generation,
        number: d.current_page(),
        pixels,
        position: d.position()?,
        progress: d.progress(),
        images: d.has_images(),
    };
    log::info!("page {} rendered in {:?}", page.number + 1, started.elapsed());
    Ok(Arc::new(page))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn layouts_coalesce_without_discarding_navigation() {
        let (tx, rx) = mpsc::channel();
        let layout = |generation| Command::Layout {
            generation,
            typography: Typography::default(),
            footer: true,
            position: Position::default(),
        };
        tx.send(layout(2)).unwrap();
        tx.send(Command::Page { generation: 2, number: 9 }).unwrap();
        let mut pending = None;
        assert!(matches!(coalesce(layout(1), &rx, &mut pending), Command::Layout { generation: 2, .. }));
        assert!(matches!(pending, Some(Command::Page { number: 9, .. })));
    }
}
