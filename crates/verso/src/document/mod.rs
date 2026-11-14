//! Books. The rest of the app talks to the `Document` trait and never to the
//! engine directly, so the engine could be swapped (e.g. for MuPDF) later.

mod crengine;
pub mod thread;

use std::path::PathBuf;

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

/// Stable DOM positions, with an exclusive end. Never pixel offsets or page numbers.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TextRange {
    pub start: Position,
    pub end: Position,
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
    Quotation(TextRange),
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
    /// Resolve original endpoints and validate their quotation; never search
    /// for an alternative location when the content/engine has changed.
    fn validate_range(&mut self, range: &TextRange, quotation: &str) -> Result<Option<Selection>> {
        let expected = crate::annotations::normalize(quotation);
        Ok(self
            .select(0, &SelectionQuery::Quotation(range.clone()))?
            .filter(|s| !expected.is_empty() && crate::annotations::normalize(&s.text) == expected))
    }
    /// Starts a literal, case-insensitive search from the start of the book.
    fn search_start(&mut self, query: &str) -> Result<()>;
    /// Continues the search for about `budget`, returning only the new hits.
    fn search_step(&mut self, budget: std::time::Duration) -> Result<SearchBatch>;
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
    /// (colon-separated) and VERSO_FONT_FACE.
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
        drop(reopened);
        selection_checks(&engine, &layout);
        search_checks(&engine, &layout);
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
        doc.set_typography(&Typography { size_pt: 16.0, ..Typography::default() }, 150, true).unwrap();
        assert_eq!(doc.search_step(std::time::Duration::from_secs(1)).unwrap().status, SearchStatus::Finished);
        let (relaid, _) = search_all(&mut doc, "ada", std::time::Duration::from_secs(1));
        assert_eq!(
            relaid.iter().map(|h| &h.range).collect::<Vec<_>>(),
            hits.iter().map(|h| &h.range).collect::<Vec<_>>()
        );
        assert!(relaid.last().unwrap().page > hits.last().unwrap().page);
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
        doc.set_typography(&Typography { size_pt: 12.0, ..Typography::default() }, 150, false).unwrap();
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
        assert_eq!(doc.annotation_add(9, &all).unwrap(), 0);
        assert_eq!(doc.annotation_finish(1), vec![9]);
        for page in 0..=last_page {
            assert_eq!(doc.annotation_page(page), vec![9]);
        }
        let reversed = TextRange { start: all.end.clone(), end: all.start.clone() };
        assert_eq!(doc.select(0, &SelectionQuery::Range(reversed)).unwrap().unwrap().text, clipped.text);
        assert!(clipped.rects.iter().all(|r| r.y >= 0 && r.bottom() <= layout.height as i32));
        assert!(doc.validate_range(&all, &clipped.text).unwrap().is_some());
        assert!(doc.validate_range(&all, "Edited quotation").unwrap().is_none());
        let root = std::env::temp_dir().join(format!("verso-highlight-milestone-{}", std::process::id()));
        let store = crate::store::Store::at(root.clone());
        let mut collection = crate::annotations::Collection::load(&store, "milestone").unwrap();
        collection.add(crate::annotations::Annotation {
            version: 1,
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
        landscape.set_typography(&Typography { size_pt: 18.0, ..Typography::default() }, 150, true).unwrap();
        let saved = &restored.records[0];
        let range = landscape.validate_range(&saved.range, &saved.quotation).unwrap().unwrap();
        assert_eq!(range.range, all);
        assert_eq!(range.text, clipped.text);
        landscape.go_to_position(&saved.range.start).unwrap();
        let jump = landscape.current_page();
        assert!(landscape.select(jump, &SelectionQuery::Geometry(all)).unwrap().unwrap().start_visible);
        std::fs::remove_dir_all(root).unwrap();
    }
}
