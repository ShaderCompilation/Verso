//! Durable dictionary sets. Missing installations keep their position and flags.
use super::{Dictionary, Preference};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

pub const DEFAULT: &str = "default";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Set {
    pub id: String,
    pub name: String,
    pub languages: Vec<String>,
    pub dictionaries: Vec<Preference>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preferences {
    pub sets: Vec<Set>,
}
impl Default for Preferences {
    fn default() -> Self {
        Self { sets: vec![Set { id: DEFAULT.into(), name: "Default".into(), languages: vec![], dictionaries: vec![] }] }
    }
}

/// Book context is resolved afresh for every query, including edited queries.
#[derive(Clone, Debug, Default)]
pub struct Selection {
    pub book_set: Option<String>,
    pub language: String,
}

/// Every edit is ordered with lookups and saved before the next command.
#[derive(Clone, Debug)]
pub enum Edit {
    Create {
        id: String,
        name: String,
    },
    Rename {
        set: String,
        name: String,
    },
    Delete {
        set: String,
    },
    #[allow(dead_code)] // Set order is retained for a future UI.
    MoveSet {
        set: String,
        position: usize,
    },
    Languages {
        set: String,
        languages: Vec<String>,
    },
    Disabled {
        set: String,
        dictionary: String,
        disabled: bool,
    },
    Forget {
        set: String,
        dictionary: String,
    },
    MoveDictionary {
        set: String,
        dictionary: String,
        position: usize,
    },
}

impl Edit {
    /// The set the edit is about; after a deletion this falls back to Default.
    pub fn set(&self) -> &str {
        match self {
            Edit::Create { id: set, .. }
            | Edit::Rename { set, .. }
            | Edit::Delete { set }
            | Edit::MoveSet { set, .. }
            | Edit::Languages { set, .. }
            | Edit::Disabled { set, .. }
            | Edit::MoveDictionary { set, .. }
            | Edit::Forget { set, .. } => set,
        }
    }
}

impl Preferences {
    /// Legacy arrays migrate verbatim. For an invalid record the caller uses
    /// empty preferences and leaves the file alone until the user changes one.
    pub fn parse(text: &str) -> Result<(Self, bool)> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Record {
            Sets(Preferences),
            Legacy(Vec<Preference>),
        }
        let (mut prefs, migrated) = match serde_json::from_str(text)? {
            Record::Sets(p) => (p, false),
            Record::Legacy(dictionaries) => {
                let mut p = Self::default();
                p.sets[0].dictionaries = dictionaries;
                (p, true)
            }
        };
        ensure!(prefs.sets.iter().any(|s| s.id == DEFAULT), "Missing Default dictionary set");
        let mut ids = std::collections::HashSet::new();
        let mut languages = std::collections::HashSet::new();
        for set in &mut prefs.sets {
            ensure!(!set.id.is_empty() && ids.insert(set.id.clone()), "Duplicate or empty set ID");
            ensure!(!set.name.trim().is_empty(), "Empty set name");
            ensure!(set.id != DEFAULT || set.languages.is_empty(), "Default has no languages");
            let mut dictionaries = std::collections::HashSet::new();
            ensure!(set.dictionaries.iter().all(|d| dictionaries.insert(&d.id)), "Duplicate dictionary ID");
            set.languages = clean_languages(&set.languages)?;
            ensure!(set.languages.iter().all(|l| languages.insert(l.clone())), "Language assigned twice");
        }
        Ok((prefs, migrated))
    }
    pub fn set(&self, id: &str) -> &Set {
        self.sets.iter().find(|s| s.id == id).unwrap_or_else(|| self.sets.iter().find(|s| s.id == DEFAULT).unwrap())
    }
    /// Only previously unknown dictionaries join sets; missing ones stay put.
    pub fn discover(&mut self, dictionaries: &[Dictionary]) -> bool {
        let mut changed = false;
        for set in &mut self.sets {
            for dictionary in dictionaries {
                if !set.dictionaries.iter().any(|p| p.id == dictionary.id) {
                    set.dictionaries.push(Preference { id: dictionary.id.clone(), disabled: set.id != DEFAULT });
                    changed = true;
                }
            }
        }
        changed
    }
    pub fn choose(&self, selection: &Selection, query: &str) -> &Set {
        let language = normalize_language(&selection.language);
        let chosen = selection
            .book_set
            .as_ref()
            .and_then(|id| self.sets.iter().find(|s| s.id == *id))
            .or_else(|| self.sets.iter().find(|s| s.languages.contains(&language)))
            .unwrap_or_else(|| self.set(DEFAULT));
        let Some(script) = word_script(query) else { return chosen };
        // Unknown languages must not cause a fallback based on a partial table.
        if chosen.languages.is_empty()
            || chosen.languages.iter().any(|l| language_script(l).is_none_or(|s| s == script))
        {
            return chosen;
        }
        self.sets
            .iter()
            .find(|s| s.languages.iter().any(|l| language_script(l) == Some(script)))
            .unwrap_or_else(|| self.set(DEFAULT))
    }
    pub fn edit(&mut self, edit: Edit) -> Result<()> {
        match edit {
            Edit::Create { id, name } => {
                ensure!(!id.is_empty() && !self.sets.iter().any(|s| s.id == id), "Set ID already exists or is empty");
                ensure!(!name.trim().is_empty(), "Set name is empty");
                let mut dictionaries = self.set(DEFAULT).dictionaries.clone();
                for d in &mut dictionaries {
                    d.disabled = true;
                }
                self.sets.push(Set { id, name: name.trim().into(), languages: vec![], dictionaries });
            }
            Edit::Delete { set } => {
                ensure!(set != DEFAULT, "Default cannot be deleted");
                self.find_mut(&set)?;
                self.sets.retain(|s| s.id != set);
            }
            Edit::Rename { set, name } => {
                ensure!(!name.trim().is_empty(), "Set name is empty");
                self.find_mut(&set)?.name = name.trim().into();
            }
            Edit::MoveSet { set, position } => {
                self.find_mut(&set)?;
                let index = self.sets.iter().position(|s| s.id == set).unwrap();
                let set = self.sets.remove(index);
                self.sets.insert(position.min(self.sets.len()), set);
            }
            Edit::Languages { set, languages } => {
                let languages = clean_languages(&languages)?;
                ensure!(set != DEFAULT || languages.is_empty(), "Default has no languages");
                self.find_mut(&set)?;
                // Assignment transfers ownership from any previous set.
                for other in &mut self.sets {
                    other.languages.retain(|l| !languages.contains(l));
                }
                self.find_mut(&set)?.languages = languages;
            }
            Edit::Disabled { set, dictionary, disabled } => {
                let dictionaries = &mut self.find_mut(&set)?.dictionaries;
                let entry = dictionaries.iter_mut().find(|d| d.id == dictionary);
                let Some(entry) = entry else { anyhow::bail!("Unknown dictionary") };
                entry.disabled = disabled;
            }
            Edit::Forget { dictionary, .. } => {
                for set in &mut self.sets {
                    set.dictionaries.retain(|d| d.id != dictionary);
                }
            }
            Edit::MoveDictionary { set, dictionary, position } => {
                let dictionaries = &mut self.find_mut(&set)?.dictionaries;
                let Some(index) = dictionaries.iter().position(|d| d.id == dictionary) else {
                    anyhow::bail!("Unknown dictionary")
                };
                let entry = dictionaries.remove(index);
                dictionaries.insert(position.min(dictionaries.len()), entry);
            }
        }
        Ok(())
    }
    fn find_mut(&mut self, id: &str) -> Result<&mut Set> {
        self.sets.iter_mut().find(|s| s.id == id).ok_or_else(|| anyhow::anyhow!("Unknown dictionary set"))
    }
}
impl Set {
    pub fn english_forms(&self) -> bool {
        self.languages.is_empty() || self.languages.iter().any(|l| l == "en")
    }
    pub fn apply(&self, dictionaries: &mut [Dictionary]) {
        dictionaries.sort_by_key(|d| self.dictionaries.iter().position(|p| p.id == d.id).unwrap_or(usize::MAX));
        for dictionary in dictionaries {
            dictionary.disabled = self.dictionaries.iter().find(|p| p.id == dictionary.id).is_none_or(|p| p.disabled);
        }
    }
}
fn clean_languages(languages: &[String]) -> Result<Vec<String>> {
    let mut result = Vec::new();
    for language in languages {
        let language = normalize_language(language);
        ensure!(
            (2..=8).contains(&language.len()) && language.bytes().all(|c| c.is_ascii_lowercase()),
            "Invalid language tag"
        );
        if !result.contains(&language) {
            result.push(language);
        }
    }
    Ok(result)
}
pub fn normalize_language(tag: &str) -> String {
    let tag = tag.trim().split(['-', '_']).next().unwrap_or("").to_ascii_lowercase();
    match tag.as_str() {
        "eng" => "en",
        "rus" => "ru",
        "fra" | "fre" => "fr",
        "deu" | "ger" => "de",
        "spa" => "es",
        "ita" => "it",
        "por" => "pt",
        "ukr" => "uk",
        "bel" => "be",
        "bul" => "bg",
        "srp" => "sr",
        "mkd" | "mac" => "mk",
        "ell" | "gre" => "el",
        "hye" | "arm" => "hy",
        "ara" => "ar",
        "heb" => "he",
        "zho" | "chi" => "zh",
        "jpn" => "ja",
        "kor" => "ko",
        "nld" | "dut" => "nl",
        "pol" => "pl",
        "ces" | "cze" => "cs",
        "slk" | "slo" => "sk",
        "ron" | "rum" => "ro",
        "hun" => "hu",
        "tur" => "tr",
        "fin" => "fi",
        "swe" => "sv",
        "dan" => "da",
        "nor" => "no",
        "vie" => "vi",
        _ => &tag,
    }
    .into()
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Script {
    Latin,
    Cyrillic,
    Greek,
    Armenian,
    Arabic,
    Hebrew,
    Cjk,
    Hangul,
}
fn language_script(language: &str) -> Option<Script> {
    Some(match language {
        "en" | "fr" | "de" | "es" | "it" | "pt" | "nl" | "pl" | "cs" | "sk" | "ro" | "hu" | "tr" | "fi" | "sv"
        | "da" | "no" | "vi" | "la" => Script::Latin,
        "ru" | "uk" | "be" | "bg" | "mk" => Script::Cyrillic,
        "el" => Script::Greek,
        "hy" => Script::Armenian,
        "ar" | "fa" | "ur" => Script::Arabic,
        "he" | "yi" => Script::Hebrew,
        "zh" | "ja" => Script::Cjk,
        "ko" => Script::Hangul,
        _ => return None,
    })
}
fn word_script(query: &str) -> Option<Script> {
    // Punctuation and combining marks carry no script. Mixed-script words are
    // ambiguous and keep their chosen set, rather than guessing from one letter.
    let mut found = None;
    for c in query.chars().filter(|c| c.is_alphabetic()) {
        let script = match c as u32 {
            0x0041..=0x024f | 0x1e00..=0x1eff => Script::Latin,
            0x0400..=0x052f | 0x2de0..=0x2dff | 0xa640..=0xa69f => Script::Cyrillic,
            0x0370..=0x03ff | 0x1f00..=0x1fff => Script::Greek,
            0x0530..=0x058f => Script::Armenian,
            0x0600..=0x06ff | 0x0750..=0x077f | 0x08a0..=0x08ff => Script::Arabic,
            0x0590..=0x05ff => Script::Hebrew,
            0x3040..=0x30ff | 0x3400..=0x9fff | 0xf900..=0xfaff | 0x20000..=0x3134f => Script::Cjk,
            0x1100..=0x11ff | 0x3130..=0x318f | 0xac00..=0xd7af => Script::Hangul,
            _ => return None,
        };
        if found.is_some_and(|previous| previous != script) {
            return None;
        }
        found = Some(script);
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    fn language_set(p: &mut Preferences, id: &str, languages: &[&str]) {
        p.edit(Edit::Create { id: id.into(), name: id.into() }).unwrap();
        p.edit(Edit::Languages { set: id.into(), languages: languages.iter().map(|s| (*s).into()).collect() }).unwrap();
    }
    #[test]
    fn language_tags_normalize_and_assignments_transfer_between_sets() {
        for (tag, expected) in [
            (" EN-us ", "en"),
            ("eng-GB", "en"),
            ("RUS_Cyrl_RU", "ru"),
            ("deu", "de"),
            ("FRE", "fr"),
            ("hy-AM", "hy"),
            ("", ""),
            ("zz-XX", "zz"),
        ] {
            assert_eq!(normalize_language(tag), expected);
        }
        let mut p = Preferences::default();
        language_set(&mut p, "one", &["en-US", "eng", "ru"]);
        assert_eq!(p.set("one").languages, ["en", "ru"]);
        language_set(&mut p, "two", &["eng"]);
        assert_eq!(p.set("one").languages, ["ru"]);
        assert_eq!(p.set("two").languages, ["en"]);
        let before = p.clone();
        assert!(p.edit(Edit::Languages { set: DEFAULT.into(), languages: vec!["en".into()] }).is_err());
        assert!(p.edit(Edit::Languages { set: "one".into(), languages: vec!["???".into()] }).is_err());
        assert_eq!(p, before);
    }
    #[test]
    fn per_book_then_language_then_default_with_script_fallback() {
        let mut p = Preferences::default();
        language_set(&mut p, "english", &["eng"]);
        language_set(&mut p, "russian", &["ru"]);
        language_set(&mut p, "unknown", &["zz"]);
        language_set(&mut p, "mixed", &["en", "ru"]);
        // Restore the single-language assignments after checking multilingual sets.
        assert_eq!(p.choose(&Selection { book_set: Some("mixed".into()), language: "".into() }, "слово").id, "mixed");
        p.edit(Edit::Languages { set: "english".into(), languages: vec!["en".into()] }).unwrap();
        p.edit(Edit::Languages { set: "russian".into(), languages: vec!["ru".into()] }).unwrap();
        for (book, language, word, expected) in [
            (None, "en-US", "word", "english"),
            (None, "rus", "слово", "russian"),
            (Some("english"), "ru", "word", "english"),
            (Some("deleted"), "en", "word", "english"),
            (Some(DEFAULT), "en", "слово", DEFAULT),
            (None, "", "word", DEFAULT),
            (None, "xx", "word", DEFAULT),
            (Some("english"), "en", "слово", "russian"),
            (Some("russian"), "ru", "word", "english"),
            (Some("english"), "en", "λέξη", DEFAULT),
            (Some("unknown"), "en", "слово", "unknown"),
            (Some("english"), "en", "123?!", "english"),
            (Some("english"), "en", "aя", "english"),
            (Some("english"), "en", "शब्द", "english"),
        ] {
            let selection = Selection { book_set: book.map(str::to_owned), language: language.into() };
            assert_eq!(p.choose(&selection, word).id, expected, "{selection:?} {word}");
        }
        p.edit(Edit::Delete { set: "russian".into() }).unwrap();
        assert_eq!(
            p.choose(&Selection { book_set: Some("english".into()), language: "en".into() }, "слово").id,
            DEFAULT
        );
    }
    #[test]
    fn migration_preserves_missing_ids_order_and_disabled_flags() {
        let legacy = r#"[{"id":"missing.ifo","disabled":true},{"id":"/external/b.ifo","disabled":false},{"id":"a.ifo","disabled":true}]"#;
        let (p, migrated) = Preferences::parse(legacy).unwrap();
        assert!(migrated);
        assert_eq!(p.sets.len(), 1);
        assert_eq!(p.set(DEFAULT).name, "Default");
        assert!(p.set(DEFAULT).languages.is_empty());
        assert_eq!(serde_json::to_string(&p.set(DEFAULT).dictionaries).unwrap(), legacy);
        let text = serde_json::to_string(&p).unwrap();
        let (restored, migrated) = Preferences::parse(&text).unwrap();
        assert!(!migrated);
        assert_eq!(restored, p);
        for invalid in [
            "{broken",
            r#"{"sets":[]}"#,
            r#"{"sets":[{"id":"default","name":"Default","languages":["en"],"dictionaries":[]}]}"#,
        ] {
            assert!(Preferences::parse(invalid).is_err());
        }
    }
}
