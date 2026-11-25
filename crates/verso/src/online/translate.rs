//! Translation behind a small trait, so the service can be swapped. Google's
//! `gtx` endpoint (KOReader uses it too) needs no setup; it isn't documented,
//! so its reply is read defensively.
use serde_json::Value;

use super::{Error, MAX_REPLY_BYTES};
use crate::dictionary::languages::{all_languages, normalize_language};
use crate::net::Client;

/// Characters translated at once.
pub const TEXT_LIMIT: usize = 4000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Translation {
    pub text: String,
    /// The language the service says the text is in: Verso's code, or the
    /// service's for a language Verso doesn't know.
    pub detected: Option<String>,
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
    (!text.trim().is_empty()).then_some(Translation { text, detected })
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
        let translation = Translation { text: "The fox jumps. He's fast.".into(), detected: Some("de".into()) };
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
    }
}
