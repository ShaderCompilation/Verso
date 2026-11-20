//! The Files tab API (docs/web-app.md, "Files: who may write where"): a plain
//! browser of the Kindle's storage (`/mnt/us`) with guardrails. Every path is
//! classified on the server, ignoring ASCII case, both as given and after
//! following symlinks, and the stricter class wins. Inside `verso` everything
//! is read-only except the folders listed as user content, so a new app file or
//! folder there is protected without changing the table.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::UNIX_EPOCH;

use serde_json::{Value, json};

use super::Activity;
use super::books::{ActivitySink, FREE_SPACE_RESERVE, UploadGuard, create_part, err, read_error, write_error};
use super::device::FreeSpace;
use super::http::{BUFFER, Body, Request, Response};
use crate::library::{collections, files as names};
use crate::store::Store;

/// Jailbreak and system folders at the top of the storage: hidden, or read
/// only in developer mode. Seen on our Kindle (PW5, WinterBreak, 2026-11-20):
/// `libkh`, `mrpackages`, `system`, `.active_content_sandbox`, `.KFPM`,
/// `lost+found`, and `voice` (VoiceView's data). Lowercase.
const SYSTEM: [&str; 7] = ["libkh", "mrpackages", "system", ".active_content_sandbox", ".kfpm", "lost+found", "voice"];

/// Folders whose contents are user content but which Verso needs in place:
/// files go in and out, but they can't be renamed, moved or deleted.
const PINNED: [&[&str]; 2] = [&["verso", "data", "dictionaries"], &["verso", "data", "exports"]];

/// Deleting these (or a library folder) asks twice in the page, and the
/// server wants `confirm=2`.
const ASK_TWICE: [&[&str]; 2] = [&["koreader"], &["extensions", "koreader"]];

/// Stop counting a folder's contents for the delete question after this many
/// entries.
const COUNT_LIMIT: u64 = 100_000;

/// Who may do what with a path (the table in docs/web-app.md).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// Read and write.
    User,
    /// `verso/data/*.json`, `books`, `annotations`: read only (developer
    /// writes come with Restart to apply, Phase 5).
    LiveData,
    /// Everything else in `verso`, and `extensions/verso`: read only.
    App,
    /// Jailbreak and system files: hidden, read only in developer mode.
    System,
    /// Verso's caches: always hidden.
    Cache,
}

impl Class {
    fn rank(self) -> u8 {
        match self {
            Class::User => 0,
            Class::LiveData | Class::App => 1,
            Class::System => 2,
            Class::Cache => 3,
        }
    }

    fn stricter(self, other: Class) -> Class {
        if other.rank() > self.rank() { other } else { self }
    }

    pub fn visible(self, developer: bool) -> bool {
        match self {
            Class::User | Class::LiveData | Class::App => true,
            Class::System => developer,
            Class::Cache => false,
        }
    }

    pub fn writable(self) -> bool {
        self == Class::User
    }
}

fn lower(rel: &[String]) -> Vec<String> {
    rel.iter().map(|c| c.to_ascii_lowercase()).collect()
}

fn is(rel: &[String], pattern: &[&str]) -> bool {
    rel.len() == pattern.len() && rel.iter().zip(pattern).all(|(a, b)| a == b)
}

fn under(rel: &[String], pattern: &[&str]) -> bool {
    rel.len() >= pattern.len() && rel.iter().zip(pattern).all(|(a, b)| a == b)
}

/// The class of a path given as its components under the storage root,
/// compared ignoring ASCII case.
pub fn classify(rel: &[String]) -> Class {
    let rel = lower(rel);
    let Some(first) = rel.first() else { return Class::User };
    if SYSTEM.contains(&first.as_str()) {
        return Class::System;
    }
    if under(&rel, &["extensions", "verso"]) {
        return Class::App;
    }
    if first != "verso" {
        return Class::User;
    }
    if PINNED.iter().any(|p| under(&rel, p)) {
        return Class::User;
    }
    if under(&rel, &["verso", "cache"])
        || ["covers", "cre", "dictionary-cache"].iter().any(|c| under(&rel, &["verso", "data", c]))
    {
        return Class::Cache;
    }
    if under(&rel, &["verso", "data", "books"])
        || under(&rel, &["verso", "data", "annotations"])
        || (rel.len() == 3 && rel[1] == "data" && rel[2].ends_with(".json"))
    {
        return Class::LiveData;
    }
    Class::App
}

/// Whether `rel` is, or holds, a path that must stay: Verso's folder, its
/// KUAL extension, the jailbreak and system folders, the pinned folders and
/// the root. Classes depend only on paths, so comparing against these
/// locations checks everything inside a folder without walking it.
fn holds_protected(rel: &[String]) -> bool {
    let rel = lower(rel);
    let prefix_of = |loc: &[&str]| rel.len() <= loc.len() && rel.iter().zip(loc).all(|(a, b)| a == b);
    rel.is_empty()
        || prefix_of(&["verso"])
        || prefix_of(&["extensions", "verso"])
        || SYSTEM.iter().any(|s| prefix_of(&[s]))
        || PINNED.iter().any(|p| is(&rel, p))
}

/// The shared name rules (`library::files::check_name`), plus no `.` or `..`.
fn check_name(name: &str) -> Result<&str, Response> {
    let name = name.trim();
    if name.is_empty() {
        return Err(err(400, "Enter a name."));
    }
    if name == "." || name == ".." {
        return Err(err(400, "That name isn't allowed."));
    }
    names::check_name(name).map_err(|e| err(400, &format!("{e:#}.")))?;
    Ok(name)
}

/// Splits a path from the page (`documents/Russian`, relative to the storage
/// root; a leading `/` is ignored) into its parts, refusing `.`, `..` and NUL.
fn parse_rel(raw: &str) -> Result<Vec<String>, Response> {
    if raw.contains('\0') {
        return Err(err(400, "Invalid path."));
    }
    let mut parts = Vec::new();
    for part in raw.split('/').filter(|p| !p.is_empty()) {
        if part == "." || part == ".." {
            return Err(err(400, "Paths can't contain . or .. parts."));
        }
        parts.push(part.to_owned());
    }
    Ok(parts)
}

fn join(rel: &[String]) -> String {
    rel.join("/")
}

/// An existing path, checked.
struct Target {
    /// As given, under the root.
    rel: Vec<String>,
    /// After following symlinks, under the root.
    canon_rel: Vec<String>,
    path: PathBuf,
    canon: PathBuf,
    class: Class,
    /// The path itself is a symlink.
    link: bool,
    /// What it is, following symlinks.
    dir: bool,
    len: u64,
}

impl Target {
    /// Can be renamed, moved or deleted.
    fn movable(&self) -> bool {
        self.class.writable() && !holds_protected(&self.rel) && !holds_protected(&self.canon_rel)
    }

    fn name(&self) -> &str {
        self.rel.last().map_or("", String::as_str)
    }
}

/// Everything the Files API handlers need.
pub struct Files {
    root: PathBuf,
    store: Store,
    free_space: FreeSpace,
    activity: Arc<dyn ActivitySink>,
    developer: bool,
    /// Set when a change may have touched a library folder: Done rescans.
    library_changed: Arc<AtomicBool>,
    /// Set when a change may have touched a dictionary folder.
    dictionaries_changed: Arc<AtomicBool>,
}

impl Files {
    pub fn new(
        root: PathBuf,
        store: Store,
        free_space: FreeSpace,
        activity: Arc<dyn ActivitySink>,
        developer: bool,
        library_changed: Arc<AtomicBool>,
        dictionaries_changed: Arc<AtomicBool>,
    ) -> Self {
        Self { root, store, free_space, activity, developer, library_changed, dictionaries_changed }
    }

    fn canon_root(&self) -> Result<PathBuf, Response> {
        fs::canonicalize(&self.root).map_err(|e| {
            log::warn!("web app: opening {}: {e}", self.root.display());
            err(500, "Couldn't open the Kindle's storage.")
        })
    }

    /// Resolves an existing path: inside the root after following symlinks,
    /// and visible to this user.
    fn existing(&self, raw: &str) -> Result<Target, Response> {
        let rel = parse_rel(raw)?;
        let canon_root = self.canon_root()?;
        let path = rel.iter().fold(self.root.clone(), |p, c| p.join(c));
        let link = match fs::symlink_metadata(&path) {
            Ok(meta) => meta.file_type().is_symlink(),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(err(404, "That no longer exists.")),
            Err(e) => {
                log::warn!("web app: reading {}: {e}", path.display());
                return Err(err(500, "Couldn't read that."));
            }
        };
        // A broken link has nowhere to go: treated as missing.
        let canon = fs::canonicalize(&path).map_err(|_| err(404, "That no longer exists."))?;
        let Ok(inside) = canon.strip_prefix(&canon_root) else {
            return Err(err(403, "That is outside the Kindle's storage."));
        };
        let canon_rel: Vec<String> =
            inside.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
        let class = classify(&rel).stricter(classify(&canon_rel));
        if !class.visible(self.developer) {
            return Err(err(404, "That no longer exists."));
        }
        let meta = fs::metadata(&canon).map_err(|_| err(404, "That no longer exists."))?;
        Ok(Target { rel, canon_rel, path, canon, class, link, dir: meta.is_dir(), len: meta.len() })
    }

    /// An existing folder that new things may go in.
    fn writable_folder(&self, raw: &str) -> Result<Target, Response> {
        let folder = self.existing(raw)?;
        if !folder.dir {
            return Err(err(400, "Choose a folder."));
        }
        if !folder.class.writable() {
            return Err(err(403, "This folder is read-only."));
        }
        Ok(folder)
    }

    /// A new name in `folder`: its class both ways must allow writing.
    fn child_class(folder: &Target, name: &str) -> Class {
        let with = |rel: &[String]| {
            let mut rel = rel.to_vec();
            rel.push(name.to_owned());
            classify(&rel)
        };
        with(&folder.rel).stricter(with(&folder.canon_rel))
    }

    fn library_roots(&self) -> Vec<PathBuf> {
        collections::distinct_roots(&self.store.library_folders())
            .into_iter()
            .filter_map(|r| fs::canonicalize(r).ok())
            .collect()
    }

    fn dictionary_roots(&self) -> Vec<PathBuf> {
        let mut roots = vec![self.store.dictionary_dir()];
        roots.extend(self.store.dictionary_folders().folders);
        roots.into_iter().filter_map(|r| fs::canonicalize(r).ok()).collect()
    }

    /// Which of Verso's views a change at these (canonical) paths affects.
    /// Read before the change: a deleted library folder can't be resolved
    /// afterwards.
    fn affects(&self, paths: &[&Path]) -> Affects {
        let related =
            |roots: Vec<PathBuf>| paths.iter().any(|p| roots.iter().any(|r| p.starts_with(r) || r.starts_with(p)));
        Affects { library: related(self.library_roots()), dictionaries: related(self.dictionary_roots()) }
    }

    /// Notes a change for Done: the library rescans, or the dictionary
    /// catalog.
    fn changed(&self, affects: Affects) {
        if affects.library {
            self.library_changed.store(true, Ordering::SeqCst);
        }
        if affects.dictionaries {
            self.dictionaries_changed.store(true, Ordering::SeqCst);
        }
    }

    /// Deleting the whole of KOReader or a library folder asks twice.
    fn ask_twice(&self, target: &Target) -> bool {
        let rel = lower(&target.canon_rel);
        ASK_TWICE.iter().any(|p| is(&rel, p))
            || ASK_TWICE.iter().any(|p| is(&lower(&target.rel), p))
            || self.library_roots().iter().any(|r| r.starts_with(&target.canon))
    }

    fn entry_json(&self, target: &Target) -> Value {
        json!({
            "name": target.name(),
            "path": join(&target.rel),
            "dir": target.dir,
            "size": (!target.dir).then_some(target.len),
            "writable": target.movable(),
            // Shown with a lock; a pinned folder is neither.
            "readonly": !target.class.writable(),
            "link": target.link,
            "twice": target.dir && self.ask_twice(target),
        })
    }

    /// `GET /api/files?path=…[&hidden=1]`
    pub fn list(&self, request: &Request) -> Response {
        let folder = match self.existing(&request.param("path").unwrap_or_default()) {
            Ok(folder) => folder,
            Err(response) => return response,
        };
        if !folder.dir {
            return err(400, "That is a file, not a folder.");
        }
        let hidden = request.param("hidden").as_deref() == Some("1");
        let read = match fs::read_dir(&folder.path) {
            Ok(read) => read,
            Err(e) => {
                log::warn!("web app: listing {}: {e}", folder.path.display());
                return err(500, "Couldn't read this folder.");
            }
        };
        let mut entries = Vec::new();
        for entry in read.flatten() {
            let Ok(name) = entry.file_name().into_string() else {
                log::info!("web app: skipping a name that isn't UTF-8 in {}", folder.path.display());
                continue;
            };
            if name.starts_with('.') && !hidden {
                continue;
            }
            let mut rel = folder.rel.clone();
            rel.push(name);
            // Hidden classes, broken links and links out of the storage are left out.
            let Ok(child) = self.existing(&join(&rel)) else { continue };
            let modified = fs::symlink_metadata(&child.path)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs());
            let mut value = self.entry_json(&child);
            value["modified"] = json!(modified);
            entries.push((child.dir, child.name().to_lowercase(), value));
        }
        entries.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        let mut crumbs = vec![json!({ "name": "Kindle", "path": "" })];
        for i in 0..folder.rel.len() {
            crumbs.push(json!({ "name": folder.rel[i], "path": join(&folder.rel[..=i]) }));
        }
        Response::json(
            200,
            &json!({
                "path": join(&folder.rel),
                "writable": folder.class.writable(),
                "crumbs": crumbs,
                "entries": entries.into_iter().map(|(_, _, v)| v).collect::<Vec<_>>(),
                "shortcuts": self.shortcuts(),
            }),
        )
    }

    /// Books · Fonts · Dictionaries · Exports · Everything, where they exist.
    fn shortcuts(&self) -> Vec<Value> {
        let mut out = Vec::new();
        let canon_root = fs::canonicalize(&self.root).ok();
        let books = canon_root.as_ref().and_then(|root| {
            self.library_roots().into_iter().find_map(|r| {
                let rel = r.strip_prefix(root).ok()?;
                Some(rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>())
            })
        });
        if let Some(books) = books {
            out.push(json!({ "label": "Books", "path": join(&books) }));
        }
        for (label, path) in
            [("Fonts", "fonts"), ("Dictionaries", "verso/data/dictionaries"), ("Exports", "verso/data/exports")]
        {
            if self.root.join(path).is_dir() {
                out.push(json!({ "label": label, "path": path }));
            }
        }
        out.push(json!({ "label": "Everything", "path": "" }));
        out
    }

    /// `GET /api/files/download?path=…`: always as an attachment, so an
    /// `.html` or `.svg` file never opens under the page's origin.
    pub fn download(&self, request: &Request) -> Response {
        let target = match self.existing(&request.param("path").unwrap_or_default()) {
            Ok(target) => target,
            Err(response) => return response,
        };
        if target.dir {
            return err(400, "Folders can't be downloaded.");
        }
        match File::open(&target.canon) {
            Ok(file) => Response::file(file, target.len, target.name()),
            Err(e) => {
                log::warn!("web app: opening {} for download: {e}", target.path.display());
                err(500, "Couldn't open the file.")
            }
        }
    }

    /// `GET /api/files/info?path=…`: what a delete would remove, for the
    /// question in the page.
    pub fn info(&self, request: &Request) -> Response {
        let target = match self.existing(&request.param("path").unwrap_or_default()) {
            Ok(target) => target,
            Err(response) => return response,
        };
        let (mut files, mut folders, mut bytes, mut more) = (0u64, 0u64, 0u64, false);
        if target.dir && !target.link {
            let mut stack = vec![target.path.clone()];
            'walk: while let Some(dir) = stack.pop() {
                let Ok(read) = fs::read_dir(&dir) else { continue };
                for entry in read.flatten() {
                    if files + folders >= COUNT_LIMIT {
                        more = true;
                        break 'walk;
                    }
                    let Ok(kind) = entry.file_type() else { continue };
                    if kind.is_dir() {
                        folders += 1;
                        stack.push(entry.path());
                    } else {
                        files += 1;
                        bytes += entry.metadata().map_or(0, |m| m.len());
                    }
                }
            }
        } else {
            files = 1;
            bytes = target.len;
        }
        let mut value = self.entry_json(&target);
        value["files"] = json!(files);
        value["folders"] = json!(folders);
        value["bytes"] = json!(bytes);
        value["more"] = json!(more);
        Response::json(200, &value)
    }

    /// `PUT /api/files?path=…[&overwrite=1]`: `path` names the new file.
    /// An existing file is replaced only with `overwrite=1` (`409` otherwise).
    pub fn upload(&self, request: &Request, body: &mut Body<'_>) -> Response {
        let rel = match parse_rel(&request.param("path").unwrap_or_default()) {
            Ok(rel) => rel,
            Err(response) => return response,
        };
        let Some((name, parent)) = rel.split_last() else {
            return err(400, "Missing file name.");
        };
        let name = match check_name(name) {
            Ok(name) => name.to_owned(),
            Err(response) => return response,
        };
        let folder = match self.writable_folder(&join(parent)) {
            Ok(folder) => folder,
            Err(response) => return response,
        };
        if !Self::child_class(&folder, &name).writable() {
            return err(403, "This folder is read-only.");
        }
        let overwrite = request.param("overwrite").as_deref() == Some("1");
        let target = folder.canon.join(&name);
        match fs::symlink_metadata(&target) {
            Ok(meta) if meta.is_dir() => return err(409, "A folder with that name is already there."),
            Ok(meta) if !meta.is_file() => return err(409, "Something with that name is already there."),
            Ok(_) if !overwrite => return conflict("A file with that name is already there."),
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return write_error("Couldn't check the name", e),
        }
        match (self.free_space)(&folder.canon) {
            Ok(free) if free < request.content_length.saturating_add(FREE_SPACE_RESERVE) => {
                return err(507, "Not enough free space on the Kindle (50 MB is kept in reserve).");
            }
            Ok(_) => {}
            Err(e) => {
                log::warn!("web app: reading free space for {}: {e}", folder.canon.display());
                return err(500, "Couldn't check free space on the Kindle.");
            }
        }

        let guard = UploadGuard::start(&*self.activity, name.clone());
        // A hidden part name: it never shows up in a listing, and a 255-byte
        // name still fits.
        let mut part = match create_part(&folder.canon, ".verso-upload") {
            Ok(part) => part,
            Err(e) => return write_error("Couldn't start saving the file", e),
        };
        let mut buf = vec![0u8; BUFFER];
        loop {
            match body.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if let Err(e) = part.write_all(&buf[..n]) {
                        return write_error("Couldn't write the file", e);
                    }
                }
                Err(e) => return read_error(e),
            }
        }
        let finished = if overwrite {
            // The overwrite was asked for; rename replaces in one step.
            match fs::symlink_metadata(&target) {
                Ok(meta) if !meta.is_file() => return err(409, "Something with that name is already there."),
                _ => part.finish(&target),
            }
        } else {
            part.try_finish_noreplace(&target)
        };
        match finished {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                return conflict("A file with that name is already there.");
            }
            Err(e) => return write_error("Couldn't save the file", e),
        }
        self.changed(self.affects(&[&target]));
        let folder_name = folder.rel.last().cloned().unwrap_or_else(|| "Kindle".into());
        guard.finish(Activity::Received { name: name.clone(), folder: folder_name });
        log::info!("web app: received {} (files)", target.display());
        let mut rel = folder.rel.clone();
        rel.push(name);
        match self.existing(&join(&rel)) {
            Ok(saved) => Response::json(201, &self.entry_json(&saved)),
            Err(_) => Response::json(201, &json!({ "path": join(&rel) })),
        }
    }

    /// `POST /api/files/mkdir` with `{"path": parent, "name": …}`
    pub fn mkdir(&self, body: &mut Body<'_>) -> Response {
        #[derive(serde::Deserialize)]
        struct Input {
            path: String,
            name: String,
        }
        let input: Input = match read_json(body) {
            Ok(input) => input,
            Err(response) => return response,
        };
        let name = match check_name(&input.name) {
            Ok(name) => name.to_owned(),
            Err(response) => return response,
        };
        let folder = match self.writable_folder(&input.path) {
            Ok(folder) => folder,
            Err(response) => return response,
        };
        if !Self::child_class(&folder, &name).writable() {
            return err(403, "This folder is read-only.");
        }
        let target = folder.canon.join(&name);
        match fs::create_dir(&target) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return err(409, "That name is already taken."),
            Err(e) => return write_error("Couldn't create the folder", e),
        }
        self.changed(self.affects(&[&target]));
        let mut rel = folder.rel.clone();
        rel.push(name);
        Response::json(201, &json!({ "path": join(&rel) }))
    }

    /// Something that may be renamed, moved or deleted.
    fn movable(&self, raw: &str) -> Result<Target, Response> {
        let target = self.existing(raw)?;
        if target.rel.is_empty() {
            return Err(err(403, "The Kindle's storage itself can't be changed."));
        }
        if !target.movable() {
            return Err(err(
                403,
                if target.class.writable() {
                    "Verso and the Kindle need this folder; it can't be renamed, moved or deleted."
                } else {
                    "This is read-only."
                },
            ));
        }
        Ok(target)
    }

    /// `POST /api/files/rename` with `{"path": …, "name": …}`
    pub fn rename(&self, body: &mut Body<'_>) -> Response {
        #[derive(serde::Deserialize)]
        struct Input {
            path: String,
            name: String,
        }
        let input: Input = match read_json(body) {
            Ok(input) => input,
            Err(response) => return response,
        };
        let source = match self.movable(&input.path) {
            Ok(source) => source,
            Err(response) => return response,
        };
        let name = match check_name(&input.name) {
            Ok(name) => name.to_owned(),
            Err(response) => return response,
        };
        let parent_rel = &source.rel[..source.rel.len() - 1];
        let parent = match self.writable_folder(&join(parent_rel)) {
            Ok(parent) => parent,
            Err(response) => return response,
        };
        if name == source.name() {
            return Response::json(200, &self.entry_json(&source));
        }
        if !Self::child_class(&parent, &name).writable() {
            return err(403, "That name belongs to Verso or the Kindle.");
        }
        let from = parent.canon.join(source.name());
        let to = parent.canon.join(&name);
        self.finish_move(&from, &to, &parent.rel, &name)
    }

    /// `POST /api/files/move` with `{"path": …, "to": folder}`
    pub fn move_to(&self, body: &mut Body<'_>) -> Response {
        #[derive(serde::Deserialize)]
        struct Input {
            path: String,
            to: String,
        }
        let input: Input = match read_json(body) {
            Ok(input) => input,
            Err(response) => return response,
        };
        let source = match self.movable(&input.path) {
            Ok(source) => source,
            Err(response) => return response,
        };
        let destination = match self.writable_folder(&input.to) {
            Ok(destination) => destination,
            Err(response) => return response,
        };
        let parent_rel = &source.rel[..source.rel.len() - 1];
        let parent = match self.existing(&join(parent_rel)) {
            Ok(parent) => parent,
            Err(response) => return response,
        };
        if destination.canon == parent.canon {
            return err(400, "It's already in that folder.");
        }
        if source.dir && !source.link && destination.canon.starts_with(&source.canon) {
            return err(400, "A folder can't be moved inside itself.");
        }
        let name = source.name().to_owned();
        if !Self::child_class(&destination, &name).writable() {
            return err(403, "That folder is read-only.");
        }
        let from = parent.canon.join(&name);
        let to = destination.canon.join(&name);
        self.finish_move(&from, &to, &destination.rel, &name)
    }

    fn finish_move(&self, from: &Path, to: &Path, folder: &[String], name: &str) -> Response {
        let affects = self.affects(&[from, to]);
        match names::rename_free(from, to) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return err(409, "That name is already taken there."),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return err(404, "That no longer exists."),
            Err(e) if e.raw_os_error() == Some(libc::EXDEV) => {
                return err(400, "That can't be moved to another storage area.");
            }
            Err(e) => return write_error("Couldn't move it", e),
        }
        log::info!("web app: moved {} → {}", from.display(), to.display());
        self.changed(affects);
        let mut rel = folder.to_vec();
        rel.push(name.to_owned());
        match self.existing(&join(&rel)) {
            Ok(moved) => Response::json(200, &self.entry_json(&moved)),
            Err(_) => Response::json(200, &json!({ "path": join(&rel) })),
        }
    }

    /// `DELETE /api/files?path=…[&confirm=2]`: KOReader and library folders
    /// need `confirm=2` (asked twice in the page), `428` otherwise.
    pub fn delete(&self, request: &Request) -> Response {
        let target = match self.movable(&request.param("path").unwrap_or_default()) {
            Ok(target) => target,
            Err(response) => return response,
        };
        if target.dir && !target.link && self.ask_twice(&target) && request.param("confirm").as_deref() != Some("2") {
            return Response::json(428, &json!({ "error": "This folder needs a second confirmation.", "twice": true }));
        }
        let parent_rel = &target.rel[..target.rel.len() - 1];
        let parent = match self.existing(&join(parent_rel)) {
            Ok(parent) => parent,
            Err(response) => return response,
        };
        // The entry itself, never what a link points to.
        let path = parent.canon.join(target.name());
        let affects = self.affects(&[&target.canon, &path]);
        let removed = if target.dir && !target.link { fs::remove_dir_all(&path) } else { fs::remove_file(&path) };
        // Even a partial delete changed things.
        self.changed(affects);
        match removed {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => return err(404, "That no longer exists."),
            Err(e) => {
                log::warn!("web app: deleting {}: {e}", path.display());
                return err(500, &format!("Couldn't delete it completely: {e}"));
            }
        }
        log::info!("web app: deleted {} (files)", path.display());
        self.activity.push(Activity::Deleted { name: target.name().to_owned() });
        Response::json(200, &json!({ "ok": true }))
    }
}

#[derive(Clone, Copy)]
struct Affects {
    library: bool,
    dictionaries: bool,
}

/// A file of that name is there: the page offers to replace it.
fn conflict(message: &str) -> Response {
    Response::json(409, &json!({ "error": message, "exists": true }))
}

fn read_json<T: serde::de::DeserializeOwned>(body: &mut Body<'_>) -> Result<T, Response> {
    let Ok(Some(bytes)) = body.read_small(8192) else {
        return Err(err(413, "Request too large."));
    };
    serde_json::from_slice(&bytes).map_err(|_| err(400, "Unexpected request."))
}

/// The simulator's Files root (VERSO_SIM_FILES, or a folder in the temporary
/// folder), never `/`. A new one gets a small Kindle-like layout.
#[cfg(not(feature = "kindle"))]
pub fn simulator_root() -> PathBuf {
    if let Some(root) = std::env::var_os("VERSO_SIM_FILES") {
        return PathBuf::from(root);
    }
    let root = std::env::temp_dir().join("verso-simulator-files");
    if !root.exists() {
        for dir in ["documents", "fonts", "screenshots", "koreader", "extensions/verso", "verso/data/dictionaries"] {
            let _ = fs::create_dir_all(root.join(dir));
        }
        let _ = fs::create_dir_all(root.join("verso/data/exports"));
        let _ = fs::write(root.join("verso/data/settings.json"), "{}\n");
        let _ = fs::write(root.join("verso/verso.log"), "");
    }
    root
}

#[cfg(test)]
#[path = "files_tests.rs"]
mod tests;
