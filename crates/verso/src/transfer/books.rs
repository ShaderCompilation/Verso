//! The Books tab API (docs/web-app.md, "Books"): list, search, upload,
//! download, delete and New folder. Uploads check `.epub` and the zip header
//! early, stream into a `.part` file while hashing SHA-256 in 64 KB chunks,
//! validate with `library::epub`, resolve name clashes as `Name (2).epub`, and
//! insert into the library worker via `PathChange::Add` without a scan pass.

use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::io::{self, Cursor, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::Activity;
use super::device::FreeSpace;
use super::http::{BUFFER, Body, Request, Response};
use super::server::PartFile;
use crate::annotations::{self, Collection, Manifest, export};
use crate::library::collections::{self, Collections};
use crate::library::{self, Book, Cover, LibraryIndex, PathChange, epub, files, search};
use crate::store::{BookState, Store, book_id};

/// Free space kept in reserve before writing an uploaded book (50 MiB).
pub const FREE_SPACE_RESERVE: u64 = 50 * 1024 * 1024;

const ZIP_HEADER: &[u8; 4] = b"PK\x03\x04";
const WINDOWS_UNSAFE: [char; 7] = [':', '*', '?', '"', '<', '>', '|'];
const MAX_FILENAME_BYTES: usize = 235;

/// Reports uploads and deletes to the Kindle screen's activity list.
pub trait ActivitySink: Send + Sync {
    fn begin_receiving(&self, name: String) -> u64;
    fn finish_receiving(&self, id: u64, received: Option<Activity>);
    fn push(&self, activity: Activity);
}

/// Keeps a `Receiving` row on the Kindle screen until the upload finishes,
/// fails or is cancelled.
pub(super) struct UploadGuard<'a> {
    sink: &'a dyn ActivitySink,
    id: Option<u64>,
}

impl<'a> UploadGuard<'a> {
    pub(super) fn start(sink: &'a dyn ActivitySink, name: String) -> Self {
        Self { sink, id: Some(sink.begin_receiving(name)) }
    }

    pub(super) fn finish(mut self, received: Activity) {
        if let Some(id) = self.id.take() {
            self.sink.finish_receiving(id, Some(received));
        }
    }
}

impl Drop for UploadGuard<'_> {
    fn drop(&mut self) {
        if let Some(id) = self.id.take() {
            self.sink.finish_receiving(id, None);
        }
    }
}

/// Everything the Books API handlers need.
pub struct Books {
    store: Store,
    library: library::worker::Worker,
    free_space: FreeSpace,
    activity: Arc<dyn ActivitySink>,
}

impl Books {
    pub fn new(
        store: Store,
        library: library::worker::Worker,
        free_space: FreeSpace,
        activity: Arc<dyn ActivitySink>,
    ) -> Self {
        Self { store, library, free_space, activity }
    }

    /// `GET /api/books[?q=…]`
    pub fn list(&self, request: &Request) -> Response {
        let roots = self.store.library_folders();
        let index = self.library.current();
        let query = request.param("q").map(|q| search::fold(q.trim())).unwrap_or_default();

        let (default_folder, folders) = destination_folders(&index, &roots);
        let states = load_book_states(&self.store, &index);
        let annotated = annotated_book_ids(&self.store);

        let mut books: Vec<&Book> = index.books.iter().filter(|b| b.available).collect();
        books.sort_by(|a, b| library::compare_books(a, b));

        let mut out = Vec::with_capacity(books.len());
        for book in books {
            let folder = book.path.parent().unwrap_or_else(|| Path::new(""));
            let group = group_label(folder, &roots);
            let filename = book.path.file_name().unwrap_or_default().to_string_lossy();
            if !query.is_empty() && !matches_query(book, &filename, &group, &query) {
                continue;
            }
            let state = states.get(&book.id);
            let ann_id = annotation_id_of(book);
            let highlight_count = annotated.get(ann_id).copied().unwrap_or(0);
            out.push(book_json(book, folder, &group, &filename, state, highlight_count));
        }

        let folder_list: Vec<Value> = folders
            .into_iter()
            .map(|(path, label)| json!({ "path": path.to_string_lossy(), "label": label }))
            .collect();

        Response::json(
            200,
            &json!({
                "default_folder": default_folder.map(|p| p.to_string_lossy().into_owned()),
                "folders": folder_list,
                "books": out,
            }),
        )
    }

    /// Books tab changes save the library index and reading state at once,
    /// so after a developer edited Verso's live data they wait for Restart
    /// to apply: they would overwrite the edits. `409`, with `restart`.
    fn locked(&self) -> Option<Response> {
        self.store.live_data_edited().then(|| {
            Response::json(
                409,
                &json!({ "error": "Restart Verso to apply your edits first: tap Done on the Kindle.", "restart": true }),
            )
        })
    }

    /// Checks a book upload before any of its body is read: lock state,
    /// destination folder, filename and free space for `size` bytes. Shared
    /// with `GET /api/books/check`, which the page asks first so a refusal
    /// before a large body is read doesn't surface as a reset connection.
    fn plan_upload(
        &self,
        folder_param: Option<String>,
        name_param: Option<String>,
        size: u64,
    ) -> Result<(Vec<PathBuf>, PathBuf, String), Response> {
        if let Some(locked) = self.locked() {
            return Err(locked);
        }
        let Some(folder_param) = folder_param else {
            return Err(err(400, "Choose a destination folder."));
        };
        let Some(name_param) = name_param else {
            return Err(err(400, "Missing file name."));
        };
        let roots = self.store.library_folders();
        let dest_dir = match validate_destination(&folder_param, &roots) {
            Ok(dir) => dir,
            Err((status, msg)) => return Err(err(status, &msg)),
        };
        let name = match sanitize_epub_name(&name_param) {
            Ok(name) => name,
            Err(msg) => return Err(err(400, msg)),
        };
        match (self.free_space)(&dest_dir) {
            Ok(free) if free < size.saturating_add(FREE_SPACE_RESERVE) => {
                return Err(err(507, "Not enough free space on the Kindle (50 MB is kept in reserve)."));
            }
            Ok(_) => {}
            Err(e) => {
                log::warn!("web app: reading free space for {}: {e}", dest_dir.display());
                return Err(err(500, "Couldn't check free space on the Kindle."));
            }
        }
        Ok((roots, dest_dir, name))
    }

    /// `GET /api/books/check?folder=…&name=…&size=…`: `200` if a `PUT` with
    /// these would be taken, or the refusal it would get.
    pub fn check_upload(&self, request: &Request) -> Response {
        let Some(size) = request.param("size").and_then(|s| s.parse::<u64>().ok()) else {
            return err(400, "Missing size.");
        };
        match self.plan_upload(request.param("folder"), request.param("name"), size) {
            Ok(_) => Response::json(200, &json!({ "ok": true })),
            Err(response) => response,
        }
    }

    /// `PUT /api/books?folder=…&name=…`
    pub fn upload(&self, request: &Request, body: &mut Body<'_>) -> Response {
        let (roots, dest_dir, name) =
            match self.plan_upload(request.param("folder"), request.param("name"), request.content_length) {
                Ok(plan) => plan,
                Err(response) => return response,
            };

        // Check the zip header before touching storage or showing a Receiving row.
        let mut buf = vec![0u8; BUFFER];
        let first_len = match read_first_chunk(body, &mut buf) {
            Ok(n) => n,
            Err(e) => return read_error(e),
        };
        if first_len < ZIP_HEADER.len() || &buf[..ZIP_HEADER.len()] != ZIP_HEADER {
            return err(400, "This file isn't a valid EPUB. Use the Files tab for other files.");
        }

        let display_stem = Path::new(&name).file_stem().unwrap_or_default().to_string_lossy().into_owned();
        let guard = UploadGuard::start(&*self.activity, display_stem);

        // A hidden `.verso-upload*.part` name, so a part file left by a crash
        // or power loss is recognized and swept like the Files tab's.
        super::files::sweep_stale_parts(&dest_dir);
        let mut part = match create_part(&dest_dir, super::files::UPLOAD_PART) {
            Ok(part) => part,
            Err(e) => return write_error("Couldn't start saving the book", e),
        };
        let mut hasher = Sha256::new();
        if let Err(e) = part.write_all(&buf[..first_len]) {
            return write_error("Couldn't write the book", e);
        }
        hasher.update(&buf[..first_len]);
        loop {
            match body.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if let Err(e) = part.write_all(&buf[..n]) {
                        return write_error("Couldn't write the book", e);
                    }
                    hasher.update(&buf[..n]);
                }
                Err(e) => return read_error(e),
            }
        }
        if let Err(e) = part.sync_and_close() {
            return write_error("Couldn't finish writing the book", e);
        }

        // Validate the EPUB the exact same way the library scanner does.
        let mut metadata = match epub::read_metadata(part.path()) {
            Ok(metadata) => metadata,
            Err(e) => return err(400, &format!("Not a valid EPUB: {e:#}")),
        };

        // Pick a free name (Name (2).epub on clash) and rename without overwriting.
        let target = match finish_book_part(&mut part, &dest_dir, &name) {
            Ok(target) => target,
            Err(e) => return write_error("Couldn't save the book", e),
        };
        if metadata.title.is_empty() {
            metadata.title = target.file_stem().unwrap_or_default().to_string_lossy().into_owned();
        }
        let stamp =
            match std::fs::metadata(&target).map_err(anyhow::Error::from).and_then(|m| library::worker::stamp(&m)) {
                Ok(stamp) => stamp,
                Err(e) => {
                    log::warn!("web app: stat {}: {e:#}", target.display());
                    return err(500, "Couldn't read the saved book.");
                }
            };
        let legacy_id = book_id(&target).unwrap_or_default();
        let fingerprint = format!("{:x}", hasher.finalize());

        let incoming = Book {
            path: collections::normalize(&target),
            id: String::new(),
            annotation_id: String::new(),
            legacy_id,
            fingerprint,
            fingerprint_attempted: true,
            stamp,
            metadata,
            error: None,
            cover: Cover::Pending,
            available: true,
            metadata_pending: false,
        };
        let Some(book) = self.library.add_book(incoming) else {
            return err(503, "Wi-Fi transfer is stopping.");
        };

        let activity_folder = activity_folder_label(&dest_dir, &roots);
        guard.finish(Activity::Received { name: book.metadata.title.clone(), folder: activity_folder });

        let group = group_label(&dest_dir, &roots);
        let filename = book.path.file_name().unwrap_or_default().to_string_lossy();
        log::info!("web app: received {} → {} (id {})", name, book.path.display(), book.id);
        Response::json(201, &book_json(&book, &dest_dir, &group, &filename, None, 0))
    }

    /// `POST /api/books/mkdir`
    pub fn mkdir(&self, body: &mut Body<'_>) -> Response {
        if let Some(locked) = self.locked() {
            return locked;
        }
        let Ok(Some(bytes)) = body.read_small(4096) else {
            return err(413, "Request too large.");
        };
        #[derive(serde::Deserialize)]
        struct Input {
            parent: String,
            name: String,
        }
        let Ok(input) = serde_json::from_slice::<Input>(&bytes) else {
            return err(400, "Expected {\"parent\": \"…\", \"name\": \"…\"}.");
        };
        let roots = self.store.library_folders();
        let parent = match validate_destination(&input.parent, &roots) {
            Ok(dir) => dir,
            Err((status, msg)) => return err(status, &msg),
        };
        let created = match files::create_folder(&parent, &input.name) {
            Ok(path) => collections::normalize(&path),
            Err(e) => {
                let msg = format!("{e:#}");
                let status = if msg.contains("already taken") { 409 } else { 400 };
                return err(status, &msg);
            }
        };
        self.library.apply_sync(vec![PathChange::AddEmpty(created.clone())]);
        Response::json(
            201,
            &json!({
                "path": created.to_string_lossy(),
                "label": folder_label(&created, &roots),
            }),
        )
    }

    /// `GET /api/books/<id>`
    pub fn download(&self, id: &str) -> Response {
        let index = self.library.current();
        let Some(book) = index.books.iter().find(|b| b.available && b.id == id) else {
            return err(404, "Book not found.");
        };
        let (file, len) = match super::files::open_regular(&book.path) {
            Ok(Some(opened)) => opened,
            Ok(None) => return err(400, "Only regular files can be downloaded."),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return err(404, "Book file no longer exists."),
            Err(e) => {
                log::warn!("web app: opening {} for download: {e}", book.path.display());
                return err(500, "Couldn't open the book.");
            }
        };
        let name =
            book.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| format!("{id}.epub"));
        Response::file(file, len, &name)
    }

    /// `DELETE /api/books/<id>`
    pub fn delete(&self, id: &str) -> Response {
        if let Some(locked) = self.locked() {
            return locked;
        }
        let index = self.library.current();
        let Some(book) = index.books.iter().find(|b| b.available && b.id == id).cloned() else {
            return err(404, "Book not found.");
        };
        let deletion = match files::delete_book(&book.id, &book.path, &self.store) {
            Ok(deletion) => deletion,
            Err(e) => {
                log::warn!("web app: deleting {}: {e:#}", book.path.display());
                return err(500, &format!("Couldn't delete the book: {e:#}"));
            }
        };
        self.library.apply_sync(deletion.changes);
        self.activity.push(Activity::Deleted { name: book.metadata.title.clone() });
        let warning = deletion
            .sidecar_error
            .map(|e| format!("Deleted “{}”, but couldn't remove its KOReader data: {e:#}", book.metadata.title));
        Response::json(200, &json!({ "ok": true, "warning": warning }))
    }

    /// `GET /api/highlights/<id>`: one book's highlights, bookmarks and notes as UTF-8
    /// Markdown, in saved order (looked up by `annotation_id`).
    pub fn highlights(&self, id: &str) -> Response {
        let index = self.library.current();
        let Some(book) = index.books.iter().find(|b| b.available && b.id == id) else {
            return err(404, "Book not found.");
        };
        let ann_id = annotation_id_of(book);
        let collection = match Collection::load(&self.store, ann_id) {
            Ok(c) => c,
            Err(e) => {
                log::warn!("web app: reading highlights for {}: {e:#}", book.id);
                return err(500, "Couldn't read this book's highlights.");
            }
        };
        if collection.records.is_empty() {
            return err(404, "This book has no highlights.");
        }
        let exp = export::Export {
            title: book.metadata.title.clone(),
            author: book.metadata.authors.clone(),
            records: collection.records,
            // No layout here: bookmarks are listed without a page.
            pages: vec![],
        };
        let md = export::markdown(&exp, &annotations::created_now());
        let name = export::file_name(&exp.title, ann_id);
        Response::download(md.into_bytes(), &name)
    }

    /// `GET /api/highlights.zip`: a zip of every library book's highlights as
    /// Markdown, in saved order. Highlights of deleted books are omitted.
    pub fn highlights_zip(&self) -> Response {
        let index = self.library.current();
        let mut books: Vec<&Book> = index.books.iter().filter(|b| b.available).collect();
        books.sort_by(|a, b| library::compare_books(a, b));

        let exported = annotations::created_now();
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        let mut used_names = HashSet::new();
        let mut count = 0usize;

        for book in books {
            let ann_id = annotation_id_of(book);
            let collection = match Collection::load(&self.store, ann_id) {
                Ok(c) => c,
                Err(e) => {
                    log::warn!("web app: skipping unreadable highlights for {}: {e:#}", book.id);
                    continue;
                }
            };
            if collection.records.is_empty() {
                continue;
            }
            let exp = export::Export {
                title: book.metadata.title.clone(),
                author: book.metadata.authors.clone(),
                records: collection.records,
                // No layout here: bookmarks are listed without a page.
                pages: vec![],
            };
            let md = export::markdown(&exp, &exported);
            let mut entry_name = export::file_name(&exp.title, ann_id);
            if !used_names.insert(entry_name.clone()) {
                let stem = entry_name.strip_suffix(".md").unwrap_or(&entry_name).to_owned();
                let mut n = 2u32;
                loop {
                    let candidate = format!("{stem} ({n}).md");
                    if used_names.insert(candidate.clone()) {
                        entry_name = candidate;
                        break;
                    }
                    n += 1;
                }
            }
            if let Err(e) = writer
                .start_file(&entry_name, options)
                .and_then(|()| writer.write_all(md.as_bytes()).map_err(Into::into))
            {
                log::warn!("web app: writing highlights zip entry {entry_name}: {e}");
                return err(500, "Couldn't build the highlights archive.");
            }
            count += 1;
        }
        if count == 0 {
            return err(404, "No highlights to download.");
        }
        let cursor = match writer.finish() {
            Ok(c) => c,
            Err(e) => {
                log::warn!("web app: finishing highlights zip: {e}");
                return err(500, "Couldn't build the highlights archive.");
            }
        };
        Response::download(cursor.into_inner(), "verso-highlights.zip")
    }
}

fn annotation_id_of(book: &Book) -> &str {
    if book.annotation_id.is_empty() { &book.id } else { &book.annotation_id }
}

pub(super) fn err(status: u16, message: &str) -> Response {
    Response::json(status, &json!({ "error": message }))
}

pub(super) fn read_error(e: io::Error) -> Response {
    match e.kind() {
        io::ErrorKind::Interrupted => err(503, "Wi-Fi transfer is stopping."),
        io::ErrorKind::TimedOut => err(408, "The upload timed out."),
        _ => err(400, "The upload didn't finish."),
    }
}

pub(super) fn write_error(context: &str, e: io::Error) -> Response {
    if e.raw_os_error() == Some(libc::ENOSPC) {
        return err(507, "Not enough free space on the Kindle.");
    }
    log::warn!("web app: {context}: {e}");
    err(500, &format!("{context}: {e}"))
}

/// Reads the first chunk from `body`, looping until at least 4 bytes arrive or
/// EOF so a split TCP write can't fail the zip header check.
fn read_first_chunk(body: &mut Body<'_>, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < ZIP_HEADER.len() {
        let n = body.read(&mut buf[filled..])?;
        if n == 0 {
            break;
        }
        filled += n;
    }
    Ok(filled)
}

pub(super) fn create_part(dir: &Path, name: &str) -> io::Result<PartFile> {
    let first = dir.join(format!("{name}.part"));
    match PartFile::create(first) {
        Ok(part) => return Ok(part),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    for n in 2u32..=64 {
        let candidate = dir.join(format!("{name}.{n}.part"));
        match PartFile::create(candidate) {
            Ok(part) => return Ok(part),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(io::ErrorKind::AlreadyExists, "too many simultaneous uploads with the same name"))
}

/// Renames `part` to `name` in `dir`, or `Name (2).epub`, `Name (3).epub`, …
/// on a clash (including an orphan `.sdr` sidecar), never overwriting.
fn finish_book_part(part: &mut PartFile, dir: &Path, name: &str) -> io::Result<PathBuf> {
    let plain = dir.join(name);
    for _ in 0..32 {
        let candidate = if files::target_free(&plain, true).map_err(io::Error::other)? {
            plain.clone()
        } else {
            files::next_free_name(dir, OsStr::new(name), true).map_err(io::Error::other)?
        };
        match part.try_finish_noreplace(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(io::ErrorKind::AlreadyExists, "couldn't pick a free file name"))
}

/// Validates that `folder` is an existing directory inside one of the
/// configured library roots, with no `..`, symlink escapes, dotfiles or `.sdr`
/// segments. Returns the normalized path under the configured root.
pub(crate) fn validate_destination(folder: &str, roots: &[PathBuf]) -> Result<PathBuf, (u16, String)> {
    let folder = folder.trim();
    if folder.is_empty() || folder.contains('\0') {
        return Err((400, "Choose a destination folder.".into()));
    }
    let raw = Path::new(folder);
    if !raw.is_absolute() || raw.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err((403, "Choose a folder inside your library.".into()));
    }
    let canon = match std::fs::canonicalize(raw) {
        Ok(c) => c,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Err((404, "That folder doesn't exist.".into())),
        Err(_) => return Err((400, "Couldn't open that folder.".into())),
    };
    if !canon.is_dir() {
        return Err((400, "Choose a folder.".into()));
    }
    let roots = collections::distinct_roots(roots);
    for root in &roots {
        let Ok(canon_root) = std::fs::canonicalize(root) else { continue };
        let Ok(rel) = canon.strip_prefix(&canon_root) else { continue };
        if has_hidden_or_sidecar(rel) {
            return Err((403, "That folder isn't part of the library.".into()));
        }
        // Also check the lexical components under the root so a hidden symlink
        // pointing at a visible folder is refused too.
        if let Ok(raw_rel) = collections::normalize(raw).strip_prefix(root)
            && has_hidden_or_sidecar(raw_rel)
        {
            return Err((403, "That folder isn't part of the library.".into()));
        }
        return Ok(collections::normalize(&root.join(rel)));
    }
    Err((403, "Choose a folder inside your library.".into()))
}

fn has_hidden_or_sidecar(rel: &Path) -> bool {
    rel.components().any(|c| {
        let name = c.as_os_str().to_string_lossy();
        name.starts_with('.') || name.to_ascii_lowercase().ends_with(".sdr")
    })
}

/// Checks that an uploaded book name is a safe `.epub` filename (no path
/// separators, dotfiles or control chars; Windows-reserved punctuation is
/// replaced so the Kindle stays readable over USB).
pub(crate) fn sanitize_epub_name(raw: &str) -> Result<String, &'static str> {
    let trimmed = raw.trim();
    if trimmed.is_empty()
        || trimmed == "."
        || trimmed == ".."
        || trimmed.starts_with('.')
        || trimmed.contains(['/', '\\'])
        || trimmed.chars().any(char::is_control)
    {
        return Err("Invalid file name.");
    }
    let ext_ok = Path::new(trimmed).extension().is_some_and(|e| e.eq_ignore_ascii_case("epub"));
    if !ext_ok {
        return Err("Only EPUB books can be added here. Use the Files tab for other files.");
    }
    let cleaned: String = trimmed.chars().map(|c| if WINDOWS_UNSAFE.contains(&c) { '_' } else { c }).collect();
    let stem = Path::new(&cleaned).file_stem().and_then(OsStr::to_str).unwrap_or_default().trim();
    if stem.is_empty() || stem.starts_with('.') || stem.to_ascii_lowercase().ends_with(".sdr") {
        return Err("Invalid file name.");
    }
    if cleaned.len() > MAX_FILENAME_BYTES {
        return Err("That file name is too long.");
    }
    Ok(cleaned)
}

fn root_labels(roots: &[PathBuf]) -> HashMap<PathBuf, String> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for root in roots {
        *counts.entry(collections::name(root)).or_insert(0) += 1;
    }
    roots
        .iter()
        .map(|root| {
            let base = collections::name(root);
            let label = if counts.get(&base).copied().unwrap_or(0) > 1 {
                root.parent()
                    .map(collections::name)
                    .filter(|p| !p.is_empty() && p != "/")
                    .map_or_else(|| base.clone(), |p| format!("{p} / {base}"))
            } else {
                base
            };
            (root.clone(), label)
        })
        .collect()
}

fn rel_joined(rel: &Path) -> String {
    rel.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join(" / ")
}

fn folder_label(folder: &Path, roots: &[PathBuf]) -> String {
    let roots = collections::distinct_roots(roots);
    let labels = root_labels(&roots);
    for root in &roots {
        if let Ok(rel) = folder.strip_prefix(root) {
            let root_label = labels.get(root).cloned().unwrap_or_else(|| collections::name(root));
            if rel.as_os_str().is_empty() {
                return root_label;
            }
            let tail = rel_joined(rel);
            return if roots.len() == 1 { tail } else { format!("{root_label} / {tail}") };
        }
    }
    collections::name(folder)
}

fn group_label(folder: &Path, roots: &[PathBuf]) -> String {
    let roots = collections::distinct_roots(roots);
    let labels = root_labels(&roots);
    for root in &roots {
        if let Ok(rel) = folder.strip_prefix(root) {
            if rel.as_os_str().is_empty() {
                return if roots.len() == 1 {
                    "Loose books".into()
                } else {
                    format!("{} (loose)", labels.get(root).cloned().unwrap_or_else(|| collections::name(root)))
                };
            }
            let tail = rel_joined(rel);
            return if roots.len() == 1 {
                tail
            } else {
                format!("{} / {tail}", labels.get(root).cloned().unwrap_or_else(|| collections::name(root)))
            };
        }
    }
    collections::name(folder)
}

fn activity_folder_label(folder: &Path, roots: &[PathBuf]) -> String {
    let roots = collections::distinct_roots(roots);
    if roots.iter().any(|r| r == folder) {
        let labels = root_labels(&roots);
        return labels.get(folder).cloned().unwrap_or_else(|| collections::name(folder));
    }
    collections::name(folder)
}

fn destination_folders(index: &LibraryIndex, roots: &[PathBuf]) -> (Option<PathBuf>, Vec<(PathBuf, String)>) {
    let roots = collections::distinct_roots(roots);
    let tree = Collections::from_index(index, &roots);
    let mut existing: Vec<PathBuf> = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    for root in &roots {
        if root.is_dir() && seen.insert(root.clone()) {
            existing.push(root.clone());
        }
    }
    let default_folder = existing.first().cloned().or_else(|| roots.first().cloned());
    let mut subs: Vec<PathBuf> = tree.folders.keys().filter(|p| !seen.contains(*p) && p.is_dir()).cloned().collect();
    subs.sort_by_cached_key(|p| {
        let root_pos = roots.iter().position(|r| p.starts_with(r)).unwrap_or(usize::MAX);
        (root_pos, folder_label(p, &roots).to_lowercase(), p.clone())
    });
    let mut folders = Vec::new();
    for root in &existing {
        folders.push((root.clone(), folder_label(root, &roots)));
        for sub in subs.iter().filter(|p| p.starts_with(root)) {
            folders.push((sub.clone(), folder_label(sub, &roots)));
        }
    }
    (default_folder, folders)
}

fn load_book_states(store: &Store, index: &LibraryIndex) -> HashMap<String, BookState> {
    let mut states = HashMap::new();
    if let Some(ids) = store.book_state_ids() {
        for id in ids {
            states.insert(id.clone(), store.book(&id));
        }
        return states;
    }
    for book in index.books.iter().filter(|b| b.available) {
        states.insert(book.id.clone(), store.book(&book.id));
    }
    states
}

/// Returns the `annotation_id`s that currently have at least one saved
/// highlight or note, mapped to their saved record count, from one listing of
/// `data/annotations/`.
fn annotated_book_ids(store: &Store) -> HashMap<String, usize> {
    let mut ids = HashMap::new();
    let Ok(entries) = std::fs::read_dir(store.annotation_dir()) else { return ids };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(id) = name.strip_prefix("book-").and_then(|s| s.strip_suffix(".json")) else { continue };
        let Ok(bytes) = std::fs::read(entry.path()) else { continue };
        if let Ok(manifest) = serde_json::from_slice::<Manifest>(&bytes)
            && manifest.version == 1
            && !manifest.records.is_empty()
        {
            ids.insert(id.to_owned(), manifest.records.len());
        }
    }
    ids
}

fn matches_query(book: &Book, filename: &str, group: &str, folded_query: &str) -> bool {
    [book.metadata.title.as_str(), book.metadata.authors.as_str(), filename, group]
        .iter()
        .any(|field| search::fold(field).contains(folded_query))
}

fn book_json(
    book: &Book,
    folder: &Path,
    group: &str,
    filename: &str,
    state: Option<&BookState>,
    highlight_count: usize,
) -> Value {
    let finished = state.is_some_and(|s| s.finished);
    let progress = state.and_then(|s| {
        (s.finished || s.progress > 0 || s.last_opened > 0 || !s.position.0.is_empty())
            .then_some(s.progress.min(10000) / 100)
    });
    json!({
        "id": book.id,
        "title": book.metadata.title,
        "author": book.metadata.authors,
        "filename": filename,
        "folder": folder.to_string_lossy(),
        "group": group,
        "size": book.stamp.size,
        "progress": progress,
        "finished": finished,
        "has_highlights": highlight_count > 0,
        "highlight_count": highlight_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::annotations::{Annotation, Collection};
    use crate::document::{Position, TextRange};
    use crate::library::epub::tests::fixture;
    use crate::platform::Waker;
    use crate::transfer::api::{Api, Context};
    use crate::transfer::auth::{self, Auth};
    use crate::transfer::server::{Server, Timeouts};
    use std::net::TcpStream;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    #[derive(Default)]
    struct RecordedActivity {
        events: Mutex<Vec<String>>,
        next: AtomicU64,
        receiving: AtomicU64,
    }

    impl ActivitySink for RecordedActivity {
        fn begin_receiving(&self, name: String) -> u64 {
            let id = self.next.fetch_add(1, Ordering::SeqCst) + 1;
            self.receiving.fetch_add(1, Ordering::SeqCst);
            self.events.lock().unwrap().push(format!("begin:{id}:{name}"));
            id
        }

        fn finish_receiving(&self, id: u64, received: Option<Activity>) {
            self.receiving.fetch_sub(1, Ordering::SeqCst);
            self.events.lock().unwrap().push(format!("finish:{id}:{received:?}"));
        }

        fn push(&self, activity: Activity) {
            self.events.lock().unwrap().push(format!("push:{activity:?}"));
        }
    }

    struct Env {
        _dir: tempfile::TempDir,
        library_dir: PathBuf,
        outside_dir: PathBuf,
        store: Store,
        worker: library::worker::Worker,
        _pause: Option<library::worker::CoverPause>,
        server: Server,
        cookie: String,
        free_bytes: Arc<AtomicU64>,
        activity: Arc<RecordedActivity>,
    }

    impl Env {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let library_dir = dir.path().join("library");
            let outside_dir = dir.path().join("outside");
            std::fs::create_dir_all(&library_dir).unwrap();
            std::fs::create_dir_all(&outside_dir).unwrap();
            let store = Store::at(dir.path().join("data"));
            store.save_library_folders(vec![library_dir.clone()]).unwrap();
            let worker = store.library_worker(Waker::new(|| {}));
            // Wait for the initial empty startup scan to finish so later tests
            // can prove PathChange::Add runs no scan pass.
            let started = Instant::now();
            loop {
                if let Some(status) = worker.take_status()
                    && !status.working
                {
                    break;
                }
                assert!(started.elapsed() < Duration::from_secs(10), "initial scan timed out");
                std::thread::sleep(Duration::from_millis(10));
            }
            let pause = Some(worker.pause_covers());
            let free_bytes = Arc::new(AtomicU64::new(500 * 1024 * 1024));
            let activity = Arc::new(RecordedActivity::default());
            let books = Books::new(
                store.clone(),
                worker.clone(),
                Arc::new({
                    let free = free_bytes.clone();
                    move |_| Ok(free.load(Ordering::SeqCst))
                }),
                activity.clone(),
            );
            let api = Arc::new(Api::new(
                Auth::with_code("4827".into()),
                Context {
                    port: 80,
                    names: Box::new(|| vec!["verso.local".into()]),
                    status: Box::new(|| json!({ "name": "verso.local" })),
                    changed: Box::new(|| {}),
                    books: Some(books),
                    dictionaries: None,
                    files: None,
                },
            ));
            let server = Server::start("127.0.0.1:0".parse().unwrap(), api, Timeouts::default()).unwrap();
            let mut env = Self {
                _dir: dir,
                library_dir,
                outside_dir,
                store,
                worker,
                _pause: pause,
                server,
                cookie: String::new(),
                free_bytes,
                activity,
            };
            let (status, head, _) = env
                .raw("POST /api/login HTTP/1.1\r\nHost: verso.local\r\nX-Verso-Request: 1\r\n", br#"{"code":"4827"}"#);
            assert_eq!(status, 200);
            env.cookie = head
                .lines()
                .find_map(|l| l.strip_prefix("Set-Cookie: "))
                .unwrap()
                .split(';')
                .next()
                .unwrap()
                .to_owned();
            env
        }

        fn raw(&self, head: &str, body: &[u8]) -> (u16, String, Vec<u8>) {
            let mut stream = TcpStream::connect(self.server.addr()).unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
            let req = format!("{head}Content-Length: {}\r\nConnection: close\r\n\r\n", body.len());
            stream.write_all(req.as_bytes()).unwrap();
            stream.write_all(body).unwrap();
            let mut raw = Vec::new();
            stream.read_to_end(&mut raw).unwrap();
            let split = raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
            let head = String::from_utf8(raw[..split].to_vec()).unwrap();
            let status = head[9..12].parse().unwrap();
            (status, head, raw[split + 4..].to_vec())
        }

        fn req(&self, method: &str, target: &str, body: &[u8]) -> (u16, String, Vec<u8>) {
            let head = format!(
                "{method} {target} HTTP/1.1\r\nHost: verso.local\r\nCookie: {}\r\nX-Verso-Request: 1\r\n",
                self.cookie
            );
            self.raw(&head, body)
        }

        fn json(&self, method: &str, target: &str, body: &[u8]) -> (u16, Value) {
            let (status, _, body) = self.req(method, target, body);
            (
                status,
                serde_json::from_slice(&body)
                    .unwrap_or_else(|e| panic!("status {status}, body {:?}: {e}", String::from_utf8_lossy(&body))),
            )
        }

        fn upload(&self, folder: &Path, name: &str, bytes: &[u8]) -> (u16, Value) {
            let target = format!("/api/books?folder={}&name={}", encode(folder.to_str().unwrap()), encode(name));
            self.json("PUT", &target, bytes)
        }
    }

    fn encode(s: &str) -> String {
        let mut out = String::new();
        for b in s.bytes() {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                out.push(b as char);
            } else {
                out.push_str(&format!("%{b:02X}"));
            }
        }
        out
    }

    fn epub_bytes(title: &str, author: &str, extra: &[(&str, &[u8])]) -> Vec<u8> {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let opf = format!(
            r#"<package><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>{title}</dc:title><dc:creator>{author}</dc:creator></metadata></package>"#
        );
        fixture(tmp.path(), &opf, extra).unwrap();
        std::fs::read(tmp.path()).unwrap()
    }

    fn fixture_epub(name: &str) -> Vec<u8> {
        std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)).unwrap()
    }

    #[test]
    fn epub_check_matches_the_library_and_cleans_up_part_files() {
        let env = Env::new();
        // Wrong extension: refused before reading/creating a part file.
        let (status, body) = env.upload(&env.library_dir, "notes.pdf", b"%PDF-1.4");
        assert_eq!(status, 400);
        assert!(body["error"].as_str().unwrap().contains("Files tab"));

        // .epub name without zip header: refused early without a part file.
        let (status, body) = env.upload(&env.library_dir, "fake.epub", b"not a zip file at all");
        assert_eq!(status, 400);
        assert!(body["error"].as_str().unwrap().contains("Files tab"));
        assert!(!env.library_dir.join("fake.epub.part").exists());

        // Valid zip header but invalid EPUB (missing container.xml): rejected by epub::read, part file deleted.
        let bad_zip = {
            let tmp = tempfile::NamedTempFile::new().unwrap();
            let mut z = zip::ZipWriter::new(std::fs::File::create(tmp.path()).unwrap());
            z.start_file("hello.txt", zip::write::SimpleFileOptions::default()).unwrap();
            z.write_all(b"hi").unwrap();
            z.finish().unwrap();
            let bytes = std::fs::read(tmp.path()).unwrap();
            assert!(epub::read(tmp.path()).is_err(), "library rejects the same zip");
            bytes
        };
        let (status, body) = env.upload(&env.library_dir, "bad.epub", &bad_zip);
        assert_eq!(status, 400);
        assert!(body["error"].as_str().unwrap().contains("Not a valid EPUB"));
        assert!(!env.library_dir.join("bad.epub").exists());
        assert!(!env.library_dir.join("bad.epub.part").exists());

        // Broken OPF XML: rejected by both epub::read and upload, part file deleted.
        let broken_opf = {
            let tmp = tempfile::NamedTempFile::new().unwrap();
            fixture(tmp.path(), "<package>", &[]).unwrap();
            assert!(epub::read(tmp.path()).is_err());
            std::fs::read(tmp.path()).unwrap()
        };
        let (status, _) = env.upload(&env.library_dir, "broken.epub", &broken_opf);
        assert_eq!(status, 400);
        assert!(!env.library_dir.join("broken.epub.part").exists());

        // Valid EPUB without mimetype entry: accepted by both library and upload.
        let valid = epub_bytes("Middlemarch", "George Eliot", &[]);
        let (status, book) = env.upload(&env.library_dir, "Middlemarch.epub", &valid);
        assert_eq!(status, 201);
        assert_eq!(book["title"], "Middlemarch");
        assert_eq!(book["author"], "George Eliot");
        assert!(env.library_dir.join("Middlemarch.epub").exists());
        assert!(!env.library_dir.join("Middlemarch.epub.part").exists());

        // Empty <dc:title> falls back to the final filename stem, never ".epub.part".
        let untitled = epub_bytes("", "Anon", &[]);
        let (status, book) = env.upload(&env.library_dir, "Fallback Title.epub", &untitled);
        assert_eq!(status, 201);
        assert_eq!(book["title"], "Fallback Title");
    }

    #[test]
    fn clashes_never_overwrite_and_respect_orphan_sidecars() {
        let env = Env::new();
        let first = epub_bytes("Book One", "Author A", &[]);
        let second = epub_bytes("Book Two", "Author B", &[]);
        let third = epub_bytes("Book Three", "Author C", &[]);

        let (s1, b1) = env.upload(&env.library_dir, "Same.epub", &first);
        assert_eq!(s1, 201);
        assert_eq!(b1["filename"], "Same.epub");

        // Second upload with the same name becomes Same (2).epub.
        let (s2, b2) = env.upload(&env.library_dir, "Same.epub", &second);
        assert_eq!(s2, 201);
        assert_eq!(b2["filename"], "Same (2).epub");
        assert_eq!(std::fs::read(env.library_dir.join("Same.epub")).unwrap(), first);
        assert_eq!(std::fs::read(env.library_dir.join("Same (2).epub")).unwrap(), second);

        // An orphan sidecar blocks Same (3).epub, so the next upload takes Same (4).epub.
        std::fs::create_dir(env.library_dir.join("Same (3).sdr")).unwrap();
        let (s3, b3) = env.upload(&env.library_dir, "Same.epub", &third);
        assert_eq!(s3, 201);
        assert_eq!(b3["filename"], "Same (4).epub");
    }

    #[test]
    fn interrupted_upload_deletes_part_file_and_clears_receiving_activity() {
        let env = Env::new();
        let target =
            format!("/api/books?folder={}&name={}", encode(env.library_dir.to_str().unwrap()), encode("Halfway.epub"));
        let mut stream = TcpStream::connect(env.server.addr()).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let head = format!(
            "PUT {target} HTTP/1.1\r\nHost: verso.local\r\nCookie: {}\r\nX-Verso-Request: 1\r\nContent-Length: 500000\r\n\r\n",
            env.cookie
        );
        stream.write_all(head.as_bytes()).unwrap();
        let mut partial = Vec::from(*ZIP_HEADER);
        partial.extend(vec![0u8; 100_000]);
        stream.write_all(&partial).unwrap();
        let part = env.library_dir.join(".verso-upload.part");
        let started = Instant::now();
        while !part.exists() {
            assert!(started.elapsed() < Duration::from_secs(5), "part file never created");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(env.activity.receiving.load(Ordering::SeqCst), 1);
        // Drop the client mid-upload: the server cleans up the part file and activity row.
        drop(stream);
        let started = Instant::now();
        while part.exists() || env.activity.receiving.load(Ordering::SeqCst) > 0 {
            assert!(started.elapsed() < Duration::from_secs(5), "part file wasn't cleaned up");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!env.library_dir.join("Halfway.epub").exists());
        assert!(env.worker.current().books.is_empty());
    }

    #[test]
    fn checks_free_space_before_writing_a_part_file() {
        let env = Env::new();
        let bytes = epub_bytes("Too Big", "Author", &[]);
        // Leave 1 byte less than needed (bytes + 50 MiB reserve).
        env.free_bytes.store(FREE_SPACE_RESERVE + bytes.len() as u64 - 1, Ordering::SeqCst);
        let (status, body) = env.upload(&env.library_dir, "TooBig.epub", &bytes);
        assert_eq!(status, 507);
        assert!(body["error"].as_str().unwrap().contains("free space"));
        assert!(!env.library_dir.join("TooBig.epub").exists());
        assert!(!env.library_dir.join("TooBig.epub.part").exists());

        // Exactly enough space succeeds.
        env.free_bytes.store(FREE_SPACE_RESERVE + bytes.len() as u64, Ordering::SeqCst);
        let (status, _) = env.upload(&env.library_dir, "TooBig.epub", &bytes);
        assert_eq!(status, 201);
    }

    #[test]
    fn enforces_destinations_inside_library_folders_and_supports_new_folder() {
        let env = Env::new();
        let bytes = epub_bytes("Valid", "Author", &[]);
        // Outside library root.
        assert_eq!(env.upload(&env.outside_dir, "a.epub", &bytes).0, 403);
        // Traversal via `..`.
        let traversal = env.library_dir.join("../outside");
        assert_eq!(env.upload(&traversal, "a.epub", &bytes).0, 403);
        // Hidden folder and .sdr folder inside library root.
        let hidden = env.library_dir.join(".hidden");
        let sidecar = env.library_dir.join("Book.sdr");
        std::fs::create_dir(&hidden).unwrap();
        std::fs::create_dir(&sidecar).unwrap();
        assert_eq!(env.upload(&hidden, "a.epub", &bytes).0, 403);
        assert_eq!(env.upload(&sidecar, "a.epub", &bytes).0, 403);
        // Symlink inside library pointing outside.
        #[cfg(unix)]
        {
            let escape = env.library_dir.join("escape");
            std::os::unix::fs::symlink(&env.outside_dir, &escape).unwrap();
            assert_eq!(env.upload(&escape, "a.epub", &bytes).0, 403);
        }
        // Missing folder.
        assert_eq!(env.upload(&env.library_dir.join("Missing"), "a.epub", &bytes).0, 404);

        // New folder validates parent and name rules.
        let (status, _) = env.json(
            "POST",
            "/api/books/mkdir",
            serde_json::to_vec(&json!({ "parent": env.outside_dir, "name": "Nope" })).unwrap().as_slice(),
        );
        assert_eq!(status, 403);
        let (status, _) = env.json(
            "POST",
            "/api/books/mkdir",
            serde_json::to_vec(&json!({ "parent": env.library_dir, "name": ".secret" })).unwrap().as_slice(),
        );
        assert_eq!(status, 400);
        let (status, created) = env.json(
            "POST",
            "/api/books/mkdir",
            serde_json::to_vec(&json!({ "parent": env.library_dir, "name": "Literature" })).unwrap().as_slice(),
        );
        assert_eq!(status, 201);
        let lit = env.library_dir.join("Literature");
        assert_eq!(created["path"], lit.to_str().unwrap());
        assert_eq!(created["label"], "Literature");
        // Duplicate folder returns 409.
        let (status, _) = env.json(
            "POST",
            "/api/books/mkdir",
            serde_json::to_vec(&json!({ "parent": env.library_dir, "name": "Literature" })).unwrap().as_slice(),
        );
        assert_eq!(status, 409);

        // Uploading into the new folder works and enters it in the index.
        let (status, book) = env.upload(&lit, "Middlemarch.epub", &bytes);
        assert_eq!(status, 201);
        assert_eq!(book["folder"], lit.to_str().unwrap());
        assert_eq!(book["group"], "Literature");
    }

    #[test]
    fn updates_index_without_a_scan_pauses_covers_until_resumed_and_matches_scan_fingerprint() {
        let mut env = Env::new();
        // Drop an unindexed EPUB directly onto disk; a scan would discover it,
        // while PathChange::Add must not.
        let bytes = fixture_epub("library3.epub");
        std::fs::write(env.library_dir.join("unscanned.epub"), &bytes).unwrap();

        let (status, uploaded) = env.upload(&env.library_dir, "uploaded.epub", &bytes);
        assert_eq!(status, 201);
        let id = uploaded["id"].as_str().unwrap().to_owned();

        let current = env.worker.current();
        assert_eq!(current.books.len(), 1, "no scan pass ran: unscanned.epub stayed unseen");
        let added = &current.books[0];
        assert_eq!(added.id, id);
        assert_eq!(added.path, env.library_dir.join("uploaded.epub"));
        assert!(added.fingerprint_attempted && !added.fingerprint.is_empty());

        // Fingerprint hashed while streaming equals a scan's fingerprint.
        let scanned =
            library::worker::scan(&env.library_dir, &LibraryIndex::default(), false, &|| false, |_| {}).unwrap();
        let scan_fp = library::worker::fingerprint_for_test(&env.library_dir.join("uploaded.epub")).unwrap();
        assert_eq!(added.fingerprint, scan_fp);
        assert_eq!(scanned.index.books.len(), 2, "a real scan sees both files");

        // While CoverPause is held, cover work stays paused (Cover::Pending).
        std::thread::sleep(Duration::from_millis(120));
        assert_eq!(env.worker.current().books[0].cover, Cover::Pending);

        // Releasing CoverPause generates the cover for the added book without a scan pass.
        env._pause = None;
        let started = Instant::now();
        loop {
            let idx = env.worker.current();
            assert_eq!(idx.books.len(), 1, "resuming cover work must not trigger a scan pass");
            if matches!(idx.books[0].cover, Cover::Ready(_)) {
                break;
            }
            assert!(started.elapsed() < Duration::from_secs(10), "cover wasn't generated after unpausing");
            std::thread::sleep(Duration::from_millis(15));
        }
    }

    #[test]
    fn second_copy_of_a_book_gets_its_own_id_and_annotation_id() {
        let env = Env::new();
        let sub = env.library_dir.join("Copies");
        std::fs::create_dir(&sub).unwrap();
        let bytes = epub_bytes("Anna Karenina", "Leo Tolstoy", &[]);

        let (s1, b1) = env.upload(&env.library_dir, "Anna.epub", &bytes);
        assert_eq!(s1, 201);
        let id1 = b1["id"].as_str().unwrap().to_owned();

        // Same filename and same bytes in a different folder: same legacy_id and same fingerprint.
        let (s2, b2) = env.upload(&sub, "Anna.epub", &bytes);
        assert_eq!(s2, 201);
        let id2 = b2["id"].as_str().unwrap().to_owned();
        assert_ne!(id1, id2, "a second copy must never take the existing book's id");

        // Same folder (clash -> Anna (2).epub) with same bytes: also gets its own id.
        let (s3, b3) = env.upload(&env.library_dir, "Anna.epub", &bytes);
        assert_eq!(s3, 201);
        let id3 = b3["id"].as_str().unwrap().to_owned();
        assert_ne!(id1, id3);
        assert_ne!(id2, id3);

        let idx = env.worker.current();
        assert_eq!(idx.books.len(), 3);
        let orig = idx.books.iter().find(|b| b.path == env.library_dir.join("Anna.epub")).unwrap();
        assert_eq!(orig.id, id1, "the first copy keeps its original id");
        let ann_ids: HashSet<_> = idx.books.iter().map(|b| &b.annotation_id).collect();
        assert_eq!(ann_ids.len(), 3, "every copy gets its own annotation id");
    }

    #[test]
    fn list_search_download_and_delete_parity_with_book_menu() {
        let mut env = Env::new();
        let sub = env.library_dir.join("Russian");
        std::fs::create_dir(&sub).unwrap();
        let bytes1 = fixture_epub("library3.epub");
        let bytes2 = epub_bytes("Война и мир", "Лев Толстой", &[]);

        let (_, b1) = env.upload(&env.library_dir, "First.epub", &bytes1);
        let (_, b2) = env.upload(&sub, "War.epub", &bytes2);
        let id1 = b1["id"].as_str().unwrap().to_owned();
        let id2 = b2["id"].as_str().unwrap().to_owned();

        // Generate a cover for b1, give b1 reading state, a KOReader sidecar, and an annotation.
        env._pause = None;
        let started = Instant::now();
        let cover_key = loop {
            if let Some(Cover::Ready(key)) = env.worker.current().books.iter().find(|b| b.id == id1).map(|b| &b.cover) {
                break key.clone();
            }
            assert!(started.elapsed() < Duration::from_secs(10));
            std::thread::sleep(Duration::from_millis(15));
        };
        assert!(env.store.cover_path(&cover_key).exists());
        env._pause = Some(env.worker.pause_covers());

        env.store
            .save_book(
                &id1,
                &BookState {
                    position: Position("/p[1]".into()),
                    progress: 4120,
                    last_opened: 100,
                    ..Default::default()
                },
            )
            .unwrap();
        std::fs::create_dir(env.library_dir.join("First.sdr")).unwrap();
        std::fs::write(env.library_dir.join("First.sdr/metadata.epub.lua"), b"progress").unwrap();

        let ann_id = env.worker.current().books.iter().find(|b| b.id == id1).unwrap().annotation_id.clone();
        let mut col = Collection::load(&env.store, &ann_id).unwrap();
        col.add(Annotation {
            version: 1,
            kind: crate::annotations::Kind::Highlight,
            id: "ann1".into(),
            book: ann_id.clone(),
            fingerprint: "fp".into(),
            range: TextRange { start: Position("a".into()), end: Position("b".into()) },
            quotation: "quote".into(),
            created: "2026-11-20".into(),
            chapter: "Ch 1".into(),
            note: None,
            provenance: None,
        });
        col.commit(&env.store, &ann_id).unwrap();

        // List returns both books, progress, highlights flag and folders.
        let (status, list) = env.json("GET", "/api/books", b"");
        assert_eq!(status, 200);
        let books = list["books"].as_array().unwrap();
        assert_eq!(books.len(), 2);
        let first = books.iter().find(|b| b["id"] == id1).unwrap();
        assert_eq!(first["progress"], 41);
        assert_eq!(first["has_highlights"], true);
        assert_eq!(first["group"], "Loose books");
        let second = books.iter().find(|b| b["id"] == id2).unwrap();
        assert!(second["progress"].is_null());
        assert_eq!(second["has_highlights"], false);
        assert_eq!(second["group"], "Russian");

        // Search filters Unicode title/author case-insensitively.
        let (status, filtered) = env.json("GET", &format!("/api/books?q={}", encode("ТОЛСТОЙ")), b"");
        assert_eq!(status, 200);
        assert_eq!(filtered["books"].as_array().unwrap().len(), 1);
        assert_eq!(filtered["books"][0]["id"], id2);

        // Download streams the exact EPUB bytes with safe download headers.
        let (status, head, downloaded) = env.req("GET", &format!("/api/books/{id1}"), b"");
        assert_eq!(status, 200);
        assert_eq!(downloaded, bytes1);
        assert!(head.contains("Content-Type: application/octet-stream"));
        assert!(head.contains("Content-Disposition: attachment;"));
        assert!(head.contains("Content-Security-Policy: sandbox"));
        assert!(head.contains("X-Content-Type-Options: nosniff"));

        // Delete removes file, KOReader sidecar, reading state, cached cover and
        // index entry — while leaving annotations in place (exact book-menu parity).
        let (status, res) = env.json("DELETE", &format!("/api/books/{id1}"), b"");
        assert_eq!(status, 200);
        assert_eq!(res["ok"], true);
        assert!(!env.library_dir.join("First.epub").exists());
        assert!(!env.library_dir.join("First.sdr").exists());
        assert_eq!(env.store.book(&id1).progress, 0);
        assert!(!env.store.cover_path(&cover_key).exists());
        assert!(!env.worker.current().books.iter().any(|b| b.id == id1));
        assert!(!env.store.library().books.iter().any(|b| b.id == id1));
        assert_eq!(Collection::load(&env.store, &ann_id).unwrap().records.len(), 1, "annotations stay in place");

        // Deleting the last book in a subfolder marks that subfolder as empty.
        let (status, _) = env.json("DELETE", &format!("/api/books/{id2}"), b"");
        assert_eq!(status, 200);
        assert!(env.worker.current().empty_folders.contains(&sub));
        assert_eq!(env.req("GET", &format!("/api/books/{id2}"), b"").0, 404);
    }

    #[test]
    fn highlights_export_one_book_and_zip_in_saved_order() {
        let env = Env::new();
        let (_, b1) = env.upload(&env.library_dir, "Marked.epub", &epub_bytes("Война и мир", "Лев Толстой", &[]));
        let (_, b2) = env.upload(&env.library_dir, "Plain.epub", &epub_bytes("Plain", "Nobody", &[]));
        let (_, b3) = env.upload(&env.library_dir, "Gone.epub", &epub_bytes("Gone", "Somebody", &[]));
        let id1 = b1["id"].as_str().unwrap().to_owned();
        let id2 = b2["id"].as_str().unwrap().to_owned();
        let id3 = b3["id"].as_str().unwrap().to_owned();
        let annotation_id =
            |id: &str| env.worker.current().books.iter().find(|b| b.id == id).unwrap().annotation_id.clone();
        let add = |ann: &str, quotes: &[(&str, &str)]| {
            let mut col = Collection::load(&env.store, ann).unwrap();
            for (id, quote) in quotes {
                col.add(Annotation {
                    version: 1,
                    kind: crate::annotations::Kind::Highlight,
                    id: (*id).into(),
                    book: ann.into(),
                    fingerprint: "fp".into(),
                    range: TextRange { start: Position("a".into()), end: Position("b".into()) },
                    quotation: (*quote).into(),
                    created: "2026-11-20".into(),
                    chapter: "Глава 1".into(),
                    note: (*id == "z").then(|| "a note".into()),
                    provenance: None,
                });
            }
            col.commit(&env.store, ann).unwrap();
        };
        // Saved order, not alphabetical: "z" was saved before "a".
        let ann1 = annotation_id(&id1);
        add(&ann1, &[("z", "Second in the text"), ("a", "First in the text")]);
        let ann3 = annotation_id(&id3);
        add(&ann3, &[("g", "From a deleted book")]);
        assert_eq!(env.json("DELETE", &format!("/api/books/{id3}"), b"").0, 200);

        let (_, list) = env.json("GET", "/api/books", b"");
        let first = list["books"].as_array().unwrap().iter().find(|b| b["id"] == id1).unwrap().clone();
        assert_eq!(first["highlight_count"], 2);

        let (status, head, body) = env.req("GET", &format!("/api/highlights/{id1}"), b"");
        assert_eq!(status, 200);
        assert!(head.contains("Content-Type: application/octet-stream"));
        assert!(head.contains("Content-Disposition: attachment;") && head.contains(".md"));
        assert!(head.contains("Content-Security-Policy: sandbox"));
        let md = String::from_utf8(body).unwrap();
        assert!(md.starts_with("# Война и мир\n\nЛев Толстой\n\n*2 highlights · 1 note"), "{md}");
        let (second, first_q) = (md.find("Second in the text").unwrap(), md.find("First in the text").unwrap());
        assert!(second < first_q, "saved order is kept");
        assert!(md.contains("**Note:** a note"));
        assert_eq!(env.req("GET", &format!("/api/highlights/{id2}"), b"").0, 404, "no highlights");
        assert_eq!(env.req("GET", &format!("/api/highlights/{id3}"), b"").0, 404, "deleted books aren't listed");

        let (status, head, body) = env.req("GET", "/api/highlights.zip", b"");
        assert_eq!(status, 200);
        assert!(head.contains("verso-highlights.zip"));
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(body)).unwrap();
        assert_eq!(zip.len(), 1, "only library books with highlights");
        let mut entry = zip.by_index(0).unwrap();
        assert_eq!(entry.name(), export::file_name("Война и мир", &ann1));
        let mut zipped = String::new();
        entry.read_to_string(&mut zipped).unwrap();
        let strip = |s: &str| s.lines().filter(|l| !l.contains("exported from Verso")).collect::<Vec<_>>().join("\n");
        assert_eq!(strip(&zipped), strip(&md));
    }

    #[test]
    fn multi_chunk_upload_fingerprint_and_streaming_download_match_byte_for_byte() {
        let env = Env::new();
        // > 2 * 64 KB of incompressible bytes so both upload hashing and
        // download streaming span multiple chunks.
        let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
        let payload: Vec<u8> = (0..180_000usize)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state & 0xff) as u8
            })
            .collect();
        let bytes = epub_bytes("Big Book", "Author", &[("OPS/blob.bin", &payload)]);
        assert!(bytes.len() > 2 * BUFFER, "test EPUB spans multiple 64 KB chunks ({})", bytes.len());

        let (status, book) = env.upload(&env.library_dir, "Big.epub", &bytes);
        assert_eq!(status, 201);
        let id = book["id"].as_str().unwrap().to_owned();
        let indexed = env.worker.current().books.iter().find(|b| b.id == id).cloned().unwrap();
        let scan_fp = library::worker::fingerprint_for_test(&env.library_dir.join("Big.epub")).unwrap();
        assert_eq!(indexed.fingerprint, scan_fp);

        let (status, _, downloaded) = env.req("GET", &format!("/api/books/{id}"), b"");
        assert_eq!(status, 200);
        assert_eq!(downloaded, bytes);
    }

    #[test]
    fn delete_parity_with_book_menu_preserves_shared_sidecar_and_annotations() {
        let mut env = Env::new();
        let dir_web = env.library_dir.join("Web");
        let dir_menu = env.library_dir.join("Menu");
        std::fs::create_dir(&dir_web).unwrap();
        std::fs::create_dir(&dir_menu).unwrap();
        let bytes = fixture_epub("library3.epub");

        let (_, web_book) = env.upload(&dir_web, "Shared.epub", &bytes);
        let (_, menu_book) = env.upload(&dir_menu, "Shared.epub", &bytes);
        let web_id = web_book["id"].as_str().unwrap().to_owned();
        let menu_id = menu_book["id"].as_str().unwrap().to_owned();

        // Generate covers for both, then pause again.
        env._pause = None;
        let started = Instant::now();
        loop {
            let idx = env.worker.current();
            if idx.books.iter().filter(|b| matches!(b.cover, Cover::Ready(_))).count() == 2 {
                break;
            }
            assert!(started.elapsed() < Duration::from_secs(10));
            std::thread::sleep(Duration::from_millis(15));
        }
        env._pause = Some(env.worker.pause_covers());

        let idx = env.worker.current();
        let setup = |dir: &Path, id: &str| -> (String, String) {
            let book = idx.books.iter().find(|b| b.id == id).unwrap();
            let Cover::Ready(cover_key) = &book.cover else { panic!("cover missing") };
            env.store
                .save_book(
                    id,
                    &BookState {
                        position: Position("/p[2]".into()),
                        progress: 5000,
                        last_opened: 7,
                        ..Default::default()
                    },
                )
                .unwrap();
            // Shared KOReader sidecar alongside a PDF with the same stem.
            std::fs::write(dir.join("Shared.pdf"), b"pdf").unwrap();
            std::fs::create_dir(dir.join("Shared.sdr")).unwrap();
            std::fs::write(dir.join("Shared.sdr/metadata.epub.lua"), b"epub progress").unwrap();
            std::fs::write(dir.join("Shared.sdr/metadata.pdf.lua"), b"pdf progress").unwrap();
            let mut col = Collection::load(&env.store, &book.annotation_id).unwrap();
            col.add(Annotation {
                version: 1,
                kind: crate::annotations::Kind::Highlight,
                id: format!("note-{id}"),
                book: book.annotation_id.clone(),
                fingerprint: "fp".into(),
                range: TextRange { start: Position("a".into()), end: Position("b".into()) },
                quotation: "q".into(),
                created: "2026-11-20".into(),
                chapter: "Ch".into(),
                note: None,
                provenance: None,
            });
            col.commit(&env.store, &book.annotation_id).unwrap();
            (cover_key.clone(), book.annotation_id.clone())
        };
        let (web_cover, web_ann) = setup(&dir_web, &web_id);
        let (menu_cover, menu_ann) = setup(&dir_menu, &menu_id);

        // Delete one through the web API, and the other through the book menu's shared function.
        assert_eq!(env.json("DELETE", &format!("/api/books/{web_id}"), b"").0, 200);
        let menu_del = files::delete_book(&menu_id, &dir_menu.join("Shared.epub"), &env.store).unwrap();
        assert!(menu_del.sidecar_error.is_none());
        env.worker.apply_sync(menu_del.changes);

        for (dir, id, cover, ann) in
            [(&dir_web, &web_id, &web_cover, &web_ann), (&dir_menu, &menu_id, &menu_cover, &menu_ann)]
        {
            assert!(!dir.join("Shared.epub").exists());
            assert!(dir.join("Shared.pdf").exists(), "sibling PDF stays");
            assert!(!dir.join("Shared.sdr/metadata.epub.lua").exists(), "EPUB sidecar removed");
            assert!(dir.join("Shared.sdr/metadata.pdf.lua").exists(), "PDF sidecar preserved");
            assert_eq!(env.store.book(id).progress, 0);
            assert!(!env.store.cover_path(cover).exists());
            assert!(!env.worker.current().books.iter().any(|b| &b.id == id));
            assert_eq!(Collection::load(&env.store, ann).unwrap().records.len(), 1);
        }
    }

    #[test]
    fn books_tab_is_read_only_after_live_data_edits() {
        let env = Env::new();
        let (status, book) = env.upload(&env.library_dir, "Kept.epub", &epub_bytes("Kept", "Author", &[]));
        assert_eq!(status, 201, "{book}");
        let id = book["id"].as_str().unwrap().to_owned();
        env.store.note_live_data_edit();
        let (status, body) = env.upload(&env.library_dir, "New.epub", &epub_bytes("New", "Author", &[]));
        assert_eq!((status, body["restart"].clone()), (409, json!(true)), "{body}");
        assert!(!env.library_dir.join("New.epub").exists());
        let mkdir = json!({ "parent": env.library_dir.to_str().unwrap(), "name": "Shelf" });
        assert_eq!(env.json("POST", "/api/books/mkdir", mkdir.to_string().as_bytes()).0, 409);
        assert!(!env.library_dir.join("Shelf").exists());
        assert_eq!(env.json("DELETE", &format!("/api/books/{id}"), b"").0, 409);
        assert!(env.library_dir.join("Kept.epub").exists());
        // Reading is still fine.
        assert_eq!(env.json("GET", "/api/books", b"").0, 200);
        assert_eq!(env.req("GET", &format!("/api/books/{id}"), b"").0, 200);
    }

    #[test]
    fn validates_epub_names() {
        for bad in [
            "",
            " ",
            ".",
            "..",
            ".hidden.epub",
            "a/b.epub",
            "a\\b.epub",
            "a\x01b.epub",
            "book.pdf",
            ".epub",
            "x.sdr.epub",
        ] {
            assert!(sanitize_epub_name(bad).is_err(), "{bad:?}");
        }
        assert_eq!(sanitize_epub_name("Dune: Messiah?.epub").unwrap(), "Dune_ Messiah_.epub");
        assert_eq!(sanitize_epub_name("Граф Монте-Кристо.EPUB").unwrap(), "Граф Монте-Кристо.EPUB");
        let _ = auth::COOKIE;
    }

    #[test]
    fn check_upload_matches_early_refusals() {
        let env = Env::new();
        env.free_bytes.store(FREE_SPACE_RESERVE + 1024, Ordering::SeqCst);
        let folder = env.library_dir.to_str().unwrap();
        let query = |name: &str, size: u64| format!("/api/books/check?folder={folder}&name={name}&size={size}");
        assert_eq!(env.json("GET", &query("Book.epub", 512), b"").0, 200);
        assert_eq!(env.json("GET", &query("Book.epub", 2048), b"").0, 507);
        assert_eq!(env.json("GET", &query("Book.pdf", 512), b"").0, 400);
        assert_eq!(env.json("GET", "/api/books/check?name=Book.epub", b"").0, 400);
        env.store.note_live_data_edit();
        let (status, body) = env.json("GET", &query("Book.epub", 512), b"");
        assert_eq!((status, body["restart"].clone()), (409, json!(true)));
    }
}
