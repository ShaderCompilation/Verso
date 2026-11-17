//! `Document` on top of crengine (via crengine-sys).

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int};
use std::path::Path;
use std::time::Instant;

use crate::page_layout::PageLayout;
use anyhow::{Context, Result, bail};
use crengine_sys as sys;

use super::{
    Document, FontFace, Layout, Metadata, Paths, Position, SearchBatch, SearchHit, SearchStatus, Selection,
    SelectionQuery, TextRange, TextShaping, TocEntry, Typography,
};

/// Upper limit for crengine's on-disk document cache.
const CACHE_MAX_BYTES: c_int = 64 << 20;

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
        if unsafe { sys::verso_cre_init(hyph.as_ptr(), cache_ptr, CACHE_MAX_BYTES) } != 0 {
            bail!("crengine failed to start");
        }
        let mut fonts = 0;
        for dir in &paths.font_dirs {
            let Ok(entries) = std::fs::read_dir(dir) else { continue };
            for path in entries.filter_map(|e| e.ok().map(|e| e.path())) {
                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
                if (ext == "ttf" || ext == "otf") && unsafe { sys::verso_cre_register_font(cstr(&path)?.as_ptr()) } == 0
                {
                    fonts += 1;
                }
            }
        }
        if fonts == 0 {
            bail!("no fonts found in {:?}", paths.font_dirs);
        }
        log::info!("crengine ready: {fonts} fonts registered in {:?}", started.elapsed());
        Ok(Self { paths, dpi })
    }

    pub fn fonts(&self) -> Vec<FontFace> {
        (0..unsafe { sys::verso_cre_font_count() })
            .filter_map(|i| {
                let name = read_string(|buf, len| unsafe { sys::verso_cre_font_name(i, buf, len) }).ok()?;
                let file =
                    read_string(|buf, len| unsafe { sys::verso_cre_font_file(i, buf, len) }).ok().map(Into::into);
                Some(FontFace { name, file })
            })
            .collect()
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
            sys::verso_cre_set_font_face(raw, face.as_ptr());
            sys::verso_cre_set_font_size(raw, layout.font_size as c_int);
            sys::verso_cre_set_margins(raw, l, t, r, b);
        }

        if let Some((t, page)) = typography {
            doc.set_typography(t, self.dpi, page)?;
        }
        let t = Instant::now();
        if unsafe { sys::verso_cre_load(raw, cstr(path)?.as_ptr()) } != 0 {
            bail!("could not open {}", path.display());
        }
        doc.stats.load_ms = t.elapsed().as_millis() as u32;
        let t = Instant::now();
        unsafe { sys::verso_cre_render(raw) };
        doc.stats.layout_ms = t.elapsed().as_millis() as u32;
        anyhow::ensure!(doc.page_count() > 0, "book has no readable pages");
        let pages = doc.page_count();
        log::info!(
            "opened {}: parse {} ms, layout {} ms, {} pages",
            path.display(),
            doc.stats.load_ms,
            doc.stats.layout_ms,
            pages
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
    let len = f(std::ptr::null_mut(), 0);
    anyhow::ensure!((0..=1_048_576).contains(&len), "engine string getter failed");
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
            SelectionQuery::Quotation(range) => (4, Point::default(), Some(range)),
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

    fn position(&mut self) -> Result<Position> {
        Ok(Position(read_string(|buf, len| unsafe { sys::verso_cre_position(self.raw, buf, len) })?))
    }

    fn go_to_position(&mut self, p: &Position) -> Result<()> {
        let p = CString::new(p.0.as_str())?;
        anyhow::ensure!(
            unsafe { sys::verso_cre_restore(self.raw, p.as_ptr()) } == 0,
            "saved position isn't in this book"
        );
        Ok(())
    }

    fn progress(&mut self) -> u32 {
        // The first page is the start, and a saved position keeps the progress
        // saved with it (both depend only on the text). Anything else counts
        // the whole book's text once, ~45–70 ms for a 1200-page book.
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

    fn text_offset(&mut self) -> Option<u64> {
        if self.current_page() == 0 {
            return Some(0);
        }
        // Like `progress`, don't count the whole book just for the saved page.
        if !self.counted && self.known_progress.is_some() {
            let here = self.position().ok();
            if self.known_progress.as_ref().is_some_and(|(position, _)| here.as_ref() == Some(position)) {
                return None;
            }
        }
        self.counted = true;
        u64::try_from(unsafe { sys::verso_cre_text_offset(self.raw, std::ptr::null()) }).ok()
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

    fn has_images(&mut self) -> bool {
        (unsafe { sys::verso_cre_images(self.raw) }) > 0
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
        let rc = unsafe {
            sys::verso_cre_typography(
                self.raw,
                face.as_ptr(),
                px,
                [100, 120, 140][t.spacing],
                g.side,
                g.top,
                g.bottom,
                t.justify as c_int,
                (t.font == "Publisher font") as c_int,
            )
        };
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
    pub fn remember_progress(&mut self, position: Position, progress: u32) {
        self.known_progress = (!position.0.is_empty()).then_some((position, progress));
    }
    pub fn annotation_clear(&mut self) {
        unsafe {
            sys::verso_cre_annotation_clear(self.raw);
        }
    }
    pub fn annotation_add(&mut self, id: usize, range: &TextRange) -> Result<usize> {
        let a = CString::new(range.start.0.as_str())?;
        let b = CString::new(range.end.0.as_str())?;
        let page = unsafe { sys::verso_cre_annotation_add(self.raw, id.try_into()?, a.as_ptr(), b.as_ptr()) };
        anyhow::ensure!(page >= 0, "invalid annotation endpoints");
        Ok(page as usize)
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
