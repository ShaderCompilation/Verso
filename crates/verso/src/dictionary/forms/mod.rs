//! Base forms for words a dictionary has no entry for: one analyzer per
//! language turns a word into candidate lemmas, and the lookup checks them
//! against real headwords. English is built in; other languages read a
//! word-form pack (`<lang>.vwf`) from the word-forms folder, and any other
//! language a Hunspell dictionary the user installed. See
//! docs/word-forms-languages.md and docs/universal-languages.md.
mod english;
mod hunspell;
mod table;

use super::languages::language_name;
use english::English;
#[cfg(test)]
pub(super) use english::candidates;
use hunspell::Problem;
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
    /// The Hunspell dictionary's `.aff` and `.dic`, below the Hunspell
    /// folder, when the forms come (or would come) from one.
    pub files: Vec<String>,
    /// Hunspell dictionaries for this language that aren't used: ones after
    /// the first by name, or all of them for a language with built-in or
    /// pack forms. `.aff` paths below the Hunspell folder.
    pub unused: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WordFormsSource {
    BuiltIn,
    /// A pack, with its source and date (`OpenCorpora 0.92, 2024-04-23`).
    Pack(String),
    /// A Hunspell dictionary: its name (`de_DE`), its encoding and whether
    /// it is loaded now. A dictionary is loaded on first use, so one that
    /// isn't loaded has only passed the file checks.
    Hunspell {
        name: String,
        encoding: String,
        loaded: bool,
    },
    /// A Hunspell dictionary in an encoding Verso can't read (`KOI8-R`).
    Unsupported {
        name: String,
        encoding: String,
    },
    /// The pack or Hunspell dictionary couldn't be opened or read.
    Error(String),
}

enum Slot {
    Unopened,
    Open(Table),
    Failed(String),
}

/// A language with Hunspell dictionaries; the first is the one used.
struct HunspellLanguage {
    language: String,
    found: Vec<hunspell::Found>,
    /// The first dictionary's encoding once checked or opened, or why it
    /// can't be used; `None` until then.
    state: Option<Result<hunspell_sys::Encoding, Problem>>,
}

/// The analyzer for one language.
enum Analyzers<'a> {
    English,
    Table(&'a Table),
    Hunspell(&'a hunspell::Dictionary),
}
impl Analyzers<'_> {
    fn candidates(&self, word: &str) -> anyhow::Result<Option<Candidates>> {
        match self {
            Analyzers::English => English.candidates(word),
            Analyzers::Table(table) => table.candidates(word),
            Analyzers::Hunspell(dictionary) => dictionary.candidates(word),
        }
    }
}

/// The worker's analyzers. Packs open on first use and stay open: they are
/// memory-mapped, so only the pages lookups touch use memory. A Hunspell
/// dictionary is parsed into memory when it opens, so only one is open at a
/// time and the worker releases it when the reader closes.
///
/// English and pack languages always use their own forms, even with a
/// Hunspell dictionary for the same language installed.
pub(super) struct WordForms {
    dir: PathBuf,
    hunspell_dir: PathBuf,
    /// Pack languages: the bundled ones and any other `<lang>.vwf` in `dir`,
    /// listed on first use.
    tables: Option<Vec<(String, Slot)>>,
    /// Languages with Hunspell dictionaries in `hunspell_dir`, listed on
    /// first use.
    hunspell: Option<Vec<HunspellLanguage>>,
    /// The one open Hunspell dictionary and its language.
    open: Option<(String, hunspell::Dictionary)>,
}

impl WordForms {
    pub fn new(dir: PathBuf, hunspell_dir: PathBuf) -> Self {
        Self { dir, hunspell_dir, tables: None, hunspell: None, open: None }
    }

    /// Forgets open packs, the open Hunspell dictionary and errors, so the
    /// next use lists the folders and opens them again (Rescan).
    pub fn reset(&mut self) {
        self.tables = None;
        self.hunspell = None;
        self.release();
    }

    /// Frees the open Hunspell dictionary; the next lookup or warm-up in its
    /// language loads it again.
    pub fn release(&mut self) {
        if let Some((language, _)) = self.open.take() {
            log::info!("word forms {language}: Hunspell dictionary released");
        }
    }

    /// The language of the open Hunspell dictionary.
    #[cfg(test)]
    pub fn open_language(&self) -> Option<&str> {
        self.open.as_ref().map(|(language, _)| language.as_str())
    }

    /// Candidates for `word` from the first of `languages` that has an
    /// analyzer. A pack or Hunspell dictionary that can't be opened is
    /// skipped; a pack that turns out corrupt while reading gives no
    /// candidates and shows its error in Settings until a rescan.
    pub fn candidates(&mut self, languages: &[String], word: &str) -> Option<Candidates> {
        for language in languages {
            let Some(analyzer) = self.analyzer(language) else { continue };
            return match analyzer.candidates(word) {
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

    /// Opens the analyzer the first of `languages` with one would use, so
    /// the first lookup doesn't wait for a Hunspell dictionary to load.
    pub fn warm(&mut self, languages: &[String]) {
        for language in languages {
            if self.analyzer(language).is_some() {
                return;
            }
        }
    }

    /// Every word-form language, English first, then the others by name.
    /// Opens packs that aren't open yet; Hunspell dictionaries are only
    /// checked, not loaded.
    pub fn status(&mut self) -> Vec<WordFormsStatus> {
        let unused_all = |found: &[hunspell::Found]| found.iter().map(|f| f.relative.display().to_string()).collect();
        let hunspell_found = |s: &mut Self, language: &str| {
            s.hunspell_languages().iter().find(|h| h.language == language).map(|h| h.found.clone()).unwrap_or_default()
        };
        let english = hunspell_found(self, "en");
        let mut status = vec![WordFormsStatus {
            language: "en".into(),
            source: WordFormsSource::BuiltIn,
            files: Vec::new(),
            unused: unused_all(&english),
        }];
        let mut others = Vec::new();
        let packs: Vec<_> = self.tables().iter().map(|(language, _)| language.clone()).collect();
        for language in &packs {
            self.table(language);
            let source = match self.tables().iter().find(|(l, _)| l == language).map(|(_, slot)| slot) {
                Some(Slot::Open(table)) => WordFormsSource::Pack(table.source()),
                Some(Slot::Failed(error)) => WordFormsSource::Error(error.clone()),
                _ => continue,
            };
            let unused = unused_all(&hunspell_found(self, language));
            others.push(WordFormsStatus { language: language.clone(), source, files: Vec::new(), unused });
        }
        let open = self.open.as_ref().map(|(language, _)| language.clone());
        for entry in self.hunspell_languages() {
            if entry.language == "en" || packs.contains(&entry.language) {
                continue;
            }
            let first = &entry.found[0];
            let state = entry.state.get_or_insert_with(|| hunspell::check(first));
            let source = match state {
                Ok(encoding) => WordFormsSource::Hunspell {
                    name: first.name.clone(),
                    encoding: encoding.to_string(),
                    loaded: open.as_ref() == Some(&entry.language),
                },
                Err(Problem::Unsupported(encoding)) => {
                    WordFormsSource::Unsupported { name: first.name.clone(), encoding: encoding.clone() }
                }
                Err(Problem::Failed(error)) => WordFormsSource::Error(error.clone()),
            };
            others.push(WordFormsStatus {
                language: entry.language.clone(),
                source,
                files: first.files(),
                unused: unused_all(&entry.found[1..]),
            });
        }
        others.sort_by(|a, b| language_name(&a.language).cmp(language_name(&b.language)));
        status.extend(others);
        status
    }

    /// The analyzer for `language`, opening its pack or Hunspell dictionary
    /// if needed; `None` when it has none or it can't be opened. English and
    /// pack languages never use Hunspell.
    fn analyzer(&mut self, language: &str) -> Option<Analyzers<'_>> {
        if language == "en" {
            return Some(Analyzers::English);
        }
        if self.tables().iter().any(|(l, _)| l == language) {
            return self.table(language).map(Analyzers::Table);
        }
        self.hunspell(language).map(Analyzers::Hunspell)
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

    fn hunspell_languages(&mut self) -> &mut Vec<HunspellLanguage> {
        self.hunspell.get_or_insert_with(|| {
            hunspell::discover(&self.hunspell_dir)
                .into_iter()
                .map(|(language, found)| HunspellLanguage { language, found, state: None })
                .collect()
        })
    }

    /// The Hunspell dictionary for `language`, loading it (and releasing any
    /// other) if it isn't the open one. A dictionary that fails stays failed
    /// until a rescan.
    fn hunspell(&mut self, language: &str) -> Option<&hunspell::Dictionary> {
        if self.open.as_ref().is_none_or(|(open, _)| open != language) {
            let entry = self.hunspell_languages().iter_mut().find(|h| h.language == language)?;
            if matches!(entry.state, Some(Err(_))) {
                return None;
            }
            let found = entry.found[0].clone();
            // Free the previous dictionary first, so two are never in memory.
            self.release();
            let started = std::time::Instant::now();
            let result = hunspell::Dictionary::open(&found);
            let entry = self.hunspell_languages().iter_mut().find(|h| h.language == language)?;
            match result {
                Ok(dictionary) => {
                    log::info!(
                        "word forms {language}: opened Hunspell {} elapsed_us={}",
                        found.relative.display(),
                        started.elapsed().as_micros()
                    );
                    entry.state = Some(Ok(dictionary.encoding()));
                    self.open = Some((language.to_owned(), dictionary));
                }
                Err(problem) => {
                    log::warn!("word forms {language}: Hunspell {}: {problem:?}", found.relative.display());
                    entry.state = Some(Err(problem));
                    return None;
                }
            }
        }
        self.open.as_ref().map(|(_, dictionary)| dictionary)
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

/// Copies the hunspell-sys test dictionary `name` (`de_DE`: UTF-8,
/// `fr_FR`: ISO8859-1) into `dir`.
#[cfg(test)]
pub(crate) fn install_hunspell_fixture(dir: &std::path::Path, name: &str) {
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../hunspell-sys/tests/fixtures");
    std::fs::create_dir_all(dir).unwrap();
    for ext in ["aff", "dic"] {
        std::fs::copy(fixtures.join(format!("{name}.{ext}")), dir.join(format!("{name}.{ext}"))).unwrap();
    }
}

/// Writes a Hunspell dictionary `name` into `dir`.
#[cfg(test)]
pub(crate) fn write_hunspell(dir: &std::path::Path, name: &str, aff: &str, dic: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join(format!("{name}.aff")), aff).unwrap();
    std::fs::write(dir.join(format!("{name}.dic")), dic).unwrap();
}

#[cfg(test)]
mod tests;
