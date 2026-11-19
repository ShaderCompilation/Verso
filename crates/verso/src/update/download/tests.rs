//! Downloading and staging against a local server, with releases built by
//! `verso-release`'s packing code. The manifests here aren't signed:
//! `run` takes a manifest that was already verified.
use super::*;
use crate::update::fetch::tests::{Reply, Server};
use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;
use tempfile::TempDir;
use verso_update::release::{self, ComponentSpec, PackOptions};
use verso_update::stage::STAGED_FILE;

/// A packed release in a folder, as the server serves it.
struct Release {
    dir: TempDir,
    manifest: Manifest,
}

impl Release {
    fn archive(&self, id: &str) -> Vec<u8> {
        fs::read(self.dir.path().join(&self.manifest.component(id).unwrap().archive.url)).unwrap()
    }
}

/// An app of `app_size` bytes (several download chunks) and two fonts.
fn build(version: &str, app_size: usize) -> Release {
    let specs: Vec<ComponentSpec> = serde_json::from_str(
        r#"[
            {"id": "app", "install": "verso", "required": true},
            {"id": "fonts", "install": "verso/fonts", "required": true}
        ]"#,
    )
    .unwrap();
    let stage = tempfile::tempdir().unwrap();
    let write = |path: &str, bytes: &[u8], mode: u32| {
        let path = stage.path().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, bytes).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
    };
    // Barely compressible, so the archive is about as large.
    let mut x = 0x2545_f491_u32;
    let app: Vec<u8> = (0..app_size)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as u8
        })
        .collect();
    write("app/verso", &app, 0o755);
    write("fonts/Literata.ttf", format!("font {version}").as_bytes(), 0o644);
    write("fonts/OFL.txt", b"SIL Open Font License", 0o644);
    let dir = tempfile::tempdir().unwrap();
    let manifest = release::pack(&PackOptions {
        specs: &specs,
        stage: stage.path(),
        out: dir.path(),
        version: version.parse().unwrap(),
        released: "2026-12-01".into(),
        notes: String::new(),
    })
    .unwrap();
    Release { dir, manifest }
}

/// Serves `release` under `/rel/`, with `replace` overriding a component's
/// archive reply.
fn serve(release: &Release, replace: Vec<(&str, Reply)>) -> Server {
    let mut routes: Vec<(String, Reply)> = release
        .manifest
        .components
        .iter()
        .map(|c| (format!("/rel/{}", c.archive.url), Reply::Ranged(release.archive(&c.id))))
        .collect();
    for (id, reply) in replace {
        let path = format!("/rel/{}", release.manifest.component(id).unwrap().archive.url);
        routes.retain(|(p, _)| *p != path);
        routes.push((path, reply));
    }
    Server::start(routes.iter().map(|(p, r)| (p.as_str(), r.clone())).collect())
}

/// A storage root with an installed version and a book: everything that
/// must stay as it is.
fn device() -> TempDir {
    let root = tempfile::tempdir().unwrap();
    for (path, bytes) in [
        ("verso/verso", &b"old app"[..]),
        ("verso/fonts/Literata.ttf", b"old font"),
        ("verso/installed.json", b"{}"),
        ("documents/book.epub", b"a book"),
    ] {
        let path = root.path().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    root
}

/// Every file under `root` outside `verso/update`.
fn installed_files(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let rel = path.strip_prefix(root).unwrap().to_string_lossy().into_owned();
            if rel == "verso/update" {
                continue;
            }
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                out.insert(rel, fs::read(&path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

fn client() -> Client {
    Client::for_downloads(Duration::from_millis(500), Duration::from_millis(500), false)
}

const PLENTY: u64 = 1 << 40;

/// Runs a download of every component of `release` from `server`.
fn stage_all(
    root: &Path,
    release: &Release,
    server: &Server,
    free: u64,
    report: &mut dyn FnMut(Step) -> bool,
) -> Result<Staged, DownloadError> {
    let ids: Vec<String> = release.manifest.components.iter().map(|c| c.id.clone()).collect();
    let free_space = move |_: &Path| Ok(free);
    let client = client();
    let job = Job {
        root,
        manifest: &release.manifest,
        manifest_url: &server.url("/rel/latest.json"),
        components: &ids,
        installed: None,
        client: &client,
        free_space: &free_space,
    };
    run(&job, report)
}

fn staged(root: &Path) -> PathBuf {
    root.join(STAGED_DIR)
}

fn downloads(root: &Path) -> Vec<String> {
    match fs::read_dir(root.join(DOWNLOAD_DIR)) {
        Ok(entries) => {
            let mut names: Vec<_> = entries.map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
            names.sort();
            names
        }
        Err(_) => Vec::new(),
    }
}

fn all(_: Step) -> bool {
    true
}

#[test]
fn stages_every_file_and_writes_the_record_last() {
    let rel = build("0.3.0", 300_000);
    let server = serve(&rel, vec![]);
    let root = device();
    let before = installed_files(root.path());
    let mut steps = Vec::new();
    let staged_record = stage_all(root.path(), &rel, &server, PLENTY, &mut |s| {
        steps.push(s);
        true
    })
    .unwrap();
    let dir = staged(root.path());
    assert_eq!(fs::read(dir.join("app/verso")).unwrap().len(), 300_000);
    assert_eq!(fs::metadata(dir.join("app/verso")).unwrap().permissions().mode() & 0o777, 0o755);
    assert_eq!(fs::read(dir.join("fonts/Literata.ttf")).unwrap(), b"font 0.3.0");
    assert_eq!(fs::metadata(dir.join("fonts/OFL.txt")).unwrap().permissions().mode() & 0o777, 0o644);
    let record = Staged::parse(&fs::read(dir.join(STAGED_FILE)).unwrap()).unwrap();
    assert_eq!(record, staged_record);
    assert_eq!(record.version.to_string(), "0.3.0");
    assert_eq!(record.components.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), ["app", "fonts"]);
    assert!(downloads(root.path()).is_empty(), "archives are deleted once staged");
    assert_eq!(installed_files(root.path()), before, "the installed version is untouched");
    // Steps in order, with progress reaching the total.
    assert_eq!(steps.first(), Some(&Step::Preparing));
    assert_eq!(steps.last(), Some(&Step::Finishing));
    let total: u64 = rel.manifest.components.iter().map(|c| c.archive.size).sum();
    assert!(steps.contains(&Step::Downloading { done: total, total }));
    assert!(steps.iter().filter(|s| matches!(s, Step::Downloading { .. })).count() > 3, "progress per chunk");
    assert!(steps.contains(&Step::Unpacking { done: 2, total: 2 }));
    // The archives came from the folder the manifest was requested from.
    assert_eq!(server.paths(), ["/rel/app-0.3.0.tar.gz", "/rel/fonts-0.3.0.tar.gz"]);
}

#[test]
fn an_interrupted_download_continues_where_it_stopped() {
    let rel = build("0.3.0", 300_000);
    let root = device();
    let cut = serve(&rel, vec![("app", Reply::Cut(rel.archive("app"), 100_000))]);
    let err = stage_all(root.path(), &rel, &cut, PLENTY, &mut all).unwrap_err();
    assert!(matches!(err, DownloadError::Network(_)), "{err:?}");
    assert!(!staged(root.path()).exists());
    let app = &rel.manifest.component("app").unwrap().archive;
    let part = root.path().join(DOWNLOAD_DIR).join(format!("{}.tar.gz", app.sha256));
    assert_eq!(fs::metadata(&part).unwrap().len(), 100_000, "the part is kept");

    let server = serve(&rel, vec![]);
    stage_all(root.path(), &rel, &server, PLENTY, &mut all).unwrap();
    let ranges = server.ranges.lock().unwrap().clone();
    assert_eq!(ranges[0], ("/rel/app-0.3.0.tar.gz".into(), Some("bytes=100000-".into())));
    assert!(staged(root.path()).join(STAGED_FILE).exists());
}

#[test]
fn a_tampered_archive_is_refused_and_deleted() {
    let rel = build("0.3.0", 50_000);
    let root = device();
    let before = installed_files(root.path());
    let mut bad = rel.archive("fonts");
    let last = bad.len() - 1;
    bad[last] ^= 1;
    let server = serve(&rel, vec![("fonts", Reply::Ranged(bad))]);
    let err = stage_all(root.path(), &rel, &server, PLENTY, &mut all).unwrap_err();
    assert!(matches!(&err, DownloadError::Damaged(why) if why.contains("fonts")), "{err:?}");
    assert!(!staged(root.path()).exists());
    let app = &rel.manifest.component("app").unwrap().archive;
    assert_eq!(downloads(root.path()), [format!("{}.tar.gz", app.sha256)], "only the good archive is kept");
    assert_eq!(installed_files(root.path()), before);
    // Fixed on the server: the next try finishes, without the app again.
    let server = serve(&rel, vec![]);
    stage_all(root.path(), &rel, &server, PLENTY, &mut all).unwrap();
    assert_eq!(server.paths(), ["/rel/fonts-0.3.0.tar.gz"]);
}

#[test]
fn a_short_or_missing_archive_fails_cleanly() {
    let rel = build("0.3.0", 50_000);
    let root = device();
    let before = installed_files(root.path());
    let short = rel.archive("fonts")[..40].to_vec();
    for reply in [Reply::Ok(short), Reply::Status(404)] {
        let server = serve(&rel, vec![("fonts", reply)]);
        let err = stage_all(root.path(), &rel, &server, PLENTY, &mut all).unwrap_err();
        assert!(matches!(err, DownloadError::Network(_)), "{err:?}");
        assert!(!staged(root.path()).exists());
        assert_eq!(installed_files(root.path()), before);
    }
}

/// A release whose fonts archive is replaced by `entries`, with the
/// manifest's archive hash updated to match: as if a broken release had
/// been signed.
fn with_fonts_archive(rel: &mut Release, archive: Vec<u8>) {
    let c = rel.manifest.components.iter_mut().find(|c| c.id == "fonts").unwrap();
    c.archive.size = archive.len() as u64;
    c.archive.sha256 = verso_update::sha256_hex(&archive);
    fs::write(rel.dir.path().join(&c.archive.url), archive).unwrap();
}

fn tar_gz(entries: &[(&str, tar::EntryType, &[u8])]) -> Vec<u8> {
    use flate2::{Compression, write::GzEncoder};
    let mut tar = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));
    for (name, kind, data) in entries {
        let mut h = tar::Header::new_ustar();
        let raw = &mut h.as_old_mut().name;
        raw[..name.len()].copy_from_slice(name.as_bytes());
        h.set_entry_type(*kind);
        if *kind == tar::EntryType::Symlink {
            h.set_link_name_literal("/etc/passwd").unwrap();
        }
        h.set_size(data.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        tar.append(&h, *data).unwrap();
    }
    tar.into_inner().unwrap().finish().unwrap()
}

#[test]
fn broken_archives_matching_their_hash_are_still_refused() {
    use tar::EntryType::{Regular, Symlink};
    let font = b"font 0.3.0".as_slice();
    let ofl = b"SIL Open Font License".as_slice();
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("traversal", tar_gz(&[("../../evil", Regular, font), ("OFL.txt", Regular, ofl)])),
        ("absolute", tar_gz(&[("/tmp/evil", Regular, font), ("OFL.txt", Regular, ofl)])),
        ("link", tar_gz(&[("Literata.ttf", Symlink, b""), ("OFL.txt", Regular, ofl)])),
        ("extra", tar_gz(&[("Literata.ttf", Regular, font), ("OFL.txt", Regular, ofl), ("x", Regular, b"x")])),
        ("missing", tar_gz(&[("OFL.txt", Regular, ofl)])),
        ("truncated", tar_gz(&[("Literata.ttf", Regular, font), ("OFL.txt", Regular, ofl)])[..30].to_vec()),
    ];
    for (what, archive) in cases {
        let mut rel = build("0.3.0", 1000);
        with_fonts_archive(&mut rel, archive);
        let root = device();
        let before = installed_files(root.path());
        let server = serve(&rel, vec![]);
        let err = stage_all(root.path(), &rel, &server, PLENTY, &mut all).unwrap_err();
        assert!(matches!(err, DownloadError::Damaged(_)), "{what}: {err:?}");
        assert!(!staged(root.path()).exists(), "{what}");
        assert_eq!(downloads(root.path()).len(), 1, "{what}: the broken archive is deleted, the app kept");
        assert_eq!(installed_files(root.path()), before, "{what}");
        assert!(!root.path().join("evil").exists() && !root.path().join("verso/evil").exists(), "{what}");
    }
}

#[test]
fn not_enough_space_changes_nothing() {
    let rel = build("0.3.0", 300_000);
    let server = serve(&rel, vec![]);
    let root = device();
    let total: u64 = rel.manifest.components.iter().map(|c| c.archive.size).sum();
    let unpacked: u64 = rel.manifest.components.iter().flat_map(|c| &c.files).map(|f| f.size).sum();
    let needed = total + unpacked + SPACE_MARGIN;
    let err = stage_all(root.path(), &rel, &server, needed - 1, &mut all).unwrap_err();
    assert_eq!(err, DownloadError::NotEnoughSpace { needed, free: needed - 1 });
    assert!(server.paths().is_empty(), "nothing downloaded");
    assert!(!staged(root.path()).exists());
    assert_eq!(err.plain(), "The update needs 0.1 MB more space on the Kindle. Remove some books and try again.");
    // A part already downloaded counts.
    let app = &rel.manifest.component("app").unwrap().archive;
    fs::write(root.path().join(DOWNLOAD_DIR).join(format!("{}.tar.gz", app.sha256)), &rel.archive("app")[..1000])
        .unwrap();
    stage_all(root.path(), &rel, &server, needed - 1000, &mut all).unwrap();
}

#[test]
fn a_full_disk_removes_the_downloads_too() {
    assert_eq!(storage(io::Error::from_raw_os_error(libc::ENOSPC)), DownloadError::DiskFull);
    assert!(matches!(storage(io::Error::from_raw_os_error(libc::EIO)), DownloadError::Storage(_)));
    let rel = build("0.3.0", 1000);
    let server = serve(&rel, vec![]);
    let root = device();
    let part = root.path().join(DOWNLOAD_DIR).join(format!("{}.tar.gz", rel.manifest.components[0].archive.sha256));
    fs::create_dir_all(part.parent().unwrap()).unwrap();
    fs::write(&part, b"x").unwrap();
    fs::create_dir_all(staged(root.path()).join("app")).unwrap();
    // The space check is where this one fills up.
    let ids: Vec<String> = vec!["app".into()];
    let client = client();
    let free_space = |_: &Path| Err(io::Error::from_raw_os_error(libc::ENOSPC));
    let job = Job {
        root: root.path(),
        manifest: &rel.manifest,
        manifest_url: &server.url("/rel/latest.json"),
        components: &ids,
        installed: None,
        client: &client,
        free_space: &free_space,
    };
    assert_eq!(run(&job, &mut all).unwrap_err(), DownloadError::DiskFull);
    assert!(!root.path().join(DOWNLOAD_DIR).exists());
    assert!(!staged(root.path()).exists());
}

#[test]
fn cancels_at_every_step_and_resumes() {
    let rel = build("0.3.0", 300_000);
    let total: u64 = rel.manifest.components.iter().map(|c| c.archive.size).sum();
    let app_size = rel.manifest.component("app").unwrap().archive.size;
    type At = Box<dyn Fn(&Step) -> bool>;
    let points: Vec<(&str, At)> = vec![
        ("preparing", Box::new(|s| *s == Step::Preparing)),
        ("first chunk", Box::new(|s| matches!(s, Step::Downloading { done, .. } if *done > 0))),
        ("between archives", Box::new(move |s| matches!(s, Step::Downloading { done, .. } if *done == app_size))),
        ("downloaded", Box::new(move |s| *s == Step::Downloading { done: total, total })),
        ("unpacking", Box::new(|s| matches!(s, Step::Unpacking { done: 0, .. }))),
        ("second component", Box::new(|s| matches!(s, Step::Unpacking { done: 1, .. }))),
        ("unpacked", Box::new(|s| matches!(s, Step::Unpacking { done: 2, .. }))),
        ("finishing", Box::new(|s| *s == Step::Finishing)),
    ];
    for (what, at) in points {
        let root = device();
        let before = installed_files(root.path());
        let server = serve(&rel, vec![]);
        let err = stage_all(root.path(), &rel, &server, PLENTY, &mut |s| !at(&s)).unwrap_err();
        assert_eq!(err, DownloadError::Cancelled, "{what}");
        assert!(!staged(root.path()).exists(), "{what}");
        assert_eq!(installed_files(root.path()), before, "{what}");
        let kept = downloads(root.path()).len();
        // Then the person tries again, and it finishes from what was kept.
        let again = serve(&rel, vec![]);
        stage_all(root.path(), &rel, &again, PLENTY, &mut all).unwrap();
        assert!(staged(root.path()).join(STAGED_FILE).exists(), "{what}");
        let fetched = again.paths().len();
        match what {
            "preparing" => assert_eq!((kept, fetched), (0, 2), "{what}"),
            "first chunk" => {
                assert_eq!(fetched, 2, "{what}");
                assert!(again.ranges.lock().unwrap()[0].1.is_some(), "{what}: continued with a range");
            }
            "between archives" => assert_eq!(fetched, 1, "{what}"),
            _ => assert_eq!((kept, fetched), (2, 0), "{what}: nothing downloaded again"),
        }
    }
}

#[test]
fn replaces_leftovers_from_earlier_attempts() {
    let rel = build("0.3.0", 1000);
    let server = serve(&rel, vec![]);
    let root = device();
    // A part from another release, and a complete staged update from before.
    let download = root.path().join(DOWNLOAD_DIR);
    fs::create_dir_all(&download).unwrap();
    fs::write(download.join("0000.tar.gz"), b"old").unwrap();
    fs::create_dir_all(staged(root.path()).join("old")).unwrap();
    fs::write(staged(root.path()).join(STAGED_FILE), b"{}").unwrap();
    // Stop before downloading: the leftovers are gone already.
    stage_all(root.path(), &rel, &server, PLENTY, &mut |s| !matches!(s, Step::Downloading { .. })).unwrap_err();
    assert!(downloads(root.path()).iter().all(|n| n != "0000.tar.gz"));
    assert!(!staged(root.path()).exists());
    stage_all(root.path(), &rel, &server, PLENTY, &mut all).unwrap();
    let mut names: Vec<_> = fs::read_dir(staged(root.path())).unwrap().map(|e| e.unwrap().file_name()).collect();
    names.sort();
    assert_eq!(names, ["app", "fonts", "installed.json", "plan.txt", "staged.json"]);
}

#[test]
fn archive_addresses_and_free_space() {
    let base = "https://github.com/x/y/releases/latest/download/latest.json";
    assert_eq!(
        archive_url(base, "app-0.3.0.tar.gz"),
        "https://github.com/x/y/releases/latest/download/app-0.3.0.tar.gz"
    );
    assert_eq!(archive_url(base, "https://example.org/a.tar.gz"), "https://example.org/a.tar.gz");
    assert_eq!(archive_url("http://localhost:8000/", "a.tar.gz"), "http://localhost:8000/a.tar.gz");
    let dir = tempfile::tempdir().unwrap();
    assert!(free_space(dir.path()).unwrap() > 0);
    assert!(free_space(&dir.path().join("missing")).is_err());
}

#[test]
fn messages_are_plain() {
    assert_eq!(
        DownloadError::LowBattery(12).plain(),
        "The battery is at 12%. Charge it to 20% or plug in a charger to update."
    );
    assert_eq!(
        DownloadError::Network(FetchError::Timeout).plain(),
        "The download was interrupted. Try again to continue it."
    );
    assert_eq!(
        DownloadError::Check(CheckError::WifiOff).plain(),
        "Turn on Wi-Fi in Settings → Device, then try again."
    );
}
