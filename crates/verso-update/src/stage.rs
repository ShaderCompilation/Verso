//! Staging an update: unpacking verified component archives into
//! `update/staged/`, and `staged.json`, the record the launcher installs
//! from. Everything here writes only inside the folders it is given; the
//! installed files are never touched.
use crate::{Component, FORMAT, Installed, InstalledComponent, MODE_EXEC, MODE_FILE, Manifest, Version, is_safe_path};
use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// Update files, relative to the storage root (`/mnt/us`).
pub const UPDATE_DIR: &str = "verso/update";
/// Archives while they download (and partial ones, kept for a retry).
pub const DOWNLOAD_DIR: &str = "verso/update/download";
/// Unpacked components, one folder per component id, and [`STAGED_FILE`].
pub const STAGED_DIR: &str = "verso/update/staged";
/// Written last into [`STAGED_DIR`]: the staged update is complete.
pub const STAGED_FILE: &str = "staged.json";
/// Written into [`STAGED_DIR`] before [`STAGED_FILE`]: the new
/// `installed.json`, which the launcher moves into place like a file.
pub const STAGED_INSTALLED: &str = "installed.json";
/// Written into [`STAGED_DIR`] before [`STAGED_FILE`]: the launcher's list
/// of moves, as plain text (see [`Staged::plan_text`]). Not a valid
/// component id, so it can't clash with a component's folder.
pub const PLAN_FILE: &str = "plan.txt";
/// Present while Verso keeps the Kindle awake for a download; the launcher
/// turns the setting back off if Verso exits without doing so.
pub const AWAKE_MARKER: &str = "verso/update/awake";

/// A staged update, as `staged.json` records it. The launcher moves each
/// component's files from `staged/<id>/<path>` to `<install>/<path>`,
/// moves `remove` aside, and moves `installed` into place as the new
/// `installed.json`; it reads all this from [`PLAN_FILE`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Staged {
    pub format: u32,
    pub version: Version,
    /// The version it replaces; None when nothing was recorded.
    #[serde(default)]
    pub from: Option<Version>,
    /// The staged components, in manifest order.
    pub components: Vec<InstalledComponent>,
    /// Files, relative to the storage root, that the replaced components
    /// owned and that nothing owns once the update is installed.
    #[serde(default)]
    pub remove: Vec<String>,
    /// `installed.json` once the update is installed.
    pub installed: Installed,
}

impl Staged {
    /// The record for staging `ids` of a verified `manifest` over
    /// `installed` (None: nothing recorded, so nothing to remove).
    pub fn plan(manifest: &Manifest, ids: &[String], installed: Option<&Installed>) -> Self {
        let staged: Vec<&Component> = manifest.components.iter().filter(|c| ids.contains(&c.id)).collect();
        let record = |c: &Component| InstalledComponent {
            id: c.id.clone(),
            hash: c.hash.clone(),
            install: c.install.clone(),
            files: c.files.iter().map(|f| f.path.clone()).collect(),
        };
        // The new installed.json: staged components replace their old
        // records, in manifest order; the rest stay as they were.
        let old = installed.map_or(&[][..], |i| &i.components[..]);
        let mut components: Vec<InstalledComponent> = Vec::new();
        for c in &manifest.components {
            if ids.contains(&c.id) {
                components.push(record(c));
            } else if let Some(had) = old.iter().find(|o| o.id == c.id) {
                components.push(had.clone());
            }
        }
        components.extend(old.iter().filter(|o| manifest.component(&o.id).is_none()).cloned());
        let owned_after: BTreeSet<String> =
            components.iter().flat_map(|c| c.files.iter().map(move |f| format!("{}/{f}", c.install))).collect();
        let remove = old
            .iter()
            .filter(|o| ids.contains(&o.id))
            .flat_map(|o| o.files.iter().map(move |f| format!("{}/{f}", o.install)))
            .filter(|path| is_safe_path(path) && !owned_after.contains(path))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        Staged {
            format: FORMAT,
            version: manifest.version.clone(),
            from: installed.map(|i| i.version.clone()),
            components: staged.into_iter().map(record).collect(),
            remove,
            installed: Installed { format: FORMAT, version: manifest.version.clone(), dev: false, components },
        }
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, crate::Error> {
        let staged: Staged = serde_json::from_slice(bytes).map_err(|e| crate::Error::Manifest(e.to_string()))?;
        if staged.format != FORMAT {
            return Err(crate::Error::UnsupportedFormat(staged.format));
        }
        Ok(staged)
    }

    /// The launcher's list of moves, one per line, fields separated by
    /// tabs, paths relative to the storage root (they can't hold tabs or
    /// line breaks: see `is_safe_path`):
    ///
    /// ```text
    /// verso-plan  1
    /// version     0.3.0
    /// from        0.2.1                 (left out when unknown)
    /// remove      <path>                (moved to previous/; nothing replaces it)
    /// install     <staged path>  <path> (the old file, if any, moved to previous/)
    /// replace     <staged path>  <path> (the old file copied to previous/, then
    ///                                    the new one renamed over it)
    /// ```
    ///
    /// `replace` is for the files Verso is started from ([`ENTRY_COMPONENTS`]):
    /// a power loss between moving the old file aside and the new one in
    /// would leave nothing to start Verso, or to finish the update, with.
    /// The last `install` is the new `installed.json`.
    pub fn plan_text(&self) -> String {
        let mut text = format!("verso-plan\t{PLAN_FORMAT}\nversion\t{}\n", self.version);
        if let Some(from) = &self.from {
            text.push_str(&format!("from\t{from}\n"));
        }
        for path in &self.remove {
            text.push_str(&format!("remove\t{path}\n"));
        }
        for c in &self.components {
            let op = if ENTRY_COMPONENTS.contains(&c.id.as_str()) { "replace" } else { "install" };
            for f in &c.files {
                text.push_str(&format!("{op}\t{STAGED_DIR}/{}/{f}\t{}/{f}\n", c.id, c.install));
            }
        }
        text.push_str(&format!("install\t{STAGED_DIR}/{STAGED_INSTALLED}\t{}\n", crate::INSTALLED_PATH));
        text
    }

    /// Writes the new `installed.json`, the plan and last `staged.json`
    /// into `dir`, durably: each a temporary file, synced, then renamed
    /// into place. Every staged file must be synced first.
    pub fn write(&self, dir: &Path) -> io::Result<()> {
        let mut installed = serde_json::to_vec_pretty(&self.installed).map_err(io::Error::other)?;
        installed.push(b'\n');
        write_synced(dir, STAGED_INSTALLED, &installed)?;
        write_synced(dir, PLAN_FILE, self.plan_text().as_bytes())?;
        let mut json = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        json.push(b'\n');
        write_synced(dir, STAGED_FILE, &json)?;
        sync_dir(dir)
    }
}

/// The plan format the launcher reads; it refuses others.
pub const PLAN_FORMAT: u32 = 1;

/// The components whose files start Verso: the launcher, the KUAL menu
/// and the library scriptlet. Their paths must never be empty, even for a moment.
pub const ENTRY_COMPONENTS: [&str; 3] = ["launcher", "kual", "scriptlet"];

/// Writes `name` in `dir` through a synced temporary file and a rename.
fn write_synced(dir: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    let tmp = dir.join(format!("{name}.tmp"));
    let mut file = File::create(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(&tmp, dir.join(name))
}

/// Why a component couldn't be staged.
#[derive(Debug)]
pub enum StageError {
    Cancelled,
    /// The archive isn't what the manifest describes: unreadable, an unsafe
    /// or unlisted path, a link, a repeated or missing file, a wrong size or
    /// hash.
    Damaged(String),
    /// Writing failed (a full disk shows as `ErrorKind::StorageFull`).
    Io(io::Error),
}

impl fmt::Display for StageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StageError::Cancelled => write!(f, "cancelled"),
            StageError::Damaged(why) => write!(f, "damaged archive: {why}"),
            StageError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for StageError {}

/// Unpacks `component`'s `.tar.gz` (already checked against the manifest's
/// archive hash) into `dest`, which must not exist yet. Only regular files
/// listed in the manifest are accepted, each exactly once with the listed
/// size and hash; each gets the listed mode and is synced. `cancelled` is
/// asked between files and while copying. On an error `dest` may hold some
/// files; the caller removes it.
pub fn unpack(
    archive: impl Read,
    component: &Component,
    dest: &Path,
    cancelled: &mut dyn FnMut() -> bool,
) -> Result<(), StageError> {
    let damaged = |why: String| StageError::Damaged(format!("{}: {why}", component.id));
    let corrupt = |e: io::Error| damaged(format!("can't be read ({e})"));
    fs::create_dir(dest).map_err(StageError::Io)?;
    let expected: BTreeMap<&str, &crate::FileEntry> = component.files.iter().map(|f| (f.path.as_str(), f)).collect();
    let mut seen = BTreeSet::new();
    let mut tar = tar::Archive::new(GzDecoder::new(archive));
    for entry in tar.entries().map_err(corrupt)? {
        if cancelled() {
            return Err(StageError::Cancelled);
        }
        let mut entry = entry.map_err(corrupt)?;
        // Long GNU names and pax paths are already applied here.
        let raw = entry.path_bytes().into_owned();
        let path = String::from_utf8(raw).map_err(|_| damaged("a path isn't UTF-8".into()))?;
        let kind = entry.header().entry_type();
        if !kind.is_file() {
            return Err(damaged(format!("{path:?} isn't a plain file ({kind:?})")));
        }
        if !is_safe_path(&path) {
            return Err(damaged(format!("unsafe path {path:?}")));
        }
        let Some(file) = expected.get(path.as_str()) else {
            return Err(damaged(format!("{path:?} isn't in the manifest")));
        };
        if !seen.insert(path.clone()) {
            return Err(damaged(format!("{path:?} appears twice")));
        }
        let size = entry.header().entry_size().map_err(corrupt)?;
        if size != file.size {
            return Err(damaged(format!("{path:?} is {size} bytes, not {}", file.size)));
        }
        let target = dest.join(&file.path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(StageError::Io)?;
        }
        let mut out = OpenOptions::new().write(true).create_new(true).open(&target).map_err(StageError::Io)?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0; 64 * 1024];
        let mut copied = 0u64;
        loop {
            let n = entry.read(&mut buf).map_err(corrupt)?;
            if n == 0 {
                break;
            }
            if cancelled() {
                return Err(StageError::Cancelled);
            }
            hasher.update(&buf[..n]);
            out.write_all(&buf[..n]).map_err(StageError::Io)?;
            copied += n as u64;
        }
        if copied != file.size {
            return Err(damaged(format!("{path:?} is truncated")));
        }
        if crate::manifest::hex(&hasher.finalize()) != file.sha256 {
            return Err(damaged(format!("{path:?} doesn't match its hash")));
        }
        let mode = if file.mode == MODE_EXEC { MODE_EXEC } else { MODE_FILE };
        out.set_permissions(fs::Permissions::from_mode(mode)).map_err(StageError::Io)?;
        out.sync_all().map_err(StageError::Io)?;
    }
    if let Some(missing) = component.files.iter().find(|f| !seen.contains(&f.path)) {
        return Err(damaged(format!("{:?} is missing", missing.path)));
    }
    Ok(())
}

/// Syncs a folder after a rename. The Kindle's storage behind FUSE may
/// not support it, which is fine.
fn sync_dir(dir: &Path) -> io::Result<()> {
    match File::open(dir)?.sync_all() {
        Err(e) if matches!(e.raw_os_error(), Some(22 | 38)) => Ok(()), // EINVAL, ENOSYS
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Archive, FileEntry, component_hash, sha256_hex};
    use flate2::{Compression, write::GzEncoder};

    fn entry(path: &str, content: &[u8], mode: u32) -> FileEntry {
        FileEntry { path: path.into(), size: content.len() as u64, sha256: sha256_hex(content), mode }
    }

    fn component(files: Vec<FileEntry>) -> Component {
        Component {
            id: "fonts".into(),
            hash: component_hash("verso/fonts", &files),
            required: true,
            min_app: None,
            install: "verso/fonts".into(),
            description: String::new(),
            archive: Archive { url: "fonts.tar.gz".into(), size: 0, sha256: sha256_hex(b"") },
            files,
        }
    }

    fn fonts() -> Component {
        component(vec![
            entry("A.ttf", &[7; 100_000], MODE_FILE),
            entry("bin/tool", b"#!/bin/sh\n", MODE_EXEC),
            entry("licences/OFL.txt", b"SIL", MODE_FILE),
        ])
    }

    /// What an archive entry should be: raw header bytes are written
    /// directly, so unsafe names the `tar` builder refuses get through.
    #[derive(Clone, Copy)]
    enum E<'a> {
        File(&'a str, &'a [u8]),
        Link(&'a str, &'a str),
        Hard(&'a str, &'a str),
        Dir(&'a str),
        /// A pax header naming the next entry's path.
        PaxPath(&'a str),
    }

    fn archive(entries: &[E]) -> Vec<u8> {
        let mut tar = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));
        for e in entries {
            let mut h = tar::Header::new_ustar();
            let (name, kind, data): (&str, tar::EntryType, Vec<u8>) = match e {
                E::File(name, data) => (name, tar::EntryType::Regular, data.to_vec()),
                E::Link(name, to) | E::Hard(name, to) => {
                    h.set_link_name_literal(to).unwrap();
                    let kind = if matches!(e, E::Link(..)) { tar::EntryType::Symlink } else { tar::EntryType::Link };
                    (name, kind, Vec::new())
                }
                E::Dir(name) => (name, tar::EntryType::Directory, Vec::new()),
                E::PaxPath(path) => {
                    // "<length> path=<path>\n", where the length counts itself.
                    let body = format!(" path={path}\n");
                    let digits = (1..).find(|d| (body.len() + d).to_string().len() == *d).unwrap();
                    let record = format!("{}{body}", body.len() + digits);
                    ("pax", tar::EntryType::XHeader, record.into_bytes())
                }
            };
            let raw = &mut h.as_old_mut().name;
            raw.fill(0);
            raw[..name.len()].copy_from_slice(name.as_bytes());
            h.set_entry_type(kind);
            h.set_size(data.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            tar.append(&h, data.as_slice()).unwrap();
        }
        tar.into_inner().unwrap().finish().unwrap()
    }

    fn good() -> Vec<u8> {
        archive(&[
            E::File("A.ttf", &[7; 100_000]),
            E::File("bin/tool", b"#!/bin/sh\n"),
            E::File("licences/OFL.txt", b"SIL"),
        ])
    }

    fn never() -> impl FnMut() -> bool {
        || false
    }

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn unpacks_listed_files_with_their_modes() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("fonts");
        unpack(good().as_slice(), &fonts(), &dest, &mut never()).unwrap();
        assert_eq!(fs::read(dest.join("A.ttf")).unwrap(), [7; 100_000]);
        assert_eq!(fs::read(dest.join("licences/OFL.txt")).unwrap(), b"SIL");
        assert_eq!(mode(&dest.join("bin/tool")), 0o755);
        assert_eq!(mode(&dest.join("A.ttf")), 0o644);
        // The archive's own modes don't matter.
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1, "nothing outside dest");
    }

    #[test]
    fn real_release_archives_unpack() {
        let manifest = Manifest::parse(include_bytes!("../testdata/latest.json")).unwrap();
        for (id, bytes) in [
            ("app", &include_bytes!("../testdata/app-0.3.0.tar.gz")[..]),
            ("fonts", &include_bytes!("../testdata/fonts-0.3.0.tar.gz")[..]),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let c = manifest.component(id).unwrap();
            unpack(bytes, c, &dir.path().join(id), &mut never()).unwrap();
        }
    }

    #[test]
    fn refuses_damaged_or_unsafe_archives() {
        let big = [7u8; 100_000];
        let mut wrong = big;
        wrong[500] = 8;
        let mut truncated = good();
        truncated.truncate(truncated.len() / 2);
        let mut flipped = good();
        let mid = flipped.len() / 2;
        flipped[mid] ^= 0xff;
        let rest = [E::File("bin/tool", b"#!/bin/sh\n"), E::File("licences/OFL.txt", b"SIL")];
        let with = |first: E| archive(&[first, rest[0], rest[1]]);
        let cases: Vec<(&str, Vec<u8>, &str)> = vec![
            ("truncated", truncated, "can't be read"),
            // Usually unreadable, at worst a wrong hash: Damaged either way.
            ("flipped bytes", flipped, ""),
            ("not gzip", b"hello, this is not an archive".to_vec(), "can't be read"),
            ("absolute", with(E::File("/A.ttf", b"x")), "unsafe path"),
            ("parent", with(E::File("../A.ttf", b"x")), "unsafe path"),
            ("inner parent", with(E::File("licences/../A.ttf", b"x")), "unsafe path"),
            ("dot", with(E::File("./A.ttf", b"x")), "unsafe path"),
            ("symlink", with(E::Link("A.ttf", "/etc/passwd")), "isn't a plain file"),
            ("hard link", with(E::Hard("A.ttf", "/etc/passwd")), "isn't a plain file"),
            ("directory", with(E::Dir("licences/")), "isn't a plain file"),
            ("extra file", with(E::File("extra.txt", b"x")), "isn't in the manifest"),
            ("wrong size", with(E::File("A.ttf", &[7; 10])), "is 10 bytes"),
            ("wrong content", with(E::File("A.ttf", &wrong)), "doesn't match its hash"),
            ("missing", archive(&rest[..]), "\"A.ttf\" is missing"),
            ("twice", archive(&[E::File("licences/OFL.txt", b"SIL"), E::File("licences/OFL.txt", b"SIL")]), "twice"),
        ];
        for (what, bytes, expect) in cases {
            let dir = tempfile::tempdir().unwrap();
            let dest = dir.path().join("staged").join("fonts");
            fs::create_dir(dir.path().join("staged")).unwrap();
            match unpack(bytes.as_slice(), &fonts(), &dest, &mut never()) {
                Err(StageError::Damaged(why)) => assert!(why.contains(expect), "{what}: {why}"),
                other => panic!("{what}: {other:?}"),
            }
            // Nothing was written outside dest.
            let names: Vec<_> = fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
            assert_eq!(names, ["staged"], "{what}");
            assert_eq!(fs::read_dir(dir.path().join("staged")).unwrap().count(), 1, "{what}");
        }
    }

    #[test]
    fn pax_paths_are_checked_too() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = archive(&[E::PaxPath("../../evil"), E::File("A.ttf", &[7; 100_000])]);
        let err = unpack(bytes.as_slice(), &fonts(), &dir.path().join("fonts"), &mut never()).unwrap_err();
        assert!(matches!(&err, StageError::Damaged(why) if why.contains("evil")), "{err:?}");
        assert!(!dir.path().parent().unwrap().join("evil").exists());
    }

    #[test]
    fn cancels_between_and_within_files() {
        for at in [1, 2, 5] {
            let dir = tempfile::tempdir().unwrap();
            let mut asked = 0;
            let mut cancel = || {
                asked += 1;
                asked >= at
            };
            let err = unpack(good().as_slice(), &fonts(), &dir.path().join("fonts"), &mut cancel).unwrap_err();
            assert!(matches!(err, StageError::Cancelled), "{at}: {err:?}");
        }
    }

    #[test]
    fn refuses_an_existing_destination() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("fonts")).unwrap();
        let err = unpack(good().as_slice(), &fonts(), &dir.path().join("fonts"), &mut never()).unwrap_err();
        assert!(matches!(err, StageError::Io(_)), "{err:?}");
    }

    fn release(version: &str, components: Vec<Component>) -> Manifest {
        Manifest {
            format: FORMAT,
            version: version.parse().unwrap(),
            released: "2026-12-01".into(),
            notes: String::new(),
            details: String::new(),
            components,
        }
    }

    fn comp(id: &str, install: &str, files: &[&str]) -> Component {
        let files: Vec<FileEntry> = files.iter().map(|p| entry(p, p.as_bytes(), MODE_FILE)).collect();
        Component { id: id.into(), install: install.into(), hash: component_hash(install, &files), files, ..fonts() }
    }

    #[test]
    fn plan_lists_moves_removals_and_the_new_record() {
        let old = release(
            "0.2.0",
            vec![
                comp("app", "verso", &["verso"]),
                comp("fonts", "verso/fonts", &["A.ttf", "B.ttf", "OFL.txt"]),
                comp("data", "verso/data", &["x.bin", "moved.bin"]),
            ],
        );
        let mut installed = Installed::from_manifest(&old, &["app", "fonts", "data"], true);
        installed.components.push(InstalledComponent {
            id: "gone".into(),
            hash: "h".into(),
            install: "verso/gone".into(),
            files: vec!["g".into()],
        });
        // fonts drops B.ttf and moves folder for OFL; data's moved.bin goes to app.
        let new = release(
            "0.3.0",
            vec![
                comp("app", "verso", &["data/moved.bin", "verso"]),
                comp("fonts", "verso/fonts2", &["A.ttf", "OFL.txt"]),
                comp("data", "verso/data", &["x.bin"]),
                comp("launcher", "verso", &["verso.sh"]),
                comp("scriptlet", "documents", &["Verso.sh"]),
            ],
        );
        let staged = Staged::plan(
            &new,
            &["app".into(), "fonts".into(), "launcher".into(), "scriptlet".into()],
            Some(&installed),
        );
        assert_eq!(staged.version.to_string(), "0.3.0");
        assert_eq!(
            staged.components.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            ["app", "fonts", "launcher", "scriptlet"]
        );
        assert_eq!(staged.remove, ["verso/fonts/A.ttf", "verso/fonts/B.ttf", "verso/fonts/OFL.txt"]);
        let after = &staged.installed;
        assert!(!after.dev);
        assert_eq!(after.version.to_string(), "0.3.0");
        let ids: Vec<&str> = after.components.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["app", "fonts", "data", "launcher", "scriptlet", "gone"]);
        assert_eq!(after.component("fonts").unwrap().install, "verso/fonts2");
        assert_eq!(after.component("data").unwrap(), installed.component("data").unwrap(), "unchanged stays");
        // data's moved.bin is still listed by the old data record, and now
        // owned by app too: it isn't removed.
        let json = serde_json::to_vec(&staged).unwrap();
        assert_eq!(Staged::parse(&json).unwrap(), staged);
        assert_eq!(staged.from.as_ref().map(ToString::to_string).as_deref(), Some("0.2.0"));
        // The launcher's moves: removals, then each staged file, then the
        // record.
        assert_eq!(
            staged.plan_text(),
            "verso-plan\t1\nversion\t0.3.0\nfrom\t0.2.0\n\
             remove\tverso/fonts/A.ttf\nremove\tverso/fonts/B.ttf\nremove\tverso/fonts/OFL.txt\n\
             install\tverso/update/staged/app/data/moved.bin\tverso/data/moved.bin\n\
             install\tverso/update/staged/app/verso\tverso/verso\n\
             install\tverso/update/staged/fonts/A.ttf\tverso/fonts2/A.ttf\n\
             install\tverso/update/staged/fonts/OFL.txt\tverso/fonts2/OFL.txt\n\
             replace\tverso/update/staged/launcher/verso.sh\tverso/verso.sh\n\
             replace\tverso/update/staged/scriptlet/Verso.sh\tdocuments/Verso.sh\n\
             install\tverso/update/staged/installed.json\tverso/installed.json\n"
        );

        // Nothing installed: everything staged, nothing removed.
        let fresh = Staged::plan(&new, &ids_of(&new), None);
        assert!(fresh.remove.is_empty());
        assert_eq!(fresh.installed.components.len(), 5);
        assert!(fresh.plan_text().starts_with("verso-plan\t1\nversion\t0.3.0\ninstall\t"), "no from line");
    }

    fn ids_of(m: &Manifest) -> Vec<String> {
        m.components.iter().map(|c| c.id.clone()).collect()
    }

    #[test]
    fn writes_staged_json_last_and_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let m = release("0.3.0", vec![comp("app", "verso", &["verso"])]);
        let staged = Staged::plan(&m, &ids_of(&m), None);
        staged.write(dir.path()).unwrap();
        let mut names: Vec<_> = fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
        names.sort();
        assert_eq!(names, [STAGED_INSTALLED, PLAN_FILE, STAGED_FILE]);
        assert_eq!(Staged::parse(&fs::read(dir.path().join(STAGED_FILE)).unwrap()).unwrap(), staged);
        assert_eq!(fs::read_to_string(dir.path().join(PLAN_FILE)).unwrap(), staged.plan_text());
        let installed = Installed::parse(&fs::read(dir.path().join(STAGED_INSTALLED)).unwrap()).unwrap();
        assert_eq!(installed, staged.installed);
        assert!(Staged::parse(br#"{"format":2}"#).is_err());
    }
}
