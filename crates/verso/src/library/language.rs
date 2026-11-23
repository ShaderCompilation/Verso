//! The book's language, detected once from a sample of its own text with
//! whatlang. EPUB language tags are often missing or wrong (conversion tools
//! stamp `en` on books in any language), so a reliable detection beats the
//! tag.
use super::epub;
use crate::dictionary::languages::normalize_language;
use anyhow::{Context, Result, ensure};
use quick_xml::{Reader, events::Event};
use serde::{Deserialize, Serialize};
use std::io::{BufReader, Read, Seek};
use std::path::Path;
use zip::ZipArchive;

/// Bump when detection changes enough that stored results should be redone:
/// books detected with an older version are detected again on their next open.
pub const DETECTOR_VERSION: u32 = 1;
/// The most text detection reads, in bytes: plenty for trigram statistics,
/// and quick to classify.
pub const SAMPLE_LIMIT: usize = 8 * 1024;

#[derive(Clone, Copy)]
struct Limits {
    /// Spine documents larger than this, uncompressed, are skipped.
    entry: u64,
    /// At most this much is decompressed for one sample, across documents.
    read: u64,
}
const LIMITS: Limits = Limits { entry: 16 * 1024 * 1024, read: 32 * 1024 * 1024 };

/// What detection found, kept in the book's reading state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetectedLanguage {
    /// Normalized code (`de`); empty when the text had no language whatlang knows.
    pub code: String,
    /// whatlang's own judgement; only reliable detections override the tag.
    pub reliable: bool,
    pub version: u32,
}
impl DetectedLanguage {
    fn current(&self) -> bool {
        self.version == DETECTOR_VERSION
    }
    /// The detected language when it may override the tag.
    pub fn trusted(&self) -> Option<&str> {
        (self.current() && self.reliable && !self.code.is_empty()).then_some(self.code.as_str())
    }
}

/// The book language lookups use, by the trust order: a reliable detection,
/// then the EPUB tag. (A dictionary set chosen for the book beats both; the
/// script default comes after both. The dictionary sets apply those two.)
pub fn resolve(tag: &str, detected: Option<&DetectedLanguage>) -> String {
    detected.and_then(DetectedLanguage::trusted).map_or_else(|| tag.to_owned(), str::to_owned)
}

/// The book's language for Book details: `German (detected; tagged English)`
/// when a reliable detection and the tag differ.
pub fn describe(tag: &str, detected: Option<&DetectedLanguage>) -> String {
    let tag = tag.trim();
    let tagged = normalize_language(tag);
    let tag_name = if crate::dictionary::languages::language_name(&tagged) == tagged {
        tag.to_owned()
    } else {
        crate::dictionary::languages::language_name(&tagged).to_owned()
    };
    match detected.and_then(DetectedLanguage::trusted) {
        Some(code) if code == tagged => tag_name,
        Some(code) => {
            let name = crate::dictionary::languages::language_name(code);
            if tag.is_empty() { format!("{name} (detected)") } else { format!("{name} (detected; tagged {tag_name})") }
        }
        None if tag.is_empty() => "Unknown".into(),
        None => tag_name,
    }
}

/// True when the book has no result from the current detector.
pub fn needs_detection(detected: Option<&DetectedLanguage>) -> bool {
    detected.is_none_or(|d| !d.current())
}

/// Reads a sample of the book's text and detects its language. An error
/// means the text couldn't be read; the caller stores nothing.
pub fn detect_book(path: &Path) -> Result<DetectedLanguage> {
    Ok(detect(&sample(path)?))
}

pub fn detect(text: &str) -> DetectedLanguage {
    let info = whatlang::detect(text);
    DetectedLanguage {
        code: info.as_ref().map(|i| normalize_language(i.lang().code())).unwrap_or_default(),
        reliable: info.is_some_and(|i| i.is_reliable()),
        version: DETECTOR_VERSION,
    }
}

/// Explicit opt-in diagnostic (`VERSO_LANGUAGE_BENCH=1`): detects the language
/// of every available EPUB in the library, read only, and reports how long it
/// took, how many books have no tag (or `und`), and how many reliable
/// detections disagree with a real tag.
/// Nothing is stored. Mismatches are listed by file name for checking by hand.
pub fn benchmark(index: &super::LibraryIndex) -> serde_json::Value {
    let (mut times, mut unreadable, mut unreliable, mut mismatches) = (Vec::new(), 0, 0, Vec::new());
    let mut untagged = 0;
    for book in index.books.iter().filter(|b| b.available) {
        if !book.path.extension().is_some_and(|e| e.eq_ignore_ascii_case("epub")) {
            continue;
        }
        let tag = normalize_language(&book.metadata.language);
        let started = std::time::Instant::now();
        let detected = detect_book(&book.path);
        let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
        match detected {
            Err(_) => unreadable += 1,
            Ok(d) => {
                times.push(elapsed_ms);
                // "und" (undetermined) is no tag at all.
                let tagged = !tag.is_empty() && tag != "und";
                untagged += !tagged as u32;
                match d.trusted() {
                    None => unreliable += 1,
                    Some(code) if tagged && code != tag => mismatches.push(serde_json::json!({
                        "file": book.path.file_name().map(|n| n.to_string_lossy().into_owned()),
                        "tag": tag,
                        "detected": code,
                    })),
                    Some(_) => {}
                }
            }
        }
    }
    times.sort_by(f64::total_cmp);
    let at = |p: usize| times.get((times.len() * p).div_ceil(100).saturating_sub(1)).copied().unwrap_or(0.0);
    serde_json::json!({
        "books_detected": times.len(),
        "unreadable": unreadable,
        "untagged": untagged,
        "unreliable_or_unknown": unreliable,
        "tag_mismatches": mismatches.len(),
        "mismatches": mismatches,
        "detect_p50_ms": at(50), "detect_p90_ms": at(90), "detect_max_ms": at(100),
        "detect_total_ms": times.iter().sum::<f64>(),
    })
}

/// Up to `SAMPLE_LIMIT` bytes of body text centred on the middle of the
/// book's spine, skipping `linear="no"` documents, markup, scripts and styles.
/// The middle avoids front and back matter, which is often in another
/// language (an English licence around a German book).
pub fn sample(path: &Path) -> Result<String> {
    sample_with(path, LIMITS)
}

fn sample_with(path: &Path, limits: Limits) -> Result<String> {
    let mut zip = epub::open(path)?;
    let documents = spine(&mut zip, limits)?;
    ensure!(!documents.is_empty(), "EPUB has no text documents");
    // The middle by uncompressed size: markup is spread about evenly, so this
    // is close to the middle of the text without reading the book twice.
    let total: u64 = documents.iter().map(|d| d.size).sum();
    let start = (total / 2).saturating_sub(SAMPLE_LIMIT as u64 / 2);
    let mut text = collect(&mut zip, &documents, start, limits)?;
    if text.len() < SAMPLE_LIMIT && start > 0 {
        // Not enough text after the middle (a short book): read it all.
        text = collect(&mut zip, &documents, 0, limits)?;
    }
    Ok(truncate(text))
}

struct Document {
    name: String,
    size: u64,
}

/// The linear spine documents present in the archive and within the size limit.
fn spine<R: Read + Seek>(zip: &mut ZipArchive<R>, limits: Limits) -> Result<Vec<Document>> {
    let (package_path, package) = epub::package(zip)?;
    let manifest = package.find("manifest").context("EPUB has no manifest")?;
    let spine = package.find("spine").context("EPUB has no spine")?;
    let mut documents = Vec::new();
    for itemref in spine.all("itemref").filter(|n| n.attr("linear") != "no") {
        let Some(item) = manifest.all("item").find(|n| n.attr("id") == itemref.attr("idref")) else { continue };
        let href = item.attr("href");
        let html = item.attr("media-type").contains("html")
            || (item.attr("media-type").is_empty()
                && [".xhtml", ".html", ".htm"].iter().any(|ext| href.to_ascii_lowercase().ends_with(ext)));
        let Ok(name) = epub::resolve(&package_path, href) else { continue };
        if !html || documents.iter().any(|d: &Document| d.name == name) {
            continue;
        }
        let Ok(size) = zip.by_name(&name).map(|f| f.size()) else { continue };
        if size <= limits.entry {
            documents.push(Document { name, size });
        }
    }
    Ok(documents)
}

/// Text from the documents starting at uncompressed byte `start` of the
/// spine, until the sample is full. Fails only when no document could be read.
fn collect<R: Read + Seek>(
    zip: &mut ZipArchive<R>,
    documents: &[Document],
    start: u64,
    limits: Limits,
) -> Result<String> {
    let mut text = Text::default();
    let mut offset = 0;
    let mut budget = limits.read;
    let mut read_any = false;
    let mut last_error = None;
    for document in documents {
        let end = offset + document.size;
        if end <= start {
            offset = end;
            continue;
        }
        let skip = start.saturating_sub(offset);
        offset = end;
        if budget == 0 {
            break;
        }
        match document_text(zip, &document.name, skip, &mut budget, &mut text) {
            Ok(()) => read_any = true,
            Err(error) => {
                log::debug!("language sample: {}: {error:#}", document.name);
                last_error = Some(error);
            }
        }
        if text.full() {
            break;
        }
    }
    if text.out.is_empty()
        && !read_any
        && let Some(error) = last_error
    {
        return Err(error.context("no readable text in the EPUB"));
    }
    Ok(text.out)
}

/// Collapses whitespace and stops a little past the sample size.
#[derive(Default)]
struct Text {
    out: String,
    space: bool,
}
impl Text {
    fn push(&mut self, s: &str) {
        for c in s.chars() {
            if c.is_whitespace() {
                self.space = true;
            } else {
                if self.space && !self.out.is_empty() {
                    self.out.push(' ');
                }
                self.space = false;
                self.out.push(c);
            }
        }
    }
    fn full(&self) -> bool {
        self.out.len() >= SAMPLE_LIMIT
    }
}

/// Elements that don't separate words; every other tag does.
const INLINE: &[&str] = &[
    "a", "abbr", "b", "bdi", "bdo", "big", "cite", "code", "del", "dfn", "em", "font", "i", "ins", "kbd", "mark", "q",
    "s", "samp", "small", "span", "strike", "strong", "sub", "sup", "tt", "u", "var", "wbr",
];
/// Elements whose content isn't body text.
const HIDDEN: &[&str] = &["head", "script", "style", "title", "noscript", "template"];

fn document_text<R: Read + Seek>(
    zip: &mut ZipArchive<R>,
    name: &str,
    skip: u64,
    budget: &mut u64,
    text: &mut Text,
) -> Result<()> {
    let file = zip.by_name(name)?;
    let limit = (*budget).min(file.size());
    let before = text.out.len();
    let mut reader = Reader::from_reader(BufReader::new(file.take(limit)));
    let config = reader.config_mut();
    config.check_end_names = false;
    config.allow_unmatched_ends = true;
    let mut buf = Vec::new();
    let mut hidden = 0usize;
    let result = loop {
        buf.clear();
        let event = match reader.read_event_into(&mut buf) {
            Ok(event) => event,
            Err(error) => break Err(error.into()),
        };
        let collecting = hidden == 0 && reader.buffer_position() > skip;
        match event {
            Event::Start(e) => {
                let tag = e.local_name().as_ref().to_ascii_lowercase();
                if HIDDEN.contains(&std::str::from_utf8(&tag).unwrap_or_default()) {
                    hidden += 1;
                } else if !INLINE.contains(&std::str::from_utf8(&tag).unwrap_or_default()) {
                    text.space = true;
                }
            }
            Event::Empty(e) => {
                let tag = e.local_name().as_ref().to_ascii_lowercase();
                if !INLINE.contains(&std::str::from_utf8(&tag).unwrap_or_default()) {
                    text.space = true;
                }
            }
            Event::End(e) => {
                let tag = e.local_name().as_ref().to_ascii_lowercase();
                let tag = std::str::from_utf8(&tag).unwrap_or_default();
                if HIDDEN.contains(&tag) {
                    hidden = hidden.saturating_sub(1);
                } else if !INLINE.contains(&tag) {
                    text.space = true;
                }
            }
            Event::Text(e) if collecting => text.push(&String::from_utf8_lossy(&e)),
            Event::CData(e) if collecting => text.push(&String::from_utf8_lossy(&e)),
            Event::GeneralRef(e) if collecting => {
                let resolved = if e.is_char_ref() {
                    e.resolve_char_ref().ok().flatten().map(String::from)
                } else {
                    e.decode()
                        .ok()
                        .and_then(|name| quick_xml::escape::resolve_predefined_entity(&name))
                        .map(String::from)
                };
                text.push(resolved.as_deref().unwrap_or(" "));
            }
            Event::Eof => break Ok(()),
            _ => {}
        }
        if text.full() {
            break Ok(());
        }
    };
    *budget -= reader.buffer_position().min(*budget);
    // Malformed markup (or a document cut off by the read budget) ends the
    // document; the text before it still counts.
    if text.out.len() > before { Ok(()) } else { result }
}

/// The sample cut to `SAMPLE_LIMIT` bytes, at a word boundary when there is one.
fn truncate(mut text: String) -> String {
    if text.len() > SAMPLE_LIMIT {
        let mut end = SAMPLE_LIMIT;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(text[..end].rfind(' ').unwrap_or(end));
    }
    text
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::library::epub::tests::fixture;

    pub const GERMAN: &str = "Am Rand des kleinen Dorfes stand ein altes Haus mit grünen Fensterläden. Jeden Morgen öffnete die Großmutter die Tür, fütterte die Hühner und sah lange über die Felder, bis der Nebel sich hob. Die Kinder aus der Nachbarschaft kamen gern zu ihr, denn sie erzählte Geschichten von Füchsen, die sprechen konnten, und von einem Brunnen, in dem der Mond wohnte. Im Winter war der Weg zum Haus oft verschneit, aber in der Küche brannte immer ein warmes Feuer, und auf dem Tisch stand frisches Brot. ";
    pub const FRENCH: &str = "Le petit port s'éveillait lentement sous un ciel couleur de perle. Les pêcheurs rangeaient leurs filets pendant que la boulangère ouvrait ses volets et que l'odeur du pain chaud se répandait dans les ruelles. Sur le quai, un vieil homme lisait son journal en buvant un café noir, sans jamais lever les yeux vers les mouettes qui se disputaient les restes du marché. Quand la marée remontait, les enfants couraient le long de la digue en criant, et leurs voix se mêlaient au bruit des vagues. ";
    pub const ENGLISH: &str = "The lighthouse keeper climbed the narrow stairs every evening, carrying a lantern and a small notebook in which he wrote down the weather. From the top he could see the whole bay, the fishing boats coming home and the long road that wound along the cliffs toward the town. He rarely had visitors, but he did not mind the quiet; the wind and the gulls were company enough, and on clear nights the stars seemed close enough to touch. ";

    fn opf(items: &[(&str, &str, bool)]) -> String {
        let manifest: String = items
            .iter()
            .map(|(id, href, _)| format!(r#"<item id="{id}" href="{href}" media-type="application/xhtml+xml"/>"#))
            .collect();
        let spine: String = items
            .iter()
            .map(|(id, _, linear)| {
                format!(r#"<itemref idref="{id}"{}/>"#, if *linear { "" } else { r#" linear="no""# })
            })
            .collect();
        format!(r#"<package><metadata/><manifest>{manifest}</manifest><spine>{spine}</spine></package>"#)
    }
    fn page(body: &str) -> Vec<u8> {
        format!(r#"<?xml version="1.0" encoding="utf-8"?><html xmlns="http://www.w3.org/1999/xhtml"><head><title>Kapitel</title></head><body><p>{body}</p></body></html>"#).into_bytes()
    }
    fn book(dir: &Path, name: &str, chapters: &[(&str, bool)]) -> std::path::PathBuf {
        let path = dir.join(name);
        let items: Vec<_> =
            (0..chapters.len()).map(|i| (format!("c{i}"), format!("c{i}.xhtml"), chapters[i].1)).collect();
        let refs: Vec<_> = items.iter().map(|(id, href, linear)| (id.as_str(), href.as_str(), *linear)).collect();
        let pages: Vec<_> = chapters.iter().map(|(body, _)| page(body)).collect();
        let names: Vec<_> = items.iter().map(|(_, href, _)| format!("OPS/{href}")).collect();
        let extra: Vec<_> = names.iter().zip(&pages).map(|(n, p)| (n.as_str(), p.as_slice())).collect();
        fixture(&path, &opf(&refs), &extra).unwrap();
        path
    }

    #[test]
    fn german_french_and_english_books_are_detected_reliably() {
        let dir = tempfile::tempdir().unwrap();
        for (text, code) in [(GERMAN, "de"), (FRENCH, "fr"), (ENGLISH, "en")] {
            let path = book(dir.path(), &format!("{code}.epub"), &[(&text.repeat(3), true)]);
            let detected = detect_book(&path).unwrap();
            assert_eq!(detected, DetectedLanguage { code: code.into(), reliable: true, version: DETECTOR_VERSION });
        }
    }

    #[test]
    fn an_english_header_and_footer_around_german_text_is_german() {
        // A single-file book, like many Project Gutenberg EPUBs: the licence
        // text before and after is longer than the whole sample.
        let dir = tempfile::tempdir().unwrap();
        let english = ENGLISH.repeat(25);
        let body = format!("{english}</p><p>{}</p><p>{english}", GERMAN.repeat(40));
        let path = book(dir.path(), "header.epub", &[(&body, true)]);
        let text = sample(&path).unwrap();
        assert!(english.len() > SAMPLE_LIMIT);
        assert!(text.contains("Großmutter") && !text.contains("lighthouse"), "{text}");
        assert_eq!(detect_book(&path).unwrap().code, "de");
    }

    #[test]
    fn the_sample_comes_from_the_middle_of_the_linear_spine() {
        let dir = tempfile::tempdir().unwrap();
        let path = book(
            dir.path(),
            "spine.epub",
            &[
                (&ENGLISH.repeat(2), true),
                (&FRENCH.repeat(40), false),
                (&GERMAN.repeat(2), true),
                (&ENGLISH.repeat(2), true),
            ],
        );
        let text = sample(&path).unwrap();
        // The short book is read whole, in spine order, without the non-linear chapter.
        assert!(text.starts_with("The lighthouse keeper") && text.contains("Großmutter"), "{text}");
        assert!(!text.contains("pêcheurs"));
        // A long book: only the middle chapter is sampled.
        let path = book(
            dir.path(),
            "long.epub",
            &[(&ENGLISH.repeat(40), true), (&GERMAN.repeat(40), true), (&ENGLISH.repeat(40), true)],
        );
        let text = sample(&path).unwrap();
        assert!(text.contains("Großmutter") && !text.contains("lighthouse"));
        assert!(text.len() <= SAMPLE_LIMIT && text.len() > SAMPLE_LIMIT - 100, "{}", text.len());
    }

    #[test]
    fn markup_scripts_and_styles_are_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("markup.epub");
        let html = r#"<?xml version="1.0"?><!DOCTYPE html><html xmlns="http://www.w3.org/1999/xhtml"><head><title>Titel</title><style>p { color: red }</style></head><body><script>var englisch = "hello";</script><h1>Erstes&#160;Kapitel</h1><p>Der <b>Hund</b> und&nbsp;die <i>Katze</i>&amp;&#x00E9;&unknown;<br/>schliefen.</p><style>.x{}</style><p>Ende</p></body></html>"#;
        fixture(&path, &opf(&[("a", "a.xhtml", true)]), &[("OPS/a.xhtml", html.as_bytes())]).unwrap();
        assert_eq!(sample(&path).unwrap(), "Erstes Kapitel Der Hund und die Katze&é schliefen. Ende");
    }

    #[test]
    fn size_limits_skip_large_documents_and_bound_reading() {
        let dir = tempfile::tempdir().unwrap();
        let big = GERMAN.repeat(10);
        let path = book(dir.path(), "limits.epub", &[(&big, true), (ENGLISH, true)]);
        // The German chapter is over the entry limit: only English is left.
        let text = sample_with(&path, Limits { entry: 2000, read: LIMITS.read }).unwrap();
        assert!(text.starts_with("The lighthouse") && !text.contains("Haus"), "{text}");
        // A tight read budget stops early but keeps what it read.
        let text = sample_with(&path, Limits { entry: LIMITS.entry, read: 1000 }).unwrap();
        assert!(!text.is_empty() && text.len() < 1000, "{}", text.len());
    }

    #[test]
    fn an_unreadable_book_has_no_detection_and_keeps_its_tag() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken.epub");
        std::fs::write(&path, b"not an EPUB").unwrap();
        assert!(detect_book(&path).is_err());
        // A spine whose only document is missing.
        fixture(&path, &opf(&[("a", "a.xhtml", true)]), &[]).unwrap();
        assert!(detect_book(&path).is_err());
        assert_eq!(resolve("en", None), "en");
        // Pictures only: readable, but no language.
        fixture(&path, &opf(&[("a", "a.xhtml", true)]), &[("OPS/a.xhtml", &page(r#"<img src="a.png"/>"#))]).unwrap();
        let detected = detect_book(&path).unwrap();
        assert_eq!(detected.code, "");
        assert!(!needs_detection(Some(&detected)));
        assert_eq!(resolve("en", Some(&detected)), "en");
    }

    #[test]
    fn trust_order_and_detector_version() {
        let detected = |code: &str, reliable, version| DetectedLanguage { code: code.into(), reliable, version };
        // A reliable detection beats the tag; an unreliable one doesn't.
        assert_eq!(resolve("en", Some(&detected("de", true, DETECTOR_VERSION))), "de");
        assert_eq!(resolve("", Some(&detected("de", true, DETECTOR_VERSION))), "de");
        assert_eq!(resolve("en", Some(&detected("de", false, DETECTOR_VERSION))), "en");
        // A result from an older detector is ignored and detected again.
        let old = detected("de", true, DETECTOR_VERSION - 1);
        assert_eq!(resolve("en", Some(&old)), "en");
        assert!(needs_detection(Some(&old)));
        assert!(needs_detection(None));
        assert!(!needs_detection(Some(&detected("de", false, DETECTOR_VERSION))));
        // Short or mixed text is not reliable.
        assert!(!detect("Hallo").reliable);
    }

    #[test]
    fn book_details_show_the_detected_and_tagged_language() {
        let german = DetectedLanguage { code: "de".into(), reliable: true, version: DETECTOR_VERSION };
        let unsure = DetectedLanguage { reliable: false, ..german.clone() };
        for (tag, detected, expected) in [
            ("en", Some(&german), "German (detected; tagged English)"),
            ("", Some(&german), "German (detected)"),
            ("de-DE", Some(&german), "German"),
            ("en", Some(&unsure), "English"),
            ("x-klingon", Some(&german), "German (detected; tagged x-klingon)"),
            ("x-klingon", None, "x-klingon"),
            ("", None, "Unknown"),
        ] {
            assert_eq!(describe(tag, detected), expected, "{tag} {detected:?}");
        }
    }

    #[test]
    fn a_chosen_set_beats_the_detected_language() {
        use crate::dictionary::sets::{Edit, Preferences, Selection};
        let mut p = Preferences::default();
        for (id, language) in [("english", "en"), ("german", "de")] {
            p.edit(Edit::Create { id: id.into(), name: id.into() }).unwrap();
            p.edit(Edit::Languages { set: id.into(), languages: vec![language.into()] }).unwrap();
        }
        let german = DetectedLanguage { code: "de".into(), reliable: true, version: DETECTOR_VERSION };
        let unsure = DetectedLanguage { reliable: false, ..german.clone() };
        for (book_set, detected, expected) in [
            (None, Some(&german), "german"),
            (None, Some(&unsure), "english"),
            (Some("english"), Some(&german), "english"),
        ] {
            let selection = Selection { book_set: book_set.map(str::to_owned), language: resolve("en", detected) };
            assert_eq!(p.choose(&selection, "Häuser").id, expected);
        }
        // Word forms follow the detected language in a set without languages.
        let default = p.set(crate::dictionary::sets::DEFAULT);
        assert_eq!(default.form_languages(&resolve("en", Some(&german)), "Häuser"), ["de", "en"]);
    }

    /// Detects every book in `testbooks/` and prints tag against detection:
    /// `cargo test -p verso --release detect_testbooks -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn detect_testbooks() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testbooks");
        let mut paths: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("epub")))
            .collect();
        paths.sort();
        println!(
            "{:<45} {:>4} {:>4} {:>8} {:>10} {:>7} {:>9}",
            "book", "tag", "det", "reliable", "confidence", "sample", "time"
        );
        for path in paths {
            let tag = epub::read(&path).map(|m| normalize_language(&m.language)).unwrap_or_default();
            let started = std::time::Instant::now();
            let result = sample(&path).map(|text| (detect(&text), whatlang::detect(&text), text.len()));
            let elapsed = started.elapsed();
            let name = path.file_name().unwrap().to_string_lossy();
            match result {
                Ok((detected, info, len)) => println!(
                    "{name:<45} {tag:>4} {:>4} {:>8} {:>10.3} {len:>7} {elapsed:>9.1?}{}",
                    detected.code,
                    detected.reliable,
                    info.map_or(0.0, |i| i.confidence()),
                    if detected.trusted().is_some_and(|code| code != tag) { "  ← overrides tag" } else { "" }
                ),
                Err(error) => println!("{name:<45} {tag:>4} error: {error:#}"),
            }
        }
    }
}
