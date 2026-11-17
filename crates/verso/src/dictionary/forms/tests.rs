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
    let mut forms = WordForms::new(dir.0.clone());
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
    let mut forms = WordForms::new(dir.0.clone());
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
    let mut forms = WordForms::new(dir.0.join("missing"));
    let status = forms.status();
    assert_eq!(status[0], WordFormsStatus { language: "en".into(), source: WordFormsSource::BuiltIn });
    assert_eq!(status[1].language, "ru");
    assert!(matches!(&status[1].source, WordFormsSource::Error(e) if e.contains("ru.vwf")), "{status:?}");
    assert_eq!(status.len(), 2);

    dir.pack("ru", RUSSIAN);
    dir.pack("uk", &[("кошки", &["кішка"])]);
    // A pack can't replace built-in English; other files are ignored.
    dir.pack("en", &[("dogs", &["cat"])]);
    fs::write(dir.0.join("notes.txt"), "").unwrap();
    fs::write(dir.0.join("Bad.vwf"), "").unwrap();
    let mut forms = WordForms::new(dir.0.clone());
    let status: Vec<_> = forms.status().into_iter().map(|s| (s.language, s.source)).collect();
    let pack = || WordFormsSource::Pack("Test pack, 2026-11-17".into());
    assert_eq!(status, [("en".into(), WordFormsSource::BuiltIn), ("ru".into(), pack()), ("uk".into(), pack())]);
    assert_eq!(forms.candidates(&languages(&["en"]), "dogs").unwrap().rules, ["dog"]);
}

#[test]
fn broken_packs_report_errors_and_reset_retries_them() {
    let dir = Dir::new();
    let path = dir.0.join("ru.vwf");
    let mut forms = WordForms::new(dir.0.clone());
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
    let mut forms = WordForms::new(dir.0.clone());
    assert!(matches!(forms.status()[1].source, WordFormsSource::Pack(_)), "the header is fine");
    assert_eq!(lemmas(&mut forms, &["ru"], "кошки"), None);
    let status = forms.status();
    assert!(matches!(&status[1].source, WordFormsSource::Error(e) if e.contains("word-form")), "{status:?}");
}
