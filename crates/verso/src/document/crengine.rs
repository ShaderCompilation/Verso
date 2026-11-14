//! `Document` on top of crengine (via crengine-sys).

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int};
use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use crengine_sys as sys;

use super::{
    Document, FontFace, Layout, Metadata, Paths, Position, SearchBatch, SearchHit, SearchStatus, Selection,
    SelectionQuery, TextRange, TocEntry, Typography,
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
    pub fn open(&self, path: &Path, layout: &Layout, typography: Option<(&Typography, bool)>) -> Result<CreDocument> {
        let raw = unsafe { sys::verso_cre_new(layout.width as c_int, layout.height as c_int, self.dpi as c_int) };
        if raw.is_null() {
            bail!("crengine could not create a document view");
        }
        let mut doc = CreDocument { raw, stats: OpenStats::default(), width: layout.width, height: layout.height };
        let css = cstr(&self.paths.engine_data.join("epub.css"))?;
        let face = CString::new(self.paths.font_face.as_str())?;
        let [l, t, r, b] = layout.margins.map(|m| m as c_int);
        unsafe {
            if sys::verso_cre_set_stylesheet_file(raw, css.as_ptr()) != 0 {
                log::warn!("could not load {}", self.paths.engine_data.join("epub.css").display());
            }
            sys::verso_cre_set_font_face(raw, face.as_ptr());
            sys::verso_cre_set_font_size(raw, layout.font_size as c_int);
            sys::verso_cre_set_margins(raw, l, t, r, b);
        }

        if let Some((t, footer)) = typography {
            doc.set_typography(t, self.dpi, footer)?;
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
        anyhow::ensure!((1..=10000).contains(&n), "invalid selection rectangles");
        let mut rects = vec![sys::VersoCreRect::default(); n as usize];
        anyhow::ensure!(
            unsafe { sys::verso_cre_selection_rects(self.raw, rects.as_mut_ptr(), n) } == n,
            "selection rectangles changed"
        );
        Ok(Some(Selection {
            range: TextRange { start: Position(string(0)?), end: Position(string(1)?) },
            text: string(2)?,
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
        unsafe { sys::verso_cre_progress(self.raw) }.clamp(0, 10000) as u32
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

    fn set_typography(&mut self, t: &Typography, dpi: u32, footer: bool) -> Result<()> {
        let face = CString::new(if t.font == "Publisher font" { "Literata" } else { &t.font })?;
        let mm = |v: f32| (v * dpi as f32 / 25.4).round() as c_int;
        let mut t = t.clone();
        t.normalize();
        let px = (t.size_pt * dpi as f32 / 72.0).round() as c_int;
        let started = Instant::now();
        let rc = unsafe {
            sys::verso_cre_typography(
                self.raw,
                face.as_ptr(),
                px,
                [100, 120, 140][t.spacing],
                side_margin(self.width, self.height, px, mm([4.0, 7.0, 10.0][t.margins]), t.margins),
                mm(6.0),
                mm(if footer { 12.0 } else { 6.0 }),
                t.justify as c_int,
                (t.font == "Publisher font") as c_int,
            )
        };
        anyhow::ensure!(rc == 0, "couldn't apply text settings");
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

/// The left and right page margin in pixels. On a landscape page the
/// margins grow so lines stay about 60–75 characters long: a column at most
/// 33, 30 or 27 ems wide for Narrow, Normal and Wide margins. Portrait pages
/// use the plain setting.
fn side_margin(width: u32, height: u32, font_px: c_int, base: c_int, setting: usize) -> c_int {
    if width <= height {
        return base;
    }
    let column = (font_px as f32 * [33.0, 30.0, 27.0][setting.min(2)]) as c_int;
    base.max((width as c_int - column) / 2)
}

fn cstr(path: &Path) -> Result<CString> {
    Ok(CString::new(path.as_os_str().as_encoded_bytes())?)
}

#[cfg(test)]
mod tests {
    use super::side_margin;

    #[test]
    fn landscape_pages_keep_a_readable_line_length() {
        // Portrait keeps the chosen margin.
        assert_eq!(side_margin(1272, 1696, 42, 83, 1), 83);
        // Landscape at 10 pt (42 px): a 1260 px column, centred.
        assert_eq!(side_margin(1696, 1272, 42, 83, 1), 218);
        // Wider margins still mean a narrower column.
        assert!(side_margin(1696, 1272, 42, 118, 2) > side_margin(1696, 1272, 42, 83, 1));
        // Large type already makes short lines: the plain margin wins.
        assert_eq!(side_margin(1696, 1272, 100, 83, 1), 83);
    }
}
