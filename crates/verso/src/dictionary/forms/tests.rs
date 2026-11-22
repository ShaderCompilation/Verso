use super::*;
use std::{
    fs,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);

/// A word-forms folder, removed when dropped.
struct Dir(PathBuf);
impl Dir {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "verso-wordforms-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn pack(&self, language: &str, forms: &[(&str, &[&str])]) {
        write_pack(&self.0.join(format!("{language}.vwf")), language, forms);
    }
}
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn languages(list: &[&str]) -> Vec<String> {
    list.iter().map(|l| (*l).to_owned()).collect()
}
fn lemmas(forms: &mut WordForms, list: &[&str], word: &str) -> Option<Vec<String>> {
    forms.candidates(&languages(list), word).map(|c| {
        assert!(c.possessive.is_none() && c.rules.is_empty(), "packs give only authoritative lemmas");
        c.exceptions
    })
}

const RUSSIAN: &[(&str, &[&str])] = &[
    ("кошки", &["кошка"]),
    ("кошка", &["кошка"]),
    ("шёл", &["идти", "иду"]),
    ("прочитана", &["прочитанный", "прочитать"]),
    ("ежа", &["ёж"]),
    ("ёж", &["ёж"]),
];

#[test]
fn tables_give_lemmas_best_first_without_the_word_itself() {
    let dir = Dir::new();
    dir.pack("ru", RUSSIAN);
    let mut forms = WordForms::new(dir.0.clone(), dir.0.join("hunspell"));
    let ru = &["ru"];
    assert_eq!(lemmas(&mut forms, ru, "кошки").unwrap(), ["кошка"]);
    assert_eq!(lemmas(&mut forms, ru, "Кошки").unwrap(), ["кошка"]);
    assert_eq!(lemmas(&mut forms, ru, "прочитана").unwrap(), ["прочитанный", "прочитать"]);
    // ё/е and stress marks fold like dictionary keys.
    assert_eq!(lemmas(&mut forms, ru, "шел").unwrap(), ["идти", "иду"]);
    assert_eq!(lemmas(&mut forms, ru, "шёл").unwrap(), ["идти", "иду"]);
    assert_eq!(lemmas(&mut forms, ru, "ко\u{301}шки").unwrap(), ["кошка"]);
    assert_eq!(lemmas(&mut forms, ru, "ежа").unwrap(), ["ёж"]);
    // A lemma folding like the word was already tried as the exact word.
    for word in ["кошка", "ёж", "еж", "собаки", "две кошки", ""] {
        assert_eq!(lemmas(&mut forms, ru, word), None, "{word:?}");
    }
}

#[test]
fn the_first_language_with_an_analyzer_decides() {
    let dir = Dir::new();
    dir.pack("ru", RUSSIAN);
    dir.pack("uk", &[("кошки", &["кішка"])]);
    let mut forms = WordForms::new(dir.0.clone(), dir.0.join("hunspell"));
    assert_eq!(lemmas(&mut forms, &["uk", "ru"], "кошки").unwrap(), ["кішка"]);
    assert_eq!(lemmas(&mut forms, &["ru", "uk"], "кошки").unwrap(), ["кошка"]);
    // Languages without an analyzer are skipped.
    assert_eq!(lemmas(&mut forms, &["be", "ru"], "кошки").unwrap(), ["кошка"]);
    assert_eq!(lemmas(&mut forms, &["be"], "кошки"), None);
    // The first analyzer's answer stands, even when it doesn't know the word.
    assert_eq!(lemmas(&mut forms, &["uk", "ru"], "шёл"), None);
    // English is built in, with its rules and possessives.
    let english = forms.candidates(&languages(&["fr", "en"]), "dogs'").unwrap();
    assert_eq!((english.possessive.as_deref(), english.rules), (Some("dogs"), vec!["dog".to_owned()]));
    assert_eq!(forms.candidates(&languages(&["en"]), "кошки"), None);
    assert_eq!(forms.candidates(&[], "dogs"), None);
}

#[test]
fn status_lists_english_bundled_and_found_packs() {
    let dir = Dir::new();
    let mut forms = WordForms::new(dir.0.join("missing"), dir.0.join("missing"));
    let status = forms.status();
    assert_eq!(
        status[0],
        WordFormsStatus { language: "en".into(), source: WordFormsSource::BuiltIn, files: vec![], unused: vec![] }
    );
    assert_eq!(status[1].language, "ru");
    assert!(matches!(&status[1].source, WordFormsSource::Error(e) if e.contains("ru.vwf")), "{status:?}");
    assert_eq!(status.len(), 2);

    dir.pack("ru", RUSSIAN);
    dir.pack("uk", &[("кошки", &["кішка"])]);
    // A pack can't replace built-in English; other files are ignored.
    dir.pack("en", &[("dogs", &["cat"])]);
    fs::write(dir.0.join("notes.txt"), "").unwrap();
    fs::write(dir.0.join("Bad.vwf"), "").unwrap();
    let mut forms = WordForms::new(dir.0.clone(), dir.0.join("hunspell"));
    let status: Vec<_> = forms.status().into_iter().map(|s| (s.language, s.source)).collect();
    let pack = || WordFormsSource::Pack("Test pack, 2026-11-17".into());
    assert_eq!(status, [("en".into(), WordFormsSource::BuiltIn), ("ru".into(), pack()), ("uk".into(), pack())]);
    assert_eq!(forms.candidates(&languages(&["en"]), "dogs").unwrap().rules, ["dog"]);
}

#[test]
fn broken_packs_report_errors_and_reset_retries_them() {
    let dir = Dir::new();
    let path = dir.0.join("ru.vwf");
    let mut forms = WordForms::new(dir.0.clone(), dir.0.join("hunspell"));
    assert_eq!(lemmas(&mut forms, &["ru"], "кошки"), None, "missing");
    // A missing pack is skipped, so the next language still answers.
    dir.pack("uk", &[("кошки", &["кішка"])]);
    forms.reset();
    assert_eq!(lemmas(&mut forms, &["ru", "uk"], "кошки").unwrap(), ["кішка"]);

    for (bytes, error) in [
        (b"not a pack at all, but long enough to have a header....................".to_vec(), "Not a word-form pack"),
        (b"VERSOVWF\x63\0\0\0".iter().copied().chain([0; 80]).collect(), "Unsupported word-form pack version"),
    ] {
        fs::write(&path, bytes).unwrap();
        forms.reset();
        assert_eq!(lemmas(&mut forms, &["ru"], "кошки"), None);
        let status = forms.status();
        assert!(matches!(&status[1].source, WordFormsSource::Error(e) if e.contains(error)), "{status:?}");
    }
    // A pack for another language is refused.
    write_pack(&path, "uk", RUSSIAN);
    forms.reset();
    assert!(matches!(&forms.status()[1].source, WordFormsSource::Error(e) if e.contains("not “ru”")));
    // The error stays until a reset, then the repaired pack works.
    dir.pack("ru", RUSSIAN);
    assert_eq!(lemmas(&mut forms, &["ru"], "кошки"), None);
    forms.reset();
    assert_eq!(lemmas(&mut forms, &["ru"], "кошки").unwrap(), ["кошка"]);
    assert_eq!(forms.status()[1].source, WordFormsSource::Pack("Test pack, 2026-11-17".into()));
}

#[test]
fn a_pack_corrupt_after_its_header_fails_on_read() {
    let dir = Dir::new();
    dir.pack("ru", RUSSIAN);
    let path = dir.0.join("ru.vwf");
    let mut bytes = fs::read(&path).unwrap();
    // Overwrite the values section (offset and length at bytes 32..48): the
    // header still opens, every value read fails.
    let values_offset = u64::from_le_bytes(bytes[32..40].try_into().unwrap()) as usize;
    let values_end = values_offset + u64::from_le_bytes(bytes[40..48].try_into().unwrap()) as usize;
    bytes[values_offset..values_end].fill(0xff);
    fs::write(&path, bytes).unwrap();
    let mut forms = WordForms::new(dir.0.clone(), dir.0.join("hunspell"));
    assert!(matches!(forms.status()[1].source, WordFormsSource::Pack(_)), "the header is fine");
    assert_eq!(lemmas(&mut forms, &["ru"], "кошки"), None);
    let status = forms.status();
    assert!(matches!(&status[1].source, WordFormsSource::Error(e) if e.contains("word-form")), "{status:?}");
}

impl Dir {
    fn hunspell(&self) -> PathBuf {
        self.0.join("hunspell")
    }
    fn forms(&self) -> WordForms {
        WordForms::new(self.0.clone(), self.hunspell())
    }
}

/// A Hunspell dictionary whose stems are easy to tell from the packs' and
/// English rules': `dogs` → `do`, `кошки` → `кошк`.
const ODD_AFF: &str = "SET UTF-8\nSFX A Y 2\nSFX A 0 gs .\nSFX A 0 и .\n";

fn source(forms: &mut WordForms, language: &str) -> WordFormsStatus {
    forms.status().into_iter().find(|s| s.language == language).unwrap()
}

#[test]
fn hunspell_gives_lemmas_for_languages_without_their_own_forms() {
    let dir = Dir::new();
    // In a subfolder, as an unpacked LibreOffice extension is, and at the top.
    install_hunspell_fixture(&dir.hunspell().join("dict-de"), "de_DE");
    install_hunspell_fixture(&dir.hunspell(), "fr_FR");
    let mut forms = dir.forms();
    assert_eq!(forms.open_language(), None, "nothing loads until used");
    assert_eq!(lemmas(&mut forms, &["de"], "Häuser").unwrap(), ["Haus"]);
    assert_eq!(lemmas(&mut forms, &["de"], "Kinder").unwrap(), ["Kind"]);
    // A sentence-initial capital; a base form is already the exact word.
    assert_eq!(lemmas(&mut forms, &["de"], "Schöner").unwrap(), ["schön"]);
    for word in ["Haus", "Baum", "zwei Häuser", "Häuser!", ""] {
        assert_eq!(lemmas(&mut forms, &["de"], word), None, "{word:?}");
    }
    assert_eq!(forms.open_language(), Some("de"));
    // ISO8859-1 words convert both ways. Opening French drops German.
    assert_eq!(lemmas(&mut forms, &["fr"], "chevaux").unwrap(), ["cheval"]);
    assert_eq!(lemmas(&mut forms, &["fr"], "Élèves").unwrap(), ["élève"]);
    assert_eq!(forms.open_language(), Some("fr"));
    // The first language with an analyzer decides, as for packs.
    assert_eq!(lemmas(&mut forms, &["it", "de", "fr"], "Häuser").unwrap(), ["Haus"]);
    assert_eq!(lemmas(&mut forms, &["de", "fr"], "chevaux"), None);
    let english = forms.candidates(&languages(&["en", "de"]), "Häuser");
    assert!(english.as_ref().is_none_or(|c| c.exceptions.is_empty()), "English answered: {english:?}");

    let status = forms.status();
    let found: Vec<_> = status.iter().map(|s| (s.language.as_str(), s.source.clone())).collect();
    let hunspell = |name: &str, encoding: &str, loaded| WordFormsSource::Hunspell {
        name: name.into(),
        encoding: encoding.into(),
        loaded,
    };
    assert_eq!(
        found[1..3],
        [("fr", hunspell("fr_FR", "ISO8859-1", false)), ("de", hunspell("de_DE", "UTF-8", true))],
        "{status:?}"
    );
    assert_eq!(status[2].files, ["dict-de/de_DE.aff", "dict-de/de_DE.dic"]);
    assert_eq!(status[3].language, "ru");
    forms.release();
    assert_eq!(forms.open_language(), None);
    assert!(matches!(source(&mut forms, "de").source, WordFormsSource::Hunspell { loaded: false, .. }));
}

#[test]
fn english_and_packs_beat_hunspell_for_their_languages() {
    let dir = Dir::new();
    dir.pack("ru", RUSSIAN);
    write_hunspell(&dir.hunspell(), "en_US", ODD_AFF, "1\ndo/A\n");
    write_hunspell(&dir.hunspell(), "ru_RU", ODD_AFF, "1\nкошк/A\n");
    let mut forms = dir.forms();
    let english = forms.candidates(&languages(&["en"]), "dogs").unwrap();
    assert_eq!((english.exceptions, english.rules), (vec![], vec!["dog".to_owned()]));
    assert_eq!(lemmas(&mut forms, &["ru"], "кошки").unwrap(), ["кошка"]);
    assert_eq!(forms.open_language(), None, "Hunspell never loads for English or a pack language");
    let status = forms.status();
    assert_eq!((&status[0].source, &status[0].unused[..]), (&WordFormsSource::BuiltIn, &["en_US.aff".to_owned()][..]));
    assert_eq!(status[1].language, "ru");
    assert_eq!(status[1].unused, ["ru_RU.aff"]);
    assert_eq!(status.len(), 2);
    // Even with the pack missing: Russian shows the pack's error, and gets
    // no Hunspell forms.
    fs::remove_file(dir.0.join("ru.vwf")).unwrap();
    forms.reset();
    assert_eq!(lemmas(&mut forms, &["ru"], "кошки"), None);
    assert!(matches!(source(&mut forms, "ru").source, WordFormsSource::Error(e) if e.contains("ru.vwf")));
    assert_eq!(forms.open_language(), None);
}

#[test]
fn hunspell_discovery_takes_the_language_from_the_name_and_the_first_by_name() {
    let dir = Dir::new();
    let h = dir.hunspell();
    install_hunspell_fixture(&h, "de_DE");
    install_hunspell_fixture(&h.join("austria"), "de_DE");
    fs::rename(h.join("austria/de_DE.aff"), h.join("austria/de_AT.aff")).unwrap();
    fs::rename(h.join("austria/de_DE.dic"), h.join("austria/de_AT.dic")).unwrap();
    install_hunspell_fixture(&h, "fr_FR");
    for (from, to) in [("fr_FR", "pt-BR"), ("fr_FR", ".hidden_es"), ("fr_FR", "notes")] {
        for ext in ["aff", "dic"] {
            fs::copy(h.join(format!("{from}.{ext}")), h.join(format!("{to}.{ext}"))).unwrap();
        }
    }
    // Two levels down is too deep.
    install_hunspell_fixture(&h.join("a/b"), "fr_FR");
    let mut forms = dir.forms();
    let status = forms.status();
    let found: Vec<_> = status.iter().map(|s| (s.language.as_str(), s.files.first().map(String::as_str))).collect();
    assert_eq!(
        found,
        [
            ("en", None),
            ("fr", Some("fr_FR.aff")),
            ("de", Some("austria/de_AT.aff")),
            ("pt", Some("pt-BR.aff")),
            ("ru", None)
        ]
    );
    assert_eq!(status[2].unused, ["de_DE.aff"]);
    assert_eq!(lemmas(&mut forms, &["de"], "Häuser").unwrap(), ["Haus"]);
    assert_eq!(lemmas(&mut forms, &["pt"], "chevaux").unwrap(), ["cheval"]);
}

#[test]
fn broken_hunspell_dictionaries_report_errors_and_reset_retries_them() {
    let dir = Dir::new();
    let h = dir.hunspell();
    write_hunspell(&h, "it_IT", "SET KOI8-R\n", "1\nparola\n");
    install_hunspell_fixture(&h, "de_DE");
    fs::remove_file(h.join("de_DE.dic")).unwrap();
    // Passes the file checks, but no word stems: nothing after the count.
    write_hunspell(&h, "fr_FR", "SET UTF-8\n", "1\n");
    let mut forms = dir.forms();
    assert_eq!(
        source(&mut forms, "it").source,
        WordFormsSource::Unsupported { name: "it_IT".into(), encoding: "KOI8-R".into() }
    );
    assert!(matches!(source(&mut forms, "de").source, WordFormsSource::Error(e) if e.contains("de_DE.dic is missing")));
    // Only loading finds an empty dictionary.
    assert!(matches!(source(&mut forms, "fr").source, WordFormsSource::Hunspell { .. }));
    for language in ["it", "de", "fr"] {
        assert_eq!(lemmas(&mut forms, &[language], "Häuser"), None);
    }
    assert!(matches!(source(&mut forms, "fr").source, WordFormsSource::Error(e) if e.contains("No words")));
    assert_eq!(forms.open_language(), None);
    // Repaired files stay failed until a reset, as packs do.
    install_hunspell_fixture(&h, "de_DE");
    install_hunspell_fixture(&h, "fr_FR");
    assert_eq!(lemmas(&mut forms, &["de"], "Häuser"), None);
    forms.reset();
    assert_eq!(lemmas(&mut forms, &["de"], "Häuser").unwrap(), ["Haus"]);
    assert_eq!(lemmas(&mut forms, &["fr"], "chevaux").unwrap(), ["cheval"]);
}

#[test]
fn warm_opens_what_the_first_language_with_an_analyzer_uses() {
    let dir = Dir::new();
    dir.pack("ru", RUSSIAN);
    install_hunspell_fixture(&dir.hunspell(), "de_DE");
    install_hunspell_fixture(&dir.hunspell(), "fr_FR");
    let mut forms = dir.forms();
    forms.warm(&languages(&["en", "de"]));
    forms.warm(&languages(&["ru", "de"]));
    forms.warm(&languages(&["it"]));
    assert_eq!(forms.open_language(), None);
    forms.warm(&languages(&["it", "de", "fr"]));
    assert_eq!(forms.open_language(), Some("de"));
    forms.warm(&languages(&["fr"]));
    assert_eq!(forms.open_language(), Some("fr"));
    forms.reset();
    assert_eq!(forms.open_language(), None, "a rescan reloads it");
}
