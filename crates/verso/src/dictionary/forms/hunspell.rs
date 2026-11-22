//! Base forms from Hunspell dictionaries the user installs: a LibreOffice
//! `.aff`/`.dic` pair in the Hunspell folder, or one level of subfolders,
//! named for its language (`de_DE.aff` is German). Every stem Hunspell finds
//! is an authoritative lemma; there are no rule guesses. See
//! docs/universal-languages.md.
use super::{Analyzer, Candidates};
use crate::dictionary::languages::{language_scripts, normalize_language};
use hunspell_sys::{Encoding, Hunspell};
use std::{
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
};
use verso_wordforms::{fold, is_word};

/// Entries read from each folder, so a stray huge folder can't stall the
/// listing.
const MAX_ENTRIES: usize = 4096;
/// Words from the start of the `.dic` stemmed after opening; a dictionary
/// that stems none of them is reported as giving nothing.
const SAMPLE_WORDS: usize = 100;
/// Bytes of the `.dic` read for those words.
const SAMPLE_BYTES: u64 = 64 * 1024;

/// One `.aff` file found in the folder, with the `.dic` of the same name
/// next to it (which may be missing).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Found {
    /// The file name without `.aff`: `de_DE`.
    pub name: String,
    /// The `.aff` path below the Hunspell folder, for Settings.
    pub relative: PathBuf,
    pub aff: PathBuf,
    pub dic: PathBuf,
}

impl Found {
    /// Both files below the Hunspell folder, for Settings.
    pub fn files(&self) -> Vec<String> {
        let dic = self.relative.with_extension("dic");
        vec![self.relative.display().to_string(), dic.display().to_string()]
    }
}

/// Why a dictionary can't be used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Problem {
    /// The `.aff` names an encoding Verso can't convert words to.
    Unsupported(String),
    Failed(String),
}

impl From<hunspell_sys::Error> for Problem {
    fn from(error: hunspell_sys::Error) -> Self {
        match error {
            hunspell_sys::Error::UnsupportedEncoding(name) => Problem::Unsupported(name),
            error => Problem::Failed(error.to_string()),
        }
    }
}

/// Every dictionary in `dir` and its direct subfolders, grouped by language
/// (sorted by code), each group sorted by file name: the first is the one
/// used. A file whose name isn't a known language is ignored.
pub(super) fn discover(dir: &Path) -> Vec<(String, Vec<Found>)> {
    let mut found = Vec::new();
    let mut folders = vec![(dir.to_path_buf(), PathBuf::new())];
    let mut depth = 0;
    while depth < 2 && !folders.is_empty() {
        let mut next = Vec::new();
        for (folder, relative) in folders {
            // A missing folder is an installation without Hunspell dictionaries.
            let Ok(entries) = std::fs::read_dir(&folder) else { continue };
            for entry in entries.flatten().take(MAX_ENTRIES) {
                let file_name = entry.file_name();
                let Some(file_name) = file_name.to_str() else { continue };
                // Hidden files include macOS's `._de_DE.aff` resource forks.
                if file_name.starts_with('.') {
                    continue;
                }
                let path = entry.path();
                if path.is_dir() {
                    next.push((path, relative.join(file_name)));
                } else if let Some(name) = file_name.strip_suffix(".aff")
                    && !name.is_empty()
                {
                    found.push(Found {
                        name: name.to_owned(),
                        relative: relative.join(file_name),
                        aff: path.clone(),
                        dic: path.with_extension("dic"),
                    });
                }
            }
        }
        folders = next;
        depth += 1;
    }
    found.sort_by(|a, b| (&a.name, &a.relative).cmp(&(&b.name, &b.relative)));
    let mut languages: Vec<(String, Vec<Found>)> = Vec::new();
    for dictionary in found {
        let language = normalize_language(&dictionary.name);
        if language_scripts(&language).is_empty() {
            continue;
        }
        match languages.iter_mut().find(|(l, _)| *l == language) {
            Some((_, list)) => list.push(dictionary),
            None => languages.push((language, vec![dictionary])),
        }
    }
    languages.sort_by(|a, b| a.0.cmp(&b.0));
    languages
}

/// The pre-load checks of `Hunspell::open`, without loading: cheap enough
/// for Settings to show every dictionary's state.
pub(super) fn check(found: &Found) -> Result<Encoding, Problem> {
    Ok(hunspell_sys::check(&found.aff, &found.dic)?)
}

/// An open Hunspell dictionary. It holds the whole `.dic` in memory (about
/// four times the file's size), so the worker keeps one at a time.
pub(super) struct Dictionary {
    hunspell: Hunspell,
}

impl Dictionary {
    /// Loads the dictionary and checks that it gives something: Hunspell
    /// reports no parse errors, so a dictionary that stems none of the first
    /// words of its own `.dic` is treated as unreadable.
    pub fn open(found: &Found) -> Result<Self, Problem> {
        let hunspell = Hunspell::open(&found.aff, &found.dic)?;
        let sample = sample_words(&found.dic, hunspell.encoding())
            .map_err(|e| Problem::Failed(format!("{} can't be read: {e}", found.dic.display())))?;
        if !sample.iter().any(|word| !hunspell.stem(word).is_empty()) {
            return Err(Problem::Failed(format!("No words could be read from {}", found.dic.display())));
        }
        Ok(Self { hunspell })
    }

    pub fn encoding(&self) -> Encoding {
        self.hunspell.encoding()
    }
}

impl Analyzer for Dictionary {
    fn candidates(&self, word: &str) -> anyhow::Result<Option<Candidates>> {
        if !is_word(word) {
            return Ok(None);
        }
        // The exact word was already looked up (`Haus` stems to itself).
        let key = fold(word);
        let mut exceptions: Vec<String> = Vec::new();
        for stem in self.hunspell.stem(word) {
            if fold(&stem) != key && !exceptions.contains(&stem) {
                exceptions.push(stem);
            }
        }
        Ok((!exceptions.is_empty()).then_some(Candidates { possessive: None, exceptions, rules: Vec::new() }))
    }
}

/// Up to `SAMPLE_WORDS` words from the start of a `.dic`, after its word
/// count: each line's word, before its flags (`/`, unless escaped as `\/`)
/// or morphological fields (after a tab or space).
fn sample_words(dic: &Path, encoding: Encoding) -> std::io::Result<Vec<String>> {
    let reader = BufReader::new(std::fs::File::open(dic)?.take(SAMPLE_BYTES));
    let mut words = Vec::new();
    for line in reader.split(b'\n').skip(1) {
        let line = line?;
        let mut word = Vec::new();
        let mut bytes = line.iter().copied().peekable();
        while let Some(b) = bytes.next() {
            match b {
                b'\\' if bytes.peek() == Some(&b'/') => word.push(bytes.next().unwrap()),
                b'/' | b'\t' | b' ' | b'\r' => break,
                _ => word.push(b),
            }
        }
        let word = match encoding {
            Encoding::Utf8 => String::from_utf8(word).ok(),
            Encoding::Latin1 => Some(word.iter().map(|&b| char::from(b)).collect()),
        };
        if let Some(word) = word.filter(|w| !w.is_empty()) {
            words.push(word);
            if words.len() == SAMPLE_WORDS {
                break;
            }
        }
    }
    Ok(words)
}
