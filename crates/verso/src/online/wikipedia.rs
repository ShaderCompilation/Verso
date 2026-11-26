//! Wikipedia through the MediaWiki Action API. A lookup searches first, which
//! handles case, accents and typos (`titles=` is case-sensitive), then fetches
//! the article if the best hit is clearly the one meant. Otherwise the reader
//! picks from the hits.
use serde::Deserialize;

use super::{Error, MAX_REPLY_BYTES, QUERY_LIMIT};
use crate::dictionary::languages::{language_name, normalize_language};
use crate::library::search::fold;
use crate::net::Client;

const HITS: &str = "10";

/// Wikipedias in Verso's languages that are open, by subdomain. Anything else
/// is refused: book language tags are free text, and this ends up in a host
/// name. Akan's is closed.
#[rustfmt::skip]
const EDITIONS: &[&str] = &[
    "af", "am", "ar", "az", "be", "bg", "bn", "br", "bs", "ca", "cs", "cy", "da", "de", "el", "en", "eo", "es", "et",
    "eu", "fa", "fi", "fo", "fr", "fy", "ga", "gd", "gl", "gu", "he", "hi", "hr", "hu", "hy", "id", "is", "it", "ja",
    "jv", "ka", "kk", "km", "kn", "ko", "ky", "la", "lb", "lt", "lv", "mk", "ml", "mn", "mr", "ms", "mt", "my", "ne",
    "nl", "nn", "no", "oc", "or", "pa", "pl", "pt", "ro", "ru", "si", "sk", "sl", "sn", "sq", "sr", "sv", "sw", "ta",
    "te", "tg", "th", "tk", "tl", "tr", "tt", "uk", "ur", "uz", "vi", "yi", "zh", "zu",
];

/// A Wikipedia that exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Edition(&'static str);

impl Edition {
    pub const ENGLISH: Edition = Edition("en");

    /// The Wikipedia in a language, given any tag for it.
    pub fn for_language(tag: &str) -> Option<Self> {
        let code = match normalize_language(tag).as_str() {
            // The Norwegian Wikipedia is in Bokmål.
            "nb" => "no".to_string(),
            code => code.to_string(),
        };
        EDITIONS.iter().find(|e| **e == code).map(|e| Edition(e))
    }

    pub fn code(self) -> &'static str {
        self.0
    }

    /// The language's English name.
    pub fn name(self) -> &'static str {
        language_name(self.0)
    }

    /// Every edition, by name.
    pub fn all() -> Vec<Self> {
        let mut all: Vec<Self> = EDITIONS.iter().map(|e| Edition(e)).collect();
        all.sort_by_key(|e| e.name());
        all
    }
}

/// A search result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    pub title: String,
    /// The redirect that matched, e.g. "USA" for "United States".
    pub redirect: Option<String>,
    /// A line of the article around the match.
    pub snippet: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    /// Level 2 is a section, 3 a subsection and so on.
    Heading {
        level: u8,
        text: String,
    },
    Paragraph(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Article {
    pub edition: Edition,
    pub title: String,
    pub redirected_from: Option<String>,
    /// The canonical address.
    pub url: String,
    /// A list of the title's meanings rather than an article.
    pub disambiguation: bool,
    pub blocks: Vec<Block>,
}

impl Article {
    /// The address with its letters readable: "Толстой" rather than
    /// "%D0%A2…". ASCII escapes stay, so it's still the same address.
    pub fn readable_url(&self) -> String {
        let bytes = self.url.as_bytes();
        let mut readable = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            let escaped = self.url.get(i..i + 3).and_then(|e| u8::from_str_radix(e.strip_prefix('%')?, 16).ok());
            match escaped.filter(|b| !b.is_ascii()) {
                Some(byte) => {
                    readable.push(byte);
                    i += 3;
                }
                None => {
                    readable.push(bytes[i]);
                    i += 1;
                }
            }
        }
        String::from_utf8(readable).unwrap_or_else(|_| self.url.clone())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Found {
    Article {
        article: Article,
        /// The other search hits.
        others: Vec<Hit>,
    },
    /// No clear best match; empty when nothing matched.
    Hits(Vec<Hit>),
}

/// One edition's API.
pub struct Wikipedia<'a> {
    pub client: &'a Client,
    pub url: &'a str,
    pub edition: Edition,
}

impl Wikipedia<'_> {
    /// Finds the article for `query`. Stops between requests once `current`
    /// is false.
    pub fn lookup(&self, query: &str, current: &dyn Fn() -> bool) -> Result<Found, Error> {
        if query.chars().count() > QUERY_LIMIT {
            return Err(Error::QueryTooLong);
        }
        let terms = search_terms(query);
        if terms.is_empty() {
            return Ok(Found::Hits(Vec::new()));
        }
        let (mut hits, rewritten) = self.search(&terms)?;
        if !clear_match(&terms, rewritten.as_deref(), &hits) {
            return Ok(Found::Hits(hits));
        }
        if !current() {
            return Err(Error::Obsolete);
        }
        let best = hits[0].clone();
        let mut article = match self.article(&best.title) {
            Ok(article) => article,
            // Deleted since it was indexed.
            Err(Error::NoArticle) => {
                hits.remove(0);
                return Ok(Found::Hits(hits));
            }
            Err(e) => return Err(e),
        };
        let others = hits.split_off(1);
        // A list of meanings is better than nothing.
        if article.blocks.is_empty() || article.disambiguation && !others.is_empty() {
            return Ok(Found::Hits(others));
        }
        // Searching follows redirects, so the article's reply doesn't say.
        if article.redirected_from.is_none() && words(&article.title) != words(&terms) {
            article.redirected_from = best.redirect.filter(|r| words(r) == words(&terms));
        }
        Ok(Found::Article { article, others })
    }

    /// The hits, and the query Wikipedia searched for instead if the given one
    /// found nothing.
    fn search(&self, terms: &str) -> Result<(Vec<Hit>, Option<String>), Error> {
        let reply = self.query(&[
            ("list", "search"),
            ("srsearch", terms),
            ("srlimit", HITS),
            ("srprop", "redirecttitle|snippet"),
            ("srinfo", "rewrittenquery"),
            ("srenablerewrites", "1"),
        ])?;
        let hits = reply.search.ok_or(Error::Unreadable)?;
        let hits = hits.into_iter().map(|h| Hit {
            title: h.title,
            redirect: h.redirecttitle,
            snippet: plain_text(&h.snippet),
        });
        Ok((hits.collect(), reply.searchinfo.and_then(|i| i.rewrittenquery)))
    }

    /// The article with this title, as plain text.
    pub fn article(&self, title: &str) -> Result<Article, Error> {
        let reply = self.query(&[
            ("titles", title),
            ("prop", "extracts|info|pageprops"),
            ("ppprop", "disambiguation"),
            ("inprop", "url"),
            ("explaintext", "1"),
            ("redirects", "1"),
        ])?;
        let page = reply.pages.into_iter().next().ok_or(Error::Unreadable)?;
        if page.missing || page.invalid {
            return Err(Error::NoArticle);
        }
        Ok(Article {
            edition: self.edition,
            title: page.title,
            redirected_from: reply.redirects.into_iter().next().map(|r| r.from),
            url: page.canonicalurl,
            disambiguation: page.pageprops.is_some_and(|p| p.disambiguation.is_some()),
            blocks: blocks(&page.extract),
        })
    }

    fn query(&self, params: &[(&str, &str)]) -> Result<QueryReply, Error> {
        let mut query = vec![("action", "query"), ("format", "json"), ("formatversion", "2")];
        query.extend_from_slice(params);
        let reply = self.client.get_query(self.url, &query, MAX_REPLY_BYTES)?;
        let reply: ApiReply = serde_json::from_slice(&reply).map_err(|_| Error::Unreadable)?;
        match reply.error {
            Some(e) => Err(Error::Refused(e.code)),
            // Left out when there is nothing to say.
            None => Ok(reply.query.unwrap_or_default()),
        }
    }
}

#[derive(Deserialize)]
struct ApiReply {
    error: Option<ApiError>,
    query: Option<QueryReply>,
}

#[derive(Deserialize)]
struct ApiError {
    code: String,
}

#[derive(Default, Deserialize)]
struct QueryReply {
    search: Option<Vec<SearchHit>>,
    searchinfo: Option<SearchInfo>,
    #[serde(default)]
    redirects: Vec<Redirect>,
    #[serde(default)]
    pages: Vec<Page>,
}

#[derive(Deserialize)]
struct SearchInfo {
    rewrittenquery: Option<String>,
}

#[derive(Deserialize)]
struct SearchHit {
    title: String,
    redirecttitle: Option<String>,
    #[serde(default)]
    snippet: String,
}

#[derive(Deserialize)]
struct Redirect {
    from: String,
}

#[derive(Deserialize)]
struct Page {
    title: String,
    #[serde(default)]
    missing: bool,
    #[serde(default)]
    invalid: bool,
    #[serde(default)]
    extract: String,
    #[serde(default)]
    canonicalurl: String,
    pageprops: Option<PageProps>,
}

#[derive(Deserialize)]
struct PageProps {
    /// Present (and empty) on disambiguation pages.
    disambiguation: Option<String>,
}

/// The query without what the search would take as operators: quotes,
/// wildcards, `intitle:`-like keywords, and `-`, `!` or `~` before a word.
fn search_terms(query: &str) -> String {
    let plain = query.replace(['"', '“', '”', '„', '«', '»', '*', '?', '\\', ':'], " ");
    let terms: Vec<&str> =
        plain.split_whitespace().map(|w| w.trim_start_matches(['-', '!', '~'])).filter(|w| !w.is_empty()).collect();
    terms.join(" ")
}

/// What titles are compared by: words without case, accents or punctuation.
fn words(text: &str) -> Vec<String> {
    fold(text).split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_owned).collect()
}

/// Whether the first hit is clearly meant: its title (or the redirect that
/// matched) is the query, or it alone has all the query's words in its title.
/// After a rewrite, only a title equal to the rewritten query counts: the
/// corrected spelling is a guess.
fn clear_match(terms: &str, rewritten: Option<&str>, hits: &[Hit]) -> bool {
    let Some((best, others)) = hits.split_first() else { return false };
    let wanted = words(rewritten.unwrap_or(terms));
    if wanted.is_empty() {
        return false;
    }
    let same = |title: &str| words(title) == wanted;
    if same(&best.title) || best.redirect.as_deref().is_some_and(same) {
        return true;
    }
    let has_all = |hit: &Hit| {
        let title = words(&hit.title);
        wanted.iter().all(|w| title.contains(w))
    };
    rewritten.is_none() && has_all(best) && !others.iter().any(has_all)
}

/// A snippet without its HTML: `<span>` around matches, and entities.
fn plain_text(html: &str) -> String {
    let mut text = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            c if !in_tag => text.push(c),
            _ => {}
        }
    }
    let text = match quick_xml::escape::unescape(&text) {
        Ok(decoded) => decoded.into_owned(),
        Err(_) => text,
    };
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// An extract's headings (`== Title ==`) and paragraphs. Sections without text
/// (lists of notes and references, which extracts leave out) are dropped.
fn blocks(extract: &str) -> Vec<Block> {
    let mut blocks: Vec<Block> = Vec::new();
    for line in extract.lines().map(str::trim).filter(|l| !l.is_empty()) {
        match heading(line) {
            Some((level, text)) => {
                drop_empty_sections(&mut blocks, level);
                blocks.push(Block::Heading { level, text: text.to_owned() });
            }
            None => blocks.push(Block::Paragraph(tidy_brackets(line))),
        }
    }
    drop_empty_sections(&mut blocks, 0);
    blocks
}

/// Extracts drop some templates (pronunciations) but keep the punctuation
/// around them: "Mount Olympus (, Greek: …". After an opening bracket, leading
/// commas, semicolons and spaces go, and a bracket left empty goes when it
/// stands alone, so "f()" stays.
fn tidy_brackets(text: &str) -> String {
    let mut tidy = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find('(') {
        tidy.push_str(&rest[..i]);
        let inside = rest[i + 1..].trim_start_matches([',', ';', ' ']);
        match inside.strip_prefix(')') {
            Some(after) if tidy.is_empty() || tidy.ends_with(' ') => {
                if after.is_empty() || after.starts_with([' ', ',', '.', ';', ':']) {
                    tidy.pop();
                }
                rest = after;
            }
            _ => {
                tidy.push('(');
                rest = inside;
            }
        }
    }
    tidy.push_str(rest);
    tidy
}

/// Removes headings at the end that a heading of `level` leaves without text.
fn drop_empty_sections(blocks: &mut Vec<Block>, level: u8) {
    while matches!(blocks.last(), Some(Block::Heading { level: last, .. }) if *last >= level) {
        blocks.pop();
    }
}

fn heading(line: &str) -> Option<(u8, &str)> {
    let level = line.bytes().take_while(|b| *b == b'=').count();
    if !(2..=6).contains(&level) {
        return None;
    }
    let text = line[level..].strip_suffix(&line[..level])?.trim();
    // An equals sign left over is a formula, not a heading.
    let is_title = !text.is_empty() && !text.starts_with('=') && !text.ends_with('=');
    is_title.then_some((level as u8, text))
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::dictionary::languages::all_languages;

    #[test]
    fn only_known_editions_are_reached() {
        let none: Vec<&str> =
            all_languages().map(|(code, _)| code).filter(|c| Edition::for_language(c).is_none()).collect();
        assert_eq!(none, ["ak"], "every other language of Verso's has a Wikipedia");
        for (tag, edition) in
            [("nb", "no"), ("nb-NO", "no"), ("no", "no"), ("nn", "nn"), ("eng", "en"), ("pt-BR", "pt"), ("iw", "he")]
        {
            assert_eq!(Edition::for_language(tag).map(Edition::code), Some(edition), "{tag}");
        }
        for tag in ["", "und", "x-klingon", "mul", "zxx", "ak", "en.example.com/", "../en", "en/", "en@host", "e"] {
            assert_eq!(Edition::for_language(tag), None, "{tag:?}");
        }
    }

    #[test]
    fn addresses_show_letters_but_keep_ascii_escapes() {
        for (url, readable) in [
            ("https://en.wikipedia.org/wiki/C%2B%2B", "https://en.wikipedia.org/wiki/C%2B%2B"),
            ("https://ru.wikipedia.org/wiki/%D0%9C%D0%B8%D1%80%3F", "https://ru.wikipedia.org/wiki/Мир%3F"),
            ("https://x/100%", "https://x/100%"),
            // Not UTF-8.
            ("https://x/%D0", "https://x/%D0"),
        ] {
            let article = Article {
                edition: Edition::ENGLISH,
                title: String::new(),
                redirected_from: None,
                url: url.into(),
                disambiguation: false,
                blocks: vec![],
            };
            assert_eq!(article.readable_url(), readable);
        }
    }

    #[test]
    fn queries_lose_search_operators() {
        for (query, terms) in [
            ("  World   War\nII ", "World War II"),
            ("“Hello”", "Hello"),
            ("intitle:foo -bar !baz ~qux", "intitle foo bar baz qux"),
            ("Who's Afraid of Virginia Woolf?", "Who's Afraid of Virginia Woolf"),
            ("Jean-Paul Sartre", "Jean-Paul Sartre"),
            ("!!! ?", ""),
        ] {
            assert_eq!(search_terms(query), terms, "{query:?}");
        }
    }

    pub(crate) fn hit(title: &str, redirect: Option<&str>) -> Hit {
        Hit { title: title.into(), redirect: redirect.map(Into::into), snippet: String::new() }
    }

    #[test]
    fn only_clear_matches_open_directly() {
        let hits = |titles: &[&str]| titles.iter().map(|t| hit(t, None)).collect::<Vec<_>>();
        // The title, whatever its case, accents and punctuation.
        assert!(clear_match("world war ii", None, &hits(&["World War II", "Allies of World War II"])));
        assert!(clear_match("emile zola", None, &hits(&["Émile Zola"])));
        assert!(clear_match("mercury planet", None, &hits(&["Mercury (planet)", "Mercury"])));
        // The redirect that matched.
        assert!(clear_match("usa", None, &[hit("United States", Some("USA")), hit("USA Today", None)]));
        assert!(!clear_match("usa", None, &[hit("United States", Some("US of A")), hit("USA Today", None)]));
        // All the words, in this title only.
        assert!(clear_match("raskolnikov", None, &hits(&["Rodion Raskolnikov", "Crime and Punishment"])));
        assert!(!clear_match("karenina", None, &hits(&["Anna Karenina", "Anna Karenina (2012 film)"])));
        // Found in the text only.
        assert!(!clear_match("theory", None, &hits(&["Big Bang"])));
        // A corrected spelling is a guess.
        assert!(clear_match("wrold war", Some("world war"), &hits(&["World war"])));
        assert!(!clear_match("battel of watreloo", Some("battle of watrelois"), &hits(&["Battle of Wattrelos"])));
        assert!(!clear_match("karenin", Some("karenina"), &hits(&["Anna Karenina"])));
        assert!(!clear_match("anything", None, &[]));
    }

    #[test]
    fn extracts_become_headings_and_paragraphs() {
        let extract = "Intro one.\nIntro two.\n\n\n== History ==\n\n\n=== Early ===\nText with a == b.\n\n\n\
                       === Late ===\n\n\n== Notes ==\n\n\n=== Citations ===\n\n\n== Links ==\n== E = mc2 ==\nx\n====\n\
                       == Unfinished\n== References ==\n";
        let heading = |level, text: &str| Block::Heading { level, text: text.into() };
        let text = |text: &str| Block::Paragraph(text.into());
        assert_eq!(
            blocks(extract),
            [
                text("Intro one."),
                text("Intro two."),
                heading(2, "History"),
                heading(3, "Early"),
                text("Text with a == b."),
                heading(2, "E = mc2"),
                text("x"),
                text("===="),
                text("== Unfinished"),
            ]
        );
        assert_eq!(blocks(""), []);
    }

    #[test]
    fn brackets_lose_what_dropped_templates_leave() {
        for (text, tidy) in [
            ("Mount Olympus (, Greek: Όλυμπος) is", "Mount Olympus (Greek: Όλυμπος) is"),
            ("Paris (; French: [paʁi]) is", "Paris (French: [paʁi]) is"),
            ("Thoreau ( ) was", "Thoreau was"),
            ("Thoreau (, ; ).", "Thoreau."),
            ("call f() or (a, b)", "call f() or (a, b)"),
            ("(1817–1862)", "(1817–1862)"),
        ] {
            assert_eq!(tidy_brackets(text), tidy, "{text:?}");
        }
    }

    #[test]
    fn snippets_lose_their_html() {
        let html = "<span class=\"searchmatch\">War</span> &amp; &quot;Peace&quot;&nbsp;(1867) by\n Tolstoy&#039;s";
        assert_eq!(plain_text(html), "War & \"Peace\" (1867) by Tolstoy's");
        assert_eq!(plain_text("AT&T &unknown; x"), "AT&T &unknown; x");
    }
}
