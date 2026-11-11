//! `Document` on top of crengine (via crengine-sys).

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int};
use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use crengine_sys as sys;

use super::{Document, Layout, Paths};

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

    /// Opens and lays out a book. Slow for big books (seconds on the Kindle).
    pub fn open(&self, path: &Path, layout: &Layout) -> Result<CreDocument> {
        let raw = unsafe { sys::verso_cre_new(layout.width as c_int, layout.height as c_int, self.dpi as c_int) };
        if raw.is_null() {
            bail!("crengine could not create a document view");
        }
        let mut doc = CreDocument { raw, font_size: layout.font_size, stats: OpenStats::default() };
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

        let t = Instant::now();
        if unsafe { sys::verso_cre_load(raw, cstr(path)?.as_ptr()) } != 0 {
            bail!("could not open {}", path.display());
        }
        doc.stats.load_ms = t.elapsed().as_millis() as u32;
        let t = Instant::now();
        unsafe { sys::verso_cre_render(raw) };
        doc.stats.layout_ms = t.elapsed().as_millis() as u32;
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
    font_size: u32,
    pub stats: OpenStats,
}

impl CreDocument {
    pub fn language(&mut self) -> String {
        self.prop(c"language")
    }

    fn prop(&mut self, key: &CStr) -> String {
        let mut buf = [0 as c_char; 512];
        unsafe {
            sys::verso_cre_get_prop(self.raw, key.as_ptr(), buf.as_mut_ptr(), buf.len() as c_int);
            CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned()
        }
    }
}

impl Document for CreDocument {
    fn page_count(&mut self) -> usize {
        unsafe { sys::verso_cre_page_count(self.raw) }.max(0) as usize
    }

    fn current_page(&mut self) -> usize {
        unsafe { sys::verso_cre_current_page(self.raw) }.max(0) as usize
    }

    fn go_to_page(&mut self, page: usize) {
        unsafe { sys::verso_cre_go_to_page(self.raw, page as c_int) };
    }

    fn font_size(&self) -> u32 {
        self.font_size
    }

    fn set_font_size(&mut self, px: u32) {
        self.font_size = px;
        unsafe {
            sys::verso_cre_set_font_size(self.raw, px as c_int);
            sys::verso_cre_render(self.raw);
        }
    }

    fn draw(&mut self, out: &mut [u8], stride: usize) -> Result<()> {
        let (w, h) = (stride as c_int, (out.len() / stride) as c_int);
        if unsafe { sys::verso_cre_draw(self.raw, out.as_mut_ptr(), w, h, stride as c_int) } != 0 {
            bail!("crengine failed to draw the page");
        }
        Ok(())
    }

    fn title(&mut self) -> String {
        self.prop(c"title")
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
