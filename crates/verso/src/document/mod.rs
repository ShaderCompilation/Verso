//! Books. The rest of the app talks to the `Document` trait and never to the
//! engine directly, so the engine could be swapped (e.g. for MuPDF) later.

mod crengine;

use std::path::{Path, PathBuf};

use anyhow::Result;

pub use crengine::{CreDocument, CreEngine};

/// Page layout settings, in pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    pub width: u32,
    pub height: u32,
    /// Left, top, right, bottom.
    pub margins: [u32; 4],
    pub font_size: u32,
}

/// An open book, laid out into pages for one screen size and typography.
pub trait Document {
    fn page_count(&mut self) -> usize;
    /// 0-based.
    fn current_page(&mut self) -> usize;
    fn go_to_page(&mut self, page: usize);
    fn font_size(&self) -> u32;
    /// Re-lays out the book, keeping the reading position.
    fn set_font_size(&mut self, px: u32);
    /// Draws the current page as 8-bit gray (0 = black), `stride` bytes per row.
    fn draw(&mut self, out: &mut [u8], stride: usize) -> Result<()>;
    fn title(&mut self) -> String;
}

/// Where the engine finds its data and the books, per device.
#[derive(Clone, Debug)]
pub struct Paths {
    /// Folder of test books for the Phase 2 spike.
    pub books: PathBuf,
    /// crengine's CSS files and `hyph/` patterns.
    pub engine_data: PathBuf,
    /// crengine's document cache (makes re-opening big books fast).
    pub cache: Option<PathBuf>,
    /// Folders scanned for .ttf/.otf files.
    pub font_dirs: Vec<PathBuf>,
    /// Default reading font family.
    pub font_face: String,
}

impl Paths {
    /// Device defaults, overridable with VERSO_BOOKS, VERSO_FONT_DIRS
    /// (colon-separated) and VERSO_FONT_FACE.
    pub fn for_device() -> Self {
        #[cfg(feature = "kindle")]
        let mut p = Self {
            books: "/mnt/us/verso/testbooks".into(),
            engine_data: "/mnt/us/verso/data/cre".into(),
            cache: Some("/mnt/us/verso/cache".into()),
            font_dirs: vec!["/usr/java/lib/fonts".into(), "/mnt/us/fonts".into()],
            font_face: "Bookerly".into(),
        };
        #[cfg(feature = "desktop")]
        let mut p = {
            let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
            Self {
                books: root.join("testbooks"),
                engine_data: root.join("third_party/crengine/cr3gui/data"),
                cache: Some(std::env::temp_dir().join("verso-cache")),
                font_dirs: vec!["/usr/share/fonts/liberation-serif-fonts".into()],
                font_face: "Liberation Serif".into(),
            }
        };
        if let Some(v) = std::env::var_os("VERSO_BOOKS") {
            p.books = v.into();
        }
        if let Ok(v) = std::env::var("VERSO_FONT_DIRS") {
            p.font_dirs = v.split(':').filter(|s| !s.is_empty()).map(PathBuf::from).collect();
        }
        if let Ok(v) = std::env::var("VERSO_FONT_FACE") {
            p.font_face = v;
        }
        p
    }
}

/// EPUB files directly inside `dir`, sorted by name.
pub fn list_books(dir: &Path) -> Vec<PathBuf> {
    let mut books: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("epub")))
                .collect()
        })
        .unwrap_or_default();
    books.sort();
    books
}

#[cfg(all(test, feature = "desktop"))]
mod tests {
    use super::*;

    /// Opens the first book in testbooks/ (skipped if there is none, as the
    /// books aren't in the repo).
    #[test]
    fn opens_draws_and_relays_out_a_book() {
        let paths = Paths { cache: None, ..Paths::for_device() };
        let Some(book) = list_books(&paths.books).into_iter().next() else {
            eprintln!("no books in {}; skipping", paths.books.display());
            return;
        };
        let engine = CreEngine::new(paths, 300).expect("engine starts");
        let layout = Layout { width: 600, height: 800, margins: [40; 4], font_size: 30 };
        let mut doc = engine.open(&book, &layout).expect("book opens");
        let pages = doc.page_count();
        assert!(pages > 10, "{pages} pages");

        doc.go_to_page(5);
        assert_eq!(doc.current_page(), 5);
        let mut buf = vec![255u8; 600 * 800];
        doc.draw(&mut buf, 600).unwrap();
        assert!(buf.iter().filter(|&&v| v < 128).count() > 1000, "page has no text");

        doc.set_font_size(40);
        assert!(doc.page_count() > pages, "a bigger font should need more pages");
    }
}
