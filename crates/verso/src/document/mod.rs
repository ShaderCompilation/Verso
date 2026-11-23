//! Books. The rest of the app talks to the `Document` trait and never to the
//! engine directly, so the engine could be swapped (e.g. for MuPDF) later.
//!
//! `crengine` owns the C++ wrapper and `fonts.list` registration cache (every
//! discovered font must register so crengine's `font_list_hash` stays stable
//! across runs); `thread` serializes all engine work on `verso-book`;
//! `prelayout` runs an interruptible child process at idle to warm `.cr3`
//! layouts in crengine's disk cache.

mod crengine;
pub mod prelayout;
pub mod thread;

use std::path::PathBuf;

use anyhow::Result;

use crate::page_layout::PageLayout;

pub use crengine::{CreDocument, CreEngine};

/// Current page's metadata collected in a single engine snapshot.
pub struct PageInfo {
    pub page: usize,
    pub position: Position,
    pub progress: u32,
    pub offset: Option<u64>,
    pub has_images: bool,
    pub links: Vec<(usize, crate::geom::Rect)>,
}

/// Page layout settings, in pixels.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Layout {
    pub width: u32,
    pub height: u32,
    /// Left, top, right, bottom.
    pub margins: [u32; 4],
    pub font_size: u32,
    pub shaping: TextShaping,
}

/// How the engine shapes text: a trade between layout speed and typography.
/// Layout time for a 1220-page book on the Kindle: full 2.9 s, light 2.5 s,
/// FreeType 0.94 s.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextShaping {
    /// HarfBuzz with ligatures and complex scripts.
    Full,
    /// HarfBuzz kerning without ligatures; same line breaks as full.
    #[default]
    Light,
    /// FreeType kerning only: fastest, plainest.
    #[serde(rename = "freetype")]
    FreeType,
}
impl TextShaping {
    pub const ALL: [TextShaping; 3] = [TextShaping::Full, TextShaping::Light, TextShaping::FreeType];
}

/// An open book, laid out into pages for one screen size and typography.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Typography {
    pub font: String,
    pub size_pt: f32,
    /// 0 = tight, 1 = normal, 2 = loose.
    pub spacing: usize,
    pub justify: bool,
    /// The old Narrow/Normal/Wide preset, read once to migrate settings into
    /// `PageLayout` and never written again.
    #[serde(rename = "margins", skip_serializing)]
    pub legacy_margins: Option<usize>,
}

impl Default for Typography {
    fn default() -> Self {
        Self { font: "Literata".into(), size_pt: 10.0, spacing: 1, justify: true, legacy_margins: None }
    }
}

impl Typography {
    pub fn normalize(&mut self) {
        self.size_pt = if self.size_pt.is_finite() { self.size_pt.clamp(6.0, 24.0) } else { 10.0 };
        self.spacing = self.spacing.min(2);
    }
    /// The engine's font size in pixels.
    pub fn font_px(&self, dpi: f32) -> i32 {
        let size = if self.size_pt.is_finite() { self.size_pt.clamp(6.0, 24.0) } else { 10.0 };
        (size * dpi / 72.0).round() as i32
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Position(pub String);

/// Stable DOM positions, with an exclusive end. Never pixel offsets or page numbers.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TextRange {
    pub start: Position,
    pub end: Position,
}

/// A page's opening words, for a page bookmark (`CreDocument::page_opening`).
/// The range runs from where the page starts to the end of `text`; on a page
/// without text the end and the text are empty.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Opening {
    pub range: TextRange,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selection {
    pub range: TextRange,
    pub text: String,
    /// Screen coordinates on the explicitly requested page.
    pub rects: Vec<crate::geom::Rect>,
    pub start_page: usize,
    pub start_visible: bool,
    pub end_visible: bool,
}

#[derive(Clone, Debug)]
pub enum SelectionQuery {
    Word(crate::geom::Point),
    /// Union a word with the fixed anchor; a collapsed anchor adjusts a handle.
    Extend {
        anchor: TextRange,
        focus: crate::geom::Point,
    },
    #[allow(dead_code)] // Public extraction API for subsequent reading tools; exercised by engine tests.
    Range(TextRange),
    Geometry(TextRange),
}

/// Where a link in the book leads (see `CreDocument::link`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub target: Position,
    /// Set when the target is a footnote to show in place rather than jump to.
    pub note: Option<Note>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Note {
    /// The marker's text without brackets: "12", "*", or a linked phrase.
    pub label: String,
    /// The note as simple HTML: blocks, line breaks, bold, italic, scripts.
    pub html: String,
}

/// One match from book search, with the text around it in the same block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchHit {
    pub range: TextRange,
    /// Valid for the layout that produced it.
    pub page: usize,
    pub before: String,
    pub matched: String,
    pub after: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SearchStatus {
    #[default]
    Searching,
    Finished,
    /// Stopped at the engine's hit limit.
    Limited,
}

/// The hits found by one bounded search step.
#[derive(Clone, Debug, Default)]
pub struct SearchBatch {
    pub hits: Vec<SearchHit>,
    pub searched_pages: usize,
    pub pages: usize,
    pub status: SearchStatus,
}

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
    fn select(&mut self, page: usize, query: &SelectionQuery) -> Result<Option<Selection>>;
    /// Starts a literal, case-insensitive search from the start of the book.
    fn search_start(&mut self, query: &str) -> Result<()>;
    /// Continues the search for about `budget`, returning only the new hits.
    fn search_step(&mut self, budget: std::time::Duration) -> Result<SearchBatch>;
    fn page_info(&mut self) -> Result<PageInfo>;
    fn go_to_position(&mut self, position: &Position) -> Result<()>;
    /// Text offsets of `positions` (e.g. chapter starts) and the book's
    /// text length. None when the book has no text.
    fn text_offsets(&mut self, positions: &[Position]) -> Option<(Vec<Option<u64>>, u64)>;
    fn toc(&mut self) -> Result<Vec<TocEntry>>;
    fn metadata(&mut self) -> Metadata;
    fn set_typography(&mut self, t: &Typography, dpi: u32, layout: &PageLayout) -> Result<()>;

    fn page_count(&mut self) -> usize;
    /// 0-based.
    fn current_page(&mut self) -> usize;
    fn go_to_page(&mut self, page: usize);
    /// Draws the current page as 8-bit gray (0 = black), `stride` bytes per row.
    fn draw(&mut self, out: &mut [u8], stride: usize) -> Result<()>;
    /// Draws the current page as 32-bit opaque RGBA (`[R, G, B, 255]`), `stride` bytes per row.
    fn draw_rgba(&mut self, out: &mut [u8], stride: usize) -> Result<()>;
}

/// Where the engine finds its data and the books, per device.
#[derive(Clone, Debug)]
pub struct Paths {
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
    /// Device defaults, overridable with VERSO_FONT_DIRS
    /// (colon-separated), VERSO_FONT_FACE and VERSO_CACHE.
    pub fn for_device() -> Self {
        #[cfg(feature = "kindle")]
        let mut p = Self {
            engine_data: "/mnt/us/verso/data/cre".into(),
            cache: Some("/mnt/us/verso/cache".into()),
            font_dirs: vec!["/mnt/us/verso/fonts".into(), "/usr/java/lib/fonts".into(), "/mnt/us/fonts".into()],
            font_face: "Literata".into(),
        };
        #[cfg(feature = "desktop")]
        let mut p = {
            let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
            Self {
                engine_data: root.join("third_party/crengine/cr3gui/data"),
                cache: Some(std::env::temp_dir().join("verso-cache")),
                font_dirs: vec![root.join("assets/fonts"), "/usr/share/fonts/liberation-serif-fonts".into()],
                font_face: "Literata".into(),
            }
        };
        if let Ok(v) = std::env::var("VERSO_FONT_DIRS") {
            p.font_dirs = v.split(':').filter(|s| !s.is_empty()).map(PathBuf::from).collect();
        }
        if let Ok(v) = std::env::var("VERSO_FONT_FACE") {
            p.font_face = v;
        }
        if let Some(v) = std::env::var_os("VERSO_CACHE") {
            p.cache = Some(v.into());
        }
        p
    }
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
        let layout = Layout { width: 600, height: 800, margins: [40; 4], font_size: 30, shaping: TextShaping::Full };
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
        let info = doc.page_info().unwrap();
        assert_eq!(info.page, 5);
        assert_eq!(info.position, position);
        assert_eq!(info.progress, progress);
        assert_eq!(info.offset, doc.text_offset());
        assert_eq!(info.has_images, doc.has_images());
        assert_eq!(info.links, doc.page_links(5));
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
        let fonts = engine.fonts();
        assert!(!fonts.is_empty());
        assert_eq!(
            fonts.iter().map(|f| (&f.name, &f.file)).collect::<Vec<_>>(),
            engine.fonts().iter().map(|f| (&f.name, &f.file)).collect::<Vec<_>>(),
            "repeated font enumeration returns identical snapshot"
        );
        assert!(doc.go_to_position(&Position("/no-such-element[999]".into())).is_err());
        doc.set_typography(&Typography { size_pt: 19.2, ..Typography::default() }, 150, &PageLayout::default())
            .unwrap();
        assert!(doc.page_count() > pages, "a bigger font should need more pages");
        doc.go_to_position(&position).unwrap();
        let typography = Typography { size_pt: 12.0, justify: false, ..Typography::default() };
        doc.set_typography(&typography, 150, &PageLayout::default()).unwrap();
        doc.go_to_position(&position).unwrap();
        assert!(doc.progress().abs_diff(progress) < 100, "position should preserve text progress");
        doc.draw(&mut buf, 600).unwrap();
        let left = buf.clone();
        doc.set_typography(&Typography { justify: true, ..typography }, 150, &PageLayout::default()).unwrap();
        doc.draw(&mut buf, 600).unwrap();
        assert_ne!(left, buf, "justification changes rendering");
        drop(doc);
        let mut reopened = engine.open(&book, &layout, None).unwrap();
        // A saved position keeps its saved progress; other pages are counted.
        reopened.remember_progress(position.clone(), 4321);
        reopened.go_to_position(&position).unwrap();
        let saved_info = reopened.page_info().unwrap();
        assert_eq!(saved_info.page, 5);
        assert_eq!(saved_info.position, position);
        assert_eq!(saved_info.progress, 4321);
        assert_eq!(saved_info.offset, None, "page_info doesn't count text on saved page");
        assert_eq!(reopened.current_page(), 5);
        assert_eq!(reopened.progress(), 4321);
        assert_eq!(reopened.text_offset(), None, "the saved page isn't counted on open");
        reopened.go_to_page(6);
        let next_info = reopened.page_info().unwrap();
        assert!(next_info.progress.abs_diff(progress) < 1000, "other pages are counted");
        let six = next_info.offset.expect("counted pages have an offset");
        assert_eq!(reopened.progress(), next_info.progress);
        assert_eq!(reopened.text_offset(), Some(six));
        reopened.go_to_page(7);
        assert!(reopened.text_offset().unwrap() > six, "offsets grow page by page");
        // Chapter starts: in order, within the book, independent of layout.
        let toc = reopened.toc().unwrap();
        let positions: Vec<_> = toc.iter().map(|e| e.position.clone()).collect();
        let (starts, length) = reopened.text_offsets(&positions).unwrap();
        let starts: Vec<u64> = starts.into_iter().map(|s| s.expect("TOC entries are in the text")).collect();
        assert!(starts.windows(2).all(|w| w[0] <= w[1]), "{starts:?}");
        assert!(starts.iter().all(|&s| s <= length));
        reopened
            .set_typography(&Typography { size_pt: 19.2, ..Typography::default() }, 150, &PageLayout::default())
            .unwrap();
        assert_eq!(reopened.text_offsets(&positions).unwrap(), (starts.into_iter().map(Some).collect(), length));
        reopened.go_to_page(0);
        assert_eq!(reopened.progress(), 0, "the first page is the start");
        assert_eq!(reopened.text_offset(), Some(0));
        drop(reopened);
        selection_checks(&engine, &layout);
        search_checks(&engine, &layout);
        footnote_checks(&engine, &layout);
        missing_mimetype_checks(&engine, &layout, &book, pages);
        color_draw_checks(&engine, &layout);
        bookmark_checks(&engine, &layout, &book);
        textless_bookmark_checks(&engine, &layout);
    }

    /// Page bookmarks: the opening words start where the page starts and end
    /// on a word boundary; the check survives a reflow and lands on the page
    /// holding the position; changed text fails; bookmarks keep reading order
    /// but are never listed for painting.
    fn bookmark_checks(engine: &CreEngine, layout: &Layout, book: &std::path::Path) {
        let mut doc = engine.open(book, layout, None).unwrap();
        doc.go_to_page(5);
        let opening = doc.page_opening(80).unwrap();
        assert_eq!(opening.range.start, doc.page_info().unwrap().position, "starts where the page starts");
        assert!(!opening.text.trim().is_empty() && opening.text.chars().count() <= 80, "{:?}", opening.text);
        assert_eq!(opening.text, opening.text.trim_end(), "no trailing space");
        let long = doc.page_opening(400).unwrap();
        assert_eq!(long.range.start, opening.range.start);
        let rest = long.text.strip_prefix(&opening.text).expect("the longer opening continues the shorter one");
        assert!(rest.is_empty() || rest.starts_with(char::is_whitespace), "ends on a word boundary: {rest:?}");
        doc.annotation_clear();
        assert!(doc.bookmark_check(&opening.range, &opening.text).unwrap());
        assert_eq!(doc.annotation_keep(0, false).unwrap(), 5);
        // A highlight of the words after the opening, on the same page.
        let after = TextRange { start: opening.range.end.clone(), end: long.range.end.clone() };
        assert!(doc.annotation_check(&after, rest).unwrap());
        assert_eq!(doc.annotation_keep(1, true).unwrap(), 5);
        assert_eq!(doc.annotation_finish(2), vec![0, 1], "the page start comes first");
        assert_eq!(doc.annotation_page(5), vec![1], "only highlights are painted");
        assert!(doc.annotation_page(4).is_empty());
        // Edited text, a reversed range, a missing start and a half-empty
        // bookmark all fail.
        assert!(!doc.bookmark_check(&opening.range, "Some other opening words").unwrap());
        let reversed = TextRange { start: opening.range.end.clone(), end: opening.range.start.clone() };
        assert!(!doc.bookmark_check(&reversed, &opening.text).unwrap());
        let missing =
            TextRange { start: Position("/body/DocFragment[999]/body/p[3]".into()), end: Position::default() };
        assert!(!doc.bookmark_check(&missing, "").unwrap());
        let no_end = TextRange { end: Position::default(), ..opening.range.clone() };
        assert!(!doc.bookmark_check(&no_end, &opening.text).unwrap());
        assert!(!doc.bookmark_check(&opening.range, "").unwrap());
        // An imported bookmark has only a position: its opening words are read
        // from there to the end of the page holding it, and then check.
        assert_eq!(doc.opening_at(&opening.range.start, 80).unwrap(), opening, "a page start reads as its page");
        let mid = doc.opening_at(&opening.range.end, 400).unwrap();
        assert_eq!(mid.range.start, opening.range.end, "starts at the position, not the page start");
        assert!(mid.text.trim().starts_with(rest.trim()), "{mid:?} / {rest:?}");
        assert!(doc.bookmark_check(&mid.range, &mid.text).unwrap());
        assert!(doc.opening_at(&missing.start, 80).is_err(), "a position not in this book");
        assert!(doc.opening_at(&Position::default(), 80).is_err());
        assert_eq!(doc.current_page(), 5, "reading the words doesn't turn the page");
        // A bigger font moves the bookmark to the page now holding its start.
        doc.set_typography(&Typography { size_pt: 19.2, ..Typography::default() }, 150, &PageLayout::default())
            .unwrap();
        doc.annotation_clear();
        assert!(doc.bookmark_check(&opening.range, &opening.text).unwrap());
        let page = doc.annotation_keep(0, false).unwrap();
        assert!(page > 5, "a bigger font pushes the page later: {page}");
        doc.go_to_position(&opening.range.start).unwrap();
        assert_eq!(doc.current_page(), page);
        doc.annotation_finish(1);
        assert!(doc.annotation_page(page).is_empty());
        let reflowed = doc.opening_at(&opening.range.start, 80).unwrap();
        assert_eq!(reflowed.range.start, opening.range.start);
        assert!(doc.bookmark_check(&reflowed.range, &reflowed.text).unwrap());
    }

    /// A page with only a picture has no opening words: its bookmark keeps
    /// just the start, which must still exist.
    fn textless_bookmark_checks(engine: &CreEngine, layout: &Layout) {
        use std::io::Write;
        let path = std::env::temp_dir().join(format!("verso-textless-{}.epub", std::process::id()));
        let mut png = std::io::Cursor::new(Vec::new());
        image::GrayImage::from_pixel(200, 300, image::Luma([90])).write_to(&mut png, image::ImageFormat::Png).unwrap();
        let mut out = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        let opts = zip::write::SimpleFileOptions::default();
        out.start_file("mimetype", opts.compression_method(zip::CompressionMethod::Stored)).unwrap();
        out.write_all(b"application/epub+zip").unwrap();
        out.start_file("META-INF/container.xml", opts).unwrap();
        out.write_all(br#"<?xml version="1.0"?><container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#).unwrap();
        out.start_file("content.opf", opts).unwrap();
        out.write_all(br#"<?xml version="1.0"?><package xmlns="http://www.idpf.org/2007/opf" version="2.0"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Textless</dc:title></metadata><manifest><item id="cover" href="cover.xhtml" media-type="application/xhtml+xml"/><item id="ch1" href="ch1.xhtml" media-type="application/xhtml+xml"/><item id="img" href="cover.png" media-type="image/png"/></manifest><spine><itemref idref="cover"/><itemref idref="ch1"/></spine></package>"#).unwrap();
        out.start_file("cover.png", opts).unwrap();
        out.write_all(png.get_ref()).unwrap();
        out.start_file("cover.xhtml", opts).unwrap();
        out.write_all(br#"<?xml version="1.0" encoding="utf-8"?><html xmlns="http://www.w3.org/1999/xhtml"><head><title>Cover</title></head><body><div><img src="cover.png" alt=""/></div></body></html>"#).unwrap();
        out.start_file("ch1.xhtml", opts).unwrap();
        out.write_all(br#"<?xml version="1.0" encoding="utf-8"?><html xmlns="http://www.w3.org/1999/xhtml"><head><title>One</title></head><body><p>The first words of the book.</p></body></html>"#).unwrap();
        out.finish().unwrap();
        let mut doc = engine.open(&path, layout, None).unwrap();
        assert!(doc.page_count() >= 2, "the picture has its own page: {}", doc.page_count());
        doc.go_to_page(0);
        let cover = doc.page_opening(80).unwrap();
        assert!(!cover.range.start.0.is_empty());
        assert_eq!((cover.range.end.0.as_str(), cover.text.as_str()), ("", ""), "{cover:?}");
        doc.annotation_clear();
        assert!(doc.bookmark_check(&cover.range, "").unwrap());
        assert_eq!(doc.annotation_keep(0, false).unwrap(), 0);
        assert!(!doc.bookmark_check(&cover.range, "Some words").unwrap(), "words a picture page never had");
        // With no words to compare, a changed book leaves the bookmark unresolved.
        let mut saved = crate::annotations::Annotation {
            version: 1,
            kind: crate::annotations::Kind::Bookmark,
            id: "cover".into(),
            book: "textless".into(),
            fingerprint: "original".into(),
            range: cover.range.clone(),
            quotation: String::new(),
            created: "2026-11-22".into(),
            chapter: String::new(),
            note: None,
            provenance: None,
        };
        assert!(thread::holds(&mut doc, &saved, "original"));
        assert!(!thread::holds(&mut doc, &saved, "changed"));
        saved.range.start = Position("/body/DocFragment[7]/body/div".into());
        assert!(!thread::holds(&mut doc, &saved, "original"), "a start that no longer exists");
        let last = doc.page_count() - 1;
        doc.go_to_page(last);
        let text = doc.page_opening(80).unwrap();
        assert_eq!(text.text, "The first words of the book.");
        drop(doc);
        let _ = std::fs::remove_file(&path);
    }

    /// Renders an EPUB with colored headings, links and an embedded RGB image
    /// via `draw_rgba` and verifies full color reaches the output buffer.
    fn color_draw_checks(engine: &CreEngine, layout: &Layout) {
        use std::io::Write;
        let path = std::env::temp_dir().join(format!("verso-color-cre-{}.epub", std::process::id()));
        let mut png = std::io::Cursor::new(Vec::new());
        image::RgbImage::from_pixel(120, 80, image::Rgb([20, 180, 40]))
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        let mut out = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        let opts = zip::write::SimpleFileOptions::default();
        out.start_file(
            "mimetype",
            zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
        )
        .unwrap();
        out.write_all(b"application/epub+zip").unwrap();
        out.start_file("META-INF/container.xml", opts).unwrap();
        out.write_all(
            br#"<?xml version="1.0"?><container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#,
        )
        .unwrap();
        out.start_file("content.opf", opts).unwrap();
        out.write_all(
            br#"<?xml version="1.0"?><package xmlns="http://www.idpf.org/2007/opf" version="2.0"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Color Fixture</dc:title></metadata><manifest><item id="ch1" href="ch1.xhtml" media-type="application/xhtml+xml"/><item id="img" href="green.png" media-type="image/png"/></manifest><spine><itemref idref="ch1"/></spine></package>"#,
        )
        .unwrap();
        out.start_file("green.png", opts).unwrap();
        out.write_all(png.get_ref()).unwrap();
        out.start_file("ch1.xhtml", opts).unwrap();
        out.write_all(
            br##"<?xml version="1.0" encoding="utf-8"?><html xmlns="http://www.w3.org/1999/xhtml"><head><title>Color</title><style>h1 { color: #d02020; } a { color: #1840d8; }</style></head><body><h1>Red Heading</h1><p>A paragraph with a <a href="#end">blue link</a>.</p><p><img src="green.png" alt="green"/></p><p id="end">End.</p></body></html>"##,
        )
        .unwrap();
        out.finish().unwrap();

        let mut doc = engine.open(&path, layout, None).unwrap();
        // Dirty sentinel bytes prove draw_rgba and draw clear the full buffer
        // so recycled buffers never need an extra memset.
        let mut rgba = vec![0xAAu8; layout.width as usize * layout.height as usize * 4];
        doc.draw_rgba(&mut rgba, layout.width as usize * 4).unwrap();
        let pixels = rgba.as_chunks::<4>().0;
        assert!(pixels.iter().all(|p| p[3] == 255), "all pixels must be opaque");
        assert_eq!(pixels[0], [255, 255, 255, 255], "top-left margin cleared to white");
        let info = doc.page_info().unwrap();
        assert!(info.has_images, "page_info detects embedded image");
        assert!(!info.links.is_empty(), "page_info detects internal link");
        let reds = pixels.iter().filter(|p| p[0] > 160 && p[1] < 80 && p[2] < 80).count();
        let blues = pixels.iter().filter(|p| p[2] > 160 && p[0] < 80 && p[1] < 100).count();
        let greens = pixels.iter().filter(|p| p[1] > 150 && p[0] < 60 && p[2] < 80).count();
        assert!(reds > 20, "expected red heading pixels, got {reds}");
        assert!(blues > 20, "expected blue link pixels, got {blues}");
        assert!(greens > 2000, "expected green image pixels, got {greens}");

        // Compare against the 8-bit gray path: same page still draws cleanly.
        let mut gray = vec![0xAAu8; layout.width as usize * layout.height as usize];
        doc.draw(&mut gray, layout.width as usize).unwrap();
        assert_eq!(gray[0], 255, "top-left margin cleared to white on Gray8");
        assert!(gray.iter().filter(|&&v| v < 200).count() > 2000);
        drop(doc);
        let _ = std::fs::remove_file(&path);
    }

    /// Some repackaged EPUBs have no `mimetype` entry. They must still open as
    /// the book, not as the first file in the ZIP (here a stray text file).
    fn missing_mimetype_checks(engine: &CreEngine, layout: &Layout, book: &std::path::Path, pages: usize) {
        use std::io::{Read, Write};
        let path = std::env::temp_dir().join(format!("verso-no-mimetype-{}.epub", std::process::id()));
        let mut source = zip::ZipArchive::new(std::fs::File::open(book).unwrap()).unwrap();
        let mut out = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        out.start_file("META-INF/calibre_bookmarks.txt", options).unwrap();
        out.write_all(b"calibre_current_page_bookmark^5#body|0.000000").unwrap();
        for i in 0..source.len() {
            let mut entry = source.by_index(i).unwrap();
            if entry.name() == "mimetype" || entry.is_dir() {
                continue;
            }
            let mut data = Vec::new();
            entry.read_to_end(&mut data).unwrap();
            out.start_file(entry.name(), options).unwrap();
            out.write_all(&data).unwrap();
        }
        out.finish().unwrap();
        let mut doc = engine.open(&path, layout, None).unwrap();
        assert_eq!(doc.page_count(), pages, "opens as the EPUB, not the stray text file");
        assert!(!doc.toc().unwrap().is_empty());
        drop(doc);
        let _ = std::fs::remove_file(&path);
    }

    fn search_all(doc: &mut CreDocument, query: &str, budget: std::time::Duration) -> (Vec<SearchHit>, usize) {
        doc.search_start(query).unwrap();
        let (mut hits, mut steps) = (Vec::new(), 0);
        loop {
            let batch = doc.search_step(budget).unwrap();
            steps += 1;
            assert!(batch.searched_pages <= batch.pages);
            hits.extend(batch.hits);
            if batch.status != SearchStatus::Searching {
                assert_eq!(batch.status, SearchStatus::Finished);
                assert_eq!(batch.searched_pages, batch.pages);
                return (hits, steps);
            }
            assert!(steps < 10_000, "search must finish");
        }
    }

    fn search_checks(engine: &CreEngine, layout: &Layout) {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/search.epub");
        let mut doc = engine.open(&path, layout, None).unwrap();
        let pages = doc.page_count();
        assert!(pages > 20, "{pages} pages");
        // Tiny budgets force many chunks; chunk boundaries must neither drop
        // nor repeat a match, and hits stay in reading order.
        let (hits, steps) = search_all(&mut doc, "ada", std::time::Duration::from_millis(1));
        assert!(steps > 3, "{steps} steps");
        assert_eq!(hits.len(), 200);
        assert!(hits.windows(2).all(|w| w[0].page <= w[1].page && w[0].range != w[1].range));
        assert!(hits.last().unwrap().page + 2 >= pages);
        for (i, hit) in hits.iter().enumerate() {
            assert_eq!(hit.matched, "Ada");
            assert!(hit.before.ends_with(&format!("Paragraph {}. ", i + 1)), "{:?}", hit.before);
            assert!(hit.after.starts_with(" watched the tide"), "{:?}", hit.after);
        }
        let (same, _) = search_all(&mut doc, "ada", std::time::Duration::from_secs(1));
        assert_eq!(same, hits, "results do not depend on step size");
        // Opening a hit resolves the same text on its page.
        let hit = &hits[150];
        let shown = doc.select(hit.page, &SelectionQuery::Range(hit.range.clone())).unwrap().unwrap();
        assert_eq!(shown.text, "Ada");
        for (query, count) in [
            ("lovelace", 1),
            ("Café", 2),
            ("don't", 2),
            ("new york", 2),
            ("привет", 3),
            ("extraordinary", 1),
            ("a.b", 1),
            ("office", 2),
            ("vin", 2),
            ("ving", 0),
            ("vineyard", 1),
            ("’s vine", 1),
            ("  ada  watched ", 200),
            ("absent phrase", 0),
        ] {
            assert_eq!(search_all(&mut doc, query, std::time::Duration::from_secs(1)).0.len(), count, "{query:?}");
        }
        let (lovelace, _) = search_all(&mut doc, "lovelace", std::time::Duration::from_secs(1));
        let shown = doc.select(lovelace[0].page, &SelectionQuery::Range(lovelace[0].range.clone())).unwrap().unwrap();
        assert_eq!(shown.text, "Lovelace");
        assert!(doc.search_start("   ").is_err());
        // A relayout invalidates a running search; the same query then finds
        // the same ranges at new page numbers.
        doc.search_start("ada").unwrap();
        doc.set_typography(&Typography { size_pt: 16.0, ..Typography::default() }, 150, &PageLayout::default())
            .unwrap();
        assert_eq!(doc.search_step(std::time::Duration::from_secs(1)).unwrap().status, SearchStatus::Finished);
        let (relaid, _) = search_all(&mut doc, "ada", std::time::Duration::from_secs(1));
        assert_eq!(
            relaid.iter().map(|h| &h.range).collect::<Vec<_>>(),
            hits.iter().map(|h| &h.range).collect::<Vec<_>>()
        );
        assert!(relaid.last().unwrap().page > hits.last().unwrap().page);
    }

    /// Footnote markers of the shapes real books use open as notes; contents
    /// entries, cross-references and the ways back jump; web links aren't listed.
    fn footnote_checks(engine: &CreEngine, layout: &Layout) {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/footnotes.epub");
        let mut doc = engine.open(&path, layout, None).unwrap();
        let mut found = Vec::new();
        let mut notes = std::collections::BTreeMap::new();
        for page in 0..doc.page_count() {
            doc.go_to_page(page);
            let links = doc.page_links(page);
            assert_eq!(doc.page_info().unwrap().links, links, "page_info links match page_links on page {page}");
            assert!(links.iter().all(|(_, r)| r.w > 0 && r.h > 0 && r.right() <= 600 && r.bottom() <= 800));
            let mut ids: Vec<_> = links.iter().map(|(id, _)| *id).collect();
            ids.dedup();
            for id in ids {
                let link = doc.link(page, id).unwrap().expect("listed links lead somewhere");
                let label = link.note.as_ref().map(|n| n.label.clone());
                found.push((page, label.is_some()));
                if let Some(note) = link.note {
                    notes.insert(note.label, note.html);
                }
                doc.go_to_position(&link.target).unwrap();
            }
        }
        assert_eq!(found.iter().filter(|(_, note)| *note).count(), 5, "{notes:?}");
        assert_eq!(found.iter().filter(|(_, note)| !*note).count(), 6, "contents, cross-reference, four ways back");
        assert!(found.iter().filter(|(_, note)| *note).all(|(page, _)| *page == 0), "only the markers");
        for (label, text) in [
            ("1", "First note"),
            ("2", "Second note"),
            ("reference", "Third note"),
            ("*", "Fourth note"),
            ("linked phrase", "Fifth note"),
        ] {
            assert!(notes.get(label).is_some_and(|html| html.contains(text)), "{label}: {:?}", notes.get(label));
        }
        // Styles come from the book; the marker's number and way back don't repeat.
        assert!(notes["2"].contains("<i>italic</i>") && notes["2"].contains("<b>bold</b>"), "{}", notes["2"]);
        assert!(!notes["2"].contains(">2<") && !notes["1"].contains("back") && !notes["1"].contains("<h"));
    }

    fn selection_checks(engine: &CreEngine, layout: &Layout) {
        use crate::geom::Point;
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/selection.epub");
        let mut doc = engine.open(&path, layout, None).unwrap();
        let mut words = std::collections::BTreeMap::new();
        // Use the real renderer's hit testing rather than hard-coding font metrics.
        for y in (40..400).step_by(8) {
            for x in (40..560).step_by(8) {
                let p = Point::new(x, y);
                if let Some(s) = doc.select(0, &SelectionQuery::Word(p)).unwrap() {
                    words.entry(s.text.clone()).or_insert((p, s));
                }
            }
        }
        for word in ["Hello", "can't", "reader’s", "Привет", "мир", "Русский", "Formatting", "extraordinary"]
        {
            assert!(words.contains_key(word), "missing {word:?}; got {:?}", words.keys());
        }
        assert!(doc.select(0, &SelectionQuery::Word(Point::new(1, 1))).unwrap().is_none());
        // Reopening at a narrow width splits the soft-hyphen word visually,
        // while its stable range and extracted text remain unchanged.
        let mut narrow = engine.open(&path, &Layout { width: 200, margins: [20; 4], ..layout.clone() }, None).unwrap();
        let hyphenated =
            narrow.select(0, &SelectionQuery::Range(words["extraordinary"].1.range.clone())).unwrap().unwrap();
        assert_eq!(hyphenated.text, "extraordinary");
        assert!(hyphenated.rects.len() > 1, "fixture must exercise a visually hyphenated word");
        drop(narrow);
        assert!(doc.select(0, &SelectionQuery::Word(Point::new(590, 790))).unwrap().is_none());
        let (hello_point, hello) = &words["Hello"];
        let (russian_point, russian) = &words["Привет"];
        doc.go_to_page(1); // Simulate the engine being left on a prefetched neighbour.
        assert_eq!(doc.select(0, &SelectionQuery::Word(*hello_point)).unwrap(), Some(hello.clone()));
        let extended = doc
            .select(0, &SelectionQuery::Extend { anchor: hello.range.clone(), focus: *russian_point })
            .unwrap()
            .unwrap();
        assert!(extended.text.is_empty(), "drag requests only geometry");
        let extended = doc.select(0, &SelectionQuery::Range(extended.range)).unwrap().unwrap();
        assert!(extended.text.starts_with("Hello") && extended.text.ends_with("Привет"), "{:?}", extended.text);
        assert!(extended.rects.len() > 1);
        // Dragging into the empty right space of a short line still snaps to
        // the end of that line, while an initial long-press there stays blank.
        let short_line_right = Point::new(580, russian.rects[0].center().y);
        assert!(doc.select(0, &SelectionQuery::Word(short_line_right)).unwrap().is_none());
        let snapped = doc
            .select(0, &SelectionQuery::Extend { anchor: hello.range.clone(), focus: short_line_right })
            .unwrap()
            .unwrap();
        let snapped = doc.select(0, &SelectionQuery::Range(snapped.range)).unwrap().unwrap();
        assert!(
            snapped.text.starts_with("Hello") && snapped.text.contains("Привет"),
            "drag past short line should include its text, got {:?}",
            snapped.text
        );
        let backwards = doc
            .select(0, &SelectionQuery::Extend { anchor: russian.range.clone(), focus: *hello_point })
            .unwrap()
            .unwrap();
        let backwards = doc.select(0, &SelectionQuery::Range(backwards.range)).unwrap().unwrap();
        assert_eq!(extended, backwards);
        let reversed = TextRange { start: extended.range.end.clone(), end: extended.range.start.clone() };
        assert_eq!(doc.select(0, &SelectionQuery::Range(reversed)).unwrap(), Some(extended.clone()));
        let fixed = hello.range.end.clone();
        let crossed = doc
            .select(
                0,
                &SelectionQuery::Extend {
                    anchor: TextRange { start: fixed.clone(), end: fixed.clone() },
                    focus: *russian_point,
                },
            )
            .unwrap()
            .unwrap();
        assert_eq!(crossed.range.start, fixed, "handle crossing keeps its opposite endpoint fixed");
        // Endpoints must remain text positions when typography changes.
        doc.set_typography(
            &Typography { size_pt: 12.0, ..Typography::default() },
            150,
            &PageLayout { bottom: crate::page_layout::Bar::default(), ..PageLayout::default() },
        )
        .unwrap();
        let restored = doc.select(0, &SelectionQuery::Range(hello.range.clone())).unwrap().unwrap();
        assert_eq!(restored.text, "Hello");
        assert_eq!(restored.range, hello.range);
        // A whole-document range is clipped to the explicitly displayed page.
        let first = doc.select(0, &SelectionQuery::Range(extended.range.clone())).unwrap().unwrap();
        let last_page = doc.page_count() - 1;
        let mut last = None;
        for y in (30..770).step_by(10) {
            for x in (30..570).step_by(20) {
                if let Some(s) = doc.select(last_page, &SelectionQuery::Word(Point::new(x, y))).unwrap() {
                    last = Some(s);
                }
            }
        }
        let all = TextRange { start: first.range.start, end: last.unwrap().range.end };
        let clipped = doc.select(0, &SelectionQuery::Range(all.clone())).unwrap().unwrap();
        assert!(clipped.text.contains("Final boundary"));
        assert_eq!(clipped.range, all);
        assert!(clipped.start_visible && !clipped.end_visible);
        let end = doc.select(last_page, &SelectionQuery::Range(all.clone())).unwrap().unwrap();
        assert_eq!(end.text, clipped.text);
        assert!(!end.start_visible && end.end_visible);
        let geometry = doc.select(last_page, &SelectionQuery::Geometry(all.clone())).unwrap().unwrap();
        assert!(geometry.text.is_empty());
        assert_eq!(geometry.rects, end.rects);
        doc.annotation_clear();
        assert!(doc.annotation_check(&all, &clipped.text).unwrap());
        assert_eq!(doc.annotation_keep(9, true).unwrap(), 0);
        assert_eq!(doc.annotation_finish(1), vec![9]);
        for page in 0..=last_page {
            assert_eq!(doc.annotation_page(page), vec![9]);
        }
        let reversed = TextRange { start: all.end.clone(), end: all.start.clone() };
        assert_eq!(doc.select(0, &SelectionQuery::Range(reversed)).unwrap().unwrap().text, clipped.text);
        assert!(clipped.rects.iter().all(|r| r.y >= 0 && r.bottom() <= layout.height as i32));
        assert!(!doc.annotation_check(&all, "Edited quotation").unwrap());
        let reversed = TextRange { start: all.end.clone(), end: all.start.clone() };
        assert!(!doc.annotation_check(&reversed, &clipped.text).unwrap());
        assert!(doc.annotation_keep(1, true).is_err(), "a failed check leaves nothing to keep");
        let root = std::env::temp_dir().join(format!("verso-highlight-milestone-{}", std::process::id()));
        let store = crate::store::Store::at(root.clone());
        let mut collection = crate::annotations::Collection::load(&store, "milestone").unwrap();
        collection.add(crate::annotations::Annotation {
            version: 1,
            kind: crate::annotations::Kind::Highlight,
            id: "passage".into(),
            book: "milestone".into(),
            fingerprint: "original".into(),
            range: all.clone(),
            quotation: clipped.text.clone(),
            created: "2026-11-15".into(),
            chapter: "Selection".into(),
            note: None,
            provenance: None,
        });
        collection.commit(&store, "milestone").unwrap();
        drop(doc);
        let restored = crate::annotations::Collection::load(&store, "milestone").unwrap();
        let mut landscape = engine.open(&path, &Layout { width: 800, height: 600, ..layout.clone() }, None).unwrap();
        landscape
            .set_typography(&Typography { size_pt: 18.0, ..Typography::default() }, 150, &PageLayout::default())
            .unwrap();
        let saved = &restored.records[0];
        assert!(landscape.annotation_check(&saved.range, &saved.quotation).unwrap());
        landscape.go_to_position(&saved.range.start).unwrap();
        let jump = landscape.current_page();
        assert!(landscape.select(jump, &SelectionQuery::Geometry(all)).unwrap().unwrap().start_visible);
        std::fs::remove_dir_all(root).unwrap();
    }
}
