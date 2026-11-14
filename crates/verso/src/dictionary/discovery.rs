//! Discovery over Verso's native folder plus configured external folders.
//!
//! IDs: a native dictionary keeps its path relative to the native folder (as
//! before, so `dictionaries.json` keeps working); an external one uses its
//! absolute real path. Relative paths never start with `/`, so the two kinds
//! cannot collide, and an external ID does not depend on which overlapping or
//! aliased root found the file. Both use the plain `.ifo` path, also for an
//! `.ifo.gz`-only dictionary.
use super::{Dictionary, Info, Preference, basename, companion, is_gzip, sibling};
use anyhow::{Result, bail, ensure};
use std::{
    collections::HashSet,
    fs,
    path::{Component, Path, PathBuf},
};

#[derive(Clone, Copy, Debug)]
struct Limits {
    /// Directory levels below each source root.
    depth: usize,
    /// Filesystem entries across all sources combined.
    entries: usize,
    /// Dictionaries across all sources combined.
    dictionaries: usize,
}
const LIMITS: Limits = Limits { depth: 32, entries: 100_000, dictionaries: 1024 };

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceStatus {
    /// Scanned completely.
    Available,
    /// The folder does not exist: a normal empty installation.
    Absent,
    /// The folder or part of it could not be read; what was readable is listed.
    Unreadable(String),
    /// A discovery limit stopped the scan; the source may hold more dictionaries.
    LimitReached,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    /// As configured (not resolved), so a missing folder keeps its spelling.
    pub root: PathBuf,
    pub native: bool,
    pub status: SourceStatus,
}
#[derive(Clone, Debug, Default)]
pub struct Catalog {
    pub dictionaries: Vec<Dictionary>,
    /// The native source first, then external folders in configured order.
    pub sources: Vec<Source>,
}
impl Source {
    /// A short name: **Verso** for the native folder, **KOReader** for its
    /// standard folder, otherwise the configured path.
    pub fn label(&self) -> String {
        if self.native {
            "Verso".into()
        } else if self.root == Path::new(crate::store::KOREADER_DICTIONARY_FOLDER) {
            "KOReader".into()
        } else {
            self.root.display().to_string()
        }
    }
}
impl Dictionary {
    /// Where it was found, e.g. `KOReader · en/wordnet.ifo`.
    pub fn location(&self, sources: &[Source]) -> String {
        let source = sources.get(self.source).map_or_else(String::new, Source::label);
        format!("{source} · {}", self.relative.display())
    }
}
impl Catalog {
    /// Whether every source was scanned fully (absent sources count as empty).
    #[allow(dead_code)] // read by Settings in Phase 4 of the folders follow-up
    pub fn complete(&self) -> bool {
        self.sources.iter().all(|s| matches!(s.status, SourceStatus::Available | SourceStatus::Absent))
    }
}

/// Scans the native folder, then each external folder. Problems are reported
/// per source and never hide dictionaries from other sources. Returns an error
/// only when cancelled, so a partial scan is never published as complete.
pub fn discover_catalog(
    native: &Path,
    external: &[PathBuf],
    preferences: &[Preference],
    current: &impl Fn() -> bool,
) -> Result<Catalog> {
    discover_limited(native, external, preferences, current, LIMITS)
}
fn discover_limited(
    native: &Path,
    external: &[PathBuf],
    preferences: &[Preference],
    current: &impl Fn() -> bool,
    limits: Limits,
) -> Result<Catalog> {
    let mut scan =
        Scan { found: Vec::new(), seen: HashSet::new(), entries: 0, exhausted: false, preferences, current, limits };
    // Real roots scanned completely; a root inside one has nothing new to offer.
    let mut covered: Vec<PathBuf> = Vec::new();
    let mut sources = Vec::new();
    let roots = std::iter::once((native, true)).chain(external.iter().map(|r| (r.as_path(), false)));
    for (source, (root, native)) in roots.enumerate() {
        ensure!(current(), "Dictionary discovery cancelled");
        let status = if scan.exhausted {
            SourceStatus::LimitReached
        } else {
            match fs::canonicalize(root) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => SourceStatus::Absent,
                Err(e) => SourceStatus::Unreadable(format!("Cannot open {}: {e}", root.display())),
                Ok(real) if !real.is_dir() => SourceStatus::Unreadable(format!("{} is not a folder", root.display())),
                Ok(real) if covered.iter().any(|c| inside_scanned(c, &real)) => SourceStatus::Available,
                Ok(real) => {
                    // Native paths keep the configured spelling (stable cache
                    // names); external paths are real so IDs match across aliases.
                    let status = scan.tree(if native { root } else { &real }, &real, native, source)?;
                    if status == SourceStatus::Available {
                        covered.push(real);
                    }
                    status
                }
            }
        };
        sources.push(Source { root: root.to_owned(), native, status });
    }
    let mut dictionaries = scan.found;
    dictionaries.sort_by(|a, b| {
        let rank = |id: &str| preferences.iter().position(|p| p.id == id).unwrap_or(usize::MAX);
        rank(&a.id).cmp(&rank(&b.id)).then(a.name.cmp(&b.name)).then(a.path.cmp(&b.path)).then(a.id.cmp(&b.id))
    });
    Ok(Catalog { dictionaries, sources })
}

/// Whether a complete scan of `scanned` already visited `root`: it lies inside
/// it and not inside a skipped `res/` folder.
fn inside_scanned(scanned: &Path, root: &Path) -> bool {
    root.strip_prefix(scanned).is_ok_and(|rest| rest.components().all(|c| c != Component::Normal("res".as_ref())))
}

struct Scan<'a, F> {
    found: Vec<Dictionary>,
    /// Real paths of the plain `.ifo` of every listed dictionary.
    seen: HashSet<PathBuf>,
    entries: usize,
    /// An aggregate limit was hit; later sources are not scanned.
    exhausted: bool,
    preferences: &'a [Preference],
    current: &'a F,
    limits: Limits,
}
impl<F: Fn() -> bool> Scan<'_, F> {
    /// Directory symlinks below the root are not followed (`file_type` does
    /// not follow links), so the real path of a file is `real` plus its path
    /// relative to `root`.
    fn tree(&mut self, root: &Path, real: &Path, native: bool, source: usize) -> Result<SourceStatus> {
        let mut status = SourceStatus::Available;
        let mut note = |problem: SourceStatus| {
            if status == SourceStatus::Available || problem == SourceStatus::LimitReached {
                status = problem;
            }
        };
        let mut folders = vec![(root.to_owned(), 0usize)];
        while let Some((folder, depth)) = folders.pop() {
            let entries = match fs::read_dir(&folder) {
                Ok(entries) => entries,
                Err(e) => {
                    note(SourceStatus::Unreadable(format!("Cannot read {}: {e}", folder.display())));
                    continue;
                }
            };
            for entry in entries {
                if !(self.current)() {
                    bail!("Dictionary discovery cancelled");
                }
                if self.entries >= self.limits.entries {
                    self.exhausted = true;
                    return Ok(SourceStatus::LimitReached);
                }
                self.entries += 1;
                let (entry, kind) = match entry.and_then(|e| Ok((e.file_type()?, e))) {
                    Ok((kind, entry)) => (entry, kind),
                    Err(e) => {
                        note(SourceStatus::Unreadable(format!("Cannot read {}: {e}", folder.display())));
                        continue;
                    }
                };
                let path = entry.path();
                if kind.is_dir() {
                    if entry.file_name() == "res" {
                        continue;
                    }
                    if depth >= self.limits.depth {
                        note(SourceStatus::LimitReached);
                        continue;
                    }
                    folders.push((path, depth + 1));
                } else if kind.is_file()
                    && let Some(base) = basename(&path)
                {
                    // One dictionary per basename: plain `.ifo` wins over `.ifo.gz`.
                    let plain = sibling(&base, "ifo");
                    if is_gzip(&path) && plain.is_file() {
                        continue;
                    }
                    let relative = plain.strip_prefix(root)?;
                    let real_plain = real.join(relative);
                    if self.seen.contains(&real_plain) {
                        continue;
                    }
                    if self.found.len() >= self.limits.dictionaries {
                        self.exhausted = true;
                        return Ok(SourceStatus::LimitReached);
                    }
                    let id = if native { relative } else { real_plain.as_path() }.to_string_lossy().into_owned();
                    self.seen.insert(real_plain);
                    let info = Info::read(&path).and_then(|i| {
                        companion(&base, "idx", "idx.gz")?;
                        companion(&base, "dict", "dict.dz")?;
                        Ok(i)
                    });
                    let (name, error) = match info {
                        Ok(i) => (i.name, None),
                        Err(e) => {
                            (base.file_name().unwrap_or_default().to_string_lossy().into(), Some(format!("{e:#}")))
                        }
                    };
                    let disabled = self.preferences.iter().any(|p| p.id == id && p.disabled);
                    let relative = relative.to_owned();
                    self.found.push(Dictionary { id, name, path, relative, source, error, disabled });
                }
            }
        }
        Ok(status)
    }
}

#[cfg(test)]
#[path = "discovery_tests.rs"]
mod tests;
