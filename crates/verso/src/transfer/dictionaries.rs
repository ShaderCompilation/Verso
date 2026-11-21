//! The Dictionaries tab API (docs/web-app.md, "Dictionaries"): list installed
//! dictionaries, upload `.zip` (through a `.part` file), `.tar.gz`/`.tgz` and
//! `.tar.bz2`/`.tbz2` (streamed), or loose StarDict files through staging
//! batches, unpack `.syn.dz`, validate with `Info::read` and `companion`, and
//! delete native dictionaries after the dictionary service closes their files.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use super::Activity;
use super::books::{ActivitySink, FREE_SPACE_RESERVE, UploadGuard, create_part, err, read_error, write_error};
use super::device::FreeSpace;
use super::http::{BUFFER, Body, Request, Response};
use crate::dictionary::{self, Info, basename, companion, discover_catalog, is_gzip, sibling};
use crate::store::Store;

/// Maximum total unpacked bytes across one archive or staging batch (2 GiB).
pub const MAX_UNPACKED_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// Maximum archive entries or loose files in one upload/batch.
pub const MAX_ENTRIES: usize = 10_000;
/// Check free space every 1 MiB while unpacking so storage can't fill up.
const FREE_SPACE_CHECK_STEP: u64 = 1024 * 1024;
const MAX_FOLDER_BYTES: usize = 120;

const LOOSE_SUFFIXES: [&str; 8] = [".ifo", ".ifo.gz", ".idx", ".idx.gz", ".dict", ".dict.dz", ".syn", ".syn.dz"];
const COMPANION_EXTS: [&str; 8] = ["ifo", "ifo.gz", "idx", "idx.gz", "dict", "dict.dz", "syn", "syn.dz"];

#[derive(Clone, Copy, Debug)]
pub(crate) struct Limits {
    pub max_bytes: u64,
    pub max_entries: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self { max_bytes: MAX_UNPACKED_BYTES, max_entries: MAX_ENTRIES }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ArchiveKind {
    Zip,
    TarGz,
    TarBz2,
}

/// Removes a staging directory when dropped so failed, cancelled or completed
/// uploads never leave stray files behind.
struct StagingDir {
    path: PathBuf,
}

impl StagingDir {
    fn create(root: &Path, name: &str) -> io::Result<Self> {
        fs::create_dir_all(root)?;
        let path = root.join(name);
        fs::create_dir(&path)?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for StagingDir {
    fn drop(&mut self) {
        if let Err(e) = fs::remove_dir_all(&self.path)
            && e.kind() != io::ErrorKind::NotFound
        {
            log::warn!("web app: removing staging dir {}: {e}", self.path.display());
        }
    }
}

struct Batch {
    dir: PathBuf,
    files: usize,
    bytes: u64,
}

/// Everything the Dictionaries API handlers need.
pub struct Dictionaries {
    store: Store,
    free_space: FreeSpace,
    activity: Arc<dyn ActivitySink>,
    changed: Arc<AtomicBool>,
    staging_root: PathBuf,
    batches: Mutex<HashMap<String, Batch>>,
    next_id: AtomicU64,
    limits: Limits,
}

impl Dictionaries {
    pub fn new(store: Store, free_space: FreeSpace, activity: Arc<dyn ActivitySink>, changed: Arc<AtomicBool>) -> Self {
        Self::with_limits(store, free_space, activity, changed, Limits::default())
    }

    pub(crate) fn with_limits(
        store: Store,
        free_space: FreeSpace,
        activity: Arc<dyn ActivitySink>,
        changed: Arc<AtomicBool>,
        limits: Limits,
    ) -> Self {
        let staging_root = store.root().join("dictionary-staging");
        // Clean up any abandoned staging directories from a prior crash.
        let _ = fs::remove_dir_all(&staging_root);
        Self {
            store,
            free_space,
            activity,
            changed,
            staging_root,
            batches: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            limits,
        }
    }

    /// Removes all uncommitted loose-file staging batches (called on stop/drop).
    pub fn cleanup_batches(&self) {
        let drained: Vec<Batch> =
            self.batches.lock().unwrap_or_else(|e| e.into_inner()).drain().map(|(_, b)| b).collect();
        for batch in drained {
            let _ = fs::remove_dir_all(&batch.dir);
        }
        let _ = fs::remove_dir(&self.staging_root);
    }

    /// `GET /api/dictionaries`
    pub fn list(&self) -> Response {
        let folders = self.store.dictionary_folders();
        let catalog = match discover_catalog(&self.store.dictionary_dir(), &folders.folders, &[], &|| true) {
            Ok(catalog) => catalog,
            Err(e) => {
                log::warn!("web app: discovering dictionaries: {e:#}");
                return err(500, "Couldn't read installed dictionaries.");
            }
        };
        let items: Vec<Value> = catalog
            .dictionaries
            .iter()
            .map(|d| {
                let source = catalog.sources.get(d.source);
                let location = source.map_or_else(String::new, dictionary::Source::label);
                let native = source.is_some_and(|s| s.native);
                json!({
                    "id": d.id,
                    "name": d.name,
                    "word_count": d.word_count,
                    "location": location,
                    "relative": d.relative.to_string_lossy(),
                    "native": native,
                    "error": d.error,
                })
            })
            .collect();
        Response::json(200, &json!({ "dictionaries": items }))
    }

    /// Checks a dictionary archive upload before reading its body. Shared with
    /// `GET /api/dictionaries/check`.
    fn plan_archive_upload(&self, name_param: Option<String>, size: u64) -> Result<(String, ArchiveKind), Response> {
        let Some(name_param) = name_param else {
            return Err(err(400, "Missing archive file name."));
        };
        let kind = match archive_kind(&name_param) {
            Ok(kind) => kind,
            Err(msg) => return Err(err(400, msg)),
        };
        self.check_free_space(size)?;
        Ok((name_param, kind))
    }

    /// Checks a loose-file batch upload before reading its body. Shared with
    /// `GET /api/dictionaries/check?batch=…`.
    fn plan_batch_file_upload(
        &self,
        batch_id: &str,
        name_param: Option<String>,
        size: u64,
    ) -> Result<(String, PathBuf, u64), Response> {
        let Some(name_param) = name_param else {
            return Err(err(400, "Missing file name."));
        };
        let name = match validate_loose_name(&name_param) {
            Ok(name) => name,
            Err(msg) => return Err(err(400, msg)),
        };
        let (batch_dir, current_files, current_bytes) = {
            let batches = self.batches.lock().unwrap_or_else(|e| e.into_inner());
            let Some(batch) = batches.get(batch_id) else {
                return Err(err(404, "Dictionary upload batch not found."));
            };
            (batch.dir.clone(), batch.files, batch.bytes)
        };
        if current_files + 1 > self.limits.max_entries {
            return Err(err(413, "Too many files in this dictionary upload."));
        }
        if current_bytes.saturating_add(size) > self.limits.max_bytes {
            return Err(err(413, "Dictionary upload exceeds the 2 GB limit."));
        }
        self.check_free_space(size)?;
        Ok((name, batch_dir, current_bytes))
    }

    /// `GET /api/dictionaries/check?name=…&size=…[&batch=…]`: `200` if a `PUT`
    /// with these would be taken, or the refusal it would get.
    pub fn check_upload(&self, request: &Request) -> Response {
        let Some(size) = request.param("size").and_then(|s| s.parse::<u64>().ok()) else {
            return err(400, "Missing size.");
        };
        let result = if let Some(batch_id) = request.param("batch") {
            self.plan_batch_file_upload(&batch_id, request.param("name"), size).map(|_| ())
        } else {
            self.plan_archive_upload(request.param("name"), size).map(|_| ())
        };
        match result {
            Ok(()) => Response::json(200, &json!({ "ok": true })),
            Err(response) => response,
        }
    }

    /// `PUT /api/dictionaries?name=…`
    pub fn upload_archive(&self, request: &Request, body: &mut Body<'_>) -> Response {
        let (name_param, kind) = match self.plan_archive_upload(request.param("name"), request.content_length) {
            Ok(plan) => plan,
            Err(resp) => return resp,
        };

        let display_name = Path::new(&name_param)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| name_param.clone());
        let guard = UploadGuard::start(&*self.activity, display_name);

        let stage_id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let staging = match StagingDir::create(&self.staging_root, &format!("archive-{stage_id}")) {
            Ok(s) => s,
            Err(e) => return write_error("Couldn't prepare dictionary staging", e),
        };
        let unpacked = staging.path().join("unpacked");
        if let Err(e) = fs::create_dir(&unpacked) {
            return write_error("Couldn't prepare dictionary staging", e);
        }

        let mut tracker = UnpackTracker::new(self.limits, &self.free_space, staging.path());
        let unpack_result = match kind {
            ArchiveKind::Zip => self.unpack_zip_upload(staging.path(), &unpacked, body, &mut tracker),
            ArchiveKind::TarGz => unpack_tar(flate2::read::GzDecoder::new(body), &unpacked, &mut tracker),
            ArchiveKind::TarBz2 => unpack_tar(bzip2_rs::DecoderReader::new(body), &unpacked, &mut tracker),
        };
        if let Err(resp) = unpack_result {
            return resp;
        }

        self.install_staged(&unpacked, &mut tracker, Some(guard))
    }

    /// `POST /api/dictionaries/batch`
    pub fn create_batch(&self) -> Response {
        let id = format!("b{}", self.next_id.fetch_add(1, Ordering::Relaxed));
        if let Err(e) = fs::create_dir_all(&self.staging_root) {
            return write_error("Couldn't create dictionary batch", e);
        }
        let dir = self.staging_root.join(format!("batch-{id}"));
        if let Err(e) = fs::create_dir(&dir) {
            return write_error("Couldn't create dictionary batch", e);
        }
        self.batches.lock().unwrap_or_else(|e| e.into_inner()).insert(id.clone(), Batch { dir, files: 0, bytes: 0 });
        Response::json(201, &json!({ "id": id }))
    }

    /// `PUT /api/dictionaries/batch/<id>?name=…`
    pub fn upload_batch_file(&self, batch_id: &str, request: &Request, body: &mut Body<'_>) -> Response {
        let (name, batch_dir, current_bytes) =
            match self.plan_batch_file_upload(batch_id, request.param("name"), request.content_length) {
                Ok(plan) => plan,
                Err(resp) => return resp,
            };

        let guard = UploadGuard::start(&*self.activity, name.clone());
        let mut part = match create_part(&batch_dir, &name) {
            Ok(part) => part,
            Err(e) => return write_error("Couldn't start saving dictionary file", e),
        };
        let mut buf = vec![0u8; BUFFER];
        let mut written = 0u64;
        loop {
            match body.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    written = written.saturating_add(n as u64);
                    if current_bytes.saturating_add(written) > self.limits.max_bytes {
                        return err(413, "Dictionary upload exceeds the 2 GB limit.");
                    }
                    if let Err(e) = part.write_all(&buf[..n]) {
                        return write_error("Couldn't write dictionary file", e);
                    }
                }
                Err(e) => return read_error(e),
            }
        }
        if let Err(e) = part.sync_and_close() {
            return write_error("Couldn't finish writing dictionary file", e);
        }
        let target = batch_dir.join(&name);
        if let Err(e) = part.finish(&target) {
            return write_error("Couldn't save dictionary file", e);
        }

        {
            let mut batches = self.batches.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(batch) = batches.get_mut(batch_id) {
                batch.files += 1;
                batch.bytes = batch.bytes.saturating_add(written);
            } else {
                // Batch was removed/cleaned up while uploading.
                let _ = fs::remove_file(&target);
                return err(404, "Dictionary upload batch was cancelled.");
            }
        }
        // Clear the temporary Receiving row without adding a finished row yet;
        // commit_batch posts the installed Activity::Dictionary row.
        drop(guard);
        Response::json(200, &json!({ "ok": true, "name": name }))
    }

    /// `DELETE /api/dictionaries/batch/<id>`
    pub fn cancel_batch(&self, batch_id: &str) -> Response {
        let Some(batch) = self.batches.lock().unwrap_or_else(|e| e.into_inner()).remove(batch_id) else {
            return err(404, "Dictionary upload batch not found.");
        };
        let _ = fs::remove_dir_all(&batch.dir);
        let _ = fs::remove_dir(&self.staging_root);
        Response::json(200, &json!({ "ok": true }))
    }

    /// `POST /api/dictionaries/batch/<id>/commit`
    pub fn commit_batch(&self, batch_id: &str) -> Response {
        let Some(batch) = self.batches.lock().unwrap_or_else(|e| e.into_inner()).remove(batch_id) else {
            return err(404, "Dictionary upload batch not found.");
        };
        let staging = StagingDir { path: batch.dir };
        let mut tracker = UnpackTracker::new(self.limits, &self.free_space, staging.path());
        tracker.bytes = batch.bytes;
        tracker.entries = batch.files;
        self.install_staged(staging.path(), &mut tracker, None)
    }

    /// `DELETE /api/dictionaries?id=…`
    pub fn delete(&self, request: &Request) -> Response {
        let Some(id) = request.param("id").filter(|s| !s.is_empty()) else {
            return err(400, "Missing dictionary id.");
        };
        let folders = self.store.dictionary_folders();
        let catalog = match discover_catalog(&self.store.dictionary_dir(), &folders.folders, &[], &|| true) {
            Ok(catalog) => catalog,
            Err(e) => {
                log::warn!("web app: discovering dictionaries for delete: {e:#}");
                return err(500, "Couldn't read installed dictionaries.");
            }
        };
        let Some(dict) = catalog.dictionaries.into_iter().find(|d| d.id == id) else {
            return err(404, "Dictionary not found.");
        };
        let native = catalog.sources.get(dict.source).is_some_and(|s| s.native);
        if !native {
            return err(
                403,
                "Only dictionaries in Verso's folder can be deleted here. Use the Files tab for other folders.",
            );
        }

        // Ask the dictionary service to close any open index/definitions handles
        // and wait for confirmation before deleting files (important on FUSE).
        if let Some(service) = self.store.dictionary_service.get() {
            service.close_sync(&dict.id);
        }

        // Remove the dictionary's cached index file.
        dictionary::remove_cache(&dict.path, &self.store.dictionary_cache());

        // Remove the dictionary's companion files.
        let Some(base) = basename(&dict.path) else {
            return err(500, "Couldn't determine dictionary files to remove.");
        };
        for ext in COMPANION_EXTS {
            let p = sibling(&base, ext);
            if let Err(e) = fs::remove_file(&p)
                && e.kind() != io::ErrorKind::NotFound
            {
                log::warn!("web app: removing {}: {e}", p.display());
                return err(500, &format!("Couldn't delete dictionary file: {e}"));
            }
        }

        // If the parent directory inside data/dictionaries/ has no other
        // dictionaries, remove its `res/` folder and prune empty folders up to
        // data/dictionaries/.
        let dict_root = self.store.dictionary_dir();
        if let Some(parent) = dict.path.parent()
            && parent != dict_root
            && parent.starts_with(&dict_root)
            && !folder_has_other_dictionary(parent)
        {
            let res_dir = parent.join("res");
            if res_dir.is_dir() {
                let _ = fs::remove_dir_all(&res_dir);
            }
            prune_empty_dirs(parent, &dict_root);
        }

        self.changed.store(true, Ordering::SeqCst);
        self.activity.push(Activity::Deleted { name: dict.name.clone() });
        log::info!("web app: deleted dictionary {} ({})", dict.name, dict.id);
        Response::json(200, &json!({ "ok": true }))
    }

    fn check_free_space(&self, needed: u64) -> Result<(), Response> {
        let dir = self.store.dictionary_dir();
        let probe = if dir.exists() { dir } else { self.store.root() };
        let _ = fs::create_dir_all(&probe);
        match (self.free_space)(&probe) {
            Ok(free) if free < needed.saturating_add(FREE_SPACE_RESERVE) => {
                Err(err(507, "Not enough free space on the Kindle (50 MB is kept in reserve)."))
            }
            Ok(_) => Ok(()),
            Err(e) => {
                log::warn!("web app: reading free space for {}: {e}", probe.display());
                Err(err(500, "Couldn't check free space on the Kindle."))
            }
        }
    }

    fn unpack_zip_upload(
        &self,
        staging: &Path,
        unpacked: &Path,
        body: &mut Body<'_>,
        tracker: &mut UnpackTracker<'_>,
    ) -> Result<(), Response> {
        let mut part = create_part(staging, "upload.zip")
            .map_err(|e| write_error("Couldn't start saving dictionary archive", e))?;
        let mut buf = vec![0u8; BUFFER];
        loop {
            match body.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    part.write_all(&buf[..n]).map_err(|e| write_error("Couldn't write dictionary archive", e))?;
                }
                Err(e) => return Err(read_error(e)),
            }
        }
        part.sync_and_close().map_err(|e| write_error("Couldn't finish saving dictionary archive", e))?;

        let file = File::open(part.path()).map_err(|e| write_error("Couldn't open dictionary archive", e))?;
        let mut archive =
            zip::ZipArchive::new(file).map_err(|e| err(400, &format!("Not a valid .zip archive: {e}")))?;

        for i in 0..archive.len() {
            tracker.count_entry()?;
            let mut entry =
                archive.by_index(i).map_err(|e| err(400, &format!("Couldn't read entry from .zip archive: {e}")))?;
            let raw_name = entry.name().to_owned();
            let clean = validate_archive_path(&raw_name)?;
            if let Some(mode) = entry.unix_mode() {
                let file_type = mode & 0o170000;
                if file_type != 0 && file_type != 0o100000 && file_type != 0o040000 {
                    return Err(err(400, "Archive contains links or special files, which aren't allowed."));
                }
            }
            if clean.as_os_str().is_empty() || is_macos_leftover(&clean) {
                continue;
            }
            let dest = unpacked.join(&clean);
            if entry.is_dir() {
                fs::create_dir_all(&dest).map_err(|e| write_error("Couldn't unpack dictionary folder", e))?;
            } else {
                if let Some(parent) = dest.parent() {
                    fs::create_dir_all(parent).map_err(|e| write_error("Couldn't unpack dictionary folder", e))?;
                }
                let mut out = File::create(&dest).map_err(|e| write_error("Couldn't unpack dictionary file", e))?;
                copy_limited(&mut entry, &mut out, tracker)?;
            }
        }
        // `part` drops here and deletes the temporary .part zip file.
        drop(part);
        Ok(())
    }

    fn install_staged(
        &self,
        staged_root: &Path,
        tracker: &mut UnpackTracker<'_>,
        guard: Option<UploadGuard<'_>>,
    ) -> Response {
        let ifos = match find_staged_ifos(staged_root) {
            Ok(ifos) => ifos,
            Err(e) => return write_error("Couldn't inspect uploaded dictionary", e),
        };
        if ifos.is_empty() {
            return err(400, "Missing .ifo file. No StarDict dictionary was found.");
        }

        // First pass: unpack any `.syn.dz` and validate every dictionary with
        // `Info::read` and `companion` before moving anything into place.
        struct Ready {
            base: PathBuf,
            info: Info,
        }
        let mut ready = Vec::with_capacity(ifos.len());
        let mut dir_counts: HashMap<PathBuf, usize> = HashMap::new();

        for ifo_path in ifos {
            let Some(base) = basename(&ifo_path) else { continue };
            let syn_dz = sibling(&base, "syn.dz");
            let syn = sibling(&base, "syn");
            if syn_dz.is_file() && !syn.exists() {
                if let Err(resp) = unpack_syn_dz(&syn_dz, &syn, tracker) {
                    return resp;
                }
                let _ = fs::remove_file(&syn_dz);
            }
            let info = match Info::read(&ifo_path) {
                Ok(info) => info,
                Err(e) => return err(400, &format!("{e:#}")),
            };
            if let Err(e) = companion(&base, "idx", "idx.gz") {
                return err(400, &format!("{e:#}"));
            }
            if let Err(e) = companion(&base, "dict", "dict.dz") {
                return err(400, &format!("{e:#}"));
            }
            if let Some(parent) = base.parent() {
                *dir_counts.entry(parent.to_path_buf()).or_insert(0) += 1;
            }
            ready.push(Ready { base, info });
        }

        // Second pass: install each validated dictionary into its own safe,
        // clash-free folder under `data/dictionaries/`.
        let dict_root = self.store.dictionary_dir();
        if let Err(e) = fs::create_dir_all(&dict_root) {
            return write_error("Couldn't create dictionaries folder", e);
        }

        let mut installed = Vec::with_capacity(ready.len());
        let mut installed_names = Vec::with_capacity(ready.len());
        let mut installed_dirs: Vec<PathBuf> = Vec::with_capacity(ready.len());
        let mut guard = guard;
        // All or nothing: a failure removes the dictionaries already installed
        // from this upload, so a retry doesn't duplicate them.
        let roll_back = |dirs: &[PathBuf]| {
            for dir in dirs {
                let _ = fs::remove_dir_all(dir);
            }
        };

        for item in ready {
            let safe_name = safe_folder_name(&item.info.name);
            let (dest_dir, folder_name) = match claim_unique_folder(&dict_root, &safe_name) {
                Ok(pair) => pair,
                Err(e) => {
                    roll_back(&installed_dirs);
                    return write_error("Couldn't create dictionary folder", e);
                }
            };
            installed_dirs.push(dest_dir.clone());
            if let Err(e) = move_dictionary_files(&item.base, &dest_dir, &dir_counts) {
                roll_back(&installed_dirs);
                return write_error("Couldn't install dictionary files", e);
            }
            installed_names.push(item.info.name.clone());
            installed.push(json!({
                "name": item.info.name,
                "folder": folder_name,
                "word_count": item.info.count,
            }));
        }

        self.changed.store(true, Ordering::SeqCst);
        for name in installed_names {
            if let Some(g) = guard.take() {
                g.finish(Activity::Dictionary { name });
            } else {
                self.activity.push(Activity::Dictionary { name });
            }
        }
        Response::json(201, &json!({ "installed": installed }))
    }
}

impl Drop for Dictionaries {
    fn drop(&mut self) {
        self.cleanup_batches();
    }
}

struct UnpackTracker<'a> {
    limits: Limits,
    free_space: &'a FreeSpace,
    probe_dir: &'a Path,
    entries: usize,
    bytes: u64,
    bytes_since_check: u64,
}

impl<'a> UnpackTracker<'a> {
    fn new(limits: Limits, free_space: &'a FreeSpace, probe_dir: &'a Path) -> Self {
        Self { limits, free_space, probe_dir, entries: 0, bytes: 0, bytes_since_check: 0 }
    }

    fn count_entry(&mut self) -> Result<(), Response> {
        self.entries += 1;
        if self.entries > self.limits.max_entries {
            return Err(err(413, "Archive has too many entries (limit 10,000)."));
        }
        self.check_free()
    }

    fn add_bytes(&mut self, n: u64) -> Result<(), Response> {
        self.bytes = self.bytes.saturating_add(n);
        if self.bytes > self.limits.max_bytes {
            return Err(err(413, "Archive exceeds the 2 GB unpacked limit."));
        }
        self.bytes_since_check = self.bytes_since_check.saturating_add(n);
        if self.bytes_since_check >= FREE_SPACE_CHECK_STEP {
            self.bytes_since_check = 0;
            self.check_free()?;
        }
        Ok(())
    }

    fn check_free(&self) -> Result<(), Response> {
        match (self.free_space)(self.probe_dir) {
            Ok(free) if free < FREE_SPACE_RESERVE => {
                Err(err(507, "Not enough free space on the Kindle (50 MB is kept in reserve)."))
            }
            Ok(_) => Ok(()),
            Err(e) => {
                log::warn!("web app: reading free space while unpacking: {e}");
                Err(err(500, "Couldn't check free space on the Kindle."))
            }
        }
    }
}

fn unpack_tar<R: Read>(reader: R, unpacked: &Path, tracker: &mut UnpackTracker<'_>) -> Result<(), Response> {
    let mut archive = tar::Archive::new(reader);
    let entries = archive.entries().map_err(tar_read_error)?;
    for entry in entries {
        tracker.count_entry()?;
        let mut entry = entry.map_err(tar_read_error)?;
        let entry_type = entry.header().entry_type();
        if entry_type.is_pax_global_extensions()
            || entry_type.is_pax_local_extensions()
            || entry_type.is_gnu_longname()
            || entry_type.is_gnu_longlink()
        {
            continue;
        }
        if !entry_type.is_file() && !entry_type.is_dir() {
            return Err(err(400, "Archive contains links or special files, which aren't allowed."));
        }
        let path_bytes = entry.path_bytes();
        let raw_path =
            std::str::from_utf8(&path_bytes).map_err(|_| err(400, "Archive contains a non-UTF-8 file name."))?;
        let clean = validate_archive_path(raw_path)?;
        if clean.as_os_str().is_empty() || is_macos_leftover(&clean) {
            continue;
        }
        let dest = unpacked.join(&clean);
        if entry_type.is_dir() {
            fs::create_dir_all(&dest).map_err(|e| write_error("Couldn't unpack dictionary folder", e))?;
        } else {
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent).map_err(|e| write_error("Couldn't unpack dictionary folder", e))?;
            }
            let mut out = File::create(&dest).map_err(|e| write_error("Couldn't unpack dictionary file", e))?;
            copy_limited(&mut entry, &mut out, tracker)?;
        }
    }
    Ok(())
}

fn tar_read_error(e: io::Error) -> Response {
    match e.kind() {
        io::ErrorKind::Interrupted | io::ErrorKind::TimedOut => read_error(e),
        _ => err(400, &format!("Not a valid tar archive: {e}")),
    }
}

fn copy_limited<R: Read, W: Write>(
    reader: &mut R,
    writer: &mut W,
    tracker: &mut UnpackTracker<'_>,
) -> Result<(), Response> {
    let mut buf = vec![0u8; BUFFER];
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(n) => n,
            Err(e) if matches!(e.kind(), io::ErrorKind::Interrupted | io::ErrorKind::TimedOut) => {
                return Err(read_error(e));
            }
            Err(e) => return Err(err(400, &format!("Corrupt archive stream: {e}"))),
        };
        tracker.add_bytes(n as u64)?;
        writer.write_all(&buf[..n]).map_err(|e| write_error("Couldn't unpack dictionary file", e))?;
    }
}

fn unpack_syn_dz(src: &Path, dst: &Path, tracker: &mut UnpackTracker<'_>) -> Result<(), Response> {
    let file = File::open(src).map_err(|e| write_error("Couldn't open .syn.dz file", e))?;
    let mut decoder = flate2::read::GzDecoder::new(file);
    let mut out = File::create(dst).map_err(|e| write_error("Couldn't unpack .syn.dz file", e))?;
    copy_limited(&mut decoder, &mut out, tracker).inspect_err(|_| {
        let _ = fs::remove_file(dst);
    })?;
    Ok(())
}

fn archive_kind(name: &str) -> Result<ArchiveKind, &'static str> {
    let lower = name.trim().to_ascii_lowercase();
    if lower.ends_with(".zip") {
        Ok(ArchiveKind::Zip)
    } else if lower.ends_with(".tar.gz") || lower.ends_with(".tgz") {
        Ok(ArchiveKind::TarGz)
    } else if lower.ends_with(".tar.bz2") || lower.ends_with(".tbz2") {
        Ok(ArchiveKind::TarBz2)
    } else if lower.ends_with(".tar.xz") || lower.ends_with(".txz") || lower.ends_with(".7z") || lower.ends_with(".rar")
    {
        Err("Unsupported archive format. Unpack it first, or re-pack it as .zip, .tar.gz or .tar.bz2.")
    } else {
        Err("Send a .zip, .tar.gz or .tar.bz2 archive, or choose loose StarDict files.")
    }
}

fn validate_loose_name(raw: &str) -> Result<String, &'static str> {
    let trimmed = raw.trim();
    if trimmed.is_empty()
        || trimmed == "."
        || trimmed == ".."
        || trimmed.starts_with('.')
        || trimmed.contains(['/', '\\'])
        || trimmed.chars().any(char::is_control)
    {
        return Err("Invalid dictionary file name.");
    }
    if trimmed.contains(WINDOWS_UNSAFE) {
        return Err("File names can't contain : * ? \" < > |, which a Windows computer can't copy.");
    }
    let lower = trimmed.to_ascii_lowercase();
    let Some(ext) = LOOSE_SUFFIXES.iter().find(|ext| lower.ends_with(**ext)) else {
        return Err("Only StarDict files (.ifo, .idx, .dict, .syn and their .gz/.dz forms) can be uploaded here.");
    };
    if trimmed.len() > 235 {
        return Err("That file name is too long.");
    }
    // The installer finds files by their lowercase suffix.
    let stem = &trimmed[..trimmed.len() - ext.len()];
    Ok(format!("{stem}{ext}"))
}

/// Characters a Windows computer can't show or copy in a file name (the
/// rest of `library::files::check_name`'s set; `/` and `\\` are rejected earlier).
const WINDOWS_UNSAFE: [char; 7] = [':', '*', '?', '"', '<', '>', '|'];

/// Lowercases a StarDict file's suffix (`X.IFO` → `X.ifo`), which the
/// installer matches case-sensitively. Other files are left alone.
fn normalize_suffix(path: PathBuf) -> PathBuf {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else { return path };
    let lower = name.to_ascii_lowercase();
    match LOOSE_SUFFIXES.iter().find(|ext| lower.ends_with(**ext)) {
        Some(ext) => path.with_file_name(format!("{}{ext}", &name[..name.len() - ext.len()])),
        None => path,
    }
}

/// Validates an archive entry path against absolute paths, `..`, backslashes
/// and NUL bytes, returning a clean relative path.
pub(crate) fn validate_archive_path(raw: &str) -> Result<PathBuf, Response> {
    if raw.contains('\0') {
        return Err(err(400, "Archive contains an invalid path."));
    }
    if raw.starts_with('/') || raw.starts_with('\\') {
        return Err(err(400, "Archive contains an absolute path, which isn't allowed."));
    }
    // Reject Windows drive letters like `C:...` or backslash path escapes.
    if raw.as_bytes().get(1) == Some(&b':') && raw.as_bytes()[0].is_ascii_alphabetic() {
        return Err(err(400, "Archive contains an absolute path, which isn't allowed."));
    }
    if raw.contains('\\') {
        return Err(err(400, "Archive contains an invalid path separator."));
    }
    let mut clean = PathBuf::new();
    for component in Path::new(raw).components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => {
                let text = part.to_string_lossy();
                if text.contains(WINDOWS_UNSAFE) {
                    return Err(err(
                        400,
                        &format!(
                            "Archive file name {text:?} has a character a Windows computer can't copy (: * ? \" < > |)."
                        ),
                    ));
                }
                clean.push(part)
            }
            Component::ParentDir => {
                return Err(err(400, "Archive contains a path escape ('..'), which isn't allowed."));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(err(400, "Archive contains an absolute path, which isn't allowed."));
            }
        }
    }
    Ok(normalize_suffix(clean))
}

fn is_macos_leftover(rel: &Path) -> bool {
    for component in rel.components() {
        if let Component::Normal(part) = component {
            let s = part.to_string_lossy();
            if s == "__MACOSX" || s == ".DS_Store" || s.starts_with("._") {
                return true;
            }
        }
    }
    false
}

/// Finds all `.ifo` and `.ifo.gz` files in `root` (skipping `res/`), preferring
/// `.ifo` over `.ifo.gz` when both exist for the same basename.
fn find_staged_ifos(root: &Path) -> io::Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let ft = entry.file_type()?;
            let path = entry.path();
            if ft.is_dir() {
                if entry.file_name() == "res" {
                    continue;
                }
                stack.push(path);
            } else if ft.is_file()
                && let Some(base) = basename(&path)
            {
                let plain = sibling(&base, "ifo");
                if is_gzip(&path) && plain.is_file() {
                    continue;
                }
                found.push(path);
            }
        }
    }
    found.sort();
    Ok(found)
}

/// Converts a `.ifo` `bookname` into a FAT/USB-safe folder name using the
/// library's character rules.
pub(crate) fn safe_folder_name(bookname: &str) -> String {
    let mut name: String = bookname
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .map(|c| if c.is_control() || r#"\/:*?"<>|"#.contains(c) { '_' } else { c })
        .collect();
    name = name.trim_matches(|c: char| c == '.' || c.is_whitespace()).to_owned();
    while name.len() > MAX_FOLDER_BYTES {
        name.pop();
    }
    name = name.trim_matches(|c: char| c == '.' || c.is_whitespace()).to_owned();
    if name.to_ascii_lowercase().ends_with(".sdr") {
        name.push('_');
    }
    if name.is_empty() { "Dictionary".into() } else { name }
}

fn claim_unique_folder(dict_root: &Path, base_name: &str) -> io::Result<(PathBuf, String)> {
    let first = dict_root.join(base_name);
    match fs::create_dir(&first) {
        Ok(()) => return Ok((first, base_name.to_owned())),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    for n in 2u32..=1000 {
        let name = format!("{base_name} ({n})");
        let candidate = dict_root.join(&name);
        match fs::create_dir(&candidate) {
            Ok(()) => return Ok((candidate, name)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(io::ErrorKind::AlreadyExists, "couldn't pick a free dictionary folder name"))
}

fn move_dictionary_files(base: &Path, dest_dir: &Path, dir_counts: &HashMap<PathBuf, usize>) -> io::Result<()> {
    let mut moved_any = false;
    for ext in COMPANION_EXTS {
        let src = sibling(base, ext);
        if src.is_file() {
            let file_name = src.file_name().ok_or_else(|| io::Error::other("missing file name"))?;
            fs::rename(&src, dest_dir.join(file_name))?;
            moved_any = true;
        }
    }
    if !moved_any {
        return Err(io::Error::new(io::ErrorKind::NotFound, "no dictionary files to install"));
    }
    if let Some(parent) = base.parent() {
        let res_src = parent.join("res");
        if res_src.is_dir() {
            let res_dst = dest_dir.join("res");
            let shared = dir_counts.get(parent).copied().unwrap_or(1) > 1;
            // A folder shared by several dictionaries is copied to each; a
            // folder of its own is moved, or copied if it can't be.
            if shared || fs::rename(&res_src, &res_dst).is_err() {
                copy_dir_recursive(&res_src, &res_dst)?;
            }
        }
    }
    Ok(())
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ft = entry.file_type()?;
        let to = dst.join(entry.file_name());
        if ft.is_dir() {
            copy_dir_recursive(&entry.path(), &to)?;
        } else if ft.is_file() {
            fs::copy(entry.path(), to)?;
        }
    }
    Ok(())
}

fn folder_has_other_dictionary(dir: &Path) -> bool {
    let Ok(entries) = fs::read_dir(dir) else { return false };
    let mut seen = HashSet::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file()
            && let Some(base) = basename(&path)
        {
            seen.insert(base);
        }
    }
    !seen.is_empty()
}

fn prune_empty_dirs(start: &Path, stop_at: &Path) {
    let mut current = start;
    while current != stop_at && current.starts_with(stop_at) {
        if fs::remove_dir(current).is_err() {
            break;
        }
        let Some(parent) = current.parent() else { break };
        current = parent;
    }
}

#[cfg(test)]
#[path = "dictionaries_tests.rs"]
mod tests;
