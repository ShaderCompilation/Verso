//! Reads the OpenCorpora dictionary export (`dict.opcorpora.xml`) and turns it
//! into pack entries: each folded form with its lemmas, linked lemmas
//! resolved.
//!
//! The XML is streamed: only the current element is buffered, and each
//! lexeme keeps its lemma, a part-of-speech flag and its folded forms in one
//! shared arena.
use super::{MAX_LEMMAS, fold, is_word, write::Writer};
use anyhow::{Context, Result, bail};
use quick_xml::{
    Reader, XmlVersion,
    events::{BytesStart, Event},
};
use std::{
    collections::{BTreeMap, HashMap},
    io::BufRead,
};

/// Link types whose `to` lexeme also gets the `from` lexeme's lemma: verb
/// forms, participles and gerunds lead to the infinitive, short forms,
/// comparatives and superlatives to the full adjective (or adverb),
/// spelling variants to the normal spelling, and substantivized feminine or
/// plural forms to the masculine. pymorphy2 merges these too.
///
/// Not followed, unlike pymorphy2: aspect pairs (`PERF-IMPF`), patronymics
/// and surnames, since each side is its own dictionary entry. pymorphy2
/// also skips `NAME-PATR`, `FULL-CONTRACTED`, `CARDINAL-ORDINAL` and
/// `ADJF_TEXT-ADJF_NUMBER`.
pub const FOLLOWED_LINKS: &[&str] = &[
    "ADJF-ADJS",
    "ADJF-COMP",
    "INFN-VERB",
    "INFN-PRTF",
    "INFN-GRND",
    "PRTF-PRTS",
    "ADJF-SUPR_ejsh",
    "ADJF_eish-SUPR_nai_eish",
    "ADJF-SUPR_ajsh",
    "ADJF_aish-SUPR_nai_aish",
    "ADJF-SUPR_suppl",
    "ADJF-SUPR_nai",
    "ADJF-SUPR_slng",
    "NORM-ORPHOVAR",
    "SBST_MASC-SBST_FEMN",
    "SBST_MASC-SBST_PLUR",
    "ADVB-COMP",
];

/// How far a chain of links is followed (`прочитан` → `прочитанный` → `прочитать` is 2).
const MAX_LINK_DEPTH: usize = 4;

/// Where a lexeme's own lemma goes among a form's lemmas.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Rank {
    /// Before linked lemmas.
    Own = 0,
    /// Personal verb forms, short participles and short adjectives are named
    /// by a form dictionaries rarely list (`иду`, `прочитан`, `красив`), so
    /// their linked lemma comes first.
    Short = 2,
    /// Proper names (first names, surnames, patronymics, places,
    /// organizations, trademarks) go last: lowercase folding lets them
    /// collide with common words (`нее` is also the river Нея).
    Name = 3,
}

/// Lexeme grammemes that mark a proper name.
const NAME_GRAMMEMES: &[&str] = &["Name", "Surn", "Patr", "Geox", "Orgn", "Trad"];

struct Lexeme {
    id: u32,
    lemma: Box<str>,
    rank: Rank,
    /// An active participle (`actv`). The export links short passive
    /// participles to the active one as well as the passive one
    /// (`написавший` and `написанный` → `написан`); only the passive link is
    /// right, so `PRTF-PRTS` links from active participles are skipped.
    active: bool,
}

/// A form of a lexeme: its folded key in the arena.
#[derive(Clone, Copy)]
struct Form {
    start: u32,
    len: u32,
    lexeme: u32,
}

/// Counts reported by the builder.
#[derive(Debug, Default)]
pub struct Summary {
    pub version: String,
    pub revision: String,
    pub lexemes: usize,
    /// Forms read, before folding and merging.
    pub forms: usize,
    /// Forms that are not a single word ([`is_word`]) and are left out.
    pub skipped_forms: usize,
    /// Distinct folded forms written.
    pub keys: usize,
    /// Keys with more than one lemma.
    pub ambiguous_keys: usize,
    /// Keys whose lemma list was cut to [`MAX_LEMMAS`].
    pub truncated_keys: usize,
    /// Links by type name, followed or not.
    pub followed_links: BTreeMap<String, usize>,
    pub ignored_links: BTreeMap<String, usize>,
    /// Links to a lexeme the export doesn't have.
    pub dangling_links: usize,
}

/// The parsed export.
pub struct Dictionary {
    pub version: String,
    pub revision: String,
    lexemes: Vec<Lexeme>,
    keys: String,
    forms: Vec<Form>,
    forms_read: usize,
    skipped_forms: usize,
    link_types: HashMap<u32, String>,
    /// `(from, to, type)` lexeme IDs as in the XML.
    links: Vec<(u32, u32, u32)>,
}

impl Dictionary {
    /// Streams the export from `input`.
    pub fn read(input: impl BufRead) -> Result<Self> {
        let mut reader = Reader::from_reader(input);
        let mut dict = Self {
            version: String::new(),
            revision: String::new(),
            lexemes: Vec::new(),
            keys: String::new(),
            forms: Vec::new(),
            forms_read: 0,
            skipped_forms: 0,
            link_types: HashMap::new(),
            links: Vec::new(),
        };
        let mut buf = Vec::new();
        // The lexeme being read: its ID and where its forms start.
        let mut lexeme: Option<(u32, usize)> = None;
        let mut lemma: Option<(Box<str>, Rank, bool)> = None;
        // Inside `<l>`: the lexeme's grammemes, part of speech first.
        let mut in_lemma_tag = false;
        let mut first_grammeme = false;
        let mut link_type: Option<(u32, String)> = None;
        loop {
            let position = reader.buffer_position();
            let event =
                reader.read_event_into(&mut buf).with_context(|| format!("Invalid XML near byte {position}"))?;
            match event {
                Event::Start(ref e) | Event::Empty(ref e) => match e.name().as_ref() {
                    b"dictionary" => {
                        dict.version = attr(e, b"version")?.unwrap_or_default();
                        dict.revision = attr(e, b"revision")?.unwrap_or_default();
                    }
                    b"lemma" => {
                        lexeme = Some((number(e, b"id")?, dict.forms.len()));
                        lemma = None;
                    }
                    b"l" if lexeme.is_some() => {
                        let text = attr(e, b"t")?.context("Lemma without text")?;
                        lemma = Some((text.into(), Rank::Own, false));
                        in_lemma_tag = true;
                        first_grammeme = true;
                    }
                    b"g" if in_lemma_tag => {
                        let grammeme = attr(e, b"v")?.unwrap_or_default();
                        if let Some((_, rank, active)) = &mut lemma {
                            if grammeme == "actv" {
                                *active = true;
                            } else if NAME_GRAMMEMES.contains(&grammeme.as_str()) {
                                *rank = Rank::Name;
                            } else if first_grammeme && matches!(grammeme.as_str(), "VERB" | "PRTS" | "ADJS") {
                                *rank = Rank::Short;
                            }
                        }
                        first_grammeme = false;
                    }
                    b"f" => {
                        in_lemma_tag = false;
                        let Some((_, first)) = lexeme else { bail!("Form outside a lemma near byte {position}") };
                        let form = attr(e, b"t")?.context("Form without text")?;
                        dict.add_form(&form, first)?;
                    }
                    b"type" => link_type = Some((number(e, b"id")?, String::new())),
                    b"link" => dict.links.push((number(e, b"from")?, number(e, b"to")?, number(e, b"type")?)),
                    _ => {}
                },
                Event::Text(ref t) => {
                    if let Some((_, name)) = &mut link_type {
                        name.push_str(&t.decode()?);
                    }
                }
                Event::End(ref e) => match e.name().as_ref() {
                    b"lemma" => {
                        let (id, _) = lexeme.take().context("Unbalanced lemma")?;
                        let (lemma, rank, active) = lemma.take().with_context(|| format!("Lemma {id} has no <l>"))?;
                        dict.lexemes.push(Lexeme { id, lemma, rank, active });
                    }
                    b"l" => in_lemma_tag = false,
                    b"type" => {
                        if let Some((id, name)) = link_type.take() {
                            dict.link_types.insert(id, name.trim().to_owned());
                        }
                    }
                    _ => {}
                },
                Event::Eof => break,
                _ => {}
            }
            buf.clear();
        }
        if dict.lexemes.is_empty() {
            bail!("No lemmas found: is this an OpenCorpora dictionary export?");
        }
        Ok(dict)
    }

    /// Adds a form of the lexeme being read, whose forms start at `first`.
    fn add_form(&mut self, form: &str, first: usize) -> Result<()> {
        self.forms_read += 1;
        if !is_word(form) {
            self.skipped_forms += 1;
            return Ok(());
        }
        let key = fold(form);
        // Lexemes repeat forms (accusative = genitive); keep one.
        if self.forms[first..].iter().any(|f| self.keys[f.start as usize..(f.start + f.len) as usize] == key) {
            return Ok(());
        }
        let start = u32::try_from(self.keys.len()).context("Too many forms")?;
        self.keys.push_str(&key);
        let lexeme = self.lexemes.len() as u32;
        self.forms.push(Form { start, len: key.len() as u32, lexeme });
        Ok(())
    }

    /// Writes every form with its lemmas to `writer`. A form's lemmas, for
    /// all lexemes that have it, in source order within each group:
    /// 1. the lexeme's own lemma (`читающего` → `читающий`);
    /// 2. lemmas reached through [`FOLLOWED_LINKS`] (→ `читать`);
    /// 3. the own lemma of short lexemes (`шёл` → `идти`, then `иду`);
    /// 4. the own lemma of proper names (`нее` → `она`, then `нея`).
    pub fn write(mut self, writer: &mut Writer) -> Result<Summary> {
        let mut summary = Summary {
            version: self.version.clone(),
            revision: self.revision.clone(),
            lexemes: self.lexemes.len(),
            forms: self.forms_read,
            skipped_forms: self.skipped_forms,
            ..Summary::default()
        };
        let parents = self.parents(&mut summary);
        let keys = std::mem::take(&mut self.keys);
        let key = |f: &Form| &keys[f.start as usize..(f.start + f.len) as usize];
        // Stable, so equal keys keep lexemes in source order.
        self.forms.sort_by(|a, b| key(a).as_bytes().cmp(key(b).as_bytes()));
        let mut tiers: [Vec<&str>; 4] = Default::default();
        let mut lemmas: Vec<&str> = Vec::new();
        for group in self.forms.chunk_by(|a, b| key(a) == key(b)) {
            tiers.iter_mut().for_each(Vec::clear);
            for form in group {
                let lexeme = &self.lexemes[form.lexeme as usize];
                tiers[lexeme.rank as usize].push(&lexeme.lemma);
                self.linked(form.lexeme, &parents, &mut tiers[1]);
            }
            lemmas.clear();
            for lemma in tiers.iter().flatten() {
                if !lemmas.contains(lemma) {
                    lemmas.push(lemma);
                }
            }
            if lemmas.len() > MAX_LEMMAS {
                summary.truncated_keys += 1;
                lemmas.truncate(MAX_LEMMAS);
            }
            summary.keys += 1;
            summary.ambiguous_keys += usize::from(lemmas.len() > 1);
            writer.insert(key(&group[0]), &lemmas)?;
        }
        Ok(summary)
    }

    /// Parent lexemes of each lexeme (by index) through followed links.
    fn parents(&self, summary: &mut Summary) -> HashMap<u32, Vec<u32>> {
        let index: HashMap<u32, u32> = self.lexemes.iter().enumerate().map(|(i, l)| (l.id, i as u32)).collect();
        let mut parents: HashMap<u32, Vec<u32>> = HashMap::new();
        for &(from, to, kind) in &self.links {
            let name = self.link_types.get(&kind).cloned().unwrap_or_else(|| format!("type {kind}"));
            if !FOLLOWED_LINKS.contains(&name.as_str()) {
                *summary.ignored_links.entry(name).or_default() += 1;
                continue;
            }
            let (Some(&from), Some(&to)) = (index.get(&from), index.get(&to)) else {
                summary.dangling_links += 1;
                continue;
            };
            if name == "PRTF-PRTS" && self.lexemes[from as usize].active {
                *summary.ignored_links.entry(format!("{name} from an active participle")).or_default() += 1;
                continue;
            }
            *summary.followed_links.entry(name).or_default() += 1;
            parents.entry(to).or_default().push(from);
        }
        parents
    }

    /// Appends the lemmas linked to `lexeme`, nearest first.
    fn linked<'a>(&'a self, lexeme: u32, parents: &HashMap<u32, Vec<u32>>, out: &mut Vec<&'a str>) {
        let mut seen = vec![lexeme];
        let mut level = vec![lexeme];
        for _ in 0..MAX_LINK_DEPTH {
            let mut next = Vec::new();
            for parent in level.iter().filter_map(|l| parents.get(l)).flatten() {
                if !seen.contains(parent) {
                    seen.push(*parent);
                    next.push(*parent);
                    out.push(&self.lexemes[*parent as usize].lemma);
                }
            }
            if next.is_empty() {
                break;
            }
            level = next;
        }
    }
}

fn attr(e: &BytesStart, name: &[u8]) -> Result<Option<String>> {
    match e.try_get_attribute(name)? {
        Some(a) => Ok(Some(a.normalized_value(XmlVersion::Explicit1_0)?.into_owned())),
        None => Ok(None),
    }
}

fn number(e: &BytesStart, name: &[u8]) -> Result<u32> {
    let text = attr(e, name)?.with_context(|| format!("Missing {}", String::from_utf8_lossy(name)))?;
    text.trim().parse().with_context(|| format!("Invalid {} {text:?}", String::from_utf8_lossy(name)))
}
