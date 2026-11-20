//! Offline StarDict. Source files are read only; rebuildable search data lives
//! beside app state. Only the worker opens dictionaries.
mod dictzip;
mod discovery;
mod entities;
mod forms;
mod index;
mod markup;
mod model;
pub mod sets;
mod text;
pub mod worker;

use anyhow::{Context, Result, ensure};
use dictzip::Definitions;
pub use discovery::{Catalog, Source, SourceStatus, discover_catalog};
#[cfg(test)]
pub(crate) use forms::write_pack;
pub use forms::{WordFormsSource, WordFormsStatus};
use index::Index;
pub(crate) use index::remove_cache;
pub use model::{Block, BlockKind, Entry, Span, Style};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preference {
    pub id: String,
    pub disabled: bool,
}
#[derive(Clone, Debug)]
pub struct Dictionary {
    pub id: String,
    pub name: String,
    pub path: PathBuf,
    /// The `.ifo` path below its source root, for telling copies apart.
    pub relative: PathBuf,
    /// Index into `Catalog::sources` of the source that listed it.
    pub source: usize,
    /// Headword count from `.ifo` (`wordcount`), when readable.
    pub word_count: Option<u32>,
    pub error: Option<String>,
    pub disabled: bool,
}
#[derive(Debug)]
pub(crate) struct Info {
    pub(crate) name: String,
    pub(crate) count: u32,
    syn_count: u32,
    index_size: u64,
    wide: bool,
    types: String,
}
fn bounded_read(path: &Path, limit: usize) -> Result<String> {
    bounded_text(fs::File::open(path)?, limit)
}
fn bounded_text(source: impl std::io::Read, limit: usize) -> Result<String> {
    use std::io::Read;
    let mut data = Vec::new();
    source.take(limit as u64 + 1).read_to_end(&mut data)?;
    ensure!(data.len() <= limit, "File is too large");
    Ok(String::from_utf8(data)?)
}
/// The basename shared by a dictionary's files: its `.ifo` or `.ifo.gz`
/// metadata path without those suffixes. `None` for any other file.
pub(crate) fn basename(path: &Path) -> Option<PathBuf> {
    let plain = path.with_extension("");
    match path.extension()?.to_str()? {
        "ifo" => Some(plain),
        "gz" if plain.extension().is_some_and(|e| e == "ifo") => Some(plain.with_extension("")),
        _ => None,
    }
}
/// A companion file: the basename plus `.{ext}`. Unlike `with_extension`, this
/// keeps dots inside the basename and works for two-part suffixes.
pub(crate) fn sibling(base: &Path, ext: &str) -> PathBuf {
    let mut path = base.as_os_str().to_owned();
    path.push(".");
    path.push(ext);
    path.into()
}
pub(crate) fn is_gzip(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "gz")
}
impl Info {
    /// Reads `.ifo` or `.ifo.gz`; the decompressed text is bounded to 64 KiB.
    pub(crate) fn read(path: &Path) -> Result<Self> {
        let base = basename(path).context("Not a StarDict .ifo or .ifo.gz file")?;
        let text = if is_gzip(path) {
            bounded_text(flate2::read::GzDecoder::new(fs::File::open(path)?), 65536)
                .context("Corrupt, truncated or oversized .ifo.gz")?
        } else {
            bounded_read(path, 65536)?
        };
        let mut lines = text.lines();
        ensure!(lines.next() == Some("StarDict's dict ifo file"), "Invalid StarDict header");
        let fields: BTreeMap<_, _> = lines.filter_map(|l| l.split_once('=')).collect();
        let get = |key| fields.get(key).copied().with_context(|| format!("Missing {key}"));
        let version = get("version")?;
        ensure!(matches!(version, "2.4.2" | "3.0.0"), "Unsupported StarDict version");
        let bits = fields.get("idxoffsetbits").copied().unwrap_or("32");
        ensure!(bits == "32" || bits == "64" && version == "3.0.0", "Invalid index offset width");
        let name = get("bookname")?.to_owned();
        ensure!(!name.is_empty(), "Missing dictionary name");
        let types = fields.get("sametypesequence").copied().unwrap_or("").to_owned();
        ensure!(types.bytes().all(|b| b.is_ascii_alphabetic()), "Invalid field types");
        let syn_count = if sibling(&base, "syn").exists() {
            get("synwordcount")?.parse()?
        } else {
            let count = fields.get("synwordcount").copied().unwrap_or("0").parse()?;
            ensure!(count == 0, "Missing synonym file");
            count
        };
        Ok(Self {
            name,
            count: get("wordcount")?.parse()?,
            syn_count,
            index_size: get("idxfilesize")?.parse()?,
            wide: bits == "64",
            types,
        })
    }
}
pub(crate) fn companion(base: &Path, plain: &str, compressed: &str) -> Result<(PathBuf, bool)> {
    let file = sibling(base, plain);
    if file.is_file() {
        return Ok((file, false));
    }
    let file = sibling(base, compressed);
    ensure!(file.is_file(), "Missing .{plain} or .{compressed} file");
    Ok((file, true))
}

#[cfg(test)]
fn discover(root: &Path, preferences: &[Preference]) -> Result<Vec<Dictionary>> {
    Ok(discover_catalog(root, &[], preferences, &|| true)?.dictionaries)
}
/// The preferences after a change: listed dictionaries in their current order,
/// then preferences of dictionaries that are not listed now (a missing or
/// removed folder), so re-adding the location restores them.
#[cfg(test)]
pub fn merged_preferences(dictionaries: &[Dictionary], previous: Vec<Preference>) -> Vec<Preference> {
    let mut merged: Vec<_> =
        dictionaries.iter().map(|d| Preference { id: d.id.clone(), disabled: d.disabled }).collect();
    merged.extend(previous.into_iter().filter(|p| !dictionaries.iter().any(|d| d.id == p.id)));
    merged
}

/// Entries collected by a base-form lookup and the base forms that added any.
#[derive(Default)]
struct FormHits {
    entries: Vec<(String, u64, u32)>,
    forms: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hits {
    pub records: Vec<(String, u64, u32)>,
    pub forms: Option<usize>,
}

/// The definitions file of the dictionary whose `.ifo` or `.ifo.gz` is `path`.
fn open_definitions(path: &Path) -> Result<Definitions> {
    let base = basename(path).context("Not a StarDict .ifo or .ifo.gz file")?;
    let (data_path, compressed) = companion(&base, "dict", "dict.dz")?;
    Definitions::open(&data_path, compressed)
}

/// What a search needs from one dictionary: its validated index cache and
/// field types. It holds one file handle and no word list, and never opens
/// the definitions file except to build a missing index.
pub struct Probe {
    index: Index,
    types: String,
}
impl Probe {
    fn open_progress(path: &Path, cache: &Path, current: &impl Fn() -> bool, preparing: &impl Fn()) -> Result<Self> {
        let info = Info::read(path)?;
        let index = Index::open(path, cache, &info, || Ok(open_definitions(path)?.len), current, preparing)?;
        Ok(Self { index, types: info.types })
    }
    fn find(&mut self, query: &str) -> Result<Hits> {
        let mut records = self.index.find(query)?;
        let mut seen = std::collections::HashSet::new();
        records.retain(|(_, offset, size)| seen.insert((*offset, *size)));
        Ok(Hits { records, forms: None })
    }
    fn displayable(&self) -> bool {
        self.types.is_empty() || self.types.bytes().any(|k| matches!(k, b'm' | b't' | b'y' | b'h' | b'x' | b'g'))
    }
    fn find_forms(&mut self, forms: &forms::Candidates, current: &impl Fn() -> bool) -> Result<Hits> {
        const MAX_FORMS: usize = 3;
        let mut hits = FormHits::default();
        'search: {
            // An intact possessive is sufficient.
            if let Some(word) = &forms.possessive
                && self.add_form(&mut hits, word, current)?
            {
                break 'search;
            }
            for word in &forms.exceptions {
                self.add_form(&mut hits, word, current)?;
                if hits.forms == MAX_FORMS {
                    break 'search;
                }
            }
            // Only the first rule with an entry counts.
            for word in &forms.rules {
                if self.add_form(&mut hits, word, current)? {
                    break;
                }
            }
        }
        ensure!(current(), "Lookup cancelled");
        Ok(Hits { records: hits.entries, forms: Some(hits.forms) })
    }
    /// Adds the entries for `word` that aren't shown yet; `true` if the
    /// dictionary has the word, even when every entry was already shown.
    fn add_form(&mut self, hits: &mut FormHits, word: &str, current: &impl Fn() -> bool) -> Result<bool> {
        ensure!(current(), "Lookup cancelled");
        let found = self.index.find(word)?;
        let hit = !found.is_empty();
        let shown = hits.entries.len();
        for entry in found {
            // A repeated lemma or a synonym sharing a definition span adds nothing.
            if !hits.entries.iter().any(|(_, offset, size)| *offset == entry.1 && *size == entry.2) {
                hits.entries.push(entry);
            }
        }
        hits.forms += usize::from(hits.entries.len() > shown);
        Ok(hit)
    }
    /// Reads and formats `entries` from this dictionary's `definitions`.
    fn render(&self, definitions: &mut Definitions, entries: Vec<(String, u64, u32)>) -> Result<Vec<Entry>> {
        const LIMIT: usize = 1024 * 1024;
        const NOTICE: &str = "[Further entries omitted]";
        let mut output = Vec::new();
        let mut bytes = 0;
        let mut omitted = false;
        for (word, offset, size) in entries {
            let data = definitions.read(offset, size)?;
            let mut blocks = text::definition(&data, &self.types)?;
            // Reserve room for a notice so the combined block text, including
            // that notice, never exceeds the cap. Clip on a character boundary.
            let mut keep = blocks.len();
            for (i, block) in blocks.iter_mut().enumerate() {
                let available = (LIMIT - NOTICE.len()).saturating_sub(bytes);
                if block.text.len() > available {
                    block.truncate(available);
                    omitted = true;
                    keep = i + 1;
                }
                bytes += block.text.len();
                if omitted {
                    break;
                }
            }
            blocks.truncate(keep);
            blocks.retain(|b| !matches!(b.kind, BlockKind::Text | BlockKind::Heading) || !b.text.is_empty());
            while omitted && blocks.last().is_some_and(|b| b.kind == BlockKind::Gap) {
                blocks.pop();
            }
            output.push(Entry { headword: word, blocks });
            if omitted {
                break;
            }
        }
        if omitted {
            output.last_mut().unwrap().blocks.push(Block::plain(NOTICE));
        }
        Ok(output)
    }
}

/// A dictionary with both files open, for checks and tests that read
/// definitions directly. The worker keeps `Probe`s and opens definitions
/// only to show them.
pub struct Backend {
    probe: Probe,
    definitions: Definitions,
}
impl Backend {
    pub fn open(path: &Path, cache: &Path, current: &impl Fn() -> bool) -> Result<Self> {
        let info = Info::read(path)?;
        let definitions = open_definitions(path)?;
        let len = definitions.len;
        let index = Index::open(path, cache, &info, || Ok(len), current, &|| {})?;
        Ok(Self { probe: Probe { index, types: info.types }, definitions })
    }
    pub fn lookup(&mut self, query: &str) -> Result<Vec<Entry>> {
        let entries = self.probe.index.find(query)?;
        self.probe.render(&mut self.definitions, entries)
    }
    /// Returns the rendered entries and how many base forms they cover, at
    /// most three. A base form is a candidate word, so case variants of one
    /// headword (`hope`, `Hope`) count once.
    fn lookup_forms(&mut self, forms: &forms::Candidates, current: &impl Fn() -> bool) -> Result<(Vec<Entry>, usize)> {
        let hits = self.probe.find_forms(forms, current)?;
        Ok((self.probe.render(&mut self.definitions, hits.records)?, hits.forms.unwrap_or(0)))
    }
}

#[cfg(test)]
mod tests;
