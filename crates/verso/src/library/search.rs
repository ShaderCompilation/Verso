//! A lazy, disposable search projection of the cached index. Never opens books.
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;

use caseless::Caseless;
use unicode_normalization::UnicodeNormalization;

use super::collections::normalize;
use super::{Book, LibraryIndex};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Scope {
    #[default]
    All,
    Collection(PathBuf),
}

struct Entry {
    signature: u64,
    path: PathBuf,
    fields: [String; 3],
}

#[derive(Default)]
pub struct SearchCache {
    entries: HashMap<PathBuf, Entry>,
}

/// What search compares: lower case, without Latin, Greek and Cyrillic accents.
/// `transfer/page.html` has a copy of this; change both together.
pub fn fold(text: &str) -> String {
    let mut plain = String::with_capacity(text.len());
    // Accents go before case folding, which turns U+0345 (Greek iota subscript)
    // into a letter.
    for c in text.nfd().filter(|c| !is_accent(*c)).default_case_fold() {
        // Letters Unicode doesn't decompose.
        match c {
            'đ' | 'ð' => plain.push('d'),
            'ø' => plain.push('o'),
            'ł' => plain.push('l'),
            'ħ' => plain.push('h'),
            'ŧ' => plain.push('t'),
            'ı' => plain.push('i'),
            'æ' => plain.push_str("ae"),
            'œ' => plain.push_str("oe"),
            'þ' => plain.push_str("th"),
            _ => plain.push(c),
        }
    }
    // Recomposed so that 가 isn't found inside 각.
    plain.nfc().collect()
}

/// Not every combining mark: dropping those would lose Hindi vowels and turn が
/// into か.
fn is_accent(c: char) -> bool {
    matches!(c, '\u{300}'..='\u{36f}' | '\u{1ab0}'..='\u{1aff}' | '\u{1dc0}'..='\u{1dff}' | '\u{20d0}'..='\u{20ff}')
}

impl SearchCache {
    pub fn sync(&mut self, index: &LibraryIndex) {
        let paths: std::collections::HashSet<_> = index.books.iter().filter(|b| b.available).map(|b| &b.path).collect();
        self.entries.retain(|path, _| paths.contains(path));
        for book in index.books.iter().filter(|b| b.available) {
            let filename = book.path.file_name().unwrap_or_default().to_string_lossy();
            let fields = [book.metadata.title.as_str(), book.metadata.authors.as_str(), filename.as_ref()];
            let mut hash = std::collections::hash_map::DefaultHasher::new();
            fields.hash(&mut hash);
            let signature = hash.finish();
            if self.entries.get(&book.path).is_some_and(|e| e.signature == signature) {
                continue;
            }
            self.entries
                .insert(book.path.clone(), Entry { signature, path: normalize(&book.path), fields: fields.map(fold) });
        }
    }

    /// `query` is folded once per edit, rather than once per candidate.
    pub fn matches(&self, book: &Book, query: &str, scope: &Scope) -> bool {
        book.available
            && self.entries.get(&book.path).is_some_and(|entry| {
                (match scope {
                    Scope::All => true,
                    Scope::Collection(folder) => entry.path.starts_with(folder),
                }) && (query.is_empty() || entry.fields.iter().any(|field| field.contains(query)))
            })
    }
}

/// Explicit opt-in diagnostic: runs before opening a platform or book engine.
/// Emits aggregate timings only; never emits paths, metadata or query text.
pub fn benchmark(index: &LibraryIndex) -> serde_json::Value {
    use std::time::Instant;
    fn rss() -> Option<u64> {
        std::fs::read_to_string("/proc/self/status")
            .ok()?
            .lines()
            .find(|line| line.starts_with("VmRSS:"))?
            .split_whitespace()
            .nth(1)?
            .parse()
            .ok()
    }
    fn percentile(values: &mut [f64], p: usize) -> f64 {
        values.sort_by(f64::total_cmp);
        values[(values.len() * p).div_ceil(100).saturating_sub(1)]
    }
    let before = rss();
    let started = Instant::now();
    let mut cache = SearchCache::default();
    cache.sync(index);
    let cache_ms = started.elapsed().as_secs_f64() * 1000.0;
    let after = rss();
    let bytes: usize = cache.entries.values().flat_map(|e| &e.fields).map(String::len).sum();
    let mut matching = Vec::new();
    let mut preparation = Vec::new();
    for _ in 0..32 {
        for query in ["", "e", "the", "ГРАФ", "Николай", "unlikely-needle-7654321"] {
            let started = Instant::now();
            let query = fold(query);
            let selected: Vec<_> = index.books.iter().filter(|b| cache.matches(b, &query, &Scope::All)).collect();
            matching.push(started.elapsed().as_secs_f64() * 1000.0);
            let mut books: Vec<_> = selected.into_iter().cloned().collect();
            super::sort_books(&mut books);
            std::hint::black_box(books);
            preparation.push(started.elapsed().as_secs_f64() * 1000.0);
        }
    }
    serde_json::json!({
        "books": cache.entries.len(), "cache_ms": cache_ms, "folded_text_bytes": bytes,
        "rss_before_kib": before, "rss_cached_kib": after, "rss_after_queries_kib": rss(),
        "queries": matching.len(), "matching_p50_ms": percentile(&mut matching,50),
        "matching_p95_ms": percentile(&mut matching,95),
        "preparation_p50_ms": percentile(&mut preparation,50),
        "preparation_p95_ms": percentile(&mut preparation,95),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::selection_tests::book;

    #[test]
    fn unicode_fields_and_literal_substrings() {
        let mut b = book(0);
        b.path = "/books/Греция-final.EPUB".into();
        b.metadata.title =
            "ЛЕГЕНДЫ Café Straße Σίσυφος ᾠδή ёлка Đi tìm lẽ sống İstanbul Kırmızı Smørrebrød Łódź が 각 किताब".into();
        b.metadata.authors = "Николай Кун".into();
        let index = LibraryIndex { books: vec![b.clone()], ..Default::default() };
        let mut cache = SearchCache::default();
        cache.sync(&index);
        for q in ["легенды", "КУН", "греция-FINAL.epub", "STRASSE", "  "] {
            assert!(cache.matches(&b, &fold(q.trim()), &Scope::All), "{q}");
        }
        // Accents don't matter, typed, missing or decomposed.
        for q in [
            "cafe",
            "café",
            "cafe\u{301}",
            "елка",
            "ЁЛКА",
            "ΣΊΣΥΦΟΣ",
            "σισυφος",
            "ωδη",
            "di tim le song",
            "Đi tìm",
            "ĐI TI\u{300}M",
            "istanbul kirmizi",
            "İSTANBUL",
            "Kırmızı",
            "smorrebrod",
            "SMØRREBRØD",
            "lodz",
            "Łódź",
            "が",
            "か\u{3099}",
            "각",
            "किताब",
        ] {
            assert!(cache.matches(&b, &fold(q), &Scope::All), "{q}");
        }
        // In other scripts marks are letters: か, 가 and Hindi without its vowel.
        for q in ["легенды кун", "か", "가", "कताब", "[.*]"] {
            assert!(!cache.matches(&b, &fold(q), &Scope::All), "{q}");
        }
    }

    #[test]
    fn recursive_component_scope_and_snapshot_changes() {
        let mut b = book(0);
        b.path = "/books/Fiction/Nested/a.epub".into();
        let mut index = LibraryIndex { books: vec![b.clone()], ..Default::default() };
        let mut cache = SearchCache::default();
        cache.sync(&index);
        let scope = Scope::Collection("/books/Fiction".into());
        assert!(cache.matches(&b, "", &scope));
        assert!(!cache.matches(&b, "", &Scope::Collection("/books/Fict".into())));
        index.books[0].metadata.title = "Changed".into();
        cache.sync(&index);
        assert!(cache.matches(&b, "changed", &scope));
        assert!(!cache.matches(&b, "title", &scope));
        index.books[0].path = "/books/Fiction-other/a.epub".into();
        cache.sync(&index);
        assert!(!cache.matches(&index.books[0], "", &scope));
        assert!(!cache.matches(&b, "", &scope));
        index.books[0].available = false;
        cache.sync(&index);
        assert!(cache.entries.is_empty());
    }
}
