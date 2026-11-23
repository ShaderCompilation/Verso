//! The only thread that touches crengine. Results carry a book/layout generation.
//!
//! Startup waits for `Command::Start` (sent after the first frame) before
//! registering fonts. Rendered pages reuse buffers from a bounded `PixelPool`
//! (at most 4 buffers / 32 MiB) and closing a book drops pooled buffers and
//! calls `malloc_trim(0)` so glibc returns freed arenas to the OS. Before any
//! book open, `prelayout.before_open` kills any idle pre-layout child and
//! reloads crengine's cache index. Search and annotation resolution run in
//! bounded slices so user turns stay responsive. Checked annotations survive a
//! relayout (crengine keeps them as DOM positions): pages keep painting them,
//! and resolving the same records again only finds their new pages.
use super::prelayout::{self, Job, Runner};
use super::{
    CreDocument, CreEngine, Document, FontFace, Layout, Link, Metadata, Opening, Paths, Position, SearchBatch,
    SearchStatus, Selection, SelectionQuery, TocEntry, Typography,
};
use crate::annotations::{Annotation, Kind};
use crate::geom::Rect;
use crate::gfx::ColorMode;
use crate::page_layout::PageLayout;
use crate::platform::Waker;
use anyhow::Result;
use std::cell::RefCell;
use std::ops::Deref;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Engine time per search step before the queue is checked again.
const SEARCH_SLICE: Duration = Duration::from_millis(60);
/// About how many characters of a page's opening words a page bookmark keeps.
pub const OPENING_CHARS: usize = 80;
/// Hitless progress replies are sent at most this often.
const SEARCH_PROGRESS: Duration = Duration::from_millis(300);
/// Maximum buffers kept in a page pixel pool (capped further by byte budget).
const POOL_MAX_BUFFERS: usize = 4;
/// Byte budget for pooled page buffers (matches `PageCache::DEFAULT_MAX_BYTES`:
/// 4 buffers on Gray8 PW6, 3 buffers on Color32 Colorsoft).
const POOL_MAX_BYTES: usize = 32 * 1024 * 1024;

struct PixelPoolInner {
    bytes: usize,
    max_buffers: usize,
    closed: bool,
    buffers: Vec<Vec<u8>>,
}

/// Bounded pool returning evicted `Page::pixels` buffers back to `verso-book`
/// so steady-state page renders avoid allocating and memsetting 2–8.6 MB per
/// turn and dropping them on the UI thread's glibc arena.
pub(crate) struct PixelPool {
    inner: Mutex<PixelPoolInner>,
}

impl PixelPool {
    fn new(bytes: usize) -> Arc<Self> {
        let max_buffers = POOL_MAX_BYTES.checked_div(bytes).map_or(0, |n| n.clamp(1, POOL_MAX_BUFFERS));
        Arc::new(Self {
            inner: Mutex::new(PixelPoolInner {
                bytes,
                max_buffers,
                closed: false,
                buffers: Vec::with_capacity(max_buffers),
            }),
        })
    }

    fn take(self: &Arc<Self>) -> PixelBuf {
        let (reused, bytes) = {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            (inner.buffers.pop(), inner.bytes)
        };
        let data = match reused {
            Some(buf) if buf.len() == bytes => buf,
            _ => vec![255; bytes],
        };
        PixelBuf { data, pool: Some(Arc::clone(self)) }
    }

    fn put(&self, buf: Vec<u8>) {
        // Drop any rejected buffer outside the lock.
        let discarded = {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            if !inner.closed && buf.len() == inner.bytes && inner.buffers.len() < inner.max_buffers {
                inner.buffers.push(buf);
                None
            } else {
                Some(buf)
            }
        };
        drop(discarded);
    }

    /// Marks the pool closed and frees all pooled buffers on the calling thread
    /// (run on `verso-book` before `release_freed_memory()` so idle Library RSS
    /// does not retain page buffers).
    fn close(&self) {
        let buffers = {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.closed = true;
            std::mem::take(&mut inner.buffers)
        };
        drop(buffers);
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).buffers.len()
    }
}

/// Page pixel buffer that recycles its backing allocation into `PixelPool` on drop.
pub struct PixelBuf {
    data: Vec<u8>,
    pool: Option<Arc<PixelPool>>,
}

impl PixelBuf {
    fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.data
    }
}

impl Deref for PixelBuf {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        &self.data
    }
}

impl AsRef<[u8]> for PixelBuf {
    fn as_ref(&self) -> &[u8] {
        &self.data
    }
}

impl From<Vec<u8>> for PixelBuf {
    fn from(data: Vec<u8>) -> Self {
        Self { data, pool: None }
    }
}

impl Clone for PixelBuf {
    fn clone(&self) -> Self {
        Self { data: self.data.clone(), pool: None }
    }
}

impl PartialEq for PixelBuf {
    fn eq(&self, other: &Self) -> bool {
        self.data == other.data
    }
}

impl PartialEq<Vec<u8>> for PixelBuf {
    fn eq(&self, other: &Vec<u8>) -> bool {
        self.data == *other
    }
}

impl PartialEq<[u8]> for PixelBuf {
    fn eq(&self, other: &[u8]) -> bool {
        self.data.as_slice() == other
    }
}

impl std::fmt::Debug for PixelBuf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PixelBuf").field("len", &self.data.len()).field("pooled", &self.pool.is_some()).finish()
    }
}

impl Drop for PixelBuf {
    fn drop(&mut self) {
        if let Some(pool) = self.pool.take()
            && !self.data.is_empty()
        {
            pool.put(std::mem::take(&mut self.data));
        }
    }
}

fn pixel_bytes((w, h): (u32, u32), color: ColorMode) -> usize {
    let bpp = match color {
        ColorMode::Gray8 => 1,
        ColorMode::Color32 => 4,
    };
    w as usize * h as usize * bpp
}

#[derive(Clone, Debug)]
pub struct Page {
    pub generation: u64,
    pub number: usize,
    /// `Gray8` (1 byte/px) or `Color32` (4 bytes/px, opaque RGBA).
    pub color: ColorMode,
    pub pixels: PixelBuf,
    pub position: Position,
    pub progress: u32,
    /// Characters of text before the page, when known (see `Document::text_offset`).
    pub offset: Option<u64>,
    pub images: bool,
    pub annotation_revision: u64,
    /// Highlights on the page as (highlight number, rectangles). The number
    /// counts highlights only, in saved order (`annotations::State::highlight`),
    /// so adding or removing a page bookmark leaves cached pages valid.
    pub annotations: Vec<(usize, Vec<Rect>)>,
    pub painted: Vec<Paint>,
    /// Internal links on the page: (link number, rectangle) for each piece
    /// of link text, for `Command::Link`.
    pub links: Vec<(usize, Rect)>,
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
/// A tap on link `link` of a displayed page; IDs are never reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LinkRequest {
    pub generation: u64,
    pub page: usize,
    pub link: usize,
    pub id: u64,
}
/// A page whose opening words a new page bookmark needs; IDs are never reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpeningRequest {
    pub generation: u64,
    pub page: usize,
    pub id: u64,
}
/// Where a position falls in a book layout, asked without moving the
/// reader (the KOReader check on open); IDs are never reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocateRequest {
    pub generation: u64,
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
    /// The page a highlight ends on (`page` for a bookmark; 0 when unresolved).
    pub last: usize,
}
struct Resolution {
    started: Instant,
    generation: u64,
    revision: u64,
    records: Vec<Annotation>,
    fingerprint: String,
    next: usize,
    found: Vec<Resolved>,
    /// A preview's page bookmarks that resolved, with their opening words.
    openings: Vec<(usize, Opening)>,
    preview: bool,
    /// The records are the ones the last full check accepted in this open
    /// book: their kept ranges are only located again.
    reuse: bool,
}
pub enum Command {
    /// Checks saved annotations against the current layout. A bookmark on a
    /// page without text holds only while the book's content `fingerprint`
    /// matches the one it was saved with.
    Resolve {
        generation: u64,
        revision: u64,
        records: Vec<Annotation>,
        fingerprint: String,
        preview: bool,
    },
    /// A page's opening words for a new page bookmark; replies with `Reply::Opening`.
    Opening(OpeningRequest),
    Select {
        request: SelectionRequest,
        query: SelectionQuery,
    },
    /// Where a tapped link leads; replies with `Reply::Link`.
    Link(LinkRequest),
    /// The page holding `position`; replies with `Reply::Located`.
    Locate {
        request: LocateRequest,
        position: Position,
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
        page: PageLayout,
        position: Position,
        /// The progress saved with `position`.
        progress: u32,
    },
    Layout {
        generation: u64,
        typography: Typography,
        page: PageLayout,
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
    /// Lays out the books likely to be opened next in a background process
    /// at these settings. Closes the open book first; a
    /// run at other settings is replaced. Send only while no reader is open.
    Prelayout {
        layout: Layout,
        typography: Typography,
        page: PageLayout,
    },
    /// Stops a background layout at once (sleep).
    StopPrelayout,
    /// From the thread reading a background layout's reports.
    PrelayoutEvent(prelayout::Event),
    /// Loads fonts and hyphenation, once the first frame is on screen. A
    /// command sent before it, such as opening a book at once, loads them first.
    Start,
    Shutdown,
}
pub enum Reply {
    Resolved {
        generation: u64,
        revision: u64,
        entries: Vec<Resolved>,
        /// A preview's page bookmarks that resolved (by index into its
        /// records), with the opening words to store for each.
        openings: Vec<(usize, Opening)>,
        preview: bool,
        /// The `Page::annotation_revision` of pages painted with these
        /// records: the revision of the resolution that last checked them.
        /// Resolving the same records again (after a relayout) only locates
        /// them, so pages painted since that check stay current.
        painted: u64,
    },
    Selection {
        request: SelectionRequest,
        result: Result<Option<Selection>, String>,
    },
    Link {
        request: LinkRequest,
        result: Result<Option<Link>, String>,
    },
    Opening {
        request: OpeningRequest,
        result: Result<Opening, String>,
    },
    /// The page holding the position and the progress at its top
    /// (0–10000), or None when the position isn't in the book.
    Located {
        request: LocateRequest,
        found: Option<(usize, u32)>,
    },
    Fonts(Vec<FontFace>),
    Opened {
        generation: u64,
        metadata: Metadata,
        toc: Vec<TocEntry>,
        pages: usize,
        current: usize,
        /// Opening: the saved position isn't in the book, so it opened at
        /// the start. Always false after a relayout.
        lost: bool,
    },
    /// Sent once per open, after the first page: the book's text length and
    /// the text offsets of chapter starts (top two TOC levels), by position.
    TextMap {
        generation: u64,
        map: Arc<crate::reader::TextMap>,
    },
    Page(Arc<Page>),
    PageFailed {
        generation: u64,
        number: usize,
        message: String,
    },
    Jumped {
        jump: u64,
        page: Arc<Page>,
    },
    Search {
        request: SearchRequest,
        result: Result<SearchBatch, String>,
    },
    Error {
        generation: u64,
        message: String,
    },
}
pub struct Worker {
    waker: Waker,
    tx: mpsc::Sender<Command>,
    rx: RefCell<mpsc::Receiver<Reply>>,
    join: Option<JoinHandle<()>>,
    fonts: RefCell<Vec<FontFace>>,
}
impl Worker {
    pub fn new(paths: Paths, dpi: u32, color: ColorMode, waker: Waker) -> Self {
        let (tx, commands) = mpsc::channel();
        let (results, rx) = mpsc::channel();
        let thread_waker = waker.clone();
        // For the background layout's report reader.
        let own = tx.clone();
        let join = std::thread::Builder::new()
            .name("verso-book".into())
            .spawn(move || {
                let send = |r| {
                    let _ = results.send(r);
                    thread_waker.wake();
                };
                // Registering the fonts takes 0.5–0.8 s on the Kindle; waiting
                // for the first frame keeps it from competing with drawing it.
                let first = match commands.recv() {
                    Ok(Command::Start) => None,
                    Ok(Command::Shutdown) | Err(_) => return,
                    Ok(command) => Some(command),
                };
                let mut prelayout = Runner::new(paths.cache.as_deref());
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
                let mut pool: Option<Arc<PixelPool>> = None;
                // The open book's file and page size, for the pre-layout record.
                let mut open_book: Option<(PathBuf, Layout)> = None;
                let mut generation = 0;
                let mut dimensions = (0, 0);
                let mut pending = first;
                let mut resolution: Option<Resolution> = None;
                // The records the last full check accepted, painted on pages
                // as `annotation_revision`. A relayout keeps them: crengine's
                // kept ranges are DOM positions, valid in any layout.
                let mut annotations: Vec<Annotation> = vec![];
                let mut annotation_revision = 0;
                let mut checked_fingerprint = String::new();
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
                                    while task.reuse
                                        && task.next < task.records.len()
                                        && started.elapsed() < Duration::from_millis(12)
                                    {
                                        match d.annotation_kept(task.next) {
                                            Some((index, page, last)) => {
                                                task.next += 1;
                                                task.found.push(Resolved { index, page: Some(page), last });
                                            }
                                            None => task.next = task.records.len(),
                                        }
                                    }
                                    while !task.reuse
                                        && task.next < task.records.len()
                                        && started.elapsed() < Duration::from_millis(12)
                                    {
                                        let i = task.next;
                                        task.next += 1;
                                        let a = &task.records[i];
                                        // An imported page bookmark has only its
                                        // position: it resolves when the position
                                        // is in this book, and takes its opening
                                        // words from there.
                                        if task.preview && a.kind == Kind::Bookmark {
                                            let opening = d.opening_at(&a.range.start, OPENING_CHARS).ok();
                                            let page = opening.as_ref().and_then(|_| {
                                                d.go_to_position(&a.range.start).ok().map(|_| d.current_page())
                                            });
                                            if let (Some(o), Some(_)) = (opening, page) {
                                                task.openings.push((i, o));
                                            }
                                            task.found.push(Resolved { index: i, page, last: page.unwrap_or(0) });
                                            continue;
                                        }
                                        let pages = holds(d, a, &task.fingerprint)
                                            .then(|| {
                                                if task.preview {
                                                    d.go_to_position(&a.range.start).ok().map(|_| {
                                                        let page = d.current_page();
                                                        (page, page)
                                                    })
                                                } else {
                                                    d.annotation_keep(i, a.kind == Kind::Highlight).ok()
                                                }
                                            })
                                            .flatten();
                                        task.found.push(Resolved {
                                            index: i,
                                            page: pages.map(|p| p.0),
                                            last: pages.map_or(0, |p| p.1),
                                        });
                                    }
                                    if task.next == task.records.len() {
                                        let mut task = resolution.take().unwrap();
                                        if task.reuse {
                                            // Already in reading order; the records
                                            // the check rejected follow, unresolved.
                                            let mut held = vec![false; task.records.len()];
                                            for e in &task.found {
                                                held[e.index] = true;
                                            }
                                            task.found.extend(
                                                (0..held.len())
                                                    .filter(|&i| !held[i])
                                                    .map(|index| Resolved { index, page: None, last: 0 }),
                                            );
                                        } else if !task.preview {
                                            let order = d.annotation_finish(task.records.len());
                                            let mut ranks = vec![usize::MAX; task.records.len()];
                                            for (rank, index) in order.into_iter().enumerate() {
                                                ranks[index] = rank;
                                            }
                                            task.found.sort_by_key(|e| ranks[e.index]);
                                            annotation_revision = task.revision;
                                            annotations = task.records;
                                            checked_fingerprint = task.fingerprint;
                                        }
                                        log::info!(
                                            "annotation resolution: count={} unresolved={} preview={} reused={} elapsed_us={}",
                                            task.found.len(),
                                            task.found.iter().filter(|e| e.page.is_none()).count(),
                                            task.preview,
                                            task.reuse,
                                            task.started.elapsed().as_micros()
                                        );
                                        send(Reply::Resolved {
                                            generation: task.generation,
                                            revision: task.revision,
                                            entries: task.found,
                                            openings: task.openings,
                                            preview: task.preview,
                                            painted: annotation_revision,
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
                    if matches!(
                        command,
                        Command::Open { .. } | Command::Layout { .. } | Command::Close | Command::Prelayout { .. }
                    ) {
                        search = None;
                        resolution = None;
                        if !matches!(command, Command::Layout { .. }) {
                            annotations.clear();
                            annotation_revision = 0;
                            checked_fingerprint.clear();
                        }
                    }
                    let mut opened = false;
                    let mut lost = false;
                    let result: Result<()> = (|| {
                        match command {
                            Command::Resolve { generation: g, revision, records, fingerprint, preview } => {
                                if g == generation
                                    && let Some(d) = &mut doc
                                {
                                    let reuse =
                                        !preview && fingerprint == checked_fingerprint && same_checks(&records, &annotations);
                                    if !preview && !reuse {
                                        d.annotation_clear();
                                    }
                                    resolution = Some(Resolution {
                                        started: Instant::now(),
                                        generation: g,
                                        revision,
                                        records,
                                        fingerprint,
                                        next: 0,
                                        found: vec![],
                                        openings: vec![],
                                        preview,
                                        reuse,
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
                            Command::Link(request) => {
                                if request.generation == generation
                                    && let Some(d) = &mut doc
                                {
                                    let result = d.link(request.page, request.link).map_err(|e| format!("{e:#}"));
                                    send(Reply::Link { request, result });
                                }
                                return Ok(());
                            }
                            Command::Locate { request, position } => {
                                if request.generation == generation
                                    && let Some(d) = &mut doc
                                {
                                    // Renders always go to their page, so the
                                    // engine's place can move here.
                                    let found = d.go_to_position(&position).ok().map(|()| {
                                        let page = d.current_page();
                                        d.go_to_page(page);
                                        (page, d.page_progress())
                                    });
                                    send(Reply::Located { request, found });
                                }
                                return Ok(());
                            }
                            Command::Opening(request) => {
                                if request.generation == generation
                                    && let Some(d) = &mut doc
                                {
                                    let last = d.page_count().saturating_sub(1);
                                    d.go_to_page(request.page.min(last));
                                    let result = if d.current_page() == request.page {
                                        d.page_opening(OPENING_CHARS).map_err(|e| format!("{e:#}"))
                                    } else {
                                        Err("The page is no longer in the book.".into())
                                    };
                                    send(Reply::Opening { request, result });
                                }
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
                            Command::Open { generation: g, path, layout, typography, page, position, progress } => {
                                generation = g;
                                if let Some(old) = pool.take() {
                                    old.close();
                                }
                                if doc.take().is_some() {
                                    release_freed_memory();
                                }
                                open_book = None;
                                prelayout.before_open(&engine);
                                dimensions = (layout.width, layout.height);
                                let mut d = engine.open(&path, &layout, Some((&typography, &page)))?;
                                prelayout.laid_out(&prelayout::key(&layout, &typography, &page, dpi), &path);
                                open_book = Some((path, layout));
                                d.remember_progress(position.clone(), progress);
                                if !position.0.is_empty()
                                    && let Err(e) = d.go_to_position(&position)
                                {
                                    log::warn!("{e}");
                                    lost = true;
                                }
                                pool = Some(PixelPool::new(pixel_bytes(dimensions, color)));
                                doc = Some(d);
                                opened = true;
                            }
                            Command::Layout { generation: g, typography, page, position } => {
                                generation = g;
                                if let Some(d) = &mut doc {
                                    d.go_to_position(&position)?;
                                    d.set_typography(&typography, dpi, &page)?;
                                    if let Some((path, layout)) = &open_book {
                                        prelayout.laid_out(&prelayout::key(layout, &typography, &page, dpi), path);
                                    }
                                }
                            }
                            Command::Page { generation: g, number } => {
                                if g == generation
                                    && let (Some(d), Some(p)) = (&mut doc, &pool)
                                {
                                    match render(d, g, number, dimensions, color, p, annotation_revision, &annotations)
                                    {
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
                                    && let (Some(d), Some(p)) = (&mut doc, &pool)
                                {
                                    d.go_to_position(&position)?;
                                    let number = d.current_page();
                                    send(Reply::Jumped {
                                        jump,
                                        page: render(
                                            d,
                                            g,
                                            number,
                                            dimensions,
                                            color,
                                            p,
                                            annotation_revision,
                                            &annotations,
                                        )?,
                                    });
                                }
                                return Ok(());
                            }
                            Command::Close => {
                                let had_book = doc.take().is_some();
                                if let Some(old) = pool.take() {
                                    old.close();
                                }
                                if had_book {
                                    release_freed_memory();
                                }
                                open_book = None;
                                return Ok(());
                            }
                            Command::Prelayout { layout, typography, page } => {
                                // Its cache file is final before another process may touch it.
                                let had_book = doc.take().is_some();
                                if let Some(old) = pool.take() {
                                    old.close();
                                }
                                if had_book {
                                    release_freed_memory();
                                }
                                open_book = None;
                                let own = own.clone();
                                prelayout.start(&Job { layout, typography, page, dpi }, move |event| {
                                    let _ = own.send(Command::PrelayoutEvent(event));
                                });
                                return Ok(());
                            }
                            Command::StopPrelayout => {
                                prelayout.stop("asked to");
                                return Ok(());
                            }
                            Command::PrelayoutEvent(event) => {
                                prelayout.on_event(event);
                                return Ok(());
                            }
                            Command::Start | Command::Shutdown => return Ok(()),
                        }
                        if let (Some(d), Some(p)) = (&mut doc, &pool) {
                            let pages = d.page_count();
                            let current = d.current_page();
                            let metadata = d.metadata();
                            log::info!(
                                "book {:?}, authors {:?}, language {:?}",
                                metadata.title,
                                metadata.authors,
                                metadata.language
                            );
                            let toc = d.toc()?;
                            let chapters: Vec<_> =
                                toc.iter().filter(|e| e.level <= 2).map(|e| e.position.clone()).collect();
                            send(Reply::Opened { generation, metadata, toc, pages, current, lost });
                            send(Reply::Page(render(
                                d,
                                generation,
                                current,
                                dimensions,
                                color,
                                p,
                                annotation_revision,
                                &annotations,
                            )?));
                            // Text offsets don't depend on the layout: once per open,
                            // after the first page is on its way.
                            if opened {
                                let started = Instant::now();
                                if let Some((offsets, length)) = d.text_offsets(&chapters) {
                                    let starts = chapters
                                        .into_iter()
                                        .zip(offsets)
                                        .filter_map(|(p, o)| Some((p.0, o?)))
                                        .collect();
                                    log::info!("chapter offsets in {:?}", started.elapsed());
                                    let map = Arc::new(crate::reader::TextMap { length, starts });
                                    send(Reply::TextMap { generation, map });
                                }
                            }
                        }
                        Ok(())
                    })();
                    if let Err(e) = result {
                        send(Reply::Error { generation, message: format!("{e:#}") });
                    }
                }
                if let Some(old) = pool.take() {
                    old.close();
                }
                prelayout.shutdown();
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
    /// Lets the book thread load fonts; see [`Command::Start`].
    pub fn start(&self) {
        self.send(Command::Start);
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
/// Whether checking `a` would give what checking `b` gave: the same records
/// in the same order, as far as `holds` reads them.
fn same_checks(a: &[Annotation], b: &[Annotation]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(a, b)| {
            a.kind == b.kind && a.range == b.range && a.quotation == b.quotation && a.fingerprint == b.fingerprint
        })
}
/// Whether a saved annotation still holds in the open book. A page bookmark
/// without text has nothing to compare, so it also needs the book's content
/// `fingerprint` to match the one it was saved with.
pub(super) fn holds(d: &mut CreDocument, a: &Annotation, fingerprint: &str) -> bool {
    match a.kind {
        Kind::Highlight => d.annotation_check(&a.range, &a.quotation).unwrap_or(false),
        Kind::Bookmark => {
            d.bookmark_check(&a.range, &a.quotation).unwrap_or(false)
                && (!a.quotation.is_empty() || a.fingerprint == fingerprint)
        }
    }
}
#[allow(clippy::too_many_arguments)]
fn render(
    d: &mut CreDocument,
    generation: u64,
    number: usize,
    (w, h): (u32, u32),
    color: ColorMode,
    pool: &Arc<PixelPool>,
    revision: u64,
    annotations: &[Annotation],
) -> Result<Arc<Page>> {
    let started = std::time::Instant::now();
    let last = d.page_count().saturating_sub(1);
    d.go_to_page(number.min(last));
    let mut pixels = pool.take();
    match color {
        ColorMode::Gray8 => d.draw(pixels.as_mut_slice(), w as usize)?,
        ColorMode::Color32 => d.draw_rgba(pixels.as_mut_slice(), w as usize * 4)?,
    }
    let info = d.page_info()?;
    let (geometry, painted) = if annotations.is_empty() {
        (Vec::new(), Vec::new())
    } else {
        let mut geometry = Vec::new();
        let mut geometry_cache = std::collections::HashMap::<(&str, &str), Vec<Rect>>::new();
        for index in d.annotation_page(info.page) {
            let Some(a) = annotations.get(index).filter(|a| a.kind == Kind::Highlight) else { continue };
            let number = annotations[..index].iter().filter(|a| a.kind == Kind::Highlight).count();
            let key = (a.range.start.0.as_str(), a.range.end.0.as_str());
            let rects = if let Some(rects) = geometry_cache.get(&key) {
                rects.clone()
            } else {
                let Some(selection) = d.select(info.page, &SelectionQuery::Geometry(a.range.clone())).ok().flatten()
                else {
                    continue;
                };
                geometry_cache.insert(key, selection.rects.clone());
                selection.rects
            };
            geometry.push((number, rects));
        }
        let painted = if geometry.is_empty() {
            Vec::new()
        } else {
            let union = crate::annotations::paint_union(geometry.iter().flat_map(|(_, rects)| rects.iter().copied()));
            match color {
                ColorMode::Gray8 => Paint::prepare(union, &pixels, w, h),
                // Color canvases tint highlights directly via `Canvas::highlight_rect`,
                // so skip preparing gray patches.
                ColorMode::Color32 => {
                    let bounds = Rect::new(0, 0, w as i32, h as i32);
                    union
                        .into_iter()
                        .filter_map(|r| r.intersect(&bounds))
                        .map(|rect| Paint { rect, pixels: Vec::new() })
                        .collect()
                }
            }
        };
        log::debug!(
            "annotation geometry: page={} revision={} ranges={} rects={}",
            info.page,
            revision,
            geometry.len(),
            geometry.iter().map(|(_, r)| r.len()).sum::<usize>()
        );
        (geometry, painted)
    };
    let page = Page {
        generation,
        number: info.page,
        color,
        pixels,
        position: info.position,
        progress: info.progress,
        offset: info.offset,
        images: info.has_images,
        annotation_revision: revision,
        painted,
        annotations: geometry,
        links: info.links,
    };
    log::info!("page {} rendered in {:?}", page.number + 1, started.elapsed());
    Ok(Arc::new(page))
}
/// Gives the pages of a closed book back to the system. glibc keeps freed
/// memory in whichever of its per-thread arenas allocated it, so a closed
/// book's DOM, caches and page buffers would otherwise stay resident until
/// that arena happens to be reused (measured: 4-9 MiB after a few reopens).
fn release_freed_memory() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        let started = Instant::now();
        // SAFETY: malloc_trim only returns free heap pages to the system.
        unsafe { libc::malloc_trim(0) };
        log::debug!("freed memory released in {:?}", started.elapsed());
    }
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
            page: PageLayout::default(),
            position: Position::default(),
        };
        tx.send(layout(2)).unwrap();
        tx.send(Command::Page { generation: 2, number: 9 }).unwrap();
        let mut pending = None;
        assert!(matches!(coalesce(layout(1), &rx, &mut pending), Command::Layout { generation: 2, .. }));
        assert!(matches!(pending, Some(Command::Page { number: 9, .. })));
    }
    #[test]
    fn pixel_pool_reuses_dropped_buffers_caps_count_and_clears_on_close() {
        let pool = PixelPool::new(1024);
        let a = pool.take();
        let ptr = a.as_ptr();
        assert_eq!(a.len(), 1024);
        assert_eq!(pool.len(), 0);
        // Cloning detaches from the pool so it does not double-return.
        let cloned = a.clone();
        drop(cloned);
        assert_eq!(pool.len(), 0);

        drop(a);
        assert_eq!(pool.len(), 1);
        let b = pool.take();
        assert_eq!(b.as_ptr(), ptr, "dropped buffer should be recycled without reallocating");
        assert_eq!(pool.len(), 0);

        // Dropping more buffers than POOL_MAX_BUFFERS caps the pool.
        let extra: Vec<_> = (0..POOL_MAX_BUFFERS + 2).map(|_| pool.take()).collect();
        drop(b);
        drop(extra);
        assert_eq!(pool.len(), POOL_MAX_BUFFERS);

        // Close frees queued buffers immediately and rejects late returns.
        let held = pool.take();
        assert_eq!(pool.len(), POOL_MAX_BUFFERS - 1);
        pool.close();
        assert_eq!(pool.len(), 0);
        drop(held);
        assert_eq!(pool.len(), 0);

        // Byte budget caps large Color32 buffers below POOL_MAX_BUFFERS while
        // keeping Gray8 at POOL_MAX_BUFFERS.
        let gray_pool = PixelPool::new(pixel_bytes((1272, 1696), ColorMode::Gray8));
        let color_pool = PixelPool::new(pixel_bytes((1272, 1696), ColorMode::Color32));
        assert_eq!(gray_pool.inner.lock().unwrap().max_buffers, 4);
        assert_eq!(color_pool.inner.lock().unwrap().max_buffers, 3);
    }
}
