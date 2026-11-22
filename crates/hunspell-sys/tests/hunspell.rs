//! Tests on small original fixtures: `de_DE` (UTF-8) and `fr_FR` (ISO8859-1),
//! plus broken dictionaries written to a temporary folder.

use std::fs;
use std::path::{Path, PathBuf};

use hunspell_sys::{Encoding, Error, Hunspell, MAX_FILE_BYTES};

fn fixture(name: &str) -> (PathBuf, PathBuf) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    (dir.join(format!("{name}.aff")), dir.join(format!("{name}.dic")))
}

fn open(name: &str) -> Hunspell {
    let (aff, dic) = fixture(name);
    Hunspell::open(&aff, &dic).unwrap()
}

/// Writes an .aff/.dic pair (raw bytes) into `dir` and opens it.
fn open_bytes(dir: &Path, aff: &[u8], dic: &[u8]) -> Result<Hunspell, Error> {
    let (a, d) = (dir.join("t.aff"), dir.join("t.dic"));
    fs::write(&a, aff).unwrap();
    fs::write(&d, dic).unwrap();
    Hunspell::open(&a, &d)
}

#[test]
fn library_version() {
    assert_eq!(hunspell_sys::library_version(), "1.7.6");
}

#[test]
fn utf8_stems() {
    let h = open("de_DE");
    assert_eq!(h.encoding(), Encoding::Utf8);
    assert_eq!(h.stem("Häuser"), ["Haus"]);
    assert_eq!(h.stem("Mäuse"), ["Maus"]);
    assert_eq!(h.stem("Kinder"), ["Kind"]);
    assert_eq!(h.stem("Kindes"), ["Kind"]);
    assert_eq!(h.stem("schönsten"), ["schön"]);
    // A base form stems to itself; a sentence-initial capital is handled.
    assert_eq!(h.stem("Haus"), ["Haus"]);
    assert_eq!(h.stem("Schöner"), ["schön"]);
    // Unknown, empty and unconvertible words give nothing.
    assert!(h.stem("Baum").is_empty());
    assert!(h.stem("").is_empty());
    assert!(h.stem("Ha\0us").is_empty());
}

#[test]
fn latin1_round_trip() {
    let h = open("fr_FR");
    assert_eq!(h.encoding(), Encoding::Latin1);
    assert_eq!(h.stem("chevaux"), ["cheval"]);
    assert_eq!(h.stem("élèves"), ["élève"]);
    assert_eq!(h.stem("étés"), ["été"]);
    assert_eq!(h.stem("Élèves"), ["élève"]);
    // Characters outside ISO8859-1 can't be looked up.
    assert!(h.stem("élèveŝ").is_empty());
    assert!(h.stem("чевал").is_empty());
}

#[test]
fn encoding_names() {
    assert_eq!(Encoding::from_name("UTF-8"), Some(Encoding::Utf8));
    assert_eq!(Encoding::from_name("ISO8859-1"), Some(Encoding::Latin1));
    assert_eq!(Encoding::from_name("iso-8859-1"), Some(Encoding::Latin1));
    assert_eq!(Encoding::from_name("ISO8859-15"), None);
    assert_eq!(Encoding::from_name("KOI8-R"), None);
    assert_eq!(Encoding::from_name("microsoft-cp1251"), None);
    // Hunspell only turns on UTF-8 mode for the exact name.
    assert_eq!(Encoding::from_name("utf-8"), None);
}

#[test]
fn is_send() {
    fn send<T: Send>() {}
    send::<Hunspell>();
}

#[test]
fn reopen_and_move_to_another_thread() {
    for _ in 0..20 {
        drop(open("de_DE"));
    }
    let h = open("de_DE");
    let stems = std::thread::spawn(move || h.stem("Häuser")).join().unwrap();
    assert_eq!(stems, ["Haus"]);
}

#[test]
fn missing_files() {
    let (aff, dic) = fixture("de_DE");
    let none = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/xx.aff");
    assert!(matches!(Hunspell::open(&none, &dic), Err(Error::Missing(p)) if p == none));
    let none = none.with_extension("dic");
    assert!(matches!(Hunspell::open(&aff, &none), Err(Error::Missing(p)) if p == none));
}

#[test]
fn directory_is_unreadable() {
    let tmp = tempfile::tempdir().unwrap();
    let (_, dic) = fixture("de_DE");
    let err = Hunspell::open(tmp.path(), &dic).err().unwrap();
    assert!(matches!(err, Error::Unreadable(..)), "{err}");
}

#[test]
fn too_large() {
    let tmp = tempfile::tempdir().unwrap();
    let (aff, _) = fixture("de_DE");
    let dic = tmp.path().join("big.dic");
    let f = fs::File::create(&dic).unwrap();
    // Sparse: no real disk space used.
    f.set_len(MAX_FILE_BYTES + 1).unwrap();
    let err = Hunspell::open(&aff, &dic).err().unwrap();
    assert!(matches!(err, Error::TooLarge { bytes, .. } if bytes == MAX_FILE_BYTES + 1), "{err}");
}

#[test]
fn unsupported_encoding() {
    let tmp = tempfile::tempdir().unwrap();
    let err = open_bytes(tmp.path(), b"SET KOI8-R\nSFX A Y 1\nSFX A 0 s .\n", b"1\nword/A\n").err().unwrap();
    assert!(matches!(&err, Error::UnsupportedEncoding(name) if name == "KOI8-R"), "{err}");
    assert_eq!(err.to_string(), "unsupported encoding KOI8-R");
}

#[test]
fn default_encoding_is_latin1() {
    let tmp = tempfile::tempdir().unwrap();
    let h = open_bytes(tmp.path(), b"SFX A Y 1\nSFX A 0 s .\n", b"1\ncaf\xE9/A\n").unwrap();
    assert_eq!(h.encoding(), Encoding::Latin1);
    assert_eq!(h.stem("cafés"), ["café"]);
}

#[test]
fn bom_and_spaces_before_word_count() {
    let tmp = tempfile::tempdir().unwrap();
    let aff = "\u{FEFF}SET UTF-8\nSFX A Y 1\nSFX A 0 s .\n";
    let h = open_bytes(tmp.path(), aff.as_bytes(), "\u{FEFF} 1\nwörd/A\n".as_bytes()).unwrap();
    assert_eq!(h.encoding(), Encoding::Utf8);
    assert_eq!(h.stem("wörds"), ["wörd"]);
}

#[test]
fn bad_dic_header() {
    let tmp = tempfile::tempdir().unwrap();
    let aff = b"SET UTF-8\n";
    for dic in [&b""[..], b"\n", b"words\nHaus\n", b"0\nHaus\n", b"-3\nHaus\n", b"10000000\nHaus\n", b"\xFF\xFE\x001\n"]
    {
        let err = open_bytes(tmp.path(), aff, dic).err();
        assert!(matches!(err, Some(Error::BadDic(_))), "{dic:?}: {err:?}");
    }
}

/// Every truncation of a real .aff, and of its .dic, opens or fails without
/// crashing, and lookups on whatever loaded don't crash either.
#[test]
fn truncated_files_dont_crash() {
    let tmp = tempfile::tempdir().unwrap();
    let (aff, dic) = fixture("de_DE");
    let (aff, dic) = (fs::read(aff).unwrap(), fs::read(dic).unwrap());
    for end in 0..=aff.len() {
        if let Ok(h) = open_bytes(tmp.path(), &aff[..end], &dic) {
            for word in ["Häuser", "Mäuse", "Kinder", "schönsten", "Haus"] {
                h.stem(word);
            }
        }
    }
    for end in 0..=dic.len() {
        if let Ok(h) = open_bytes(tmp.path(), &aff, &dic[..end]) {
            h.stem("Häuser");
        }
    }
}

/// Malformed affix rules and dictionary lines: errors or empty results, no
/// crashes.
#[test]
fn malformed_rules_dont_crash() {
    let tmp = tempfile::tempdir().unwrap();
    let cases: &[(&str, &str)] = &[
        // An affix header promising more rules than follow.
        ("SET UTF-8\nSFX A Y 5\nSFX A 0 s .\n", "1\nword/A\n"),
        // Rules with missing fields and a header with a bad count.
        ("SET UTF-8\nSFX A Y 2\nSFX A\nSFX A 0\n", "1\nword/A\n"),
        ("SET UTF-8\nSFX A Y many\nSFX A 0 s .\n", "1\nword/A\n"),
        ("SET UTF-8\nPFX\nSFX\nREP 3\nREP a\n", "1\nword\n"),
        // Long and numeric flags with odd or garbage flag strings.
        ("SET UTF-8\nFLAG long\nSFX Aa Y 1\nSFX Aa 0 s .\n", "2\nword/Aab\nother/A\n"),
        ("SET UTF-8\nFLAG num\nSFX 1 Y 1\nSFX 1 0 s .\n", "2\nword/1,x,99999999\nother/,\n"),
        // Flag aliases pointing nowhere.
        ("SET UTF-8\nAF 1\nAF A\nSFX A Y 1\nSFX A 0 s .\n", "2\nword/7\nother/0\n"),
        // A condition with an unclosed bracket and a strip longer than words.
        ("SET UTF-8\nSFX A Y 2\nSFX A 0 s [abc\nSFX A wordwordword s .\n", "1\nword/A\n"),
        // A word count far smaller than the words, empty and flag-only lines.
        ("SET UTF-8\nSFX A Y 1\nSFX A 0 s .\n", "1\nword/A\n/A\n\n/\nother/\n"),
        // Invalid UTF-8 in a UTF-8 dictionary.
        ("SET UTF-8\nSFX A Y 1\nSFX A 0 s .\n", "2\nw\u{FFFD}rd/A\nword/A\n"),
        // Compounding with missing parameters.
        ("SET UTF-8\nCOMPOUNDFLAG\nCOMPOUNDMIN\nCOMPOUNDRULE 1\nCOMPOUNDRULE (\n", "1\nword\n"),
    ];
    for (aff, dic) in cases {
        if let Ok(h) = open_bytes(tmp.path(), aff.as_bytes(), dic.as_bytes()) {
            for word in ["word", "words", "wordword", "other", "Words", "s"] {
                h.stem(word);
            }
        }
    }
    // Raw invalid UTF-8 bytes in the .dic of a UTF-8 dictionary.
    let h = open_bytes(tmp.path(), b"SET UTF-8\n", b"2\nw\xC3rd\nword\n").unwrap();
    assert_eq!(h.stem("word"), ["word"]);
}

/// Pseudo-random bytes after a valid header, in both files.
#[test]
fn garbage_doesnt_crash() {
    let tmp = tempfile::tempdir().unwrap();
    let mut seed: u64 = 0x5eed;
    let mut next = move || {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (seed >> 33) as u8
    };
    // Mostly affix-file characters, so the parser gets past the first token.
    let alphabet = b"SFXPY 0123456789.[]^-/\nAaBbs\xC3\xA4\xC3\xB6\xC3\xFF\t";
    for round in 0..200 {
        let mut aff = if round % 2 == 0 { b"SET UTF-8\n".to_vec() } else { b"SET ISO8859-1\n".to_vec() };
        let mut dic = b"20\n".to_vec();
        for _ in 0..400 {
            aff.push(alphabet[next() as usize % alphabet.len()]);
            dic.push(alphabet[next() as usize % alphabet.len()]);
        }
        if let Ok(h) = open_bytes(tmp.path(), &aff, &dic) {
            for word in ["aSa", "bä0s", "S", "XX", "word"] {
                h.stem(word);
            }
        }
    }
}
