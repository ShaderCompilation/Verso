//! `Document` on top of crengine (via crengine-sys).

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int};
use std::path::Path;
use std::time::Instant;

use crate::page_layout::PageLayout;
use anyhow::{Context, Result, bail};
use crengine_sys as sys;

use super::{
    Document, FontFace, Layout, Link, Metadata, Note, Opening, PageInfo, Paragraphs, Paths, Position, SearchBatch,
    SearchHit, SearchStatus, Selection, SelectionQuery, TextRange, TextShaping, TocEntry, Typography,
};

/// crengine's gamma table without correction; `Typography::contrast` steps from it.
const GAMMA_NONE: c_int = 15;
/// Upper limit for crengine's on-disk document cache.
const CACHE_MAX_BYTES: c_int = 64 << 20;
/// The font list's file in crengine's cache folder, which crengine leaves
/// alone (it only removes `.cr3` files it doesn't list).
const FONT_LIST: &str = "fonts.list";
/// Bump when crengine's font registration (`RegisterFont`, `inspectFTFace`)
/// records something new about a face: older lists are then ignored.
const FONT_LIST_VERSION: u32 = 1;

/// The faces crengine registered from each font file, written on one start
/// and read on the next, so fonts register without opening every file: the
/// 93 on the Kindle took 0.4–1.3 s to open.
#[derive(Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
struct FontList {
    version: u32,
    files: Vec<FontFile>,
}
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct FontFile {
    path: std::path::PathBuf,
    len: u64,
    modified_ns: u64,
    /// `verso_cre_describe_font`'s text; empty when the file has no usable face.
    faces: String,
}

/// Registers every .ttf/.otf file in `dirs`, from `list` where it still
/// describes the file (same size and time) and by opening the file where it
/// doesn't. Every font is always registered: registering fewer would change
/// crengine's font hash, which is part of each cached book's key, and those
/// books would lay out again. Returns the fonts registered and how many of
/// them came from the list.
fn register_fonts(dirs: &[std::path::PathBuf], list: Option<&Path>) -> Result<(usize, usize)> {
    let old = list
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|bytes| serde_json::from_slice::<FontList>(&bytes).ok())
        .filter(|l| l.version == FONT_LIST_VERSION)
        .unwrap_or_default();
    let known: std::collections::HashMap<_, _> = old.files.iter().map(|f| (f.path.as_path(), f)).collect();
    let mut new = FontList { version: FONT_LIST_VERSION, files: Vec::new() };
    let (mut fonts, mut listed) = (0, 0);
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else { continue };
        for path in entries.filter_map(|e| e.ok().map(|e| e.path())) {
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
            if ext != "ttf" && ext != "otf" {
                continue;
            }
            let Ok(meta) = std::fs::metadata(&path) else { continue };
            let modified_ns = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos() as u64);
            let file = cstr(&path)?;
            if let Some(entry) =
                known.get(path.as_path()).filter(|f| f.len == meta.len() && f.modified_ns == modified_ns)
                && let Ok(faces) = CString::new(entry.faces.as_str())
            {
                let count = unsafe { sys::verso_cre_register_font_faces(file.as_ptr(), faces.as_ptr()) };
                if count >= 0 {
                    #[cfg(debug_assertions)]
                    {
                        let again =
                            read_string(|buf, len| unsafe { sys::verso_cre_describe_font(file.as_ptr(), buf, len) });
                        if again.as_deref().ok() != Some(entry.faces.as_str()) {
                            log::warn!("font list entry for {} reads back differently: {again:?}", path.display());
                        }
                    }
                    fonts += usize::from(count > 0);
                    listed += usize::from(count > 0);
                    new.files.push((*entry).clone());
                    continue;
                }
                log::warn!("font list entry for {} is unreadable; opening the file", path.display());
            }
            let registered = unsafe { sys::verso_cre_register_font(file.as_ptr()) } == 0;
            fonts += usize::from(registered);
            let faces = read_string(|buf, len| unsafe { sys::verso_cre_describe_font(file.as_ptr(), buf, len) })
                .unwrap_or_default();
            // A registered font crengine can't describe is opened every time.
            if registered != faces.is_empty() {
                new.files.push(FontFile { path, len: meta.len(), modified_ns, faces });
            }
        }
    }
    if let Some(list) = list
        && new != old
    {
        let write = || -> Result<()> {
            let temp = list.with_extension(format!("{}.tmp", std::process::id()));
            std::fs::write(&temp, serde_json::to_vec(&new)?)?;
            std::fs::rename(&temp, list).inspect_err(|_| {
                let _ = std::fs::remove_file(&temp);
            })?;
            Ok(())
        };
        if let Err(e) = write() {
            log::warn!("couldn't save the font list: {e:#}");
        }
    }
    Ok((fonts, listed))
}

/// crengine's error messages, into the app's log.
extern "C" fn log_message(message: *const c_char) {
    // SAFETY: crengine passes a NUL-terminated string that lives for this call.
    let message = unsafe { CStr::from_ptr(message) }.to_string_lossy();
    // The cache's index of embedded blobs, which `ldomBlobCache::loadIndex`
    // reads as empty when missing: every book without blobs says so. Any
    // other missing block is a damaged cache file.
    if message != "CacheFile::read: Block 16:0 not found in file" {
        log::error!(target: "crengine", "{}", message.trim_end());
    }
}

/// crengine's global state (fonts, hyphenation, cache). Create one per process.
pub struct CreEngine {
    paths: Paths,
    dpi: u32,
}

impl CreEngine {
    pub fn new(paths: Paths, dpi: u32) -> Result<Self> {
        let started = Instant::now();
        let hyph = cstr(&paths.engine_data.join("hyph"))?;
        let cache = match &paths.cache {
            Some(dir) => {
                std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
                Some(cstr(dir)?)
            }
            None => None,
        };
        let cache_ptr = cache.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());
        unsafe { sys::verso_cre_set_logger(log_message) };
        if unsafe { sys::verso_cre_init(hyph.as_ptr(), cache_ptr, CACHE_MAX_BYTES) } != 0 {
            bail!("crengine failed to start");
        }
        let fonts_started = Instant::now();
        let list = paths.cache.as_ref().map(|dir| dir.join(FONT_LIST));
        let (fonts, listed) = register_fonts(&paths.font_dirs, list.as_deref())?;
        if fonts == 0 {
            bail!("no fonts found in {:?}", paths.font_dirs);
        }
        log::info!(
            "crengine ready: {fonts} fonts registered ({listed} from the font list) in {:?}, total {:?}, font hash {:08x}",
            fonts_started.elapsed(),
            started.elapsed(),
            unsafe { sys::verso_cre_font_list_hash() }
        );
        Ok(Self { paths, dpi })
    }

    /// The hash of every registered font, part of each cached layout's key.
    pub fn font_hash(&self) -> u32 {
        unsafe { sys::verso_cre_font_list_hash() }
    }
    /// Reads crengine's cache index again: the pre-layout process changed it.
    pub fn reload_cache(&self) -> Result<()> {
        if self.paths.cache.is_some() {
            anyhow::ensure!(unsafe { sys::verso_cre_cache_reload() } == 0, "couldn't reload the book cache index");
        }
        Ok(())
    }

    pub fn fonts(&self) -> Vec<FontFace> {
        let count = unsafe { sys::verso_cre_font_count() };
        let faces = (0..count)
            .filter_map(|i| {
                let name = read_string(|buf, len| unsafe { sys::verso_cre_font_name(i, buf, len) }).ok()?;
                let file =
                    read_string(|buf, len| unsafe { sys::verso_cre_font_file(i, buf, len) }).ok().map(Into::into);
                Some(FontFace { name, file })
            })
            .collect();
        unsafe { sys::verso_cre_fonts_clear() };
        faces
    }

    /// Opens and lays out a book. Slow for big books (seconds on the Kindle).
    pub fn open(
        &self,
        path: &Path,
        layout: &Layout,
        typography: Option<(&Typography, &PageLayout)>,
    ) -> Result<CreDocument> {
        let raw = unsafe { sys::verso_cre_new(layout.width as c_int, layout.height as c_int, self.dpi as c_int) };
        if raw.is_null() {
            bail!("crengine could not create a document view");
        }
        let mut doc = CreDocument {
            raw,
            stats: OpenStats::default(),
            width: layout.width,
            height: layout.height,
            known_progress: None,
            counted: false,
        };
        let css = cstr(&self.paths.engine_data.join("epub.css"))?;
        let face = CString::new(self.paths.font_face.as_str())?;
        let [l, t, r, b] = layout.margins.map(|m| m as c_int);
        unsafe {
            if sys::verso_cre_set_stylesheet_file(raw, css.as_ptr()) != 0 {
                log::warn!("could not load {}", self.paths.engine_data.join("epub.css").display());
            }
            sys::verso_cre_set_shaping(
                raw,
                match layout.shaping {
                    TextShaping::FreeType => 1,
                    TextShaping::Light => 2,
                    TextShaping::Full => 3,
                },
            );
            sys::verso_cre_set_hanging_punctuation(raw, layout.hanging_punctuation as c_int);
            sys::verso_cre_set_font_face(raw, face.as_ptr());
            sys::verso_cre_set_font_size(raw, layout.font_size as c_int);
            sys::verso_cre_set_margins(raw, l, t, r, b);
        }

        if let Some((t, page)) = typography {
            doc.set_typography(t, self.dpi, page)?;
        }
        let (t, cpu) = (Instant::now(), crate::perf::thread_cpu());
        if unsafe { sys::verso_cre_load(raw, cstr(path)?.as_ptr()) } != 0 {
            bail!("could not open {}", path.display());
        }
        doc.stats.load_ms = t.elapsed().as_millis() as u32;
        let (t, parsed_cpu) = (Instant::now(), crate::perf::thread_cpu());
        unsafe { sys::verso_cre_render(raw) };
        doc.stats.layout_ms = t.elapsed().as_millis() as u32;
        let layout_cpu = crate::perf::thread_cpu().saturating_sub(parsed_cpu);
        anyhow::ensure!(doc.page_count() > 0, "book has no readable pages");
        let pages = doc.page_count();
        log::info!(
            "opened {}: parse {} ms, layout {} ms, {} pages, CPU {} + {} ms",
            path.display(),
            doc.stats.load_ms,
            doc.stats.layout_ms,
            pages,
            parsed_cpu.saturating_sub(cpu).as_millis(),
            layout_cpu.as_millis(),
        );
        Ok(doc)
    }
}

/// How long opening a book took.
#[derive(Clone, Copy, Debug, Default)]
pub struct OpenStats {
    /// Parsing the file (or reading it back from the cache).
    pub load_ms: u32,
    /// Laying out all pages.
    pub layout_ms: u32,
}

pub struct CreDocument {
    raw: *mut sys::VersoCreDoc,
    pub stats: OpenStats,
    /// The page size in pixels.
    width: u32,
    height: u32,
    /// A saved position and its progress, so reopening a book doesn't count
    /// the whole text before the first page.
    known_progress: Option<(Position, u32)>,
    /// The engine has its table of text offsets (built on first use, about
    /// 45–70 ms for a 1200-page book, dropped by a re-layout).
    counted: bool,
}

impl CreDocument {
    fn prop(&mut self, key: &CStr) -> String {
        read_string(|buf, len| unsafe { sys::verso_cre_get_prop(self.raw, key.as_ptr(), buf, len) }).unwrap_or_default()
    }
}

fn read_string(f: impl Fn(*mut c_char, c_int) -> c_int) -> Result<String> {
    let mut stack = [0u8; 256];
    let len = f(stack.as_mut_ptr().cast(), stack.len() as c_int);
    anyhow::ensure!((0..=1_048_576).contains(&len), "engine string getter failed");
    if (len as usize) < stack.len() {
        return Ok(String::from_utf8(stack[..len as usize].to_vec())?);
    }
    let mut buf = vec![0u8; len as usize + 1];
    anyhow::ensure!(f(buf.as_mut_ptr().cast(), buf.len() as c_int) == len, "engine string changed");
    buf.pop();
    Ok(String::from_utf8(buf)?)
}

impl Document for CreDocument {
    fn select(&mut self, page: usize, query: &SelectionQuery) -> Result<Option<Selection>> {
        use crate::geom::{Point, Rect};
        let (mode, point, range) = match query {
            SelectionQuery::Word(p) => (0, *p, None),
            SelectionQuery::Extend { anchor, focus } => (1, *focus, Some(anchor)),
            SelectionQuery::Range(range) => (2, Point::default(), Some(range)),
            SelectionQuery::Geometry(range) => (3, Point::default(), Some(range)),
        };
        let start = range.map(|r| CString::new(r.start.0.as_str())).transpose()?;
        let end = range.map(|r| CString::new(r.end.0.as_str())).transpose()?;
        let rc = unsafe {
            sys::verso_cre_select(
                self.raw,
                c_int::try_from(page)?,
                mode,
                point.x,
                point.y,
                start.as_ref().map_or(std::ptr::null(), |s| s.as_ptr()),
                end.as_ref().map_or(std::ptr::null(), |s| s.as_ptr()),
            )
        };
        anyhow::ensure!(rc >= 0, "couldn't resolve selection");
        if rc == 0 {
            return Ok(None);
        }
        let string =
            |field| read_string(|buf, len| unsafe { sys::verso_cre_selection_text(self.raw, field, buf, len) });
        let n = unsafe { sys::verso_cre_selection_rects(self.raw, std::ptr::null_mut(), 0) };
        anyhow::ensure!((0..=10000).contains(&n), "invalid selection rectangles");
        let mut rects = vec![sys::VersoCreRect::default(); n as usize];
        anyhow::ensure!(
            unsafe { sys::verso_cre_selection_rects(self.raw, rects.as_mut_ptr(), n) } == n,
            "selection rectangles changed"
        );
        Ok(Some(Selection {
            range: TextRange { start: Position(string(0)?), end: Position(string(1)?) },
            text: string(2)?,
            start_page: unsafe { sys::verso_cre_selection_text(self.raw, 4, std::ptr::null_mut(), 0) }.max(0) as usize,
            start_visible: unsafe { sys::verso_cre_selection_text(self.raw, 3, std::ptr::null_mut(), 0) } & 1 != 0,
            end_visible: unsafe { sys::verso_cre_selection_text(self.raw, 3, std::ptr::null_mut(), 0) } & 2 != 0,
            rects: rects.into_iter().map(|r| Rect::new(r.left, r.top, r.right - r.left, r.bottom - r.top)).collect(),
        }))
    }

    fn search_start(&mut self, query: &str) -> Result<()> {
        let query = CString::new(query)?;
        anyhow::ensure!(unsafe { sys::verso_cre_search_start(self.raw, query.as_ptr()) } == 0, "couldn't search");
        Ok(())
    }

    fn search_step(&mut self, budget: std::time::Duration) -> Result<SearchBatch> {
        let ms = c_int::try_from(budget.as_millis()).unwrap_or(c_int::MAX).max(1);
        let n = unsafe { sys::verso_cre_search_step(self.raw, ms) };
        anyhow::ensure!(n >= 0, "search failed");
        let mut hits = Vec::with_capacity(n as usize);
        for i in 0..n {
            let string =
                |field| read_string(|buf, len| unsafe { sys::verso_cre_search_hit(self.raw, i, field, buf, len) });
            let page = unsafe { sys::verso_cre_search_hit(self.raw, i, 5, std::ptr::null_mut(), 0) };
            hits.push(SearchHit {
                range: TextRange { start: Position(string(0)?), end: Position(string(1)?) },
                page: page.max(0) as usize,
                before: string(2)?,
                matched: string(3)?,
                after: string(4)?,
            });
        }
        let mut searched: c_int = 0;
        let status = match unsafe { sys::verso_cre_search_state(self.raw, &mut searched) } {
            0 => SearchStatus::Searching,
            1 => SearchStatus::Finished,
            2 => SearchStatus::Limited,
            _ => bail!("search failed"),
        };
        Ok(SearchBatch { hits, searched_pages: searched.max(0) as usize, pages: self.page_count(), status })
    }

    fn page_info(&mut self) -> Result<PageInfo> {
        let skip_pos = if !self.counted {
            self.known_progress.as_ref().and_then(|(p, _)| CString::new(p.0.as_str()).ok())
        } else {
            None
        };
        let mut pos_buf = [0u8; 512];
        let mut raw = sys::VersoCrePageInfo::default();
        let rc = unsafe {
            sys::verso_cre_page_info(
                self.raw,
                skip_pos.as_ref().map_or(std::ptr::null(), |s| s.as_ptr()),
                pos_buf.as_mut_ptr().cast(),
                pos_buf.len() as c_int,
                &mut raw,
            )
        };
        anyhow::ensure!(rc == 0, "couldn't read page info");
        anyhow::ensure!((0..=1_048_576).contains(&raw.position_len), "invalid page position length");
        let position = if (raw.position_len as usize) < pos_buf.len() {
            Position(String::from_utf8(pos_buf[..raw.position_len as usize].to_vec())?)
        } else {
            let mut buf = vec![0u8; raw.position_len as usize + 1];
            let n = unsafe { sys::verso_cre_page_position(self.raw, buf.as_mut_ptr().cast(), buf.len() as c_int) };
            anyhow::ensure!(n == raw.position_len, "page position changed");
            buf.pop();
            Position(String::from_utf8(buf)?)
        };
        let page = raw.page.max(0) as usize;
        let (progress, offset) = if page == 0 {
            (0, Some(0))
        } else if raw.progress < 0 {
            (self.known_progress.as_ref().map_or(0, |(_, p)| *p), None)
        } else {
            self.counted = true;
            let progress = if let Some((kp, saved)) = &self.known_progress
                && position == *kp
            {
                *saved
            } else {
                raw.progress.clamp(0, 10000) as u32
            };
            (progress, u64::try_from(raw.text_offset).ok())
        };
        let links = if (1..=10000).contains(&raw.link_rects) {
            let n = raw.link_rects;
            let mut rects = vec![sys::VersoCreRect::default(); n as usize];
            let mut ids = vec![0; n as usize];
            let copied =
                unsafe { sys::verso_cre_page_links(self.raw, raw.page, rects.as_mut_ptr(), ids.as_mut_ptr(), n) };
            if copied == n {
                ids.into_iter()
                    .zip(rects)
                    .map(|(id, r)| {
                        (id as usize, crate::geom::Rect::new(r.left, r.top, r.right - r.left, r.bottom - r.top))
                    })
                    .collect()
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };
        Ok(PageInfo { page, position, progress, offset, has_images: raw.has_images > 0, links })
    }

    fn go_to_position(&mut self, p: &Position) -> Result<()> {
        let p = CString::new(p.0.as_str())?;
        anyhow::ensure!(
            unsafe { sys::verso_cre_restore(self.raw, p.as_ptr()) } == 0,
            "saved position isn't in this book"
        );
        Ok(())
    }

    fn text_offsets(&mut self, positions: &[Position]) -> Option<(Vec<Option<u64>>, u64)> {
        self.counted = true;
        let length = u64::try_from(unsafe { sys::verso_cre_text_length(self.raw) }).ok().filter(|&l| l > 0)?;
        let offsets = positions
            .iter()
            .map(|p| {
                let p = CString::new(p.0.as_str()).ok()?;
                u64::try_from(unsafe { sys::verso_cre_text_offset(self.raw, p.as_ptr()) }).ok()
            })
            .collect();
        Some((offsets, length))
    }

    fn toc(&mut self) -> Result<Vec<TocEntry>> {
        let n = unsafe { sys::verso_cre_toc_count(self.raw) };
        anyhow::ensure!(n >= 0, "couldn't read contents");
        let mut entries = Vec::new();
        for i in 0..n {
            let string =
                |field| read_string(|buf, len| unsafe { sys::verso_cre_toc_entry(self.raw, i, field, buf, len) });
            let page = unsafe { sys::verso_cre_toc_entry(self.raw, i, 2, std::ptr::null_mut(), 0) };
            if page < 0 {
                continue;
            }
            entries.push(TocEntry {
                title: string(0)?,
                position: Position(string(1)?),
                page: page as usize,
                level: unsafe { sys::verso_cre_toc_entry(self.raw, i, 3, std::ptr::null_mut(), 0) }.max(1) as usize,
            });
        }
        entries.sort_by_key(|e| e.page);
        Ok(entries)
    }

    fn metadata(&mut self) -> Metadata {
        Metadata {
            title: self.prop(c"title"),
            authors: self.prop(c"authors"),
            language: self.prop(c"language"),
            publisher_fonts: unsafe { sys::verso_cre_embedded_fonts(self.raw) } > 0,
        }
    }

    fn set_typography(&mut self, t: &Typography, dpi: u32, layout: &PageLayout) -> Result<()> {
        let face = CString::new(if t.font == "Publisher font" { "Literata" } else { &t.font })?;
        let mut t = t.clone();
        t.normalize();
        let px = t.font_px(dpi as f32);
        let mut layout = layout.clone();
        layout.normalize();
        let g = layout.geometry(self.width as i32, self.height as i32, dpi as f32, px);
        let started = Instant::now();
        let settings = sys::VersoCreTypography {
            face: face.as_ptr(),
            px,
            line_spacing: t.line_spacing,
            margin: g.side,
            top: g.top,
            bottom: g.bottom,
            justify: t.justify as c_int,
            publisher: (t.font == "Publisher font") as c_int,
            weight: t.weight,
            gamma_index: GAMMA_NONE + t.contrast,
            word_spacing: t.word_spacing,
            paragraphs: match t.paragraphs {
                Paragraphs::Book => 0,
                Paragraphs::Indented => 1,
                Paragraphs::Spaced => 2,
            },
            hyphenate: t.hyphenate as c_int,
            book_styles: t.book_styles as c_int,
        };
        let rc = unsafe { sys::verso_cre_typography(self.raw, &settings) };
        anyhow::ensure!(rc == 0, "couldn't apply text settings");
        self.counted = false;
        log::info!("typography applied in {:?}", started.elapsed());
        Ok(())
    }

    fn page_count(&mut self) -> usize {
        unsafe { sys::verso_cre_page_count(self.raw) }.max(0) as usize
    }

    fn current_page(&mut self) -> usize {
        unsafe { sys::verso_cre_current_page(self.raw) }.max(0) as usize
    }

    fn go_to_page(&mut self, page: usize) {
        unsafe { sys::verso_cre_go_to_page(self.raw, page as c_int) };
    }

    fn draw(&mut self, out: &mut [u8], stride: usize) -> Result<()> {
        anyhow::ensure!(stride > 0 && !out.is_empty() && out.len().is_multiple_of(stride), "invalid page buffer");
        let (w, h) = (c_int::try_from(stride)?, c_int::try_from(out.len() / stride)?);
        if unsafe { sys::verso_cre_draw(self.raw, out.as_mut_ptr(), w, h, stride as c_int) } != 0 {
            bail!("crengine failed to draw the page");
        }
        Ok(())
    }

    fn draw_rgba(&mut self, out: &mut [u8], stride: usize) -> Result<()> {
        anyhow::ensure!(
            stride >= 4 && stride.is_multiple_of(4) && !out.is_empty() && out.len().is_multiple_of(stride),
            "invalid color page buffer"
        );
        let (w, h) = (c_int::try_from(stride / 4)?, c_int::try_from(out.len() / stride)?);
        if unsafe { sys::verso_cre_draw_rgba(self.raw, out.as_mut_ptr(), w, h, stride as c_int) } != 0 {
            bail!("crengine failed to draw the color page");
        }
        Ok(())
    }
}

impl Drop for CreDocument {
    fn drop(&mut self) {
        // Also writes the document cache.
        unsafe { sys::verso_cre_free(self.raw) };
    }
}

fn cstr(path: &Path) -> Result<CString> {
    Ok(CString::new(path.as_os_str().as_encoded_bytes())?)
}

impl CreDocument {
    /// `progress` was computed for `position` in this book; reuse it there.
    /// The progress at the top of the current page (0–10000), counting the
    /// book's text if it hasn't been yet.
    pub fn page_progress(&mut self) -> u32 {
        if self.current_page() == 0 {
            return 0;
        }
        self.counted = true;
        unsafe { sys::verso_cre_progress(self.raw) }.clamp(0, 10000) as u32
    }

    pub fn remember_progress(&mut self, position: Position, progress: u32) {
        self.known_progress = (!position.0.is_empty()).then_some((position, progress));
    }

    #[cfg(test)]
    pub fn position(&mut self) -> Result<Position> {
        Ok(Position(read_string(|buf, len| unsafe { sys::verso_cre_position(self.raw, buf, len) })?))
    }

    #[cfg(test)]
    pub fn progress(&mut self) -> u32 {
        if self.current_page() == 0 {
            return 0;
        }
        if self.known_progress.is_some() {
            let here = self.position().ok();
            if let Some((position, progress)) = &self.known_progress
                && here.as_ref() == Some(position)
            {
                return *progress;
            }
        }
        self.counted = true;
        unsafe { sys::verso_cre_progress(self.raw) }.clamp(0, 10000) as u32
    }

    #[cfg(test)]
    pub fn text_offset(&mut self) -> Option<u64> {
        if self.current_page() == 0 {
            return Some(0);
        }
        if !self.counted && self.known_progress.is_some() {
            let here = self.position().ok();
            if self.known_progress.as_ref().is_some_and(|(position, _)| here.as_ref() == Some(position)) {
                return None;
            }
        }
        self.counted = true;
        u64::try_from(unsafe { sys::verso_cre_text_offset(self.raw, std::ptr::null()) }).ok()
    }

    #[cfg(test)]
    pub fn has_images(&mut self) -> bool {
        (unsafe { sys::verso_cre_images(self.raw) }) > 0
    }
    pub fn annotation_clear(&mut self) {
        unsafe {
            sys::verso_cre_annotation_clear(self.raw);
        }
    }
    /// Whether `range` is still valid text that reads as `quotation`. Never
    /// searches for another location when the book or engine changed.
    /// A true result can then be kept with [`Self::annotation_keep`].
    pub fn annotation_check(&mut self, range: &TextRange, quotation: &str) -> Result<bool> {
        let a = CString::new(range.start.0.as_str())?;
        let b = CString::new(range.end.0.as_str())?;
        if unsafe { sys::verso_cre_annotation_check(self.raw, a.as_ptr(), b.as_ptr()) } < 0 {
            return Ok(false);
        }
        let text = read_string(|buf, len| unsafe { sys::verso_cre_annotation_checked_text(self.raw, buf, len) })?;
        Ok(crate::annotations::same_quotation(&text, quotation))
    }
    /// Whether a saved page bookmark still holds: its start exists and the
    /// text up to its end still reads as its opening words. A bookmark on a
    /// page without text (empty end and quotation) checks only the start; the
    /// caller also requires an unchanged book. Never searches elsewhere.
    /// A true result can then be kept with [`Self::annotation_keep`].
    pub fn bookmark_check(&mut self, range: &TextRange, quotation: &str) -> Result<bool> {
        let a = CString::new(range.start.0.as_str())?;
        let b = CString::new(range.end.0.as_str())?;
        let length = unsafe { sys::verso_cre_bookmark_check(self.raw, a.as_ptr(), b.as_ptr()) };
        if length < 0 {
            return Ok(false);
        }
        if quotation.is_empty() || range.end.0.is_empty() {
            return Ok(quotation.is_empty() && range.end.0.is_empty());
        }
        let text = read_string(|buf, len| unsafe { sys::verso_cre_annotation_checked_text(self.raw, buf, len) })?;
        Ok(crate::annotations::same_quotation(&text, quotation))
    }
    /// The current page's opening words for a page bookmark: up to about
    /// `max_chars` characters from where the page starts, ending on a word
    /// boundary and within the page. Empty end and text on a page without text.
    pub fn page_opening(&mut self, max_chars: usize) -> Result<Opening> {
        self.opening(None, max_chars)
    }
    /// The opening words of an imported page bookmark: from `start` (its
    /// first text at or after it) up to about `max_chars` characters, within
    /// the page that holds it. Fails when `start` isn't in this book.
    pub fn opening_at(&mut self, start: &Position, max_chars: usize) -> Result<Opening> {
        anyhow::ensure!(!start.0.is_empty(), "the bookmark has no position");
        self.opening(Some(start), max_chars)
    }
    fn opening(&mut self, start: Option<&Position>, max_chars: usize) -> Result<Opening> {
        let start = start.map(|p| CString::new(p.0.as_str())).transpose()?;
        let at = start.as_ref().map_or(std::ptr::null(), |s| s.as_ptr());
        let n = unsafe { sys::verso_cre_page_opening(self.raw, at, max_chars.try_into()?) };
        anyhow::ensure!(n >= 0, "couldn't read the page's opening words");
        let field = |f| read_string(|buf, len| unsafe { sys::verso_cre_page_opening_text(self.raw, f, buf, len) });
        let start = Position(field(0)?);
        anyhow::ensure!(!start.0.is_empty(), "the page has no position");
        Ok(Opening { range: TextRange { start, end: Position(field(1)?) }, text: field(2)? })
    }
    /// Adds the range the last successful check accepted as annotation `id`,
    /// returning its first and last page (the same page unless `painted`).
    /// An unpainted entry (a page bookmark) counts for reading order but is
    /// never listed by [`Self::annotation_page`].
    pub fn annotation_keep(&mut self, id: usize, painted: bool) -> Result<(usize, usize)> {
        let mut last = 0;
        let page = unsafe { sys::verso_cre_annotation_keep(self.raw, id.try_into()?, painted.into(), &mut last) };
        anyhow::ensure!(page >= 0, "no checked annotation range");
        Ok((page as usize, last.max(page) as usize))
    }
    /// The `index`-th finished annotation in reading order, as (id, first
    /// page, last page) in the current layout; None past the end. Finished
    /// ranges stay valid when the same open book is laid out again.
    pub fn annotation_kept(&mut self, index: usize) -> Option<(usize, usize, usize)> {
        let (mut page, mut last) = (0, 0);
        let id =
            unsafe { sys::verso_cre_annotation_kept(self.raw, c_int::try_from(index).ok()?, &mut page, &mut last) };
        (id >= 0).then(|| (id as usize, page.max(0) as usize, last.max(page).max(0) as usize))
    }
    pub fn annotation_finish(&mut self, count: usize) -> Vec<usize> {
        unsafe {
            sys::verso_cre_annotation_finish(self.raw);
        }
        (0..count)
            .map_while(|i| {
                let id = unsafe { sys::verso_cre_annotation_order(self.raw, i as c_int) };
                (id >= 0).then_some(id as usize)
            })
            .collect()
    }
    /// The internal links on `page`, as (link number, screen rectangle) for
    /// each piece of link text. Empty when the engine can't list them.
    #[cfg(test)]
    pub fn page_links(&mut self, page: usize) -> Vec<(usize, crate::geom::Rect)> {
        let Ok(page) = c_int::try_from(page) else { return Vec::new() };
        let n = unsafe { sys::verso_cre_page_links(self.raw, page, std::ptr::null_mut(), std::ptr::null_mut(), 0) };
        if !(1..=10000).contains(&n) {
            return Vec::new();
        }
        let mut rects = vec![sys::VersoCreRect::default(); n as usize];
        let mut ids = vec![0; n as usize];
        let copied = unsafe { sys::verso_cre_page_links(self.raw, page, rects.as_mut_ptr(), ids.as_mut_ptr(), n) };
        if copied != n {
            return Vec::new();
        }
        ids.into_iter()
            .zip(rects)
            .map(|(id, r)| (id as usize, crate::geom::Rect::new(r.left, r.top, r.right - r.left, r.bottom - r.top)))
            .collect()
    }

    /// Where link number `link` of `page` (from `page_links`) leads, and
    /// whether it's a footnote. None when it leads nowhere in the book.
    pub fn link(&mut self, page: usize, link: usize) -> Result<Option<Link>> {
        let kind = unsafe { sys::verso_cre_link(self.raw, c_int::try_from(page)?, c_int::try_from(link)?) };
        anyhow::ensure!(kind >= 0, "couldn't resolve link");
        if kind == 0 {
            return Ok(None);
        }
        let field = |f| read_string(|buf, len| unsafe { sys::verso_cre_link_text(self.raw, f, buf, len) });
        let target = Position(field(0)?);
        let rule = field(3)?;
        let note = if kind == 1 { Some(Note { label: field(2)?, html: field(1)? }) } else { None };
        log::info!("link {link} on page {}: {} ({rule})", page + 1, if note.is_some() { "footnote" } else { "jump" });
        Ok(Some(Link { target, note }))
    }

    pub fn annotation_page(&mut self, page: usize) -> Vec<usize> {
        let n = unsafe { sys::verso_cre_annotation_page(self.raw, page as c_int, std::ptr::null_mut(), 0) };
        if n <= 0 {
            return vec![];
        }
        let mut ids = vec![0; n as usize];
        unsafe {
            sys::verso_cre_annotation_page(self.raw, page as c_int, ids.as_mut_ptr(), n);
        }
        ids.into_iter().map(|i| i as usize).collect()
    }
}
