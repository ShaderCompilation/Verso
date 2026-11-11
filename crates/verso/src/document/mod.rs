//! Books. The rest of the app talks to the `Document` trait and never to the
//! engine directly, so the engine could be swapped (e.g. for MuPDF) later.

mod crengine;
pub mod thread;

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
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Typography {
    pub font: String,
    pub size_pt: f32,
    /// 0 = tight/narrow, 1 = normal, 2 = loose/wide.
    pub spacing: usize,
    pub margins: usize,
    pub justify: bool,
}

impl Default for Typography {
    fn default() -> Self {
        Self { font: "Literata".into(), size_pt: 10.0, spacing: 1, margins: 1, justify: true }
    }
}

impl Typography {
    pub fn normalize(&mut self) {
        self.size_pt = if self.size_pt.is_finite() { self.size_pt.clamp(6.0, 24.0) } else { 10.0 };
        self.spacing = self.spacing.min(2);
        self.margins = self.margins.min(2);
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Position(pub String);

#[derive(Clone, Debug)]
pub struct TocEntry {
    pub title: String,
    pub level: usize,
    pub page: usize,
    pub position: Position,
}

#[derive(Clone, Debug, Default)]
pub struct Metadata {
    pub title: String,
    pub authors: String,
    pub language: String,
    pub publisher_fonts: bool,
}

#[derive(Clone, Debug)]
pub struct FontFace {
    pub name: String,
    pub file: Option<PathBuf>,
}

pub trait Document {
    fn position(&mut self) -> Result<Position>;
    fn go_to_position(&mut self, position: &Position) -> Result<()>;
    /// Text offset in ten-thousandths; independent of page count.
    fn progress(&mut self) -> u32;
    fn toc(&mut self) -> Result<Vec<TocEntry>>;
    fn metadata(&mut self) -> Metadata;
    fn set_typography(&mut self, t: &Typography, dpi: u32, footer: bool) -> Result<()>;
    fn has_images(&mut self) -> bool;

    fn page_count(&mut self) -> usize;
    /// 0-based.
    fn current_page(&mut self) -> usize;
    fn go_to_page(&mut self, page: usize);
    /// Draws the current page as 8-bit gray (0 = black), `stride` bytes per row.
    fn draw(&mut self, out: &mut [u8], stride: usize) -> Result<()>;
}

/// Where the engine finds its data and the books, per device.
#[derive(Clone, Debug)]
pub struct Paths {
    /// Configured library folder.
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
            books: crate::store::Store::new().library_folder(),
            engine_data: "/mnt/us/verso/data/cre".into(),
            cache: Some("/mnt/us/verso/cache".into()),
            font_dirs: vec!["/mnt/us/verso/fonts".into(), "/usr/java/lib/fonts".into(), "/mnt/us/fonts".into()],
            font_face: "Literata".into(),
        };
        #[cfg(feature = "desktop")]
        let mut p = {
            let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
            Self {
                books: crate::store::Store::new().library_folder(),
                engine_data: root.join("third_party/crengine/cr3gui/data"),
                cache: Some(std::env::temp_dir().join("verso-cache")),
                font_dirs: vec![root.join("assets/fonts"), "/usr/share/fonts/liberation-serif-fonts".into()],
                font_face: "Literata".into(),
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

    /// Uses the redistributable fixture; private books are exercised on device.
    #[test]
    fn opens_draws_and_relays_out_a_book() {
        let paths = Paths {
            cache: Some(std::env::temp_dir().join(format!("verso-engine-test-{}", std::process::id()))),
            ..Paths::for_device()
        };
        let book = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/reader.epub");
        let engine = CreEngine::new(paths, 300).expect("engine starts");
        let layout = Layout { width: 600, height: 800, margins: [40; 4], font_size: 30 };
        let mut doc = engine.open(&book, &layout, None).expect("book opens");
        let pages = doc.page_count();
        assert!(pages > 10, "{pages} pages");

        doc.go_to_page(5);
        assert_eq!(doc.current_page(), 5);
        let mut buf = vec![255u8; 600 * 800];
        doc.draw(&mut buf, 600).unwrap();
        assert!(buf.iter().filter(|&&v| v < 128).count() > 1000, "page has no text");

        let position = doc.position().unwrap();
        let progress = doc.progress();
        assert!(!position.0.is_empty());
        let toc = doc.toc().unwrap();
        assert!(!toc.is_empty());
        assert!(toc.windows(2).all(|w| w[0].page <= w[1].page));
        for entry in toc.iter().take(3) {
            doc.go_to_position(&entry.position).unwrap();
        }
        doc.go_to_position(&position).unwrap();
        assert_eq!(doc.current_page(), 5);
        let metadata = doc.metadata();
        assert!(!metadata.title.is_empty());
        assert!(!engine.fonts().is_empty());
        assert!(doc.go_to_position(&Position("/no-such-element[999]".into())).is_err());
        doc.set_typography(&Typography { size_pt: 19.2, ..Typography::default() }, 150, true).unwrap();
        assert!(doc.page_count() > pages, "a bigger font should need more pages");
        doc.go_to_position(&position).unwrap();
        let typography = Typography { size_pt: 12.0, justify: false, ..Typography::default() };
        doc.set_typography(&typography, 150, true).unwrap();
        doc.go_to_position(&position).unwrap();
        assert!(doc.progress().abs_diff(progress) < 100, "position should preserve text progress");
        doc.draw(&mut buf, 600).unwrap();
        let left = buf.clone();
        doc.set_typography(&Typography { justify: true, ..typography }, 150, true).unwrap();
        doc.draw(&mut buf, 600).unwrap();
        assert_ne!(left, buf, "justification changes rendering");
        drop(doc);
        let mut reopened = engine.open(&book, &layout, None).unwrap();
        reopened.go_to_position(&position).unwrap();
        assert_eq!(reopened.current_page(), 5);
    }
}
