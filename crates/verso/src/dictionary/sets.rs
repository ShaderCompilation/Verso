//! Durable dictionary sets. Missing installations keep their position and flags.
//!
//! Each set keeps its enabled dictionaries first, in lookup order, and the
//! disabled ones after them: turning one on adds it at the end of the enabled
//! ones, and turning one off moves it to the top of the disabled ones.
use super::languages::{Script, language_scripts, normalize_language, word_script};
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
    /// Legacy arrays migrate in order, with enabled dictionaries first (lookups
    /// skip disabled ones, so lookup order is kept). For an invalid record the caller uses
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
            set.group();
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
                    let disabled = set.id != DEFAULT;
                    let at = if disabled { set.dictionaries.len() } else { set.boundary() };
                    set.dictionaries.insert(at, Preference { id: dictionary.id.clone(), disabled });
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
            || chosen.languages.iter().any(|l| language_scripts(l).is_empty() || language_scripts(l).contains(&script))
        {
            return chosen;
        }
        self.sets
            .iter()
            .find(|s| s.languages.iter().any(|l| language_scripts(l).contains(&script)))
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
                let set = self.find_mut(&set)?;
                let Some(index) = set.dictionaries.iter().position(|d| d.id == dictionary) else {
                    anyhow::bail!("Unknown dictionary")
                };
                if set.dictionaries[index].disabled != disabled {
                    let mut entry = set.dictionaries.remove(index);
                    entry.disabled = disabled;
                    let at = set.boundary();
                    set.dictionaries.insert(at, entry);
                }
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
    /// The index after the last enabled dictionary.
    pub fn boundary(&self) -> usize {
        self.dictionaries.iter().rposition(|d| !d.disabled).map_or(0, |i| i + 1)
    }
    /// Enabled dictionaries first, each group in its current order.
    fn group(&mut self) {
        self.dictionaries.sort_by_key(|d| d.disabled);
    }
    /// The languages whose base forms to try for `query`, best first; the
    /// lookup uses the first one with an analyzer. The set's own languages,
    /// or for a set without any (Default) the book's language and then the
    /// usual language of the word's script (Latin → English, Cyrillic →
    /// Russian). Only languages written in the word's script are kept: the
    /// script decides, since book language tags are often missing or wrong,
    /// and the book language only breaks ties within a script.
    pub fn form_languages(&self, book_language: &str, query: &str) -> Vec<String> {
        let Some(script) = word_script(query) else { return Vec::new() };
        let candidates = if self.languages.is_empty() {
            let usual = match script {
                Script::Latin => Some("en"),
                Script::Cyrillic => Some("ru"),
                _ => None,
            };
            std::iter::once(normalize_language(book_language)).chain(usual.map(str::to_owned)).collect()
        } else {
            self.languages.clone()
        };
        let mut languages = Vec::new();
        for language in candidates {
            if language_scripts(&language).contains(&script) && !languages.contains(&language) {
                languages.push(language);
            }
        }
        languages
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
            // Devanagari is a known script now: no Devanagari set, so Default.
            (Some("english"), "en", "शब्द", DEFAULT),
            // A script outside the table keeps the chosen set.
            (Some("english"), "en", "བོད", "english"),
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
    fn form_languages_follow_the_set_then_book_and_script() {
        let mut p = Preferences::default();
        language_set(&mut p, "russian", &["ru"]);
        language_set(&mut p, "slavic", &["uk", "be", "en"]);
        language_set(&mut p, "unknown", &["zz"]);
        for (set, book, word, expected) in [
            // Default: the book's language, then the script's usual language.
            (DEFAULT, "", "thunders", vec!["en"]),
            (DEFAULT, "", "кошки", vec!["ru"]),
            (DEFAULT, "ru", "thunders", vec!["en"]),
            (DEFAULT, "en", "кошки", vec!["ru"]),
            (DEFAULT, "uk-UA", "кошки", vec!["uk", "ru"]),
            (DEFAULT, "fra", "chats", vec!["fr", "en"]),
            (DEFAULT, "rus", "кошки", vec!["ru"]),
            // A set's languages replace both, in their order, within the script.
            ("russian", "uk", "кошки", vec!["ru"]),
            ("russian", "en", "thunders", vec![]),
            ("slavic", "ru", "кошки", vec!["uk", "be"]),
            ("slavic", "", "thunders", vec!["en"]),
            ("unknown", "", "thunders", vec![]),
            // No single script: no base forms.
            (DEFAULT, "en", "aя", vec![]),
            (DEFAULT, "en", "123", vec![]),
            (DEFAULT, "el", "λέξεις", vec!["el"]),
            (DEFAULT, "", "λέξεις", vec![]),
        ] {
            assert_eq!(p.set(set).form_languages(book, word), expected, "{set} {book} {word}");
        }
    }
    #[test]
    fn enabled_dictionaries_stay_first_in_lookup_order() {
        fn order(p: &Preferences, set: &str) -> String {
            let set = p.set(set);
            set.dictionaries.iter().map(|d| if d.disabled { d.id.to_lowercase() } else { d.id.clone() }).collect()
        }
        let dictionary = |id: &str| Dictionary {
            id: id.into(),
            name: id.into(),
            path: id.into(),
            relative: id.into(),
            source: 0,
            word_count: None,
            error: None,
            disabled: false,
        };
        let mut p = Preferences::default();
        p.discover(&[dictionary("A"), dictionary("B"), dictionary("C")]);
        language_set(&mut p, "x", &[]);
        assert_eq!(order(&p, DEFAULT), "ABC");
        assert_eq!(order(&p, "x"), "abc");
        let toggle = |p: &mut Preferences, id: &str, disabled| {
            p.edit(Edit::Disabled { set: "x".into(), dictionary: id.into(), disabled }).unwrap();
        };
        // Turning on appends to the enabled ones, in the order they were turned on.
        toggle(&mut p, "C", false);
        toggle(&mut p, "A", false);
        assert_eq!(order(&p, "x"), "CAb");
        // Turning off moves to the top of the disabled ones; a repeat is a no-op.
        toggle(&mut p, "C", true);
        assert_eq!(order(&p, "x"), "Acb");
        toggle(&mut p, "C", true);
        assert_eq!(order(&p, "x"), "Acb");
        // New dictionaries: enabled ones join the end of Default's enabled
        // group, disabled ones go last elsewhere.
        toggle(&mut p, "A", true);
        p.edit(Edit::Disabled { set: DEFAULT.into(), dictionary: "B".into(), disabled: true }).unwrap();
        p.discover(&[dictionary("D")]);
        assert_eq!(order(&p, DEFAULT), "ACDb");
        assert_eq!(order(&p, "x"), "acbd");
        assert_eq!(p.set(DEFAULT).boundary(), 3);
        assert_eq!(p.set("x").boundary(), 0);
    }
    #[test]
    fn migration_preserves_missing_ids_order_and_disabled_flags() {
        let legacy = r#"[{"id":"missing.ifo","disabled":true},{"id":"/external/b.ifo","disabled":false},{"id":"a.ifo","disabled":true}]"#;
        let (p, migrated) = Preferences::parse(legacy).unwrap();
        assert!(migrated);
        assert_eq!(p.sets.len(), 1);
        assert_eq!(p.set(DEFAULT).name, "Default");
        assert!(p.set(DEFAULT).languages.is_empty());
        // Enabled first; each group keeps its order and flags.
        let grouped = r#"[{"id":"/external/b.ifo","disabled":false},{"id":"missing.ifo","disabled":true},{"id":"a.ifo","disabled":true}]"#;
        assert_eq!(serde_json::to_string(&p.set(DEFAULT).dictionaries).unwrap(), grouped);
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
