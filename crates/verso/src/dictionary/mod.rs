//! Offline StarDict. Source files are read only; rebuildable search data lives
//! beside app state. Only the worker opens dictionaries.
mod dictzip;
mod index;
mod text;
pub mod worker;

use anyhow::{Context, Result, ensure};
use dictzip::Definitions;
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
    use std::io::Read;
    let mut data = Vec::new();
    fs::File::open(path)?.take(limit as u64 + 1).read_to_end(&mut data)?;
    ensure!(data.len() <= limit, "File is too large");
    Ok(String::from_utf8(data)?)
}
impl Info {
    fn read(path: &Path) -> Result<Self> {
        let text = bounded_read(path, 65536)?;
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
        let syn_count = if path.with_extension("syn").exists() {
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
fn companion(path: &Path, plain: &str, compressed: &str) -> Result<(PathBuf, bool)> {
    let file = path.with_extension(plain);
    if file.is_file() {
        return Ok((file, false));
    }
    let file = path.with_extension(compressed);
    ensure!(file.is_file(), "Missing .{plain} or .{compressed} file");
    Ok((file, true))
}

#[cfg(test)]
fn discover(root: &Path, preferences: &[Preference]) -> Result<Vec<Dictionary>> {
    discover_current(root, preferences, &|| true)
}
fn discover_current(root: &Path, preferences: &[Preference], current: &impl Fn() -> bool) -> Result<Vec<Dictionary>> {
    if !root.try_exists()? {
        return Ok(Vec::new());
    }
    let mut folders = vec![(root.to_owned(), 0)];
    let mut found = Vec::new();
    let mut visited = 0usize;
    while let Some((folder, depth)) = folders.pop() {
        ensure!(depth <= 32 && found.len() < 1024, "Dictionary discovery limit exceeded");
        for entry in fs::read_dir(&folder).with_context(|| format!("Cannot read {}", folder.display()))? {
            visited += 1;
            ensure!(visited <= 100000 && current(), "Dictionary discovery cancelled or limit exceeded");
            let entry = entry?;
            let kind = entry.file_type()?;
            let path = entry.path();
            if kind.is_dir() && entry.file_name() != "res" {
                folders.push((path, depth + 1));
            } else if kind.is_file() && path.extension().is_some_and(|e| e == "ifo") {
                let id = path.strip_prefix(root)?.to_string_lossy().into_owned();
                let info = Info::read(&path).and_then(|i| {
                    companion(&path, "idx", "idx.gz")?;
                    companion(&path, "dict", "dict.dz")?;
                    Ok(i)
                });
                let (name, error) = match info {
                    Ok(i) => (i.name, None),
                    Err(e) => (path.file_stem().unwrap_or_default().to_string_lossy().into(), Some(format!("{e:#}"))),
                };
                let disabled = preferences.iter().any(|p| p.id == id && p.disabled);
                found.push(Dictionary { id, name, path, error, disabled });
            }
        }
    }
    found.sort_by(|a, b| {
        let rank = |id: &str| preferences.iter().position(|p| p.id == id).unwrap_or(usize::MAX);
        rank(&a.id).cmp(&rank(&b.id)).then(a.name.cmp(&b.name)).then(a.id.cmp(&b.id))
    });
    Ok(found)
}

pub struct Backend {
    index: Index,
    definitions: Definitions,
    types: String,
}
impl Backend {
    pub fn open(path: &Path, cache: &Path, current: &impl Fn() -> bool) -> Result<Self> {
        let info = Info::read(path)?;
        let (data_path, compressed) = companion(path, "dict", "dict.dz")?;
        let definitions = Definitions::open(&data_path, compressed)?;
        let index = Index::open(path, cache, &info, definitions.len, current)?;
        Ok(Self { index, definitions, types: info.types })
    }
    pub fn lookup(&mut self, query: &str) -> Result<String> {
        let entries = self.index.find(query)?;
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
