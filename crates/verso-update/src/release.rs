//! Writing releases: staged component folders → reproducible archives,
//! `latest.json`, `installed.json`, the first-install zip and the KPM `.kpkg`.
//! Used by `verso-release` (scripts/package.sh and scripts/deploy.sh).
use crate::{
    Archive, Component, FORMAT, FileEntry, INSTALLED_PATH, Installed, InstalledComponent, MODE_EXEC, MODE_FILE,
    Manifest, Version, component_hash, is_safe_path, sha256_hex, verify_manifest,
};
use anyhow::{Context, Result, bail, ensure};
use flate2::{Compression, GzBuilder, read::GzDecoder};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{self, BufWriter, Read, Write},
    os::unix::fs::PermissionsExt,
    path::Path,
};
use zip::write::SimpleFileOptions;

/// Every archived and zipped file gets this time (2026-01-01 00:00 UTC), so
/// rebuilding the same files gives the same bytes.
pub const FIXED_MTIME: u64 = 1_767_225_600;

/// One entry of `dist/components.json`: a component's fixed properties. Its
/// files are whatever scripts/stage.sh put in `<stage>/<id>/`.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentSpec {
    pub id: String,
    pub install: String,
    pub required: bool,
    #[serde(default)]
    pub min_app: Option<Version>,
    #[serde(default)]
    pub description: String,
}

pub fn read_specs(path: &Path) -> Result<Vec<ComponentSpec>> {
    let text = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let specs: Vec<ComponentSpec> =
        serde_json::from_slice(&text).with_context(|| format!("parsing {}", path.display()))?;
    ensure!(!specs.is_empty(), "{} lists no components", path.display());
    Ok(specs)
}

/// The archive name of a component in a release.
pub fn archive_name(id: &str, version: &Version) -> String {
    format!("{id}-{version}.tar.gz")
}

/// The first-install zip's name.
pub fn zip_name(version: &Version) -> String {
    format!("verso-{version}.zip")
}

/// The KPM package's name.
pub fn kpkg_name(version: &Version) -> String {
    format!("verso-{version}.kpkg")
}

/// Lists the regular files under `dir`, sorted by path, with their sizes and
/// hashes. A file with any execute bit gets [`MODE_EXEC`], others
/// [`MODE_FILE`]. Links and other special files are refused.
pub fn scan(dir: &Path) -> Result<Vec<FileEntry>> {
    let mut files = Vec::new();
    walk(dir, "", &mut files)?;
    ensure!(!files.is_empty(), "{} has no files", dir.display());
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

fn walk(dir: &Path, prefix: &str, out: &mut Vec<FileEntry>) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry?;
        let name = entry.file_name().into_string().map_err(|n| anyhow::anyhow!("{n:?} isn't UTF-8"))?;
        let path = if prefix.is_empty() { name } else { format!("{prefix}/{name}") };
        let meta = fs::symlink_metadata(entry.path())?;
        if meta.is_dir() {
            walk(&entry.path(), &path, out)?;
        } else if meta.is_file() {
            ensure!(is_safe_path(&path), "{path:?} can't be installed on the Kindle");
            let bytes = fs::read(entry.path())?;
            let mode = if meta.permissions().mode() & 0o111 != 0 { MODE_EXEC } else { MODE_FILE };
            out.push(FileEntry { path, size: bytes.len() as u64, sha256: sha256_hex(&bytes), mode });
        } else {
            bail!("{} is a link or special file; components hold only regular files", entry.path().display());
        }
    }
    Ok(())
}

/// Writes `files` (from `dir`) as a reproducible `.tar.gz`: sorted, regular
/// files only, owner 0, a fixed time, no gzip name or time. Returns the
/// archive's size and hash, with `url` set to its file name.
pub fn write_archive(dir: &Path, files: &[FileEntry], out: &Path) -> Result<Archive> {
    let file = BufWriter::new(File::create(out).with_context(|| format!("creating {}", out.display()))?);
    let gz = GzBuilder::new().mtime(0).operating_system(255).write(file, Compression::best());
    let mut tar = tar::Builder::new(gz);
    for f in files {
        let bytes = fs::read(dir.join(&f.path))?;
        ensure!(sha256_hex(&bytes) == f.sha256, "{} changed while packaging", f.path);
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(bytes.len() as u64);
        header.set_mode(f.mode);
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(FIXED_MTIME);
        tar.append_data(&mut header, &f.path, bytes.as_slice())?;
    }
    tar.into_inner()?.finish()?.into_inner().map_err(|e| e.into_error())?.sync_all()?;
    let bytes = fs::read(out)?;
    let url = out.file_name().unwrap().to_string_lossy().into_owned();
    Ok(Archive { url, size: bytes.len() as u64, sha256: sha256_hex(&bytes) })
}

/// What `scripts/package.sh` asks for.
pub struct PackOptions<'a> {
    pub specs: &'a [ComponentSpec],
    /// Holds `<id>/` for every component.
    pub stage: &'a Path,
    /// Created; must not exist yet or be empty.
    pub out: &'a Path,
    pub version: Version,
    pub released: String,
    pub notes: String,
    /// When set, holds the KPM hooks (`install.sh`, `launch.sh`, `uninstall.sh`)
    /// and `pack` also emits `verso-<version>.kpkg`.
    pub kpm: Option<&'a Path>,
}

/// Builds a release in `out`: one archive per component, `latest.json`, the
/// first-install zip (the required components plus `installed.json`), and
/// optionally `verso-<version>.kpkg`. Signing is a separate step.
pub fn pack(opts: &PackOptions) -> Result<Manifest> {
    check_stage(opts.specs, opts.stage)?;
    fs::create_dir_all(opts.out)?;
    ensure!(fs::read_dir(opts.out)?.next().is_none(), "{} isn't empty", opts.out.display());
    let mut components = Vec::new();
    for spec in opts.specs {
        let dir = opts.stage.join(&spec.id);
        let files = scan(&dir)?;
        let archive = write_archive(&dir, &files, &opts.out.join(archive_name(&spec.id, &opts.version)))?;
        components.push(Component {
            id: spec.id.clone(),
            hash: component_hash(&spec.install, &files),
            required: spec.required,
            min_app: spec.min_app.clone(),
            install: spec.install.clone(),
            description: spec.description.clone(),
            archive,
            files,
        });
    }
    let manifest = Manifest {
        format: FORMAT,
        version: opts.version.clone(),
        released: opts.released.clone(),
        notes: opts.notes.trim().to_string(),
        components,
    };
    manifest.validate()?;
    let mut json = serde_json::to_vec_pretty(&manifest)?;
    json.push(b'\n');
    fs::write(opts.out.join("latest.json"), json)?;

    let required: Vec<&str> = manifest.components.iter().filter(|c| c.required).map(|c| c.id.as_str()).collect();
    let installed = Installed::from_manifest(&manifest, &required, false);
    let entries = collect_install_entries(opts.stage, &manifest, &installed)?;
    write_zip(&entries, &opts.out.join(zip_name(&opts.version)))?;
    if let Some(kpm_dir) = opts.kpm {
        write_kpkg(kpm_dir, &entries, &opts.version, &opts.out.join(kpkg_name(&opts.version)))?;
    }
    Ok(manifest)
}

/// The `installed.json` for the staged components without making archives,
/// for `scripts/deploy.sh`.
pub fn installed(specs: &[ComponentSpec], stage: &Path, version: Version, dev: bool) -> Result<Installed> {
    check_stage(specs, stage)?;
    let mut components = Vec::new();
    for spec in specs {
        let files = scan(&stage.join(&spec.id))?;
        components.push(InstalledComponent {
            id: spec.id.clone(),
            hash: component_hash(&spec.install, &files),
            install: spec.install.clone(),
            files: files.into_iter().map(|f| f.path).collect(),
        });
    }
    Ok(Installed { format: FORMAT, version, dev, components })
}

/// The stage must hold a folder for every component and nothing else, so a
/// component added to scripts/stage.sh but not to dist/components.json (or
/// the other way round) is noticed.
fn check_stage(specs: &[ComponentSpec], stage: &Path) -> Result<()> {
    let mut found = BTreeSet::new();
    for entry in fs::read_dir(stage).with_context(|| format!("reading {}", stage.display()))? {
        found.insert(entry?.file_name().to_string_lossy().into_owned());
    }
    let listed: BTreeSet<String> = specs.iter().map(|s| s.id.clone()).collect();
    ensure!(listed.len() == specs.len(), "a component is listed twice");
    let missing: Vec<&String> = listed.difference(&found).collect();
    let extra: Vec<&String> = found.difference(&listed).collect();
    ensure!(missing.is_empty(), "the stage has no files for {missing:?}");
    ensure!(extra.is_empty(), "the stage has {extra:?}, which no component lists");
    Ok(())
}

/// Collects every file installed on a fresh Kindle install (required components
/// plus `verso/installed.json` and `verso/update/hand-install`), sorted by path.
/// Shared by `write_zip` and `write_kpkg` so both install the exact same files.
fn collect_install_entries(
    stage: &Path,
    manifest: &Manifest,
    installed: &Installed,
) -> Result<BTreeMap<String, (Vec<u8>, u32)>> {
    let mut entries: BTreeMap<String, (Vec<u8>, u32)> = BTreeMap::new();
    for c in manifest.components.iter().filter(|c| installed.component(&c.id).is_some()) {
        for f in &c.files {
            let bytes = fs::read(stage.join(&c.id).join(&f.path))?;
            ensure!(sha256_hex(&bytes) == f.sha256, "{} changed while packaging", f.path);
            entries.insert(format!("{}/{}", c.install, f.path), (bytes, f.mode));
        }
    }
    let mut json = serde_json::to_vec_pretty(installed)?;
    json.push(b'\n');
    ensure!(
        entries.insert(INSTALLED_PATH.to_string(), (json, MODE_FILE)).is_none(),
        "a component owns {INSTALLED_PATH}"
    );
    ensure!(
        entries.insert(crate::launcher::HAND_INSTALL_PATH.to_string(), (Vec::new(), MODE_FILE)).is_none(),
        "a component owns {}",
        crate::launcher::HAND_INSTALL_PATH
    );
    Ok(entries)
}

/// A zip that mirrors the Kindle's storage: unpacked at its root, it installs
/// Verso. Folders, then files, sorted, with fixed times.
fn write_zip(entries: &BTreeMap<String, (Vec<u8>, u32)>, out: &Path) -> Result<()> {
    let mut folders = BTreeSet::new();
    for path in entries.keys() {
        let mut p = path.as_str();
        while let Some((parent, _)) = p.rsplit_once('/') {
            folders.insert(format!("{parent}/"));
            p = parent;
        }
    }
    let time = zip::DateTime::from_date_and_time(2026, 1, 1, 0, 0, 0).unwrap();
    let options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .compression_level(Some(9))
        .last_modified_time(time);
    let mut zip = zip::ZipWriter::new(BufWriter::new(File::create(out)?));
    for folder in folders {
        zip.add_directory(folder, options.unix_permissions(0o755))?;
    }
    for (path, (bytes, mode)) in entries {
        zip.start_file(path, options.unix_permissions(*mode))?;
        zip.write_all(bytes)?;
    }
    zip.finish()?.into_inner().map_err(|e| e.into_error())?.sync_all()?;
    Ok(())
}

/// KPM v2 package manifest (`manifest.json` at the root of a `.kpkg`).
/// KPM's `version` field is strictly three integers `[major, minor, patch]`
/// (no pre-release suffix); `payload/verso/installed.json` carries the full
/// Verso version string.
#[derive(Serialize)]
struct KpmManifest<'a> {
    manifest_version: u32,
    id: &'a str,
    name: &'a str,
    author: &'a str,
    description: &'a str,
    version: [u64; 3],
    dependencies: [&'a str; 0],
    supported_platforms: [&'a str; 1],
}

const KPM_HOOKS: [&str; 3] = ["install.sh", "launch.sh", "uninstall.sh"];

/// Reproducible KPM v2 `.kpkg` (`.tar.gz`) containing `manifest.json`,
/// `files.txt`, the POSIX `sh` hooks (`install.sh`, `launch.sh`, `uninstall.sh`)
/// and `payload/` holding the exact files shipped in the first-install zip.
fn write_kpkg(kpm_dir: &Path, entries: &BTreeMap<String, (Vec<u8>, u32)>, version: &Version, out: &Path) -> Result<()> {
    let mut hooks = Vec::with_capacity(KPM_HOOKS.len());
    for name in KPM_HOOKS {
        let path = kpm_dir.join(name);
        let meta = fs::symlink_metadata(&path).with_context(|| format!("reading {}", path.display()))?;
        ensure!(meta.is_file(), "{} must be a regular file", path.display());
        let bytes = fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
        hooks.push((name, bytes));
    }

    let kpm_manifest = KpmManifest {
        manifest_version: 2,
        id: "verso",
        name: "Verso",
        author: "Verso",
        description: "A calm, lightweight e-reader for jailbroken Kindles",
        version: [version.major, version.minor, version.patch],
        dependencies: [],
        supported_platforms: ["kindlehf"],
    };
    let mut manifest_json = serde_json::to_vec_pretty(&kpm_manifest)?;
    manifest_json.push(b'\n');

    let mut files_txt = String::new();
    for path in entries.keys() {
        files_txt.push_str(path);
        files_txt.push('\n');
    }

    let mut folders = BTreeSet::new();
    folders.insert("payload/".to_string());
    for path in entries.keys() {
        let full = format!("payload/{path}");
        let mut p = full.as_str();
        while let Some((parent, _)) = p.rsplit_once('/') {
            folders.insert(format!("{parent}/"));
            p = parent;
        }
    }

    let file = BufWriter::new(File::create(out).with_context(|| format!("creating {}", out.display()))?);
    let gz = GzBuilder::new().mtime(0).operating_system(255).write(file, Compression::best());
    let mut tar = tar::Builder::new(gz);
    // Put manifest.json first so KPM's Internal_GetManifest reads it without
    // decompressing the payload binaries.
    append_tar_file(&mut tar, "manifest.json", &manifest_json, MODE_FILE)?;
    append_tar_file(&mut tar, "files.txt", files_txt.as_bytes(), MODE_FILE)?;
    for (name, bytes) in &hooks {
        append_tar_file(&mut tar, name, bytes, MODE_EXEC)?;
    }
    for folder in folders {
        append_tar_dir(&mut tar, &folder)?;
    }
    for (path, (bytes, mode)) in entries {
        append_tar_file(&mut tar, &format!("payload/{path}"), bytes, *mode)?;
    }
    tar.into_inner()?.finish()?.into_inner().map_err(|e| e.into_error())?.sync_all()?;
    Ok(())
}

fn append_tar_file<W: Write>(tar: &mut tar::Builder<W>, path: &str, bytes: &[u8], mode: u32) -> Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Regular);
    header.set_size(bytes.len() as u64);
    header.set_mode(mode);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(FIXED_MTIME);
    tar.append_data(&mut header, path, bytes)?;
    Ok(())
}

fn append_tar_dir<W: Write>(tar: &mut tar::Builder<W>, path: &str) -> Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Directory);
    header.set_size(0);
    header.set_mode(0o755);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(FIXED_MTIME);
    tar.append_data(&mut header, path, io::empty())?;
    Ok(())
}

/// Checks a release folder before publishing: the signature with `keys`, then
/// every archive's size and hash, and that each archive holds exactly the
/// component's files with the listed hashes and modes.
pub fn verify_release(dir: &Path, keys: &[&str]) -> Result<Manifest> {
    let manifest_bytes = fs::read(dir.join("latest.json"))?;
    let signature =
        fs::read_to_string(dir.join("latest.json.minisig")).context("reading latest.json.minisig (not signed yet?)")?;
    let manifest = verify_manifest(&manifest_bytes, &signature, keys)?;
    for c in &manifest.components {
        let bytes = fs::read(dir.join(&c.archive.url)).with_context(|| format!("reading {}", c.archive.url))?;
        ensure!(bytes.len() as u64 == c.archive.size, "{}: size differs from the manifest", c.archive.url);
        ensure!(sha256_hex(&bytes) == c.archive.sha256, "{}: hash differs from the manifest", c.archive.url);
        check_archive(&bytes, c).with_context(|| c.archive.url.clone())?;
    }
    Ok(manifest)
}

/// Reads a component archive without unpacking it and checks it holds
/// exactly the component's files.
pub fn check_archive(archive: &[u8], component: &Component) -> Result<()> {
    let expected: BTreeMap<&str, &FileEntry> = component.files.iter().map(|f| (f.path.as_str(), f)).collect();
    let mut seen = BTreeSet::new();
    let mut tar = tar::Archive::new(GzDecoder::new(archive));
    for entry in tar.entries()? {
        let mut entry = entry?;
        ensure!(entry.header().entry_type() == tar::EntryType::Regular, "holds something other than a file");
        let path = entry.path()?.to_str().context("a path isn't UTF-8")?.to_string();
        let file = expected.get(path.as_str()).with_context(|| format!("{path:?} isn't in the manifest"))?;
        ensure!(seen.insert(path.clone()), "{path:?} appears twice");
        ensure!(entry.header().mode()? == file.mode, "{path:?} has the wrong mode");
        let mut hasher = Sha256::new();
        let size = io::copy(&mut entry.by_ref().take(file.size + 1), &mut HashWriter(&mut hasher))?;
        ensure!(size == file.size, "{path:?} has the wrong size");
        ensure!(crate::manifest::hex(&hasher.finalize()) == file.sha256, "{path:?} has the wrong hash");
    }
    ensure!(seen.len() == expected.len(), "files are missing");
    Ok(())
}

struct HashWriter<'a>(&'a mut Sha256);

impl Write for HashWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.update(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
