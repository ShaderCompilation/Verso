use super::{Encoding, MAGIC, Pack, VERSION, fold, is_word, opencorpora::Dictionary, write::Writer};
use anyhow::Result;

const FIXTURE: &str = include_str!("../testdata/opencorpora.xml");

fn pack_bytes(encoding: Encoding) -> Result<Vec<u8>> {
    let dict = Dictionary::read(FIXTURE.as_bytes())?;
    let mut writer = Writer::new(encoding, &[("language", "ru"), ("revision", &dict.revision.clone())])?;
    let summary = dict.write(&mut writer)?;
    assert_eq!(summary.lexemes, 24);
    assert_eq!(summary.ignored_links.get("PRTF-PRTS from an active participle"), Some(&1));
    assert_eq!(summary.dangling_links, 1);
    assert_eq!(summary.ignored_links.get("PERF-IMPF"), Some(&1));
    assert_eq!(summary.skipped_forms, 1, "1-й is not a word");
    Ok(writer.finish()?.0)
}

fn pack(encoding: Encoding) -> Result<Pack> {
    Pack::from_bytes(pack_bytes(encoding)?)
}

fn lemmas(pack: &Pack, word: &str) -> Vec<String> {
    pack.lemmas(word).unwrap().unwrap_or_default()
}

#[test]
fn links_lead_to_the_infinitive_and_full_adjective() -> Result<()> {
    for encoding in [Encoding::Sets, Encoding::Rules] {
        let pack = pack(encoding)?;
        assert_eq!(lemmas(&pack, "читающего"), ["читающий", "читать"], "own lemma first");
        assert_eq!(lemmas(&pack, "читая"), ["читая", "читать"]);
        assert_eq!(
            lemmas(&pack, "прочитана"),
            ["прочитанный", "прочитать", "прочитан"],
            "chain of links, no active participle"
        );
        assert_eq!(lemmas(&pack, "шла"), ["идти", "иду"], "verb forms put the infinitive first");
        assert_eq!(lemmas(&pack, "хороша"), ["хороший", "хорош"]);
        assert_eq!(lemmas(&pack, "получше"), ["лучше", "хороший", "хорошо"], "two parents");
        assert_eq!(lemmas(&pack, "делать"), ["делать"], "aspect pairs aren't followed");
        assert_eq!(lemmas(&pack, "ежа"), ["ёж"], "dangling links are ignored");
    }
    Ok(())
}

#[test]
fn ambiguous_forms_keep_own_lemmas_first_then_source_order() -> Result<()> {
    let pack = pack(Encoding::Sets)?;
    assert_eq!(lemmas(&pack, "стали"), ["сталь", "стать", "стал"]);
    assert_eq!(lemmas(&pack, "стала"), ["стать", "стал"]);
    assert_eq!(lemmas(&pack, "нее"), ["она", "нея"], "proper names go last");
    Ok(())
}

#[test]
fn keys_ignore_yo_case_and_stress_but_lemmas_keep_yo() -> Result<()> {
    for encoding in [Encoding::Sets, Encoding::Rules] {
        let pack = pack(encoding)?;
        for word in ["шёл", "шел", "ШЁЛ", "ше\u{301}л"] {
            assert_eq!(lemmas(&pack, word), ["идти", "иду"], "{word}");
        }
        assert_eq!(lemmas(&pack, "елки"), ["ёлка"]);
        assert_eq!(lemmas(&pack, "Ежи"), ["ёж"]);
    }
    Ok(())
}

#[test]
fn hyphenated_words_and_the_word_gate() -> Result<()> {
    let pack = pack(Encoding::Sets)?;
    assert_eq!(lemmas(&pack, "кое-кого"), ["кое-кто"]);
    for word in ["кое--кого", "-кого", "кое-", "", "кое кого", "1-й", "кошки!"] {
        assert!(pack.lemmas(word)?.is_none(), "{word:?}");
    }
    assert!(is_word("моло\u{301}ко") && !is_word("\u{301}ко") && !is_word(&"я".repeat(65)));
    assert!(pack.lemmas("кошки")?.is_none(), "unknown word");
    Ok(())
}

#[test]
fn both_encodings_give_the_same_lemmas_for_every_form() -> Result<()> {
    let (sets, rules) = (pack(Encoding::Sets)?, pack(Encoding::Rules)?);
    assert_eq!(sets.encoding(), Encoding::Sets);
    assert_eq!(rules.encoding(), Encoding::Rules);
    assert_eq!(sets.len(), rules.len());
    let mut forms = 0;
    for form in FIXTURE.split("<f t=\"").skip(1).filter_map(|s| s.split('"').next()) {
        if is_word(form) {
            forms += 1;
            assert!(!lemmas(&sets, form).is_empty(), "{form}");
            assert_eq!(lemmas(&sets, form), lemmas(&rules, form), "{form}");
        }
    }
    assert!(forms > 30);
    assert_eq!(sets.meta("language"), Some("ru"));
    assert_eq!(sets.meta("revision"), Some("417258"));
    assert_eq!(sets.meta("date"), None);
    Ok(())
}

#[test]
fn packs_round_trip_through_a_file() -> Result<()> {
    let dir = std::env::temp_dir().join(format!("vwf-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("ru.vwf");
    std::fs::write(&path, pack_bytes(Encoding::Rules)?)?;
    let pack = Pack::open(&path)?;
    assert_eq!(lemmas(&pack, "читающего"), ["читающий", "читать"]);
    drop(pack);
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

#[test]
fn the_writer_refuses_keys_out_of_order() -> Result<()> {
    let mut writer = Writer::new(Encoding::Sets, &[])?;
    writer.insert("б", &["б"])?;
    assert!(writer.insert("а", &["а"]).is_err());
    assert!(writer.insert("в", &[]).is_err(), "no lemmas");
    assert!(Writer::new(Encoding::Sets, &[("source", "a\tb")]).is_err());
    Ok(())
}

#[test]
fn bad_magic_version_or_sections_are_rejected() -> Result<()> {
    let good = pack_bytes(Encoding::Sets)?;
    let refuse = |bytes: Vec<u8>, why: &str| {
        let error = Pack::from_bytes(bytes).err().unwrap_or_else(|| panic!("accepted {why}"));
        error.to_string()
    };
    let mut bytes = good.clone();
    bytes[..8].copy_from_slice(b"NOTAPACK");
    assert!(refuse(bytes, "bad magic").contains("Not a word-form pack"));
    let mut bytes = good.clone();
    bytes[8..12].copy_from_slice(&(VERSION + 1).to_le_bytes());
    assert!(refuse(bytes, "a newer version").contains("version"));
    let mut bytes = good.clone();
    bytes[12..16].copy_from_slice(&9u32.to_le_bytes());
    assert!(refuse(bytes, "an unknown encoding").contains("encoding"));
    refuse(good[..40].to_vec(), "a truncated header");
    refuse(good[..good.len() - 1].to_vec(), "a truncated map");
    assert_eq!(&good[..8], MAGIC);

    // A values section cut to nothing: opening works, lookups report corruption.
    let mut bytes = good.clone();
    bytes[40..48].copy_from_slice(&0u64.to_le_bytes());
    let pack = Pack::from_bytes(bytes)?;
    assert!(pack.lemmas("стали").is_err());
    Ok(())
}

#[test]
fn fold_matches_dictionary_keys() {
    // The cases from the app's dictionary key tests (Phase 1), which now use
    // this function.
    assert_eq!(fold("ЁЖ"), "еж");
    assert_eq!(fold("моло\u{301}ко"), "молоко");
    assert_eq!(fold("ѝ"), "и");
    assert_eq!(fold("ї"), "ї");
    assert_eq!(fold("Й"), "й");
    assert_eq!(fold("CAFE\u{301}"), "café");
}
