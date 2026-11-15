//! Offline StarDict. Source files are read only; rebuildable search data lives
//! beside app state. Only the worker opens dictionaries.
mod dictzip;
mod discovery;
mod forms;
mod index;
mod text;
pub mod worker;

use anyhow::{Context, Result, ensure};
use dictzip::Definitions;
pub use discovery::{Catalog, Source, SourceStatus, discover_catalog};
use index::Index;
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
    pub error: Option<String>,
    pub disabled: bool,
}
#[derive(Debug)]
struct Info {
    name: String,
    count: u32,
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
fn basename(path: &Path) -> Option<PathBuf> {
    let plain = path.with_extension("");
    match path.extension()?.to_str()? {
        "ifo" => Some(plain),
        "gz" if plain.extension().is_some_and(|e| e == "ifo") => Some(plain.with_extension("")),
        _ => None,
    }
}
/// A companion file: the basename plus `.{ext}`. Unlike `with_extension`, this
/// keeps dots inside the basename and works for two-part suffixes.
fn sibling(base: &Path, ext: &str) -> PathBuf {
    let mut path = base.as_os_str().to_owned();
    path.push(".");
    path.push(ext);
    path.into()
}
fn is_gzip(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "gz")
}
impl Info {
    /// Reads `.ifo` or `.ifo.gz`; the decompressed text is bounded to 64 KiB.
    fn read(path: &Path) -> Result<Self> {
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
fn companion(base: &Path, plain: &str, compressed: &str) -> Result<(PathBuf, bool)> {
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
pub fn merged_preferences(dictionaries: &[Dictionary], previous: Vec<Preference>) -> Vec<Preference> {
    let mut merged: Vec<_> =
        dictionaries.iter().map(|d| Preference { id: d.id.clone(), disabled: d.disabled }).collect();
    merged.extend(previous.into_iter().filter(|p| !dictionaries.iter().any(|d| d.id == p.id)));
    merged
}

pub struct Backend {
    index: Index,
    definitions: Definitions,
    types: String,
}
impl Backend {
    pub fn open(path: &Path, cache: &Path, current: &impl Fn() -> bool) -> Result<Self> {
        let info = Info::read(path)?;
        let base = basename(path).context("Not a StarDict .ifo or .ifo.gz file")?;
        let (data_path, compressed) = companion(&base, "dict", "dict.dz")?;
        let definitions = Definitions::open(&data_path, compressed)?;
        let index = Index::open(path, cache, &info, definitions.len, current)?;
        Ok(Self { index, definitions, types: info.types })
    }
    pub fn lookup(&mut self, query: &str) -> Result<String> {
        let entries = self.index.find(query)?;
        self.render(entries)
    }
    /// Returns the rendered entries and their distinct headword count.
    fn lookup_forms(&mut self, forms: &forms::Candidates, current: &impl Fn() -> bool) -> Result<(String, usize)> {
        let mut entries = Vec::new();
        let mut headwords = Vec::new();
        let candidates = forms
            .possessive
            .iter()
            .map(String::as_str)
            .chain(forms.exceptions.iter().copied())
            .chain(forms.rules.iter().map(String::as_str));
        let possessive_count = usize::from(forms.possessive.is_some());
        let rules_start = possessive_count + forms.exceptions.len();
        for (i, candidate) in candidates.enumerate() {
            ensure!(current(), "Lookup cancelled");
            let found = self.index.find(candidate)?;
            let hit = !found.is_empty();
            for entry in found {
                if entries.iter().any(|(_, offset, size)| *offset == entry.1 && *size == entry.2) {
                    continue;
                }
                if !headwords.contains(&entry.0) {
                    if headwords.len() == 3 {
                        continue;
                    }
                    headwords.push(entry.0.clone());
                }
                entries.push(entry);
            }
            // A possessive hit is sufficient; rules contribute only their first hit.
            if hit && (i < possessive_count || i >= rules_start) || headwords.len() == 3 {
                break;
            }
        }
        ensure!(current(), "Lookup cancelled");
        let count = headwords.len();
        Ok((self.render(entries)?, count))
    }
    fn render(&mut self, entries: Vec<(String, u64, u32)>) -> Result<String> {
        let mut output = String::new();
        for (word, offset, size) in entries {
            let data = self.definitions.read(offset, size)?;
            let definition = text::definition(&data, &self.types)?;
            if !output.is_empty() {
                output.push_str("\n\n");
            }
            output.push_str(&word);
            output.push_str("\n\n");
            output.push_str(&definition);
            if output.len() > 1024 * 1024 {
                output.push_str("\n\n[Further entries omitted]");
                break;
            }
        }
        Ok(output)
    }
}

#[cfg(test)]
mod tests;
