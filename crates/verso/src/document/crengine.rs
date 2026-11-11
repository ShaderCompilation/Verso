//! `Document` on top of crengine (via crengine-sys).

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int};
use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use crengine_sys as sys;

use super::{Document, FontFace, Layout, Metadata, Paths, Position, TocEntry, Typography};

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
        let mut doc = CreDocument { raw, stats: OpenStats::default() };
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
                mm([4.0, 7.0, 10.0][t.margins]),
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

fn cstr(path: &Path) -> Result<CString> {
    Ok(CString::new(path.as_os_str().as_encoded_bytes())?)
}
