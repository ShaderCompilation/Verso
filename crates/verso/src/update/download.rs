//! Downloading and staging an update: the changed components' archives are
//! downloaded into `update/download/`, checked against the verified
//! manifest, unpacked into `update/staged/` and recorded in `staged.json`,
//! written last. Nothing outside `update/` is touched; installing is the
//! launcher's job. See docs/updates.md.
use std::fs;
use std::io::{self, BufReader};
use std::path::{Path, PathBuf};

use verso_update::stage::{self, DOWNLOAD_DIR, STAGED_DIR, StageError, Staged};
use verso_update::{Component, Installed, Manifest, Version};

use super::CheckError;
use super::fetch::{Client, FetchError, SaveError};

/// Updating needs this much battery, or a charger.
pub const MIN_BATTERY: u8 = 20;
/// Free space kept on top of the archives and their unpacked files.
pub const SPACE_MARGIN: u64 = 20_000_000;

/// Where a download is, for the progress screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// Checking the battery, Wi-Fi, the release and free space.
    Preparing,
    /// Bytes of all archives, including parts kept from an earlier try.
    Downloading { done: u64, total: u64 },
    /// Components unpacked so far.
    Unpacking { done: usize, total: usize },
    /// Writing `staged.json`.
    Finishing,
}

/// Why an update couldn't be downloaded and staged. The installed version
/// is unchanged in every case.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DownloadError {
    /// Wi-Fi, or fetching and verifying the release, as for a check.
    Check(CheckError),
    /// Below [`MIN_BATTERY`] percent and not charging.
    LowBattery(u8),
    /// Found before downloading: bytes needed in all, and free.
    NotEnoughSpace {
        needed: u64,
        free: u64,
    },
    /// The storage filled up while writing.
    DiskFull,
    /// The release is no longer newer than what's installed.
    UpToDate,
    /// The server now offers another version than the one asked for.
    ReleaseChanged(Version),
    /// An archive's download failed; what arrived is kept for a retry.
    Network(FetchError),
    /// An archive doesn't match the signed release.
    Damaged(String),
    /// Reading or writing the storage failed.
    Storage(String),
    Cancelled,
}

impl DownloadError {
    /// For the update screen: what happened and, where it helps, what to do.
    pub fn plain(&self) -> String {
        match self {
            DownloadError::Check(e) => e.plain(),
            DownloadError::LowBattery(percent) => {
                format!("The battery is at {percent}%. Charge it to {MIN_BATTERY}% or plug in a charger to update.")
            }
            DownloadError::NotEnoughSpace { needed, free } => format!(
                "Not enough space: the update needs {} more. Remove some books and try again.",
                super::megabytes(needed.saturating_sub(*free))
            ),
            DownloadError::DiskFull => {
                "The Kindle’s storage filled up during the download. Remove some books and try again.".into()
            }
            DownloadError::UpToDate => "Verso is already up to date.".into(),
            DownloadError::ReleaseChanged(v) => format!("Verso {v} came out in the meantime. Update again to get it."),
            DownloadError::Network(_) => "The download was interrupted. Try again to continue it.".into(),
            DownloadError::Damaged(_) => {
                "The downloaded update was damaged, so nothing was changed. Try again later.".into()
            }
            DownloadError::Storage(_) => "Couldn’t save the update on the Kindle’s storage.".into(),
            DownloadError::Cancelled => "Update cancelled.".into(),
        }
    }
}

fn storage(e: io::Error) -> DownloadError {
    if e.kind() == io::ErrorKind::StorageFull { DownloadError::DiskFull } else { DownloadError::Storage(e.to_string()) }
}

/// Bytes free on the filesystem holding a path ([`free_space`] outside tests).
pub type FreeSpace = dyn Fn(&Path) -> io::Result<u64> + Send + Sync;

/// One download: a verified manifest and the components to stage from it.
pub(crate) struct Job<'a> {
    /// The storage root (`/mnt/us`).
    pub root: &'a Path,
    pub manifest: &'a Manifest,
    /// The address the manifest was requested from: archive addresses are
    /// relative to it.
    pub manifest_url: &'a str,
    pub components: &'a [String],
    pub installed: Option<&'a Installed>,
    pub client: &'a Client,
    pub free_space: &'a FreeSpace,
}

/// Downloads, verifies and stages `job`. `report` hears each step (often,
/// while downloading) and returns false to cancel. On failure the staging
/// folder is removed; downloaded archives are kept for a retry unless they
/// were damaged or the storage filled up.
pub(crate) fn run(job: &Job, report: &mut dyn FnMut(Step) -> bool) -> Result<Staged, DownloadError> {
    let download = job.root.join(DOWNLOAD_DIR);
    let staged = job.root.join(STAGED_DIR);
    let result = stage(job, &download, &staged, report);
    let remove = |dir: &Path| {
        if let Err(e) = fs::remove_dir_all(dir)
            && e.kind() != io::ErrorKind::NotFound
        {
            log::error!("removing {}: {e}", dir.display());
        }
    };
    match &result {
        Ok(_) => remove(&download),
        Err(e) => {
            log::warn!("update download failed: {e:?}");
            remove(&staged);
            if *e == DownloadError::DiskFull {
                remove(&download);
            }
        }
    }
    result
}

fn stage(
    job: &Job,
    download: &Path,
    staged: &Path,
    report: &mut dyn FnMut(Step) -> bool,
) -> Result<Staged, DownloadError> {
    step(report, Step::Preparing)?;
    let components: Vec<&Component> = job
        .components
        .iter()
        .map(|id| job.manifest.component(id).ok_or_else(|| DownloadError::Damaged(format!("no component {id:?}"))))
        .collect::<Result<_, _>>()?;
    // A staged update from before is replaced, even if it was complete.
    match fs::remove_dir_all(staged) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(storage(e)),
        _ => {}
    }
    fs::create_dir_all(download).map_err(storage)?;
    let archives: Vec<PathBuf> = components.iter().map(|c| download.join(archive_file(c))).collect();
    forget_other_downloads(download, &archives);

    // Space for the rest of the archives, all unpacked files and a margin.
    let have: Vec<u64> = archives
        .iter()
        .zip(&components)
        .map(|(path, c)| fs::metadata(path).map_or(0, |m| m.len()).min(c.archive.size))
        .collect();
    let total: u64 = components.iter().map(|c| c.archive.size).sum();
    let unpacked: u64 = components.iter().flat_map(|c| &c.files).map(|f| f.size).sum();
    let needed = total - have.iter().sum::<u64>() + unpacked + SPACE_MARGIN;
    let free = (job.free_space)(job.root).map_err(storage)?;
    if free < needed {
        return Err(DownloadError::NotEnoughSpace { needed, free });
    }

    let mut before = 0;
    for (c, path) in components.iter().zip(&archives) {
        let url = archive_url(job.manifest_url, &c.archive.url);
        let saved = job
            .client
            .save(&url, path, c.archive.size, &mut |length| report(Step::Downloading { done: before + length, total }));
        match saved {
            Ok(()) => {}
            Err(SaveError::Cancelled) => return Err(DownloadError::Cancelled),
            Err(SaveError::Fetch(e)) => return Err(DownloadError::Network(e)),
            Err(SaveError::Io(e)) => return Err(storage(e)),
        }
        step(report, Step::Downloading { done: before + c.archive.size, total })?;
        if let Err(why) = check_archive(path, c) {
            let _ = fs::remove_file(path);
            return Err(why);
        }
        before += c.archive.size;
    }

    fs::create_dir_all(staged).map_err(storage)?;
    for (i, (c, path)) in components.iter().zip(&archives).enumerate() {
        step(report, Step::Unpacking { done: i, total: components.len() })?;
        let file = fs::File::open(path).map_err(storage)?;
        let mut cancelled = || !report(Step::Unpacking { done: i, total: components.len() });
        match stage::unpack(BufReader::new(file), c, &staged.join(&c.id), &mut cancelled) {
            Ok(()) => {}
            Err(StageError::Cancelled) => return Err(DownloadError::Cancelled),
            Err(StageError::Damaged(why)) => {
                // Signed and matching its hash, yet not what the manifest
                // lists: a broken release. Don't keep it.
                let _ = fs::remove_file(path);
                return Err(DownloadError::Damaged(why));
            }
            Err(StageError::Io(e)) => return Err(storage(e)),
        }
    }
    step(report, Step::Unpacking { done: components.len(), total: components.len() })?;

    step(report, Step::Finishing)?;
    let record = Staged::plan(job.manifest, job.components, job.installed);
    record.write(staged).map_err(storage)?;
    Ok(record)
}

/// Reports a step; an error when the person cancelled.
fn step(report: &mut dyn FnMut(Step) -> bool, s: Step) -> Result<(), DownloadError> {
    if report(s) { Ok(()) } else { Err(DownloadError::Cancelled) }
}

/// The local name of a component's archive: its hash, so a part left from
/// another release is never continued.
fn archive_file(c: &Component) -> String {
    format!("{}.tar.gz", c.archive.sha256)
}

/// Removes everything in the download folder except `keep`.
fn forget_other_downloads(dir: &Path, keep: &[PathBuf]) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if !keep.contains(&path) {
            let removed = if path.is_dir() { fs::remove_dir_all(&path) } else { fs::remove_file(&path) };
            if let Err(e) = removed {
                log::warn!("removing {}: {e}", path.display());
            }
        }
    }
}

/// Checks a downloaded archive's size and hash against the manifest.
fn check_archive(path: &Path, c: &Component) -> Result<(), DownloadError> {
    use sha2::{Digest, Sha256};
    let mut file = fs::File::open(path).map_err(storage)?;
    let mut hasher = Sha256::new();
    let size = io::copy(&mut file, &mut hasher).map_err(storage)?;
    let hash: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
    if size != c.archive.size || hash != c.archive.sha256 {
        return Err(DownloadError::Damaged(format!("{}: the archive doesn't match the release", c.id)));
    }
    Ok(())
}

/// An archive's address: relative to the folder the manifest was requested
/// from (not where a redirect led), unless it is a full address.
pub fn archive_url(manifest_url: &str, archive: &str) -> String {
    if archive.contains("://") {
        return archive.to_owned();
    }
    let base = manifest_url.rfind('/').map_or(manifest_url, |i| &manifest_url[..=i]);
    format!("{base}{archive}")
}

/// Bytes an unprivileged writer can use on the filesystem holding `path`.
pub fn free_space(path: &Path) -> io::Result<u64> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(io::Error::other)?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `path` is a valid C string and `stat` a writable statvfs.
    if unsafe { libc::statvfs(path.as_ptr(), &mut stat) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // The field types differ between targets (u32 on 32-bit ARM).
    #[allow(clippy::unnecessary_cast)]
    Ok(stat.f_bavail as u64 * stat.f_frsize as u64)
}

#[cfg(test)]
mod tests;
