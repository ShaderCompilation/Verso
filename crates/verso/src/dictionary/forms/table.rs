//! An analyzer over a word-form pack: every lemma the pack lists for a form
//! is authoritative, best first, and there are no rule guesses.
use super::{Analyzer, Candidates};
use anyhow::{Result, ensure};
use std::path::Path;
use verso_wordforms::{Pack, fold};

pub(super) struct Table {
    pack: Pack,
}

impl Table {
    /// Opens a pack and checks it is for `language`. Cheap: it maps the
    /// file and reads only the header.
    pub fn open(path: &Path, language: &str) -> Result<Self> {
        let pack = Pack::open(path)?;
        let found = pack.meta("language").unwrap_or("");
        ensure!(found == language, "{} is for language “{found}”, not “{language}”", path.display());
        Ok(Self { pack })
    }

    /// Where the forms come from, for Settings: `OpenCorpora 0.92, 2024-04-23`.
    pub fn source(&self) -> String {
        let source = self.pack.meta("source").unwrap_or("Word-form pack");
        match self.pack.meta("date") {
            Some(date) => format!("{source}, {date}"),
            None => source.to_owned(),
        }
    }
}

impl Analyzer for Table {
    fn candidates(&self, word: &str) -> Result<Option<Candidates>> {
        let Some(lemmas) = self.pack.lemmas(word)? else { return Ok(None) };
        // The exact word was already looked up (`ёж` is its own lemma).
        let key = fold(word);
        let exceptions: Vec<_> = lemmas.into_iter().filter(|lemma| fold(lemma) != key).collect();
        Ok((!exceptions.is_empty()).then_some(Candidates { possessive: None, exceptions, rules: Vec::new() }))
    }
}
