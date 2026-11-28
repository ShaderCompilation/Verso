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
    /// Punctuation hangs into the margin. Settings → Advanced; doesn't move
    /// page breaks, so it isn't part of the pre-layout key.
    pub hanging_punctuation: bool,
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
    /// CSS weight of regular text (400); bold text moves with it.
    pub weight: i32,
    /// Steps of glyph gamma from none (0); higher is darker. Only redraws.
    pub contrast: i32,
    /// Line height, percent of the font's.
    pub line_spacing: i32,
    /// Width of spaces, percent of the font's.
    pub word_spacing: i32,
    pub paragraphs: Paragraphs,
    pub alignment: Alignment,
    pub hyphenate: bool,
    /// The book's own CSS (sizes, indents, colours); off uses only Verso's.
    pub book_styles: bool,
    /// The old Narrow/Normal/Wide preset, read once to migrate settings into
    /// `PageLayout` and never written again.
    #[serde(rename = "margins", skip_serializing)]
    pub legacy_margins: Option<usize>,
}

impl Default for Typography {
    fn default() -> Self {
        Self {
            font: "Literata".into(),
            size_pt: 10.0,
            weight: 400,
            contrast: 0,
            line_spacing: 120,
            word_spacing: 100,
            paragraphs: Paragraphs::Book,
            alignment: Alignment::Book,
            hyphenate: true,
            book_styles: true,
            legacy_margins: None,
        }
    }
}

/// How paragraphs are set apart.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Paragraphs {
    /// As the book's CSS says.
    #[default]
    Book,
    /// A first-line indent and no gap.
    Indented,
    /// A gap and no indent.
    Spaced,
}
impl Paragraphs {
    pub const ALL: [Paragraphs; 3] = [Paragraphs::Book, Paragraphs::Indented, Paragraphs::Spaced];
}

/// Text alignment for body copy; overrides preserve `center` and `right`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Alignment {
    /// As the book's CSS says (justified by default via `epub.css`).
    #[default]
    Book,
    /// Ragged right (replaces `justify`, keeps `center` and `right`).
    Left,
    /// Justified (replaces `left`/`start` on `p, li`, keeps `center` and `right`).
    Justify,
}
impl Alignment {
    pub const ALL: [Alignment; 3] = [Alignment::Book, Alignment::Left, Alignment::Justify];
}

/// A setting's range and step.
#[derive(Clone, Copy, Debug)]
pub struct Steps {
    pub min: i32,
    pub max: i32,
    pub step: i32,
}
impl Steps {
    pub fn clamp(self, v: i32) -> i32 {
        v.clamp(self.min, self.max)
    }
    /// One step up (`more`) or down, within the range.
    pub fn next(self, v: i32, more: bool) -> i32 {
        self.clamp(v + if more { self.step } else { -self.step })
    }
}

impl Typography {
    pub const SIZE: Steps = Steps { min: 6, max: 24, step: 1 };
    pub const WEIGHT: Steps = Steps { min: 300, max: 700, step: 25 };
    /// Gamma 0.8 to 2.0 (crengine's table index 10 to 31).
    pub const CONTRAST: Steps = Steps { min: -5, max: 16, step: 1 };
    pub const LINE_SPACING: Steps = Steps { min: 90, max: 200, step: 5 };
    pub const WORD_SPACING: Steps = Steps { min: 50, max: 200, step: 10 };

    pub fn normalize(&mut self) {
        self.size_pt = if self.size_pt.is_finite() { self.size_pt.clamp(6.0, 24.0) } else { 10.0 };
        self.weight = Self::WEIGHT.clamp(self.weight);
        self.contrast = Self::CONTRAST.clamp(self.contrast);
        self.line_spacing = Self::LINE_SPACING.clamp(self.line_spacing);
        self.word_spacing = Self::WORD_SPACING.clamp(self.word_spacing);
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
    /// Sets only `Typography::contrast`: pages draw lighter or darker in the
    /// same layout.
    fn set_contrast(&mut self, contrast: i32);
    /// The contrast pages are drawn with now.
    fn contrast(&self) -> i32;

    fn page_count(&mut self) -> usize;
    /// 0-based.
    fn current_page(&mut self) -> usize;
    fn go_to_page(&mut self, page: usize);
    /// Draws the current page as 8-bit gray (0 = black), `stride` bytes per row.
    fn draw(&mut self, out: &mut [u8], stride: usize, dark: bool) -> Result<()>;
    /// Draws the current page as 32-bit opaque RGBA (`[R, G, B, 255]`), `stride` bytes per row.
    fn draw_rgba(&mut self, out: &mut [u8], stride: usize, dark: bool) -> Result<()>;
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
                font_dirs: vec![
                    root.join("assets/fonts"),
                    "/usr/share/fonts/liberation-serif-fonts".into(),
                    "/usr/share/fonts/liberation-mono-fonts".into(),
                ],
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
        let cache_dir = paths.cache.clone().unwrap();
        let book = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/reader.epub");
        let engine = CreEngine::new(paths, 300).expect("engine starts");
        let layout = Layout {
            width: 600,
            height: 800,
            margins: [40; 4],
            font_size: 30,
            shaping: TextShaping::Full,
            hanging_punctuation: true,
        };
        let mut doc = engine.open(&book, &layout, None).expect("book opens");
        let pages = doc.page_count();
        assert!(pages > 10, "{pages} pages");

        doc.go_to_page(5);
        assert_eq!(doc.current_page(), 5);
        let mut buf = vec![255u8; 600 * 800];
        doc.draw(&mut buf, 600, false).unwrap();
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
        let typography = Typography { size_pt: 12.0, alignment: Alignment::Left, ..Typography::default() };
        doc.set_typography(&typography, 150, &PageLayout::default()).unwrap();
        doc.go_to_position(&position).unwrap();
        assert!(doc.progress().abs_diff(progress) < 100, "position should preserve text progress");
        doc.draw(&mut buf, 600, false).unwrap();
        let left = buf.clone();
        let base = Typography { alignment: Alignment::Book, ..typography };
        doc.set_typography(&base, 150, &PageLayout::default()).unwrap();
        doc.draw(&mut buf, 600, false).unwrap();
        assert_ne!(left, buf, "alignment changes rendering");
        // Every other text setting reaches the engine; contrast only redraws,
        // set on its own.
        let justified = buf.clone();
        let page_count = doc.page_count();
        for changed in [
            Typography { weight: 550, ..base.clone() },
            Typography { contrast: 8, ..base.clone() },
            Typography { line_spacing: 150, ..base.clone() },
            Typography { word_spacing: 150, ..base.clone() },
            Typography { paragraphs: Paragraphs::Spaced, ..base.clone() },
            Typography { hyphenate: false, ..base.clone() },
        ] {
            if changed.contrast != 0 {
                doc.set_contrast(changed.contrast);
            } else {
                doc.set_typography(&changed, 150, &PageLayout::default()).unwrap();
            }
            doc.go_to_position(&position).unwrap();
            doc.draw(&mut buf, 600, false).unwrap();
            assert_ne!(justified, buf, "{changed:?} changes the page");
            if changed.contrast != 0 {
                assert_eq!(doc.page_count(), page_count, "contrast keeps the layout");
                let ink = |b: &[u8]| b.iter().map(|&g| 255 - g as u64).sum::<u64>();
                assert!(ink(&buf) > ink(&justified), "more contrast is darker");
            }
            doc.set_typography(&base, 150, &PageLayout::default()).unwrap();
        }
        doc.go_to_position(&position).unwrap();
        doc.draw(&mut buf, 600, false).unwrap();
        assert_eq!(justified, buf, "settings back as they were draw the same page");
        drop(doc);
        // Hanging punctuation is set when a book opens.
        let mut hanging = engine
            .open(
                &book,
                &Layout { hanging_punctuation: false, ..layout.clone() },
                Some((&base, &PageLayout::default())),
            )
            .unwrap();
        let mut hung = engine.open(&book, &layout, Some((&base, &PageLayout::default()))).unwrap();
        let (mut a, mut b) = (vec![255u8; 600 * 800], vec![255u8; 600 * 800]);
        hanging.go_to_page(5);
        hung.go_to_page(5);
        hanging.draw(&mut a, 600, false).unwrap();
        hung.draw(&mut b, 600, false).unwrap();
        assert_ne!(a, b, "punctuation hangs into the margin");
        drop((hanging, hung));
        // This book has no CSS of its own; the dictionary fixture's sets margins and line height.
        let styled = book.with_file_name("dictionary-words.epub");
        let mut other = engine.open(&styled, &layout, Some((&base, &PageLayout::default()))).unwrap();
        other.draw(&mut buf, 600, false).unwrap();
        let with_styles = buf.clone();
        other.set_typography(&Typography { book_styles: false, ..base.clone() }, 150, &PageLayout::default()).unwrap();
        other.draw(&mut buf, 600, false).unwrap();
        assert_ne!(with_styles, buf, "turning the book's styles off changes the page");
        drop(other);
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
        image_scaling_checks(&engine, &layout);
        bookmark_checks(&engine, &layout, &book);
        textless_bookmark_checks(&engine, &layout);
        alignment_checks(&engine, &layout);
        markdown_checks(&engine, &layout, &cache_dir).unwrap();
    }

    /// Book alignment keeps center/right `<p>` headers; Left and Justify
    /// override body paragraphs while still preserving center/right (direct
    /// or inherited from a parent container).
    fn alignment_checks(engine: &CreEngine, layout: &Layout) {
        use std::io::Write;
        let path = std::env::temp_dir().join(format!("verso-align-cre-{}.epub", std::process::id()));
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
            br#"<?xml version="1.0"?><package xmlns="http://www.idpf.org/2007/opf" version="2.0"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Alignment Fixture</dc:title></metadata><manifest><item id="ch1" href="ch1.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="ch1"/></spine></package>"#,
        )
        .unwrap();
        out.start_file("ch1.xhtml", opts).unwrap();
        out.write_all(
            br#"<?xml version="1.0" encoding="utf-8"?><html xmlns="http://www.w3.org/1999/xhtml"><head><title>Align</title><style>
.right { text-align: right; text-indent: 0; }
.center { text-align: center; text-indent: 0; }
.wrap { text-align: center; }
.inwrap { text-indent: 0; }
.just { text-align: justify; text-indent: 0; }
.rag { text-align: left; text-indent: 0; }
</style></head><body>
<p class="right">RightMark</p>
<p class="center">CenterMark</p>
<div class="wrap"><p class="inwrap">WrapMark</p></div>
<p class="just">JustStart alpha beta 123456789012345678901234567890 words follow on the next line.</p>
<p class="rag">RagStart alpha beta 123456789012345678901234567890 words follow on the next line.</p>
</body></html>"#,
        )
        .unwrap();
        out.finish().unwrap();

        let word_rect = |d: &mut CreDocument, q: &str| {
            d.search_start(q).unwrap();
            loop {
                let b = d.search_step(std::time::Duration::from_secs(1)).unwrap();
                if let Some(hit) = b.hits.first() {
                    return d.select(hit.page, &SelectionQuery::Range(hit.range.clone())).unwrap().unwrap().rects[0];
                }
                assert_eq!(b.status, SearchStatus::Searching, "missing {q}");
            }
        };
        let line_width = |d: &mut CreDocument, first_word: &str| {
            // Measure how far across the page the first line of the paragraph spans
            // by checking the rightmost dark pixel on that line's band.
            let r = word_rect(d, first_word);
            let mut buf = vec![255u8; 600 * 800];
            d.draw(&mut buf, 600, false).unwrap();
            let mut max_x = 0usize;
            for y in (r.y.max(0) as usize)..(r.bottom().min(800) as usize) {
                for x in (0..600).rev() {
                    if buf[y * 600 + x] < 200 {
                        max_x = max_x.max(x);
                        break;
                    }
                }
            }
            max_x as i32
        };

        let base = Typography { size_pt: 12.0, alignment: Alignment::Book, ..Typography::default() };
        let mut doc = engine.open(&path, layout, Some((&base, &PageLayout::default()))).unwrap();

        let check_headers = |d: &mut CreDocument, label: &str| {
            let right = word_rect(d, "RightMark");
            let center = word_rect(d, "CenterMark");
            let wrap = word_rect(d, "WrapMark");
            assert!(
                right.right() > 540 && right.x > 600 - right.right() + 150,
                "{label}: RightMark should be right-aligned: {right:?}"
            );
            assert!((center.x + center.right() - 600).abs() < 40, "{label}: CenterMark should be centered: {center:?}");
            assert!((wrap.x + wrap.right() - 600).abs() < 40, "{label}: WrapMark should inherit center: {wrap:?}");
        };

        // Book: right/center preserved; explicit justify stays justified, explicit left stays ragged.
        check_headers(&mut doc, "Book");
        let just_book = line_width(&mut doc, "JustStart");
        let rag_book = line_width(&mut doc, "RagStart");
        assert!(
            just_book > rag_book + 20,
            "Book: justified ({just_book}) should reach further right than ragged ({rag_book})"
        );

        // Left: explicit justify becomes ragged, while right/center stay untouched.
        doc.set_typography(&Typography { alignment: Alignment::Left, ..base.clone() }, 150, &PageLayout::default())
            .unwrap();
        check_headers(&mut doc, "Left");
        let just_left = line_width(&mut doc, "JustStart");
        assert!(
            just_left < just_book - 20,
            "Left: justified paragraph should become ragged ({just_left} vs {just_book})"
        );

        // Justify: explicit left becomes justified, while right/center stay untouched.
        doc.set_typography(&Typography { alignment: Alignment::Justify, ..base.clone() }, 150, &PageLayout::default())
            .unwrap();
        check_headers(&mut doc, "Justify");
        let rag_just = line_width(&mut doc, "RagStart");
        assert!(
            rag_just > rag_book + 20,
            "Justify: ragged paragraph should become justified ({rag_just} vs {rag_book})"
        );

        drop(doc);
        let _ = std::fs::remove_file(&path);
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
        assert_eq!(doc.annotation_keep(0, false).unwrap(), (5, 5));
        // A highlight of the words after the opening, on the same page.
        let after = TextRange { start: opening.range.end.clone(), end: long.range.end.clone() };
        assert!(doc.annotation_check(&after, rest).unwrap());
        assert_eq!(doc.annotation_keep(1, true).unwrap(), (5, 5));
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
        // A bigger font and wider margins move both to the page now holding
        // them. The kept ranges, located again without a check, land where
        // checking them again does, and keep painting.
        let wide = PageLayout {
            margins: crate::page_layout::Margins { sides: 14, top: 10, bottom: 10 },
            ..PageLayout::default()
        };
        doc.set_typography(&Typography { size_pt: 19.2, ..Typography::default() }, 150, &wide).unwrap();
        let kept: Vec<_> = (0..).map_while(|i| doc.annotation_kept(i)).collect();
        doc.annotation_clear();
        assert!(doc.bookmark_check(&opening.range, &opening.text).unwrap());
        let (page, _) = doc.annotation_keep(0, false).unwrap();
        assert!(page > 5, "a bigger font pushes the page later: {page}");
        assert!(doc.annotation_check(&after, rest).unwrap());
        let (first, last) = doc.annotation_keep(1, true).unwrap();
        assert_eq!(kept, vec![(0, page, page), (1, first, last)]);
        assert!(doc.annotation_page(first).contains(&1) && doc.annotation_page(last).contains(&1));
        doc.go_to_position(&opening.range.start).unwrap();
        assert_eq!(doc.current_page(), page);
        doc.annotation_clear();
        assert!(doc.bookmark_check(&opening.range, &opening.text).unwrap());
        doc.annotation_keep(0, false).unwrap();
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
        assert_eq!(doc.annotation_keep(0, false).unwrap(), (0, 0));
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
        // A bulk-imported bookmark has only its start: placing it reads the
        // words there (none on a picture page, or the text's) and it holds.
        let mut awaiting = crate::annotations::Annotation {
            fingerprint: String::new(),
            range: TextRange { start: cover.range.start.clone(), end: Position::default() },
            ..saved.clone()
        };
        assert!(awaiting.awaits_opening());
        assert_eq!(thread::place(&mut doc, &mut awaiting, "original"), Some(cover.clone()));
        assert!(!awaiting.awaits_opening() && thread::holds(&mut doc, &awaiting, "original"));
        let first = doc.page_count() - 1;
        doc.go_to_page(first);
        let words = doc.page_opening(80).unwrap();
        let mut awaiting = crate::annotations::Annotation {
            fingerprint: String::new(),
            range: TextRange { start: words.range.start.clone(), end: Position::default() },
            ..saved.clone()
        };
        assert_eq!(thread::place(&mut doc, &mut awaiting, "original").map(|o| o.text), Some(words.text.clone()));
        assert!(thread::holds(&mut doc, &awaiting, "original"));
        saved.range.start = Position("/body/DocFragment[7]/body/div".into());
        assert!(!thread::holds(&mut doc, &saved, "original"), "a start that no longer exists");
        let mut lost = crate::annotations::Annotation { fingerprint: String::new(), ..saved.clone() };
        assert_eq!(thread::place(&mut doc, &mut lost, "original"), None, "not in this book");
        assert!(lost.awaits_opening(), "left as it was");
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
        doc.draw_rgba(&mut rgba, layout.width as usize * 4, false).unwrap();
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

        // Dark-mode draw pre-flips colored text luma while keeping images readable after panel flip.
        let mut dark_rgba = vec![0xAAu8; layout.width as usize * layout.height as usize * 4];
        doc.draw_rgba(&mut dark_rgba, layout.width as usize * 4, true).unwrap();
        let flipped: Vec<[u8; 4]> =
            dark_rgba.as_chunks::<4>().0.iter().map(|p| [255 - p[0], 255 - p[1], 255 - p[2], p[3]]).collect();
        assert!(flipped.iter().all(|p| p[3] == 255), "all dark pixels stay opaque");
        assert_eq!(flipped[0], [0, 0, 0, 255], "top-left margin flips to dark paper");
        let dark_reds = flipped.iter().filter(|p| p[0] > 160 && p[1] < 130 && p[2] < 130).count();
        let dark_blues = flipped.iter().filter(|p| p[2] > 160 && p[0] < 130 && p[1] < 150).count();
        let dark_greens = flipped.iter().filter(|p| p[1] > 150 && p[0] < 60 && p[2] < 80).count();
        assert!(dark_reds > 20, "expected readable red heading pixels in dark mode, got {dark_reds}");
        assert!(dark_blues > 20, "expected readable blue link pixels in dark mode, got {dark_blues}");
        assert!(dark_greens > 2000, "expected intact green image pixels in dark mode, got {dark_greens}");

        // Compare against the 8-bit gray path: same page still draws cleanly.
        let mut gray = vec![0xAAu8; layout.width as usize * layout.height as usize];
        doc.draw(&mut gray, layout.width as usize, false).unwrap();
        assert_eq!(gray[0], 255, "top-left margin cleared to white on Gray8");
        assert!(gray.iter().filter(|&&v| v < 200).count() > 2000);
        drop(doc);
        let _ = std::fs::remove_file(&path);
    }

    /// Raster images scale only by `PROP_RENDER_DPI` (not legacy `PROP_IMG_SCALING_*`
    /// zoom-in multipliers) and use smooth interpolation on both RGBA and Gray8 paths.
    fn image_scaling_checks(engine: &CreEngine, layout: &Layout) {
        use std::collections::BTreeSet;
        use std::io::Write;
        let path = std::env::temp_dir().join(format!("verso-img-scale-{}.epub", std::process::id()));
        let mut png = std::io::Cursor::new(Vec::new());
        image::RgbImage::from_fn(
            16,
            16,
            |x, _| {
                if x < 8 { image::Rgb([10, 220, 10]) } else { image::Rgb([10, 10, 180]) }
            },
        )
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
        let mut out = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        let opts = zip::write::SimpleFileOptions::default();
        out.start_file("mimetype", opts.compression_method(zip::CompressionMethod::Stored)).unwrap();
        out.write_all(b"application/epub+zip").unwrap();
        out.start_file("META-INF/container.xml", opts).unwrap();
        out.write_all(
            br#"<?xml version="1.0"?><container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#,
        )
        .unwrap();
        out.start_file("content.opf", opts).unwrap();
        out.write_all(
            br#"<?xml version="1.0"?><package xmlns="http://www.idpf.org/opf" version="2.0"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Scale Fixture</dc:title></metadata><manifest><item id="block" href="block.xhtml" media-type="application/xhtml+xml"/><item id="inline" href="inline.xhtml" media-type="application/xhtml+xml"/><item id="img" href="split.png" media-type="image/png"/></manifest><spine><itemref idref="block"/><itemref idref="inline"/></spine></package>"#,
        )
        .unwrap();
        out.start_file("split.png", opts).unwrap();
        out.write_all(png.get_ref()).unwrap();
        out.start_file("block.xhtml", opts).unwrap();
        out.write_all(
            br#"<?xml version="1.0" encoding="utf-8"?><html xmlns="http://www.w3.org/1999/xhtml"><head><title>Block</title></head><body><div><img src="split.png" alt=""/></div></body></html>"#,
        )
        .unwrap();
        out.start_file("inline.xhtml", opts).unwrap();
        out.write_all(
            br#"<?xml version="1.0" encoding="utf-8"?><html xmlns="http://www.w3.org/1999/xhtml"><head><title>Inline</title></head><body><p>Before <img src="split.png" alt=""/> after.</p></body></html>"#,
        )
        .unwrap();
        out.finish().unwrap();

        let big_font = Layout { font_size: 52, ..layout.clone() };
        let mut doc = engine.open(&path, &big_font, None).unwrap();
        assert!(doc.page_count() >= 2);
        let (w, h) = (big_font.width as usize, big_font.height as usize);
        let mut rgba = vec![0u8; w * h * 4];
        let mut block_colors = BTreeSet::new();
        for (page, label) in [(0, "block"), (1, "inline")] {
            doc.go_to_page(page);
            doc.draw_rgba(&mut rgba, w * 4, false).unwrap();
            let (mut min_x, mut min_y, mut max_x, mut max_y) = (w, h, 0, 0);
            for (i, p) in rgba.as_chunks::<4>().0.iter().enumerate() {
                if p[0].max(p[1]).max(p[2]) - p[0].min(p[1]).min(p[2]) > 20 {
                    let (x, y) = (i % w, i / w);
                    min_x = min_x.min(x);
                    max_x = max_x.max(x);
                    min_y = min_y.min(y);
                    max_y = max_y.max(y);
                    if page == 0 {
                        block_colors.insert([p[0], p[1], p[2]]);
                    }
                }
            }
            let (iw, ih) = (max_x + 1 - min_x, max_y + 1 - min_y);
            assert!((46..=54).contains(&iw) && (46..=54).contains(&ih), "{label} scaled size {iw}x{ih}");
        }
        assert!(block_colors.len() >= 3, "expected smooth RGBA interpolation, got {block_colors:?}");

        doc.go_to_page(0);
        let mut gray = vec![255u8; w * h];
        doc.draw(&mut gray, w, false).unwrap();
        let shades: BTreeSet<u8> = gray.into_iter().filter(|&v| v < 250).collect();
        assert!(shades.len() >= 3, "expected smooth Gray8 interpolation, got {shades:?}");

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
            let name = entry.name().unwrap().into_owned();
            if name == "mimetype" || entry.is_dir() {
                continue;
            }
            let mut data = Vec::new();
            entry.read_to_end(&mut data).unwrap();
            out.start_file(&name, options).unwrap();
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
        let mut narrow = engine.open(&path, &Layout { width: 240, margins: [20; 4], ..layout.clone() }, None).unwrap();
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
        assert_eq!(doc.annotation_keep(9, true).unwrap(), (0, last_page), "a highlight reports its last page");
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

    fn markdown_checks(engine: &CreEngine, layout: &Layout, cache_dir: &std::path::Path) -> Result<()> {
        let dir = tempfile::tempdir()?;

        // 1. Open both `.md` and `.markdown` fixtures through `CreEngine::open`.
        let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let mut rich = engine.open(&fixtures.join("rich.md"), layout, None)?;
        assert!(rich.page_count() >= 1);
        let rich_meta = rich.metadata();
        assert_eq!(rich_meta.title, "Field Notes on E-Ink Typography");
        assert_eq!(rich_meta.language, "en");
        let mut buf = vec![255u8; 600 * 800];
        rich.draw(&mut buf, 600, false)?;
        assert!(buf.iter().filter(|&&v| v < 128).count() > 500, "rich.md rendered no ink");

        let mut prose = engine.open(&fixtures.join("prose.markdown"), layout, None)?;
        assert_eq!(prose.metadata().title, "Plain Prose Notes");
        assert!(prose.page_count() >= 1);
        drop(prose);

        // Invalid Markdown (NUL byte) is rejected before crengine opens it.
        let bad_md = dir.path().join("bad.md");
        std::fs::write(&bad_md, b"bad\0markdown")?;
        assert!(engine.open(&bad_md, layout, None).is_err());

        // 2. TOC built from h1-h6 and relative image loading on rich.md.
        let rich_toc = rich.toc()?;
        let rich_titles: Vec<&str> = rich_toc.iter().map(|e| e.title.as_str()).collect();
        assert!(rich_titles.contains(&"Field Notes on E-Ink Typography"), "{rich_titles:?}");
        assert!(rich_titles.contains(&"Code and Wrapping"), "{rich_titles:?}");
        assert!(rich_titles.contains(&"Figure"), "{rich_titles:?}");
        let mut rich_notes = std::collections::BTreeMap::new();
        for p in 0..rich.page_count() {
            rich.go_to_page(p);
            let info = rich.page_info()?;
            let mut ids: Vec<usize> = info.links.iter().map(|(id, _)| *id).collect();
            ids.dedup();
            for id in ids {
                if let Some(link) = rich.link(p, id)?
                    && let Some(note) = link.note
                {
                    rich_notes.insert(note.label, note.html);
                }
            }
        }
        // The image's page depends on the installed fonts. Its 32x48 checkerboard
        // of 180/220 grays renders at 100x150 (15,000 px) at 300 DPI, well above
        // text smoothing in the 190..=215 band.
        let mut figure = false;
        for p in 0..rich.page_count() {
            rich.go_to_page(p);
            rich.draw(&mut buf, 600, false)?;
            figure |= buf.iter().filter(|&&v| (190..=215).contains(&v)).count() > 10_000;
        }
        assert!(figure, "rich.md relative checkerboard image must render");
        assert!(rich_notes.get("1").is_some_and(|h| h.contains("First footnote explaining")), "{rich_notes:?}");
        assert!(
            rich_notes.get("2").is_some_and(|h| h.contains("Second footnote") && h.contains("<b>formatted</b>")),
            "{rich_notes:?}"
        );

        // 3. Text sheet `paragraphs` override over Markdown defaults.
        rich.go_to_page(0);
        let base_typo = Typography { paragraphs: Paragraphs::Book, ..Typography::default() };
        rich.set_typography(&base_typo, 150, &PageLayout::default())?;
        rich.draw(&mut buf, 600, false)?;
        let md_default_buf = buf.clone();
        rich.set_typography(
            &Typography { paragraphs: Paragraphs::Indented, ..base_typo.clone() },
            150,
            &PageLayout::default(),
        )?;
        rich.draw(&mut buf, 600, false)?;
        let indented_buf = buf.clone();
        assert_ne!(md_default_buf, indented_buf, "Paragraphs::Indented must override markdown.css");
        rich.set_typography(
            &Typography { paragraphs: Paragraphs::Spaced, ..base_typo.clone() },
            150,
            &PageLayout::default(),
        )?;
        rich.draw(&mut buf, 600, false)?;
        assert_ne!(indented_buf, buf, "Paragraphs::Spaced overrides Paragraphs::Indented");
        assert_eq!(md_default_buf, buf, "Markdown default matches spaced paragraphs");

        // 4. Search, word selection, and annotation XPointer round-trip across reopen.
        rich.set_typography(&base_typo, 150, &PageLayout::default())?;
        let mut hits = Vec::new();
        rich.search_start("restrained")?;
        loop {
            let batch = rich.search_step(std::time::Duration::from_secs(1))?;
            hits.extend(batch.hits);
            if batch.status != SearchStatus::Searching {
                break;
            }
        }
        assert_eq!(hits.len(), 1);
        let hit = &hits[0];
        let sel = rich.select(hit.page, &SelectionQuery::Range(hit.range.clone()))?.unwrap();
        assert_eq!(sel.text, "restrained");
        let word = rich
            .select(hit.page, &SelectionQuery::Word(sel.rects[0].center()))?
            .expect("word selection on Markdown page");
        assert_eq!(word.text, "restrained");
        rich.annotation_clear();
        assert!(rich.annotation_check(&word.range, &word.text)?);
        let (ann_page, _) = rich.annotation_keep(1, true)?;
        assert_eq!(rich.annotation_finish(1), vec![1]);
        assert_eq!(rich.annotation_page(ann_page), vec![1]);
        drop(rich);

        let mut reopened_rich =
            engine.open(&fixtures.join("rich.md"), layout, Some((&base_typo, &PageLayout::default())))?;
        assert!(reopened_rich.annotation_check(&word.range, &word.text)?);
        reopened_rich.go_to_position(&word.range.start)?;
        assert_eq!(reopened_rich.current_page(), ann_page);
        drop(reopened_rich);

        // 5. All h1-h6 levels, internal #anchor navigation, and actual green PNG decoding from relative path.
        let book_dir = dir.path().join("md-book");
        std::fs::create_dir_all(book_dir.join("sub"))?;
        let mut png = std::io::Cursor::new(Vec::new());
        image::RgbImage::from_pixel(120, 80, image::Rgb([20, 180, 40])).write_to(&mut png, image::ImageFormat::Png)?;
        std::fs::write(book_dir.join("green.png"), png.get_ref())?;
        std::fs::write(book_dir.join("sub/green.png"), png.get_ref())?;

        let md_path = book_dir.join("doc.md");
        let mut md_body = String::from(
            "---\ntitle: Anchor & Image Doc\nlang: en\n---\n\n\
             # One\n\nIntro with a note[^a].\n\n\
             ## Two\n### Three\n#### Four\n##### Five\n###### Six\n\n\
             ![Green sample](green.png)\n\n\
             ![Sub green sample](./sub/green.png)\n\n",
        );
        for i in 0..45 {
            md_body.push_str(&format!("Filler paragraph {i} pushing the anchor link onto a later page.\n\n"));
        }
        md_body.push_str("Jump [back to marker](#fnref-1).\n\n[^a]: Note body.\n");
        std::fs::write(&md_path, &md_body)?;

        let mut doc = engine.open(&md_path, layout, None)?;
        let toc = doc.toc()?;
        let levels: Vec<(usize, &str)> = toc.iter().map(|e| (e.level, e.title.as_str())).collect();
        for expected in [(1, "One"), (2, "Two"), (3, "Three"), (4, "Four"), (5, "Five"), (6, "Six")] {
            assert!(levels.contains(&expected), "missing heading {expected:?} in {levels:?}");
        }
        // Verify green.png was opened via m_doc container and drawn onto its page.
        let mut saw_green = false;
        let mut rgba = vec![0u8; 600 * 800 * 4];
        for p in 0..doc.page_count() {
            doc.go_to_page(p);
            if doc.page_info()?.has_images {
                doc.draw_rgba(&mut rgba, 600 * 4, false)?;
                let greens =
                    rgba.as_chunks::<4>().0.iter().filter(|px| px[1] > 150 && px[0] < 60 && px[2] < 80).count();
                if greens > 2000 {
                    saw_green = true;
                    break;
                }
            }
        }
        assert!(saw_green, "relative green.png image must decode and draw");

        // Follow the internal `#fnref-1` jump link from the later page back to page 0.
        let mut jumped = false;
        for p in 1..doc.page_count() {
            doc.go_to_page(p);
            for (id, _) in doc.page_links(p) {
                if let Some(link) = doc.link(p, id)?
                    && link.note.is_none()
                {
                    doc.go_to_position(&link.target)?;
                    assert_eq!(doc.current_page(), 0, "#fnref-1 should jump back to page 0");
                    jumped = true;
                    break;
                }
            }
            if jumped {
                break;
            }
        }
        assert!(jumped, "expected internal #anchor jump link to resolve");
        drop(doc);

        // 6. .cr3 cache hit on reopen and invalidation after editing the .md file.
        let cr3_count = || {
            std::fs::read_dir(cache_dir)
                .unwrap()
                .filter_map(|e| e.ok())
                .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("cr3"))
                .count()
        };
        assert!(cr3_count() >= 1, "expected .cr3 cache file after closing doc");
        let mut cached = engine.open(&md_path, layout, None)?;
        cached.search_start("FreshEditToken")?;
        let mut found_before = 0usize;
        while let Ok(b) = cached.search_step(std::time::Duration::from_secs(1)) {
            found_before += b.hits.len();
            if b.status != SearchStatus::Searching {
                break;
            }
        }
        assert_eq!(found_before, 0);
        drop(cached);

        md_body.push_str("\nFreshEditToken appended after caching.\n");
        std::fs::write(&md_path, &md_body)?;
        let mut invalidated = engine.open(&md_path, layout, None)?;
        invalidated.search_start("FreshEditToken")?;
        let mut found_after = 0usize;
        while let Ok(b) = invalidated.search_step(std::time::Duration::from_secs(1)) {
            found_after += b.hits.len();
            if b.status != SearchStatus::Searching {
                break;
            }
        }
        assert_eq!(found_after, 1, "editing .md must invalidate .cr3 cache");
        drop(invalidated);

        // 7. Frontmatter `lang:` sets hyphenation language.
        let ru_path = book_dir.join("ru.md");
        std::fs::write(&ru_path, "---\nlang: ru\n---\n\nдостопримечательность\n")?;
        let narrow = Layout { width: 240, height: 400, margins: [20; 4], ..layout.clone() };
        let hyph_on = Typography { hyphenate: true, ..Typography::default() };
        let hyph_off = Typography { hyphenate: false, ..Typography::default() };
        let mut ru_doc = engine.open(&ru_path, &narrow, Some((&hyph_on, &PageLayout::default())))?;
        assert_eq!(ru_doc.metadata().language, "ru");
        let mut narrow_buf = vec![255u8; 240 * 400];
        ru_doc.draw(&mut narrow_buf, 240, false)?;
        let ru_with_hyph = narrow_buf.clone();
        ru_doc.set_typography(&hyph_off, 150, &PageLayout::default())?;
        ru_doc.draw(&mut narrow_buf, 240, false)?;
        assert_ne!(ru_with_hyph, narrow_buf, "frontmatter lang: ru must drive Russian hyphenation");
        drop(ru_doc);

        // 8. Monospace preserved under a non-Publisher proportional body font.
        let mono_path = book_dir.join("mono.md");
        std::fs::write(&mono_path, "Proportional prose.\n\n```text\niiiiii\nMMMMMM\n```\n")?;
        let mut mono_doc = engine.open(&mono_path, layout, Some((&base_typo, &PageLayout::default())))?;
        let word_width = |d: &mut CreDocument, q: &str| -> Result<i32> {
            d.search_start(q)?;
            loop {
                let b = d.search_step(std::time::Duration::from_secs(1))?;
                if let Some(hit) = b.hits.first() {
                    let s = d.select(hit.page, &SelectionQuery::Range(hit.range.clone()))?.unwrap();
                    return Ok(s.rects[0].w);
                }
                if b.status != SearchStatus::Searching {
                    anyhow::bail!("missing {q}");
                }
            }
        };
        let i_w = word_width(&mut mono_doc, "iiiiii")?;
        let m_w = word_width(&mut mono_doc, "MMMMMM")?;
        if std::path::Path::new("/usr/share/fonts/liberation-mono-fonts").is_dir() {
            assert_eq!(i_w, m_w, "code block must keep monospace under Literata ({i_w} vs {m_w})");
        }
        Ok(())
    }
}
