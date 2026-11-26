//! The release manifest (`latest.json`), the record of what is installed
//! (`installed.json`) and component hashing.
use crate::{Error, Version};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashSet};

/// The manifest and `installed.json` format this code reads and writes.
pub const FORMAT: u32 = 1;
/// The longest short release note (`notes`), in bytes. Builds up to
/// 0.1.0-beta.10 refuse a manifest whose `notes` are longer.
pub const MAX_NOTES: usize = 2000;
/// The longest full release note (`details`), in bytes.
pub const MAX_DETAILS: usize = 16_000;
/// Upper bounds for the downloads of a check, so a broken server can't fill
/// memory. Manifests are a few kilobytes.
pub const MAX_MANIFEST_BYTES: usize = 1 << 20;
pub const MAX_SIGNATURE_BYTES: usize = 4096;
/// Where `installed.json` lives, relative to the storage root (`/mnt/us`).
pub const INSTALLED_PATH: &str = "verso/installed.json";
/// File modes a component may give its files: plain or executable.
pub const MODE_FILE: u32 = 0o644;
pub const MODE_EXEC: u32 = 0o755;

/// One release: what `latest.json` holds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub format: u32,
    pub version: Version,
    /// Release date, `YYYY-MM-DD`.
    pub released: String,
    /// Short plain text for builds that don't read `details`, derived from
    /// it by `verso-release pack`.
    #[serde(default)]
    pub notes: String,
    /// The full release notes: plain text, `## ` starts a heading and `- ` a
    /// bullet. Builds after 0.1.0-beta.10 show these when present, else `notes`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub details: String,
    pub components: Vec<Component>,
}

/// A part of Verso that is installed or replaced as a unit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Component {
    pub id: String,
    /// [`component_hash`] of `install` and `files`: the component's version.
    pub hash: String,
    /// Required components are always installed and kept current; optional
    /// ones are updated only when installed.
    pub required: bool,
    /// The oldest app that can use this component.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_app: Option<Version>,
    /// The folder the files go in, relative to the storage root (`/mnt/us`).
    pub install: String,
    /// One line for a list of optional downloads.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    pub archive: Archive,
    /// Every file the component owns, sorted by path.
    pub files: Vec<FileEntry>,
}

/// The `.tar.gz` that carries a component.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Archive {
    /// Relative to the URL the manifest was requested from (before redirects).
    pub url: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    /// Relative to the component's `install` folder, `/`-separated.
    pub path: String,
    pub size: u64,
    pub sha256: String,
    /// [`MODE_FILE`] or [`MODE_EXEC`].
    pub mode: u32,
}

/// What is on the device: `/mnt/us/verso/installed.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Installed {
    pub format: u32,
    pub version: Version,
    /// Installed by `scripts/deploy.sh` from a development build; automatic
    /// update checks are off.
    #[serde(default)]
    pub dev: bool,
    pub components: Vec<InstalledComponent>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledComponent {
    pub id: String,
    pub hash: String,
    pub install: String,
    /// The component's files, so files a new version drops can be removed.
    pub files: Vec<String>,
}

impl Manifest {
    /// Parses and validates a manifest.
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        let manifest: Manifest = serde_json::from_slice(bytes).map_err(|e| Error::Manifest(e.to_string()))?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn component(&self, id: &str) -> Option<&Component> {
        self.components.iter().find(|c| c.id == id)
    }

    /// The notes a build that reads `details` shows: those, else `notes`.
    pub fn shown_notes(&self) -> &str {
        if self.details.trim().is_empty() { &self.notes } else { &self.details }
    }

    /// Checks everything that doesn't need the archives: the format, safe
    /// paths, that no two components claim the same file, and every
    /// component's hash.
    pub fn validate(&self) -> Result<(), Error> {
        let bad = |msg: String| Err(Error::Manifest(msg));
        if self.format != FORMAT {
            return Err(Error::UnsupportedFormat(self.format));
        }
        if !is_date(&self.released) {
            return bad(format!("release date {:?} isn't YYYY-MM-DD", self.released));
        }
        if self.notes.len() > MAX_NOTES {
            return bad(format!("release notes are {} bytes, more than {MAX_NOTES}", self.notes.len()));
        }
        if self.details.len() > MAX_DETAILS {
            return bad(format!("release details are {} bytes, more than {MAX_DETAILS}", self.details.len()));
        }
        if self.components.is_empty() {
            return bad("no components".into());
        }
        let mut ids = HashSet::new();
        let mut owned = BTreeSet::new();
        for c in &self.components {
            if !is_id(&c.id) {
                return bad(format!("component id {:?} isn't lowercase letters, digits and dashes", c.id));
            }
            if !ids.insert(c.id.as_str()) {
                return bad(format!("component {:?} is listed twice", c.id));
            }
            if !is_safe_path(&c.install) {
                return bad(format!("component {:?} has an unsafe install folder {:?}", c.id, c.install));
            }
            if c.archive.url.is_empty() || c.archive.url.chars().any(|ch| ch.is_whitespace() || ch.is_control()) {
                return bad(format!("component {:?} has a bad archive URL", c.id));
            }
            if !is_sha256(&c.archive.sha256) {
                return bad(format!("component {:?} has a bad archive hash", c.id));
            }
            if c.files.is_empty() {
                return bad(format!("component {:?} has no files", c.id));
            }
            // FAT ignores letter case, so two paths differing only by case
            // would collide when the update is unpacked on the Kindle.
            let mut folded = HashSet::new();
            for (i, f) in c.files.iter().enumerate() {
                if !is_safe_path(&f.path) {
                    return bad(format!("component {:?} has an unsafe path {:?}", c.id, f.path));
                }
                if i > 0 && c.files[i - 1].path >= f.path {
                    return bad(format!("component {:?}: files aren't sorted or repeat {:?}", c.id, f.path));
                }
                if !folded.insert(f.path.to_lowercase()) {
                    return bad(format!(
                        "component {:?}: {:?} differs from another file only by letter case",
                        c.id, f.path
                    ));
                }
                if !is_sha256(&f.sha256) {
                    return bad(format!("component {:?}: bad hash for {:?}", c.id, f.path));
                }
                if f.mode != MODE_FILE && f.mode != MODE_EXEC {
                    return bad(format!("component {:?}: mode {:o} for {:?}", c.id, f.mode, f.path));
                }
                owned.insert(format!("{}/{}", c.install, f.path));
            }
            if c.hash != component_hash(&c.install, &c.files) {
                return bad(format!("component {:?}: its hash doesn't match its files", c.id));
            }
        }
        // A file can't be another file's folder, and two components can't
        // share a file. (Duplicates within a component are caught above.)
        let total: usize = self.components.iter().map(|c| c.files.len()).sum();
        if owned.len() != total {
            return bad("two components install the same file".into());
        }
        for path in &owned {
            let mut folder = path.as_str();
            while let Some((parent, _)) = folder.rsplit_once('/') {
                if owned.contains(parent) {
                    return bad(format!("{parent:?} is both a file and a folder"));
                }
                folder = parent;
            }
        }
        Ok(())
    }
}

impl Installed {
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        let installed: Installed = serde_json::from_slice(bytes).map_err(|e| Error::Manifest(e.to_string()))?;
        if installed.format != FORMAT {
            return Err(Error::UnsupportedFormat(installed.format));
        }
        Ok(installed)
    }

    /// The record for a manifest's components with the given ids, as they
    /// are once installed.
    pub fn from_manifest(manifest: &Manifest, ids: &[&str], dev: bool) -> Self {
        let components = manifest
            .components
            .iter()
            .filter(|c| ids.contains(&c.id.as_str()))
            .map(|c| InstalledComponent {
                id: c.id.clone(),
                hash: c.hash.clone(),
                install: c.install.clone(),
                files: c.files.iter().map(|f| f.path.clone()).collect(),
            })
            .collect();
        Installed { format: FORMAT, version: manifest.version.clone(), dev, components }
    }

    pub fn component(&self, id: &str) -> Option<&InstalledComponent> {
        self.components.iter().find(|c| c.id == id)
    }
}

/// A component's version: SHA-256 over its install folder and its sorted
/// (path, mode, file hash) list. Rebuilding unchanged files gives the same
/// hash; any change to a file, its mode, the file list or the folder gives a
/// new one.
pub fn component_hash(install: &str, files: &[FileEntry]) -> String {
    let mut sorted: Vec<&FileEntry> = files.iter().collect();
    sorted.sort_by(|a, b| a.path.cmp(&b.path));
    let mut h = Sha256::new();
    h.update(b"verso-component 1\n");
    h.update(install.as_bytes());
    h.update(b"\n");
    for f in sorted {
        h.update(f.path.as_bytes());
        h.update(format!("\0{:o}\0{}\n", f.mode, f.sha256).as_bytes());
    }
    hex(&h.finalize())
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn is_sha256(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn is_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes().all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'-'))
        && !s.starts_with('-')
}

fn is_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter().enumerate().all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
}

/// A relative `/`-separated path with no `.`, `..` or empty parts, and only
/// characters the Kindle's FAT-backed storage accepts.
pub fn is_safe_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 255
        && path.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && !part.ends_with('.')
                && !part.ends_with(' ')
                && part.chars().all(|c| !c.is_control() && !matches!(c, '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, content: &[u8], mode: u32) -> FileEntry {
        FileEntry { path: path.into(), size: content.len() as u64, sha256: sha256_hex(content), mode }
    }

    fn component(id: &str, install: &str, files: Vec<FileEntry>) -> Component {
        Component {
            id: id.into(),
            hash: component_hash(install, &files),
            required: true,
            min_app: None,
            install: install.into(),
            description: String::new(),
            archive: Archive { url: format!("{id}-0.3.0.tar.gz"), size: 1, sha256: sha256_hex(id.as_bytes()) },
            files,
        }
    }

    fn manifest() -> Manifest {
        Manifest {
            format: FORMAT,
            version: "0.3.0".parse().unwrap(),
            released: "2026-12-01".into(),
            notes: "Faster page turns.".into(),
            details: "## Reading\n- Faster page turns.".into(),
            components: vec![
                component("app", "verso", vec![file("verso", b"binary", MODE_EXEC)]),
                component("launcher", "verso", vec![file("verso.sh", b"#!/bin/sh", MODE_EXEC)]),
                component(
                    "fonts",
                    "verso/fonts",
                    vec![file("A.ttf", b"a", MODE_FILE), file("LICENSE.txt", b"ofl", MODE_FILE)],
                ),
            ],
        }
    }

    #[test]
    fn hash_is_stable_and_order_independent() {
        let a = vec![file("a", b"1", MODE_FILE), file("b/c", b"2", MODE_EXEC)];
        let b = vec![a[1].clone(), a[0].clone()];
        assert_eq!(component_hash("x", &a), component_hash("x", &b));
        assert_eq!(component_hash("x", &a), component_hash("x", &a.clone()));
    }

    #[test]
    fn hash_changes_with_any_change() {
        let base = vec![file("a", b"1", MODE_FILE), file("b", b"2", MODE_FILE)];
        let h = component_hash("x", &base);
        let mut content = base.clone();
        content[1] = file("b", b"3", MODE_FILE);
        let mut mode = base.clone();
        mode[0].mode = MODE_EXEC;
        let mut renamed = base.clone();
        renamed[0].path = "c".into();
        let added = [base.clone(), vec![file("d", b"", MODE_FILE)]].concat();
        let removed = base[..1].to_vec();
        for (what, files) in
            [("content", content), ("mode", mode), ("rename", renamed), ("add", added), ("remove", removed)]
        {
            assert_ne!(component_hash("x", &files), h, "{what}");
        }
        assert_ne!(component_hash("y", &base), h, "install folder");
        // Paths and hashes can't run into each other.
        let joined = vec![file("a\u{1}", b"", MODE_FILE)];
        assert_ne!(component_hash("x", &joined), component_hash("x", &[file("a", b"", MODE_FILE)]));
    }

    #[test]
    fn round_trip() {
        let m = manifest();
        let json = serde_json::to_vec_pretty(&m).unwrap();
        assert_eq!(Manifest::parse(&json).unwrap(), m);
        let installed = Installed::from_manifest(&m, &["app", "fonts"], true);
        let json = serde_json::to_vec(&installed).unwrap();
        let back = Installed::parse(&json).unwrap();
        assert_eq!(back, installed);
        assert!(back.dev);
        assert_eq!(back.component("fonts").unwrap().files, ["A.ttf", "LICENSE.txt"]);
        assert!(back.component("launcher").is_none());
    }

    #[test]
    fn ignores_unknown_fields() {
        let mut json: serde_json::Value = serde_json::to_value(manifest()).unwrap();
        json["future"] = "x".into();
        json["components"][0]["future"] = 1.into();
        Manifest::parse(&serde_json::to_vec(&json).unwrap()).unwrap();
    }

    #[test]
    fn rejects_bad_manifests() {
        type Change = Box<dyn Fn(&mut Manifest)>;
        let cases: Vec<(&str, Change)> = vec![
            ("format", Box::new(|m| m.format = 2)),
            ("date", Box::new(|m| m.released = "1 Dec 2026".into())),
            ("notes", Box::new(|m| m.notes = "x".repeat(MAX_NOTES + 1))),
            ("details", Box::new(|m| m.details = "x".repeat(MAX_DETAILS + 1))),
            ("empty", Box::new(|m| m.components.clear())),
            ("id", Box::new(|m| m.components[0].id = "App".into())),
            ("duplicate id", Box::new(|m| m.components[1].id = "app".into())),
            ("hash", Box::new(|m| m.components[0].hash = sha256_hex(b"other"))),
            ("file hash", Box::new(|m| m.components[0].files[0].sha256 = "abc".into())),
            ("mode", Box::new(|m| m.components[0].files[0].mode = 0o4755)),
            ("archive url", Box::new(|m| m.components[0].archive.url = "a b".into())),
            ("unsorted", Box::new(|m| m.components[2].files.reverse())),
            ("no files", Box::new(|m| m.components[2].files.clear())),
        ];
        for (what, change) in cases {
            let mut m = manifest();
            change(&mut m);
            assert!(m.validate().is_err(), "{what}");
        }
    }

    #[test]
    fn rejects_case_only_collisions() {
        let mut m = manifest();
        let files =
            vec![file("OFL.txt", b"a", MODE_FILE), file("a.txt", b"b", MODE_FILE), file("ofl.txt", b"c", MODE_FILE)];
        m.components[2] = component("fonts", "verso/fonts", files);
        assert!(m.validate().is_err());
    }

    #[test]
    fn rejects_unsafe_paths() {
        for path in ["/etc/passwd", "../x", "a/../b", "a//b", "./a", "a/", "", "a\\b", "c:x", "a\0b", "x.", "x "] {
            assert!(!is_safe_path(path), "{path:?}");
            let mut m = manifest();
            let c = &mut m.components[0];
            c.files[0].path = path.into();
            c.hash = component_hash(&c.install, &c.files);
            assert!(m.validate().is_err(), "file {path:?}");
            let mut m = manifest();
            let c = &mut m.components[0];
            c.install = path.into();
            c.hash = component_hash(&c.install, &c.files);
            assert!(m.validate().is_err(), "install {path:?}");
        }
        assert!(is_safe_path("verso/data/cre/hyph/Russian_EnUS.pattern"));
        assert!(is_safe_path("Ру сский.txt"));
    }

    #[test]
    fn rejects_overlapping_components() {
        // Two components owning one file.
        let mut m = manifest();
        m.components[1] = component("launcher", "verso", vec![file("verso", b"x", MODE_FILE)]);
        assert!(m.validate().is_err());
        // A file where another component needs a folder.
        let mut m = manifest();
        m.components[1] = component("launcher", "verso/verso", vec![file("x", b"x", MODE_FILE)]);
        assert!(m.validate().is_err());
        // The same, with a path sorting between them ("verso/verso.sh").
        let mut m = manifest();
        m.components[2] = component("fonts", "verso/verso", vec![file("x", b"x", MODE_FILE)]);
        assert!(m.validate().is_err());
        // Sharing a folder is fine.
        let mut m = manifest();
        m.components[1] = component("launcher", "verso", vec![file("versos", b"x", MODE_FILE)]);
        m.validate().unwrap();
    }
}
