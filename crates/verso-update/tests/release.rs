//! Packaging: reproducible archives and zips, stable component hashes, and
//! the checks `verso-release verify` makes on archives.
use std::{
    fs,
    io::{Read, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};
use tempfile::TempDir;
use verso_update::{
    INSTALLED_PATH, Installed, MODE_EXEC, MODE_FILE, Manifest,
    release::{self, ComponentSpec, PackOptions},
};

fn specs() -> Vec<ComponentSpec> {
    serde_json::from_str(
        r#"[
            {"id": "app", "install": "verso", "required": true},
            {"id": "launcher", "install": "verso", "required": true},
            {"id": "fonts", "install": "verso/fonts", "required": true},
            {"id": "extra", "install": "verso/extra", "required": false, "description": "Optional"}
        ]"#,
    )
    .unwrap()
}

fn write(path: &Path, content: &[u8], mode: u32) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

fn stage(root: &Path) -> PathBuf {
    let stage = root.join("stage");
    write(&stage.join("app/verso"), b"\x7fELF app", 0o775);
    write(&stage.join("launcher/verso.sh"), b"#!/bin/sh\n", 0o700);
    write(&stage.join("fonts/Literata-Regular.ttf"), &[7; 5000], 0o600);
    write(&stage.join("fonts/licences/OFL.txt"), b"SIL Open Font License", 0o644);
    write(&stage.join("extra/data.bin"), b"extra", 0o644);
    stage
}

fn pack(stage: &Path, out: &Path) -> Manifest {
    release::pack(&PackOptions {
        specs: &specs(),
        stage,
        out,
        version: "0.3.0".parse().unwrap(),
        released: "2026-12-01".into(),
        notes: "  Faster page turns.\n".into(),
    })
    .unwrap()
}

fn files(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            (e.file_name().into_string().unwrap(), fs::read(e.path()).unwrap())
        })
        .collect();
    out.sort();
    out
}

fn hashes(m: &Manifest) -> Vec<(String, String)> {
    m.components.iter().map(|c| (c.id.clone(), c.hash.clone())).collect()
}

#[test]
fn packs_a_release() {
    let tmp = TempDir::new().unwrap();
    let stage = stage(tmp.path());
    let out = tmp.path().join("out");
    let m = pack(&stage, &out);

    assert_eq!(m.notes, "Faster page turns.");
    let names: Vec<String> = files(&out).into_iter().map(|(n, _)| n).collect();
    assert_eq!(
        names,
        [
            "app-0.3.0.tar.gz",
            "extra-0.3.0.tar.gz",
            "fonts-0.3.0.tar.gz",
            "latest.json",
            "launcher-0.3.0.tar.gz",
            "verso-0.3.0.zip"
        ]
    );
    // latest.json is what pack returned, and parses with every check.
    assert_eq!(Manifest::parse(&fs::read(out.join("latest.json")).unwrap()).unwrap(), m);
    let fonts = m.component("fonts").unwrap();
    assert_eq!(
        fonts.files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(),
        ["Literata-Regular.ttf", "licences/OFL.txt"]
    );
    assert_eq!(m.component("app").unwrap().files[0].mode, MODE_EXEC);
    assert_eq!(m.component("launcher").unwrap().files[0].mode, MODE_EXEC);
    assert_eq!(fonts.files[0].mode, MODE_FILE);

    // Every archive holds exactly its component's files.
    for c in &m.components {
        let bytes = fs::read(out.join(&c.archive.url)).unwrap();
        assert_eq!(bytes.len() as u64, c.archive.size);
        release::check_archive(&bytes, c).unwrap();
    }
    // The installed.json for the same stage agrees with the manifest.
    let dev = release::installed(&specs(), &stage, "0.3.0".parse().unwrap(), true).unwrap();
    assert!(dev.dev);
    assert_eq!(dev.components.iter().map(|c| (c.id.clone(), c.hash.clone())).collect::<Vec<_>>(), hashes(&m));
}

#[test]
fn zip_installs_required_components() {
    let tmp = TempDir::new().unwrap();
    let out = tmp.path().join("out");
    let m = pack(&stage(tmp.path()), &out);
    let mut zip = zip::ZipArchive::new(fs::File::open(out.join("verso-0.3.0.zip")).unwrap()).unwrap();
    let names: Vec<String> = zip.file_names().map(str::to_string).collect();
    assert_eq!(
        names,
        [
            "verso/",
            "verso/fonts/",
            "verso/fonts/licences/",
            "verso/update/",
            "verso/fonts/Literata-Regular.ttf",
            "verso/fonts/licences/OFL.txt",
            "verso/installed.json",
            "verso/update/hand-install",
            "verso/verso",
            "verso/verso.sh"
        ]
    );
    assert_eq!(zip.by_name("verso/verso").unwrap().unix_mode().unwrap() & 0o777, 0o755);
    assert_eq!(zip.by_name("verso/installed.json").unwrap().unix_mode().unwrap() & 0o777, 0o644);
    let mut json = Vec::new();
    zip.by_name(INSTALLED_PATH).unwrap().read_to_end(&mut json).unwrap();
    let installed = Installed::parse(&json).unwrap();
    assert!(!installed.dev);
    assert_eq!(installed.version, m.version);
    // The optional component isn't installed by the zip.
    assert_eq!(installed.components.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), ["app", "launcher", "fonts"]);
}

#[test]
fn rebuilds_are_identical() {
    let tmp = TempDir::new().unwrap();
    let stage_a = stage(&tmp.path().join("a"));
    let a = pack(&stage_a, &tmp.path().join("out-a"));
    // A rebuild: the same content written again later, in another place,
    // with other permission bits that mean the same thing.
    std::thread::sleep(std::time::Duration::from_millis(20));
    let stage_b = stage(&tmp.path().join("b"));
    fs::set_permissions(stage_b.join("app/verso"), fs::Permissions::from_mode(0o755)).unwrap();
    fs::set_permissions(stage_b.join("fonts/Literata-Regular.ttf"), fs::Permissions::from_mode(0o644)).unwrap();
    let b = pack(&stage_b, &tmp.path().join("out-b"));
    assert_eq!(a, b);
    assert_eq!(files(&tmp.path().join("out-a")), files(&tmp.path().join("out-b")));
}

#[test]
fn hash_changes_only_for_the_changed_component() {
    let tmp = TempDir::new().unwrap();
    let base = hashes(&pack(&stage(&tmp.path().join("base")), &tmp.path().join("out-base")));
    type Change = Box<dyn Fn(&Path)>;
    let changes: Vec<(&str, Change)> = vec![
        ("app", Box::new(|s| write(&s.join("app/verso"), b"\x7fELF app 2", 0o755))),
        (
            "launcher",
            Box::new(|s| fs::set_permissions(s.join("launcher/verso.sh"), fs::Permissions::from_mode(0o644)).unwrap()),
        ),
        ("fonts", Box::new(|s| write(&s.join("fonts/Literata-Italic.ttf"), b"i", 0o644))),
        ("fonts", Box::new(|s| fs::remove_file(s.join("fonts/licences/OFL.txt")).unwrap())),
        ("fonts", Box::new(|s| fs::rename(s.join("fonts/licences"), s.join("fonts/licenses")).unwrap())),
        ("extra", Box::new(|s| write(&s.join("extra/data.bin"), b"extrA", 0o644))),
    ];
    for (i, (changed, change)) in changes.iter().enumerate() {
        let root = tmp.path().join(format!("case{i}"));
        let s = stage(&root);
        change(&s);
        let now = hashes(&pack(&s, &root.join("out")));
        for ((id, before), (_, after)) in base.iter().zip(&now) {
            assert_eq!(id == changed, before != after, "case {i}: component {id}");
        }
    }
}

#[test]
fn refuses_links_and_unsafe_names() {
    let tmp = TempDir::new().unwrap();
    let s = stage(tmp.path());
    std::os::unix::fs::symlink("/etc/passwd", s.join("app/passwd")).unwrap();
    assert!(release::scan(&s.join("app")).is_err());
    let s = stage(&tmp.path().join("2"));
    write(&s.join("app/what?.txt"), b"", 0o644);
    assert!(release::scan(&s.join("app")).is_err());
    let s = stage(&tmp.path().join("3"));
    fs::create_dir_all(s.join("empty")).unwrap();
    assert!(release::scan(&s.join("empty")).is_err());
}

#[test]
fn stage_must_match_the_component_list() {
    let tmp = TempDir::new().unwrap();
    let s = stage(tmp.path());
    let version: verso_update::Version = "0.3.0".parse().unwrap();
    release::installed(&specs(), &s, version.clone(), false).unwrap();
    write(&s.join("unlisted/x"), b"", 0o644);
    assert!(release::installed(&specs(), &s, version.clone(), false).is_err());
    fs::remove_dir_all(s.join("unlisted")).unwrap();
    fs::remove_dir_all(s.join("extra")).unwrap();
    assert!(release::installed(&specs(), &s, version, false).is_err());
}

#[test]
fn out_folder_must_be_empty() {
    let tmp = TempDir::new().unwrap();
    let s = stage(tmp.path());
    let out = tmp.path().join("out");
    write(&out.join("old.tar.gz"), b"", 0o644);
    assert!(
        release::pack(&PackOptions {
            specs: &specs(),
            stage: &s,
            out: &out,
            version: "0.3.0".parse().unwrap(),
            released: "2026-12-01".into(),
            notes: String::new(),
        })
        .is_err()
    );
}

/// A tar.gz with arbitrary entries, for archives a real release never has.
fn tar_gz(entries: &[(&str, tar::EntryType, &[u8], u32)]) -> Vec<u8> {
    let mut tar = tar::Builder::new(flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast()));
    for (path, kind, data, mode) in entries {
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(*kind);
        h.set_size(data.len() as u64);
        h.set_mode(*mode);
        if *kind == tar::EntryType::Symlink {
            h.set_link_name("/etc/passwd").unwrap();
        }
        // Bypass the builder's path checks, as a hostile archive would.
        let name = path.as_bytes();
        h.as_old_mut().name[..name.len()].copy_from_slice(name);
        h.set_cksum();
        tar.append(&h, *data).unwrap();
    }
    tar.into_inner().unwrap().finish().unwrap()
}

#[test]
fn check_archive_refuses_bad_archives() {
    let tmp = TempDir::new().unwrap();
    let out = tmp.path().join("out");
    let m = pack(&stage(tmp.path()), &out);
    let app = m.component("app").unwrap();
    let good = fs::read(out.join(&app.archive.url)).unwrap();
    release::check_archive(&good, app).unwrap();
    // The same content rebuilt by hand passes too.
    release::check_archive(&tar_gz(&[("verso", tar::EntryType::Regular, b"\x7fELF app", 0o755)]), app).unwrap();

    let regular = tar::EntryType::Regular;
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("truncated", good[..good.len() / 2].to_vec()),
        ("corrupt", {
            let mut b = good.clone();
            let mid = b.len() / 2;
            b[mid] ^= 0xff;
            b
        }),
        ("not gzip", b"verso".to_vec()),
        ("changed content", tar_gz(&[("verso", regular, b"\x7fELF evil", 0o755)])),
        ("changed mode", tar_gz(&[("verso", regular, b"\x7fELF app", 0o644)])),
        ("extra file", tar_gz(&[("verso", regular, b"\x7fELF app", 0o755), ("x", regular, b"", 0o644)])),
        ("missing file", tar_gz(&[])),
        ("twice", tar_gz(&[("verso", regular, b"\x7fELF app", 0o755), ("verso", regular, b"\x7fELF app", 0o755)])),
        ("traversal", tar_gz(&[("../verso", regular, b"\x7fELF app", 0o755)])),
        ("absolute", tar_gz(&[("/verso", regular, b"\x7fELF app", 0o755)])),
        ("link", tar_gz(&[("verso", tar::EntryType::Symlink, b"", 0o755)])),
        ("folder", tar_gz(&[("verso", tar::EntryType::Directory, b"", 0o755)])),
    ];
    for (what, bytes) in cases {
        assert!(release::check_archive(&bytes, app).is_err(), "{what}");
    }
}

#[test]
fn verify_release_checks_signature_then_archives() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    let copy = |name: &str| fs::copy(Path::new("testdata").join(name), dir.join(name)).unwrap();
    let key = include_str!("../testdata/test.pub");
    let err_of = |keys: &[&str]| format!("{:#}", release::verify_release(dir, keys).unwrap_err());

    copy("latest.json");
    assert!(err_of(&[key]).contains("not signed yet"));
    copy("latest.json.minisig");
    assert!(err_of(&[key]).contains("app-0.3.0.tar.gz"));
    copy("app-0.3.0.tar.gz");
    copy("fonts-0.3.0.tar.gz");
    let m = release::verify_release(dir, &[key]).unwrap();
    assert_eq!(m.version.to_string(), "0.3.0");

    assert!(err_of(&[include_str!("../testdata/other.pub")]).contains("unknown key"));
    let mut f = fs::OpenOptions::new().append(true).open(dir.join("fonts-0.3.0.tar.gz")).unwrap();
    f.write_all(b"x").unwrap();
    assert!(err_of(&[key]).contains("size"));
}
