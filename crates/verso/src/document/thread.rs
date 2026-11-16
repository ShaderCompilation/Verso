//! The only thread that touches crengine. Results carry a book/layout generation.
use super::{
    CreDocument, CreEngine, Document, FontFace, Layout, Metadata, Paths, Position, SearchBatch, SearchStatus,
    Selection, SelectionQuery, TocEntry, Typography,
};
use crate::annotations::Annotation;
use crate::geom::Rect;
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
    pub annotation_revision: u64,
    pub annotations: Vec<(usize, Vec<Rect>)>,
    pub painted: Vec<Paint>,
}

#[derive(Clone, Debug)]
pub struct Paint {
    pub rect: Rect,
    pub pixels: Vec<u8>,
}
impl Paint {
    /// At most 512 KiB of prepared patches per cached page. Dense full-page
    /// stress ranges fall back to idempotent composition on the drawing canvas.
    fn prepare(rects: Vec<Rect>, pixels: &[u8], width: u32, height: u32) -> Vec<Self> {
        let bounds = Rect::new(0, 0, width as i32, height as i32);
        let rects: Vec<_> = rects.into_iter().filter_map(|r| r.intersect(&bounds)).collect();
        let cached = rects.iter().map(|r| r.w as usize * r.h as usize).sum::<usize>() <= 512 * 1024;
        rects
            .into_iter()
            .map(|rect| {
                let mut patch = Vec::new();
                if cached {
                    patch.reserve(rect.w as usize * rect.h as usize);
                    for y in rect.y..rect.bottom() {
                        let row = y as usize * width as usize + rect.x as usize;
                        patch.extend(pixels[row..row + rect.w as usize].iter().map(|v| (*v).min(210)));
                    }
                }
                Self { rect, pixels: patch }
            })
            .collect()
    }
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
#[derive(Clone, Debug)]
pub struct Resolved {
    pub index: usize,
    pub page: Option<usize>,
}
struct Resolution {
    started: Instant,
    generation: u64,
    revision: u64,
    records: Vec<Annotation>,
    next: usize,
    found: Vec<Resolved>,
    preview: bool,
}
pub enum Command {
    Resolve {
        generation: u64,
        revision: u64,
        records: Vec<Annotation>,
        preview: bool,
    },
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
    Resolved { generation: u64, revision: u64, entries: Vec<Resolved>, preview: bool },
    Selection { request: SelectionRequest, result: Result<Option<Selection>, String> },
    Fonts(Vec<FontFace>),
    Opened { generation: u64, metadata: Metadata, toc: Vec<TocEntry>, pages: usize, current: usize },
    Page(Arc<Page>),
    PageFailed { generation: u64, number: usize, message: String },
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
                let mut resolution: Option<Resolution> = None;
                let mut annotations: Vec<Annotation> = vec![];
                let mut annotation_revision = 0;
                // (request, last reply, started, hits so far)
                let mut search: Option<(SearchRequest, Instant, Instant, usize)> = None;
                loop {
                    let command = match pending.take() {
                        Some(command) => command,
                        // A running search steps only while no command waits.
                        None if search.is_some() || resolution.is_some() => match commands.try_recv() {
                            Ok(command) => command,
                            Err(mpsc::TryRecvError::Empty) => {
                                if let (Some(task), Some(d)) = (&mut resolution, &mut doc) {
                                    let started = Instant::now();
                                    while task.next < task.records.len()
                                        && started.elapsed() < Duration::from_millis(12)
                                    {
                                        let i = task.next;
                                        task.next += 1;
                                        let a = &task.records[i];
                                        let page =
                                            d.validate_range(&a.range, &a.quotation).ok().flatten().and_then(|_| {
                                                if task.preview {
                                                    d.go_to_position(&a.range.start).ok().map(|_| d.current_page())
                                                } else {
                                                    d.annotation_add(i, &a.range).ok()
                                                }
                                            });
                                        task.found.push(Resolved { index: i, page });
                                    }
                                    if task.next == task.records.len() {
                                        let mut task = resolution.take().unwrap();
                                        if !task.preview {
                                            let order = d.annotation_finish(task.records.len());
                                            let mut ranks = vec![usize::MAX; task.records.len()];
                                            for (rank, index) in order.into_iter().enumerate() {
                                                ranks[index] = rank;
                                            }
                                            task.found.sort_by_key(|e| ranks[e.index]);
                                            annotation_revision = task.revision;
                                            annotations = task.records;
                                        }
                                        log::info!(
                                            "annotation resolution: count={} unresolved={} preview={} elapsed_us={}",
                                            task.found.len(),
                                            task.found.iter().filter(|e| e.page.is_none()).count(),
                                            task.preview,
                                            task.started.elapsed().as_micros()
                                        );
                                        send(Reply::Resolved {
                                            generation: task.generation,
                                            revision: task.revision,
                                            entries: task.found,
                                            preview: task.preview,
                                        });
                                    }
                                    continue;
                                }
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
                        resolution = None;
                        annotations.clear();
                        annotation_revision = 0;
                    }
                    let result: Result<()> = (|| {
                        match command {
                            Command::Resolve { generation: g, revision, records, preview } => {
                                if g == generation
                                    && let Some(d) = &mut doc
                                {
                                    if !preview {
                                        d.annotation_clear();
                                    }
                                    resolution = Some(Resolution {
                                        started: Instant::now(),
                                        generation: g,
                                        revision,
                                        records,
                                        next: 0,
                                        found: vec![],
                                        preview,
                                    });
                                }
                                return Ok(());
                            }
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
                                    match render(d, g, number, dimensions, annotation_revision, &annotations) {
                                        Ok(page) => send(Reply::Page(page)),
                                        Err(e) => {
                                            send(Reply::PageFailed { generation: g, number, message: format!("{e:#}") })
                                        }
                                    }
                                }
                                return Ok(());
                            }
                            Command::Position { generation: g, position, jump } => {
                                if g == generation
                                    && let Some(d) = &mut doc
                                {
                                    d.go_to_position(&position)?;
                                    let number = d.current_page();
                                    send(Reply::Jumped {
                                        jump,
                                        page: render(d, g, number, dimensions, annotation_revision, &annotations)?,
                                    });
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
                            send(Reply::Page(render(
                                d,
                                generation,
                                current,
                                dimensions,
                                annotation_revision,
                                &annotations,
                            )?));
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
fn render(
    d: &mut CreDocument,
    generation: u64,
    number: usize,
    (w, h): (u32, u32),
    revision: u64,
    annotations: &[Annotation],
) -> Result<Arc<Page>> {
    let started = std::time::Instant::now();
    let last = d.page_count().saturating_sub(1);
    d.go_to_page(number.min(last));
    let mut pixels = vec![255; w as usize * h as usize];
    d.draw(&mut pixels, w as usize)?;
    let current = d.current_page();
    let mut geometry = Vec::new();
    let mut geometry_cache = std::collections::HashMap::<(&str, &str), Vec<Rect>>::new();
    for index in d.annotation_page(current) {
        let Some(a) = annotations.get(index) else { continue };
        let key = (a.range.start.0.as_str(), a.range.end.0.as_str());
        let rects = if let Some(rects) = geometry_cache.get(&key) {
            rects.clone()
        } else {
            let Some(selection) = d.select(number, &SelectionQuery::Geometry(a.range.clone())).ok().flatten() else {
                continue;
            };
            geometry_cache.insert(key, selection.rects.clone());
            selection.rects
        };
        geometry.push((index, rects));
    }
    let painted = Paint::prepare(
        crate::annotations::paint_union(geometry.iter().flat_map(|(_, rects)| rects.iter().copied())),
        &pixels,
        w,
        h,
    );
    log::debug!(
        "annotation geometry: page={} revision={} ranges={} rects={}",
        current,
        revision,
        geometry.len(),
        geometry.iter().map(|(_, r)| r.len()).sum::<usize>()
    );
    let page = Page {
        generation,
        number: d.current_page(),
        pixels,
        position: d.position()?,
        progress: d.progress(),
        images: d.has_images(),
        annotation_revision: revision,
        painted,
        annotations: geometry,
    };
    log::info!("page {} rendered in {:?}", page.number + 1, started.elapsed());
    Ok(Arc::new(page))
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepared_highlights_preserve_cached_pixels_and_cap_patch_memory() {
        let mut source = vec![255; 400];
        source[6 * 20 + 6] = 0;
        let original = source.clone();
        let rects = crate::annotations::paint_union([Rect::new(0, 0, 10, 10), Rect::new(5, 5, 10, 10)].into_iter());
        let patches = Paint::prepare(rects, &source, 20, 20);
        let mut canvas = crate::gfx::Canvas::new(20, 20);
        canvas.set_gray(&source);
        for p in patches {
            assert!(!p.pixels.is_empty());
            canvas.blit_gray(p.rect, &p.pixels, p.rect.w as usize);
        }
        assert_eq!(source, original);
        for y in 0..20 {
            for x in 0..20 {
                let inside = x < 10 && y < 10 || (5..15).contains(&x) && (5..15).contains(&y);
                assert_eq!(
                    canvas.gray_data()[y * 20 + x],
                    if inside { source[y * 20 + x].min(210) } else { source[y * 20 + x] }
                );
            }
        }
        let dense = Paint::prepare(vec![Rect::new(0, 0, 1000, 1000)], &vec![255; 1_000_000], 1000, 1000);
        assert!(dense[0].pixels.is_empty(), "dense ranges don't retain a second page buffer");
    }
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
