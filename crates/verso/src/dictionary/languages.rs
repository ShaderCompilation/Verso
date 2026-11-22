//! Language codes, the scripts they are written in and their English names,
//! for every language whatlang detects and the usual Hunspell dictionaries.
//! One table drives tag normalization, script matching and display names.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Script {
    Latin,
    Cyrillic,
    Greek,
    Armenian,
    Georgian,
    Arabic,
    Hebrew,
    Devanagari,
    Bengali,
    Gurmukhi,
    Gujarati,
    Oriya,
    Tamil,
    Telugu,
    Kannada,
    Malayalam,
    Sinhala,
    Thai,
    Khmer,
    Myanmar,
    Ethiopic,
    Cjk,
    Hangul,
}

struct Language {
    /// The code Verso uses: ISO 639-1 where there is one.
    code: &'static str,
    /// Other spellings that mean the same language: ISO 639-3 (what whatlang
    /// returns), 639-2/B and retired codes.
    aliases: &'static [&'static str],
    scripts: &'static [Script],
    name: &'static str,
}

const fn l(
    code: &'static str,
    aliases: &'static [&'static str],
    scripts: &'static [Script],
    name: &'static str,
) -> Language {
    Language { code, aliases, scripts, name }
}

use Script::*;
const LATIN: &[Script] = &[Latin];
const CYRILLIC: &[Script] = &[Cyrillic];

/// Sorted by English name, which is the order Settings lists them in.
const LANGUAGES: &[Language] = &[
    l("af", &["afr"], LATIN, "Afrikaans"),
    l("ak", &["aka"], LATIN, "Akan"),
    l("sq", &["sqi", "alb"], LATIN, "Albanian"),
    l("am", &["amh"], &[Ethiopic], "Amharic"),
    l("ar", &["ara"], &[Arabic], "Arabic"),
    l("hy", &["hye", "arm"], &[Armenian], "Armenian"),
    l("az", &["aze"], LATIN, "Azerbaijani"),
    l("eu", &["eus", "baq"], LATIN, "Basque"),
    l("be", &["bel"], CYRILLIC, "Belarusian"),
    l("bn", &["ben"], &[Bengali], "Bengali"),
    l("bs", &["bos"], LATIN, "Bosnian"),
    l("br", &["bre"], LATIN, "Breton"),
    l("bg", &["bul"], CYRILLIC, "Bulgarian"),
    l("my", &["mya", "bur"], &[Myanmar], "Burmese"),
    l("ca", &["cat"], LATIN, "Catalan"),
    l("zh", &["zho", "chi", "cmn"], &[Cjk], "Chinese"),
    l("hr", &["hrv"], LATIN, "Croatian"),
    l("cs", &["ces", "cze"], LATIN, "Czech"),
    l("da", &["dan"], LATIN, "Danish"),
    l("nl", &["nld", "dut"], LATIN, "Dutch"),
    l("en", &["eng"], LATIN, "English"),
    l("eo", &["epo"], LATIN, "Esperanto"),
    l("et", &["est", "ekk"], LATIN, "Estonian"),
    l("fo", &["fao"], LATIN, "Faroese"),
    l("fi", &["fin"], LATIN, "Finnish"),
    l("fr", &["fra", "fre"], LATIN, "French"),
    l("fy", &["fry"], LATIN, "Frisian"),
    l("gl", &["glg"], LATIN, "Galician"),
    l("ka", &["kat", "geo"], &[Georgian], "Georgian"),
    l("de", &["deu", "ger"], LATIN, "German"),
    l("el", &["ell", "gre"], &[Greek], "Greek"),
    l("gu", &["guj"], &[Gujarati], "Gujarati"),
    l("he", &["heb", "iw"], &[Hebrew], "Hebrew"),
    l("hi", &["hin"], &[Devanagari], "Hindi"),
    l("hu", &["hun"], LATIN, "Hungarian"),
    l("is", &["isl", "ice"], LATIN, "Icelandic"),
    l("id", &["ind", "in"], LATIN, "Indonesian"),
    l("ga", &["gle"], LATIN, "Irish"),
    l("it", &["ita"], LATIN, "Italian"),
    l("ja", &["jpn"], &[Cjk], "Japanese"),
    l("jv", &["jav"], LATIN, "Javanese"),
    l("kn", &["kan"], &[Kannada], "Kannada"),
    l("kk", &["kaz"], CYRILLIC, "Kazakh"),
    l("km", &["khm"], &[Khmer], "Khmer"),
    l("ko", &["kor"], &[Hangul], "Korean"),
    l("ky", &["kir"], CYRILLIC, "Kyrgyz"),
    l("la", &["lat"], LATIN, "Latin"),
    l("lv", &["lav", "lvs"], LATIN, "Latvian"),
    l("lt", &["lit"], LATIN, "Lithuanian"),
    l("lb", &["ltz"], LATIN, "Luxembourgish"),
    l("mk", &["mkd", "mac"], CYRILLIC, "Macedonian"),
    l("ms", &["msa", "may", "zsm"], LATIN, "Malay"),
    l("ml", &["mal"], &[Malayalam], "Malayalam"),
    l("mt", &["mlt"], LATIN, "Maltese"),
    l("mr", &["mar"], &[Devanagari], "Marathi"),
    l("mn", &["mon", "khk"], CYRILLIC, "Mongolian"),
    l("ne", &["nep", "npi"], &[Devanagari], "Nepali"),
    l("no", &["nor"], LATIN, "Norwegian"),
    l("nb", &["nob"], LATIN, "Norwegian Bokmål"),
    l("nn", &["nno"], LATIN, "Norwegian Nynorsk"),
    l("oc", &["oci"], LATIN, "Occitan"),
    l("or", &["ori", "ory"], &[Oriya], "Odia"),
    l("fa", &["fas", "per", "pes"], &[Arabic], "Persian"),
    l("pl", &["pol"], LATIN, "Polish"),
    l("pt", &["por"], LATIN, "Portuguese"),
    l("pa", &["pan"], &[Gurmukhi], "Punjabi"),
    l("ro", &["ron", "rum", "mo", "mol"], LATIN, "Romanian"),
    l("ru", &["rus"], CYRILLIC, "Russian"),
    l("gd", &["gla"], LATIN, "Scottish Gaelic"),
    // Serbian is written in both; whatlang detects only the Cyrillic form.
    l("sr", &["srp"], &[Cyrillic, Latin], "Serbian"),
    l("sn", &["sna"], LATIN, "Shona"),
    l("si", &["sin"], &[Sinhala], "Sinhala"),
    l("sk", &["slk", "slo"], LATIN, "Slovak"),
    l("sl", &["slv"], LATIN, "Slovenian"),
    l("es", &["spa"], LATIN, "Spanish"),
    l("sw", &["swa", "swh"], LATIN, "Swahili"),
    l("sv", &["swe"], LATIN, "Swedish"),
    l("tl", &["tgl", "fil"], LATIN, "Tagalog"),
    l("tg", &["tgk"], CYRILLIC, "Tajik"),
    l("ta", &["tam"], &[Tamil], "Tamil"),
    l("tt", &["tat"], CYRILLIC, "Tatar"),
    l("te", &["tel"], &[Telugu], "Telugu"),
    l("th", &["tha"], &[Thai], "Thai"),
    l("tr", &["tur"], LATIN, "Turkish"),
    l("tk", &["tuk"], LATIN, "Turkmen"),
    l("uk", &["ukr"], CYRILLIC, "Ukrainian"),
    l("ur", &["urd"], &[Arabic], "Urdu"),
    l("uz", &["uzb"], LATIN, "Uzbek"),
    l("vi", &["vie"], LATIN, "Vietnamese"),
    l("cy", &["cym", "wel"], LATIN, "Welsh"),
    l("yi", &["yid", "ji"], &[Hebrew], "Yiddish"),
    l("zu", &["zul"], LATIN, "Zulu"),
];

fn find(code: &str) -> Option<&'static Language> {
    LANGUAGES.iter().find(|l| l.code == code)
}

/// The primary subtag, lowercased, with three-letter and retired codes
/// mapped to Verso's code: `deu-DE` → `de`, `cmn` → `zh`. Unknown
/// languages keep their primary subtag.
pub fn normalize_language(tag: &str) -> String {
    let tag = tag.trim().split(['-', '_']).next().unwrap_or("").to_ascii_lowercase();
    match LANGUAGES.iter().find(|l| l.aliases.contains(&tag.as_str())) {
        Some(language) => language.code.into(),
        None => tag,
    }
}

/// The scripts a normalized language is written in; empty when unknown.
pub fn language_scripts(language: &str) -> &'static [Script] {
    find(language).map_or(&[], |l| l.scripts)
}

/// Every known language as (code, English name), sorted by name.
pub fn all_languages() -> impl Iterator<Item = (&'static str, &'static str)> {
    LANGUAGES.iter().map(|l| (l.code, l.name))
}

/// The English name of a normalized language, or the code itself when unknown.
pub fn language_name(code: &str) -> &str {
    find(code).map_or(code, |l| l.name)
}

/// The script of every letter in `query`; None for no letters, mixed
/// scripts or a script outside the table.
pub fn word_script(query: &str) -> Option<Script> {
    // Punctuation and combining marks carry no script. Mixed-script words are
    // ambiguous and keep their chosen set, rather than guessing from one letter.
    let mut found = None;
    for c in query.chars().filter(|c| c.is_alphabetic()) {
        let script = match c as u32 {
            0x0041..=0x024f | 0x1e00..=0x1eff => Latin,
            0x0400..=0x052f | 0x1c80..=0x1c8f | 0x2de0..=0x2dff | 0xa640..=0xa69f => Cyrillic,
            0x0370..=0x03ff | 0x1f00..=0x1fff => Greek,
            0x0530..=0x058f => Armenian,
            0x10a0..=0x10ff | 0x1c90..=0x1cbf | 0x2d00..=0x2d2f => Georgian,
            0x0600..=0x06ff | 0x0750..=0x077f | 0x08a0..=0x08ff | 0xfb50..=0xfdff | 0xfe70..=0xfeff => Arabic,
            0x0590..=0x05ff | 0xfb1d..=0xfb4f => Hebrew,
            0x0900..=0x097f | 0xa8e0..=0xa8ff => Devanagari,
            0x0980..=0x09ff => Bengali,
            0x0a00..=0x0a7f => Gurmukhi,
            0x0a80..=0x0aff => Gujarati,
            0x0b00..=0x0b7f => Oriya,
            0x0b80..=0x0bff => Tamil,
            0x0c00..=0x0c7f => Telugu,
            0x0c80..=0x0cff => Kannada,
            0x0d00..=0x0d7f => Malayalam,
            0x0d80..=0x0dff => Sinhala,
            0x0e00..=0x0e7f => Thai,
            0x1780..=0x17ff | 0x19e0..=0x19ff => Khmer,
            0x1000..=0x109f => Myanmar,
            0x1200..=0x139f | 0x2d80..=0x2ddf => Ethiopic,
            0x3040..=0x30ff | 0x3400..=0x9fff | 0xf900..=0xfaff | 0x20000..=0x3134f => Cjk,
            0x1100..=0x11ff | 0x3130..=0x318f | 0xac00..=0xd7af => Hangul,
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
    #[test]
    fn codes_are_unique_and_aliases_unambiguous() {
        let mut seen = std::collections::HashSet::new();
        for language in LANGUAGES {
            assert!(seen.insert(language.code), "{}", language.code);
            assert!(!language.scripts.is_empty(), "{}", language.code);
            for alias in language.aliases {
                assert!(seen.insert(alias), "{alias}");
            }
        }
        assert!(LANGUAGES.windows(2).all(|w| w[0].name < w[1].name), "sorted by name");
    }
    #[test]
    fn every_whatlang_language_has_a_script_and_a_name() {
        for lang in whatlang::Lang::all() {
            let code = normalize_language(lang.code());
            assert_eq!(code.len(), 2, "{lang:?} → {code}");
            assert!(!language_scripts(&code).is_empty(), "{lang:?} has no script");
            assert_ne!(language_name(&code), code, "{lang:?} has no name");
        }
    }
    #[test]
    fn whatlang_scripts_match_the_table() {
        // A sample sentence's script must be one the detected language is written in.
        for (text, code) in [
            ("Der schnelle braune Fuchs springt über den faulen Hund", "de"),
            ("Быстрая коричневая лиса прыгает через ленивую собаку", "ru"),
            ("तेज़ भूरी लोमड़ी आलसी कुत्ते के ऊपर कूदती है", "hi"),
            ("სწრაფი ყავისფერი მელა ზარმაც ძაღლს გადაახტა", "ka"),
            ("ระบบการศึกษาของประเทศไทยมีการพัฒนาอย่างต่อเนื่อง", "th"),
        ] {
            let info = whatlang::detect(text).unwrap();
            assert_eq!(normalize_language(info.lang().code()), code);
            let word = text.split_whitespace().next().unwrap();
            let script = word_script(word).unwrap();
            assert!(language_scripts(code).contains(&script), "{code} {script:?}");
        }
    }
}
