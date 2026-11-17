//! Base forms for words a dictionary has no entry for: one analyzer per
//! language turns a word into candidate lemmas, and the lookup checks them
//! against real headwords. English is built in; other languages read a
//! word-form pack (`<lang>.vwf`) from the word-forms folder. See
//! docs/word-forms-languages.md.
mod english;
mod table;

use english::English;
#[cfg(test)]
pub(super) use english::candidates;
use std::path::PathBuf;
use table::Table;

/// Packs Verso ships, shown in Settings even when the file is missing.
const BUNDLED: &[&str] = &["ru"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Candidates {
    /// Try this as an exact word before exceptions or rules.
    pub possessive: Option<String>,
    /// All authoritative lemmas, best first. When present, there are no rule
    /// guesses, even if no lemma has an entry.
    pub exceptions: Vec<String>,
    /// Ordered, unique guesses for words without authoritative lemmas; only
    /// the first rule with an entry counts.
    pub rules: Vec<String>,
}
impl Candidates {
    /// Every generated candidate, for diagnostics.
    pub fn count(&self) -> usize {
        usize::from(self.possessive.is_some()) + self.exceptions.len() + self.rules.len()
    }
}

/// Turns one language's words into base-form candidates. `None` when the
/// analyzer doesn't know the word or it isn't a single word; an error means
/// the analyzer's data is broken.
trait Analyzer {
    fn candidates(&self, word: &str) -> anyhow::Result<Option<Candidates>>;
}

/// A word-form language for Settings: where its forms come from, or why
/// they're unavailable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WordFormsStatus {
    /// A normalized language code (`en`, `ru`).
    pub language: String,
    pub source: WordFormsSource,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WordFormsSource {
    BuiltIn,
    /// A pack, with its source and date (`OpenCorpora 0.92, 2024-04-23`).
    Pack(String),
    /// The pack couldn't be opened or read.
    Error(String),
}

enum Slot {
    Unopened,
    Open(Table),
    Failed(String),
}

/// The worker's analyzers. Packs open on first use and stay open: they are
/// memory-mapped, so only the pages lookups touch use memory.
pub(super) struct WordForms {
    dir: PathBuf,
    /// Pack languages: the bundled ones and any other `<lang>.vwf` in `dir`,
    /// listed on first use.
    tables: Option<Vec<(String, Slot)>>,
}

impl WordForms {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir, tables: None }
    }

    /// Forgets open packs and errors, so the next use lists the folder and
    /// opens packs again (Rescan).
    pub fn reset(&mut self) {
        self.tables = None;
    }

    /// Candidates for `word` from the first of `languages` that has an
    /// analyzer. A pack that can't be opened is skipped; one that turns out
    /// corrupt while reading gives no candidates and shows its error in
    /// Settings until a rescan.
    pub fn candidates(&mut self, languages: &[String], word: &str) -> Option<Candidates> {
        for language in languages {
            let result = if language == "en" {
                English.candidates(word)
            } else {
                match self.table(language) {
                    Some(table) => table.candidates(word),
                    None => continue,
                }
            };
            return match result {
                Ok(candidates) => candidates,
                Err(error) => {
                    let error = format!("{error:#}");
                    log::warn!("word forms {language}: {error}");
                    if let Some((_, slot)) = self.tables().iter_mut().find(|(l, _)| l == language) {
                        *slot = Slot::Failed(error);
                    }
                    None
                }
            };
        }
        None
    }

    /// Every word-form language, English first, opening packs that aren't
    /// open yet.
    pub fn status(&mut self) -> Vec<WordFormsStatus> {
        let mut status = vec![WordFormsStatus { language: "en".into(), source: WordFormsSource::BuiltIn }];
        let languages: Vec<_> = self.tables().iter().map(|(language, _)| language.clone()).collect();
        for language in languages {
            self.table(&language);
            let source = match self.tables().iter().find(|(l, _)| *l == language).map(|(_, slot)| slot) {
                Some(Slot::Open(table)) => WordFormsSource::Pack(table.source()),
                Some(Slot::Failed(error)) => WordFormsSource::Error(error.clone()),
                _ => continue,
            };
            status.push(WordFormsStatus { language, source });
        }
        status
    }

    fn tables(&mut self) -> &mut Vec<(String, Slot)> {
        self.tables.get_or_insert_with(|| {
            let mut languages: Vec<String> = BUNDLED.iter().map(|l| (*l).to_owned()).collect();
            // A missing folder only means no extra packs; bundled ones then
            // report their own missing file.
            if let Ok(entries) = std::fs::read_dir(&self.dir) {
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    let Some(language) = name.to_str().and_then(|n| n.strip_suffix(".vwf")) else { continue };
                    // English is built in; a pack can't replace it.
                    if (2..=8).contains(&language.len())
                        && language.bytes().all(|b| b.is_ascii_lowercase())
                        && language != "en"
                        && !languages.iter().any(|l| l == language)
                    {
                        languages.push(language.to_owned());
                    }
                }
            }
            languages.sort();
            languages.into_iter().map(|language| (language, Slot::Unopened)).collect()
        })
    }

    /// The open pack for `language`, opening it on first use.
    fn table(&mut self, language: &str) -> Option<&Table> {
        let dir = self.dir.clone();
        let (_, slot) = self.tables().iter_mut().find(|(l, _)| l == language)?;
        if let Slot::Unopened = slot {
            let started = std::time::Instant::now();
            *slot = match Table::open(&dir.join(format!("{language}.vwf")), language) {
                Ok(table) => {
                    log::debug!("word forms {language}: opened in {:?}", started.elapsed());
                    Slot::Open(table)
                }
                Err(error) => {
                    let error = format!("{error:#}");
                    log::warn!("word forms {language}: {error}");
                    Slot::Failed(error)
                }
            };
        }
        match slot {
            Slot::Open(table) => Some(table),
            _ => None,
        }
    }
}

/// Writes a pack for `language` with `forms` (form, lemmas best first), as
/// `vwf-build` would.
#[cfg(test)]
pub(crate) fn write_pack(path: &std::path::Path, language: &str, forms: &[(&str, &[&str])]) {
    use verso_wordforms::{Encoding, fold, write::Writer};
    let mut forms: Vec<_> = forms.iter().map(|(form, lemmas)| (fold(form), *lemmas)).collect();
    forms.sort();
    let meta = [("language", language), ("source", "Test pack"), ("date", "2026-11-17")];
    let mut writer = Writer::new(Encoding::Rules, &meta).unwrap();
    for (key, lemmas) in &forms {
        writer.insert(key, lemmas).unwrap();
    }
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, writer.finish().unwrap().0).unwrap();
}

#[cfg(test)]
mod tests;
