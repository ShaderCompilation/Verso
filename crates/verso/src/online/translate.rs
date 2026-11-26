//! Translation behind a small trait, so the service can be swapped. Google's
//! `gtx` endpoint (KOReader uses it too) needs no setup; DeepL Free/Pro is
//! optional with a user-supplied API key (`:fx` selects `api-free.deepl.com`).
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{Error, MAX_REPLY_BYTES};
use crate::dictionary::languages::{all_languages, normalize_language};
use crate::net::{Client, FetchError};

/// Characters translated at once.
pub const TEXT_LIMIT: usize = 4000;

/// DeepL v2 translation endpoints: keys ending with `:fx` belong to Free.
pub const DEEPL_FREE_URL: &str = "https://api-free.deepl.com/v2/translate";
pub const DEEPL_PRO_URL: &str = "https://api.deepl.com/v2/translate";

/// Which translation backend is configured or answered a lookup.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    #[default]
    Google,
    #[serde(rename = "deepl")]
    DeepL,
}

impl Provider {
    pub const ALL: [Provider; 2] = [Provider::Google, Provider::DeepL];

    pub fn service_name(self) -> &'static str {
        match self {
            Provider::Google => "Google Translate",
            Provider::DeepL => "DeepL",
        }
    }

    /// For the lookup card's narrow service button.
    pub fn short_name(self) -> &'static str {
        match self {
            Provider::Google => "Google",
            Provider::DeepL => "DeepL",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeepLTier {
    Free,
    Pro,
}

impl DeepLTier {
    pub fn as_str(self) -> &'static str {
        match self {
            DeepLTier::Free => "free",
            DeepLTier::Pro => "pro",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            DeepLTier::Free => "Free",
            DeepLTier::Pro => "Pro",
        }
    }
}

/// Free API keys end with `:fx`; all other DeepL keys target the Pro host.
pub fn deepl_tier(api_key: &str) -> DeepLTier {
    if api_key.trim().ends_with(":fx") { DeepLTier::Free } else { DeepLTier::Pro }
}

pub fn deepl_endpoint(api_key: &str) -> &'static str {
    match deepl_tier(api_key) {
        DeepLTier::Free => DEEPL_FREE_URL,
        DeepLTier::Pro => DEEPL_PRO_URL,
    }
}

/// Validates and trims a DeepL API key entered by the user. Empty input clears
/// the stored key (`Ok(None)`).
pub fn parse_deepl_key(raw: &str) -> Result<Option<String>, &'static str> {
    let key = raw.trim();
    if key.is_empty() {
        return Ok(None);
    }
    if key.len() > 256 {
        return Err("API key is too long.");
    }
    if !key.bytes().all(|b| b.is_ascii_graphic()) {
        return Err("That doesn’t look like a DeepL API key.");
    }
    Ok(Some(key.to_owned()))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Translation {
    pub text: String,
    /// The language the service says the text is in: Verso's code, or the
    /// service's for a language Verso doesn't know.
    pub detected: Option<String>,
    /// The service that produced this translation (may be Google if DeepL fell
    /// back because the key was empty or a language wasn't supported).
    pub provider: Provider,
}

pub trait Translator {
    /// Translates into `target`, from `source` or else whatever language the
    /// text turns out to be in.
    fn translate(&self, text: &str, source: Option<&str>, target: &str) -> Result<Translation, Error>;
}

/// Verso's code for a language tag. Norwegian is usually Bokmål, so they are
/// one language here.
fn language_code(tag: &str) -> String {
    match normalize_language(tag) {
        code if code == "nb" => "no".to_string(),
        code => code,
    }
}

/// Whether two language tags are one language to a reader.
pub fn same_language(a: &str, b: &str) -> bool {
    language_code(a) == language_code(b)
}

/// What needs no network to refuse.
pub fn check(text: &str, source: Option<&str>, target: &str) -> Result<(), Error> {
    if text.chars().count() > TEXT_LIMIT {
        return Err(Error::TextTooLong);
    }
    if source.is_some_and(|source| same_language(source, target)) {
        return Err(Error::SameLanguage);
    }
    Ok(())
}

pub struct GoogleGtx<'a> {
    pub client: &'a Client,
    pub url: &'a str,
}

/// The languages that can be translated, by name. Google has no Nynorsk.
pub fn languages() -> impl Iterator<Item = &'static str> {
    all_languages().map(|(code, _)| code).filter(|code| *code != "nn" && language_code(code) == *code)
}

/// The one a tag means.
pub fn language(tag: &str) -> Option<&'static str> {
    let code = language_code(tag);
    languages().find(|known| *known == code)
}

/// Google's code for a language. It answers unknown ones with an error page.
fn google_code(tag: &str) -> Result<&'static str, Error> {
    match language(tag) {
        Some("zh") => Ok("zh-CN"),
        Some(code) => Ok(code),
        None => Err(Error::Language(normalize_language(tag))),
    }
}

impl Translator for GoogleGtx<'_> {
    fn translate(&self, text: &str, source: Option<&str>, target: &str) -> Result<Translation, Error> {
        check(text, source, target)?;
        let source = source.map_or(Ok("auto"), google_code)?;
        let form = [("client", "gtx"), ("dt", "t"), ("sl", source), ("tl", google_code(target)?), ("q", text)];
        let reply = self.client.post_form(self.url, &form, MAX_REPLY_BYTES)?;
        parse_gtx(&reply).ok_or(Error::Unreadable)
    }
}

/// `[[["sentence", "original", …], …], null, "detected", …]`
fn parse_gtx(reply: &[u8]) -> Option<Translation> {
    let reply: Value = serde_json::from_slice(reply).ok()?;
    let sentences = reply.get(0)?.as_array()?;
    let text: String = sentences.iter().filter_map(|s| s.get(0)?.as_str()).collect();
    let detected = reply.get(2).and_then(Value::as_str).filter(|code| !code.is_empty() && *code != "auto");
    let detected = detected.map(|code| match code {
        // Google's old code for Javanese.
        "jw" => "jv".to_string(),
        code => language_code(code),
    });
    (!text.trim().is_empty()).then_some(Translation { text, detected, provider: Provider::Google })
}

pub struct DeepL<'a> {
    pub client: &'a Client,
    pub url: &'a str,
    pub api_key: &'a str,
}

/// DeepL's base uppercase code for its core Free/Pro languages.
fn deepl_base(tag: &str) -> Result<&'static str, Error> {
    match language_code(tag).as_str() {
        "ar" => Ok("AR"),
        "bg" => Ok("BG"),
        "cs" => Ok("CS"),
        "da" => Ok("DA"),
        "de" => Ok("DE"),
        "el" => Ok("EL"),
        "en" => Ok("EN"),
        "es" => Ok("ES"),
        "et" => Ok("ET"),
        "fi" => Ok("FI"),
        "fr" => Ok("FR"),
        "hu" => Ok("HU"),
        "id" => Ok("ID"),
        "it" => Ok("IT"),
        "ja" => Ok("JA"),
        "ko" => Ok("KO"),
        "lt" => Ok("LT"),
        "lv" => Ok("LV"),
        "nl" => Ok("NL"),
        "no" => Ok("NB"),
        "pl" => Ok("PL"),
        "pt" => Ok("PT"),
        "ro" => Ok("RO"),
        "ru" => Ok("RU"),
        "sk" => Ok("SK"),
        "sl" => Ok("SL"),
        "sv" => Ok("SV"),
        "tr" => Ok("TR"),
        "uk" => Ok("UK"),
        "zh" => Ok("ZH"),
        _ => Err(Error::Language(normalize_language(tag))),
    }
}

/// DeepL's `source_lang` rejects regional suffixes (`EN`, `PT`, `ZH`, `NB`).
fn deepl_source_code(tag: &str) -> Result<&'static str, Error> {
    deepl_base(tag)
}

/// DeepL's `target_lang` prefers explicit variants for English, Portuguese and
/// Simplified Chinese.
fn deepl_target_code(tag: &str) -> Result<&'static str, Error> {
    match deepl_base(tag)? {
        "EN" => Ok("EN-US"),
        "PT" => Ok("PT-BR"),
        "ZH" => Ok("ZH-HANS"),
        code => Ok(code),
    }
}

/// Why a lookup set to DeepL goes to Google instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fallback<'a> {
    NoKey,
    /// DeepL can't translate into this language.
    Into(&'a str),
    /// DeepL can't translate from this language, chosen by hand.
    From(&'a str),
}

/// What sends a DeepL lookup to Google, if anything.
pub fn deepl_fallback<'a>(api_key: Option<&str>, source: Option<&'a str>, target: &'a str) -> Option<Fallback<'a>> {
    if !api_key.is_some_and(|k| !k.trim().is_empty()) {
        Some(Fallback::NoKey)
    } else if deepl_target_code(target).is_err() {
        Some(Fallback::Into(target))
    } else {
        source.filter(|s| deepl_source_code(s).is_err()).map(Fallback::From)
    }
}

/// Which backend actually handles a request: DeepL falls back to Google when
/// no key is stored or when DeepL doesn't support a chosen language.
pub fn effective_provider(provider: Provider, api_key: Option<&str>, source: Option<&str>, target: &str) -> Provider {
    match provider {
        Provider::DeepL if deepl_fallback(api_key, source, target).is_none() => Provider::DeepL,
        _ => Provider::Google,
    }
}

impl Translator for DeepL<'_> {
    fn translate(&self, text: &str, source: Option<&str>, target: &str) -> Result<Translation, Error> {
        check(text, source, target)?;
        let key = self.api_key.trim();
        if key.is_empty() {
            return Err(Error::Unauthorized);
        }
        let target = deepl_target_code(target)?;
        let mut form = vec![("text", text), ("target_lang", target)];
        if let Some(source) = source {
            form.push(("source_lang", deepl_source_code(source)?));
        }
        let auth = format!("DeepL-Auth-Key {key}");
        let reply = self
            .client
            .post_form_headers(self.url, &[("Authorization", &auth)], &form, MAX_REPLY_BYTES)
            .map_err(|e| match e {
                FetchError::Status(403) => Error::Unauthorized,
                FetchError::Status(456) => Error::QuotaExceeded,
                other => Error::Fetch(other),
            })?;
        parse_deepl(&reply).ok_or(Error::Unreadable)
    }
}

/// `{"translations":[{"detected_source_language":"DE","text":"…"}]}`
fn parse_deepl(reply: &[u8]) -> Option<Translation> {
    let reply: Value = serde_json::from_slice(reply).ok()?;
    let items = reply.get("translations")?.as_array()?;
    let first = items.first()?;
    let text: String = items.iter().filter_map(|t| t.get("text")?.as_str()).collect::<Vec<_>>().join("\n");
    let detected = first
        .get("detected_source_language")
        .and_then(Value::as_str)
        .filter(|code| !code.is_empty())
        .map(language_code);
    (!text.trim().is_empty()).then_some(Translation { text, detected, provider: Provider::DeepL })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn languages_map_to_googles() {
        let missing: Vec<&str> = all_languages().map(|(code, _)| code).filter(|c| google_code(c).is_err()).collect();
        assert_eq!(missing, ["nn"]);
        for (tag, code) in [("zh-Hant", "zh-CN"), ("deu", "de"), ("iw", "he"), ("nb-NO", "no")] {
            assert_eq!(google_code(tag), Ok(code), "{tag}");
        }
        // None is listed under a second code.
        assert!(languages().all(|code| language(code) == Some(code)));
        for tag in ["und", "x-klingon", "", "auto", "en&q=x"] {
            assert!(matches!(google_code(tag), Err(Error::Language(_))), "{tag:?}");
        }
    }

    #[test]
    fn deepl_maps_endpoints_and_languages() {
        assert_eq!(deepl_endpoint("abc-123:fx"), DEEPL_FREE_URL);
        assert_eq!(deepl_endpoint("  abc-123:fx \n"), DEEPL_FREE_URL);
        assert_eq!(deepl_endpoint("abc-123"), DEEPL_PRO_URL);
        assert_eq!(deepl_tier("abc:fx"), DeepLTier::Free);
        assert_eq!(deepl_tier("abc"), DeepLTier::Pro);

        for (tag, src, dst) in [
            ("en-GB", "EN", "EN-US"),
            ("pt-PT", "PT", "PT-BR"),
            ("zh-Hant", "ZH", "ZH-HANS"),
            ("nb-NO", "NB", "NB"),
            ("no", "NB", "NB"),
            ("deu", "DE", "DE"),
            ("ru", "RU", "RU"),
        ] {
            assert_eq!(deepl_source_code(tag), Ok(src), "{tag}");
            assert_eq!(deepl_target_code(tag), Ok(dst), "{tag}");
        }
        for unsupported in ["af", "ka", "hi", "la", "und", "x-klingon"] {
            assert!(matches!(deepl_target_code(unsupported), Err(Error::Language(_))), "{unsupported}");
            assert_eq!(deepl_fallback(Some("k"), None, unsupported), Some(Fallback::Into(unsupported)));
        }

        assert_eq!(effective_provider(Provider::Google, Some("k:fx"), None, "de"), Provider::Google);
        assert_eq!(effective_provider(Provider::DeepL, None, None, "de"), Provider::Google);
        assert_eq!(effective_provider(Provider::DeepL, Some("   "), None, "de"), Provider::Google);
        assert_eq!(effective_provider(Provider::DeepL, Some("k:fx"), None, "de"), Provider::DeepL);
        assert_eq!(effective_provider(Provider::DeepL, Some("k:fx"), None, "ka"), Provider::Google);
        assert_eq!(effective_provider(Provider::DeepL, Some("k:fx"), Some("la"), "en"), Provider::Google);
        // The reason names the language DeepL lacks, the target first.
        assert_eq!(deepl_fallback(None, Some("hy"), "gd"), Some(Fallback::NoKey));
        assert_eq!(deepl_fallback(Some("k:fx"), Some("la"), "hy"), Some(Fallback::Into("hy")));
        assert_eq!(deepl_fallback(Some("k:fx"), Some("gd"), "en"), Some(Fallback::From("gd")));
        assert_eq!(deepl_fallback(Some("k:fx"), None, "en"), None);
        assert_eq!(deepl_fallback(Some("k:fx"), Some("fr"), "en"), None);
    }

    #[test]
    fn refuses_long_text_and_equal_languages() {
        let longest = "ж".repeat(TEXT_LIMIT);
        assert_eq!(check(&longest, None, "en"), Ok(()));
        assert_eq!(check(&(longest + "."), None, "en"), Err(Error::TextTooLong));
        assert_eq!(check("x", Some("de"), "en"), Ok(()));
        for (source, target) in [("en-GB", "en"), ("nb", "no"), ("deu", "de")] {
            assert_eq!(check("x", Some(source), target), Err(Error::SameLanguage), "{source} → {target}");
        }
        assert_eq!(check("x", Some("nn"), "nb"), Ok(()));
    }

    #[test]
    fn reads_replies_defensively() {
        let reply = r#"[[["The fox jumps. ","Der Fuchs springt. ",null],["He's fast.","Er ist schnell."],
            [null,null,"translit"]],null,"de",null]"#;
        let translation = Translation {
            text: "The fox jumps. He's fast.".into(),
            detected: Some("de".into()),
            provider: Provider::Google,
        };
        assert_eq!(parse_gtx(reply.as_bytes()), Some(translation));
        let detected = |code: &str| parse_gtx(format!(r#"[[["x","y"]],null,{code}]"#).as_bytes()).unwrap().detected;
        assert_eq!(detected("\"iw\"").as_deref(), Some("he"));
        assert_eq!(detected("\"zh-TW\"").as_deref(), Some("zh"));
        assert_eq!(detected("\"jw\"").as_deref(), Some("jv"));
        assert_eq!(detected("\"nb\"").as_deref(), Some("no"));
        assert_eq!(detected("null"), None);
        for reply in ["", "<html>", "{}", "[]", "[null]", "[[]]", "[\"x\"]", r#"[[[null,"y"]],null,"de"]"#] {
            assert_eq!(parse_gtx(reply.as_bytes()), None, "{reply}");
        }

        let deepl = r#"{"translations":[{"detected_source_language":"NB","text":"The fox jumps."}]}"#;
        assert_eq!(
            parse_deepl(deepl.as_bytes()),
            Some(Translation { text: "The fox jumps.".into(), detected: Some("no".into()), provider: Provider::DeepL })
        );
        for bad in ["", "<html>", "{}", r#"{"translations":[]}"#, r#"{"translations":[{"text":"  "}]}"#] {
            assert_eq!(parse_deepl(bad.as_bytes()), None, "{bad}");
        }
    }
}
