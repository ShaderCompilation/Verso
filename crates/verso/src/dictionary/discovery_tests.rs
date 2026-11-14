use super::*;
use crate::dictionary::merged_preferences;
use std::{
    os::unix::fs::{PermissionsExt, symlink},
    sync::atomic::{AtomicU64, AtomicUsize, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "verso-discovery-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        // Real path, so expected external IDs match on systems where temp is a link.
        Self(fs::canonicalize(root).unwrap())
    }
    fn join(&self, path: &str) -> PathBuf {
        self.0.join(path)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
/// A valid one-word dictionary at `ifo` (`.ifo` path), named `name`.
fn install(ifo: &Path, name: &str) {
    fs::create_dir_all(ifo.parent().unwrap()).unwrap();
    let mut idx = b"word\0".to_vec();
    idx.extend_from_slice(&0u32.to_be_bytes());
    idx.extend_from_slice(&4u32.to_be_bytes());
    let base = basename(ifo).unwrap();
    fs::write(sibling(&base, "idx"), &idx).unwrap();
    fs::write(sibling(&base, "dict"), b"text").unwrap();
    fs::write(
        ifo,
        format!(
            "StarDict's dict ifo file\nversion=3.0.0\nbookname={name}\nwordcount=1\nidxfilesize={}\nsametypesequence=m\n",
            idx.len()
        ),
    )
    .unwrap();
}
fn scan(native: &Path, external: &[PathBuf], preferences: &[Preference]) -> Catalog {
    discover_catalog(native, external, preferences, &|| true).unwrap()
}
fn ids(catalog: &Catalog) -> Vec<String> {
    catalog.dictionaries.iter().map(|d| d.id.clone()).collect()
}
fn statuses(catalog: &Catalog) -> Vec<SourceStatus> {
    catalog.sources.iter().map(|s| s.status.clone()).collect()
}
fn id(path: PathBuf) -> String {
    path.to_string_lossy().into_owned()
}
fn pref(id: &str, disabled: bool) -> Preference {
    Preference { id: id.into(), disabled }
}

#[test]
fn native_relative_and_external_absolute_ids_with_legacy_preferences_and_order() {
    let t = Temp::new();
    let native = t.join("data/dictionaries");
    install(&native.join("en/a.ifo"), "Alpha");
    install(&native.join("z.ifo"), "Zulu");
    let koreader = t.join("koreader/dict");
    install(&koreader.join("b.ifo"), "Beta");
    install(&koreader.join("c.ifo"), "Alpha");
    // `.ifo.gz`-only external dictionary: the ID is its plain `.ifo` path.
    install(&koreader.join("g.ifo"), "Gamma");
    let text = fs::read(koreader.join("g.ifo")).unwrap();
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    std::io::Write::write_all(&mut gz, &text).unwrap();
    fs::write(koreader.join("g.ifo.gz"), gz.finish().unwrap()).unwrap();
    fs::remove_file(koreader.join("g.ifo")).unwrap();
    let c = id(koreader.join("c.ifo"));
    // Legacy native IDs and new external IDs side by side, plus preferences
    // for a missing native dictionary and a removed external folder.
    let preferences =
        [pref(&c, true), pref("missing.ifo", true), pref("en/a.ifo", true), pref("/removed/x.ifo", false)];
    let catalog = scan(&native, std::slice::from_ref(&koreader), &preferences);
    assert_eq!(
        ids(&catalog),
        [c.as_str(), "en/a.ifo", &id(koreader.join("b.ifo")), &id(koreader.join("g.ifo")), "z.ifo"],
        "preferences first, then name/path order"
    );
    let disabled: Vec<_> = catalog.dictionaries.iter().map(|d| d.disabled).collect();
    assert_eq!(disabled, [true, true, false, false, false]);
    assert!(catalog.dictionaries.iter().all(|d| d.error.is_none()));
    assert_eq!(catalog.dictionaries[3].path, koreader.join("g.ifo.gz"));
    assert_eq!(statuses(&catalog), [SourceStatus::Available, SourceStatus::Available]);
    assert!(catalog.complete());
    // Removing KOReader's folder keeps its preferences for when it returns.
    let native_only = scan(&native, &[], &preferences);
    let merged = merged_preferences(&native_only.dictionaries, preferences.to_vec());
    assert_eq!(
        merged.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
        ["en/a.ifo", "z.ifo", &c, "missing.ifo", "/removed/x.ifo"]
    );
    assert!(merged[2].disabled);
    let restored = scan(&native, &[koreader], &merged);
    assert_eq!(restored.dictionaries[0].id, "en/a.ifo");
    assert!(restored.dictionaries.iter().find(|d| d.id == c).unwrap().disabled);
}

#[test]
fn overlapping_roots_aliases_symlinks_and_same_name_copies() {
    let t = Temp::new();
    let native = t.join("native");
    let parent = t.join("parent");
    install(&parent.join("x.ifo"), "Same");
    install(&parent.join("child/y.ifo"), "Same");
    install(&parent.join("child/copy/x.ifo"), "Same");
    install(&t.join("outside/z.ifo"), "Outside");
    // A nested directory symlink is not followed; a file symlink is not listed.
    symlink(t.join("outside"), parent.join("linked")).unwrap();
    symlink(parent.join("x.ifo"), parent.join("child/link.ifo")).unwrap();
    symlink(&parent, t.join("alias")).unwrap();
    let expected = vec![id(parent.join("child/copy/x.ifo")), id(parent.join("child/y.ifo")), id(parent.join("x.ifo"))];
    for roots in [
        vec![parent.clone(), parent.join("child")],
        vec![parent.join("child"), parent.clone()],
        vec![t.join("alias"), parent.join("child"), parent.clone()],
        vec![t.join("alias/child"), t.join("alias")],
    ] {
        let catalog = scan(&native, &roots, &[]);
        assert_eq!(ids(&catalog), expected, "{roots:?}");
        assert!(catalog.dictionaries.iter().all(|d| d.name == "Same"), "same-name copies stay distinct");
        assert_eq!(catalog.sources[0].status, SourceStatus::Absent, "absent native folder is normal");
        assert!(catalog.sources[1..].iter().all(|s| s.status == SourceStatus::Available));
        assert_eq!(catalog.sources.iter().skip(1).map(|s| s.root.clone()).collect::<Vec<_>>(), roots);
    }
    // An explicitly configured root may itself be a symlink to a directory.
    assert_eq!(scan(&native, &[parent.join("linked")], &[]).dictionaries[0].id, id(t.join("outside/z.ifo")));
    // A `res/` folder is skipped inside a parent but scanned when configured.
    install(&parent.join("res/r.ifo"), "Resource");
    let catalog = scan(&native, &[parent.clone(), parent.join("res")], &[]);
    assert_eq!(catalog.dictionaries.len(), 4);
    assert!(ids(&catalog).contains(&id(parent.join("res/r.ifo"))));
}

#[test]
fn native_identity_wins_for_a_file_also_reached_externally() {
    let t = Temp::new();
    let native = t.join("data/dictionaries");
    install(&native.join("en/a.ifo"), "Alpha");
    symlink(&native, t.join("native-alias")).unwrap();
    for roots in [vec![t.join("data")], vec![native.join("en")], vec![t.join("native-alias")]] {
        let catalog = scan(&native, &roots, &[pref("en/a.ifo", true)]);
        assert_eq!(ids(&catalog), ["en/a.ifo"], "{roots:?}");
        assert!(catalog.dictionaries[0].disabled);
        assert_eq!(catalog.dictionaries[0].path, native.join("en/a.ifo"));
        assert!(catalog.complete());
    }
}

#[test]
fn absent_and_unreadable_sources_do_not_hide_working_ones() {
    let t = Temp::new();
    let native = t.join("native");
    install(&native.join("n.ifo"), "Native");
    install(&t.join("good/g.ifo"), "Good");
    fs::write(t.join("file"), b"not a folder").unwrap();
    install(&t.join("partly/ok.ifo"), "Readable");
    install(&t.join("partly/locked/hidden.ifo"), "Hidden");
    install(&t.join("locked/l.ifo"), "Locked");
    let lock = |path: &Path, mode| fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    lock(&t.join("locked"), 0o000);
    lock(&t.join("partly/locked"), 0o000);
    let enforced = fs::read_dir(t.join("locked")).is_err(); // false when run as root
    let roots = [t.join("missing"), t.join("file"), t.join("locked"), t.join("partly"), t.join("good")];
    let catalog = scan(&native, &roots, &[]);
    lock(&t.join("locked"), 0o755);
    lock(&t.join("partly/locked"), 0o755);
    let status = statuses(&catalog);
    assert_eq!(status[0], SourceStatus::Available);
    assert_eq!(status[1], SourceStatus::Absent);
    assert!(matches!(&status[2], SourceStatus::Unreadable(e) if e.contains("not a folder")), "{status:?}");
    assert_eq!(status[5], SourceStatus::Available);
    let found = ids(&catalog);
    assert!(found.contains(&"n.ifo".to_owned()) && found.contains(&id(t.join("good/g.ifo"))));
    assert!(found.contains(&id(t.join("partly/ok.ifo"))));
    if enforced {
        assert!(matches!(&status[3], SourceStatus::Unreadable(_)), "{status:?}");
        assert!(matches!(&status[4], SourceStatus::Unreadable(e) if e.contains("locked")), "{status:?}");
        assert_eq!(found.len(), 3);
        assert!(!catalog.complete());
    }
}

#[test]
fn limits_apply_across_sources_and_are_reported() {
    let t = Temp::new();
    let native = t.join("native");
    install(&native.join("n.ifo"), "Native");
    install(&t.join("one/a.ifo"), "A");
    install(&t.join("one/b.ifo"), "B");
    install(&t.join("two/c.ifo"), "C");
    let roots = [t.join("one"), t.join("two")];
    let limited = |limits| discover_limited(&native, &roots, &[], &|| true, limits).unwrap();
    // Dictionaries: two in total, so the first external folder stops the scan.
    let catalog = limited(Limits { dictionaries: 2, ..LIMITS });
    assert_eq!(catalog.dictionaries.len(), 2);
    assert_eq!(statuses(&catalog), [SourceStatus::Available, SourceStatus::LimitReached, SourceStatus::LimitReached]);
    assert!(!catalog.complete());
    // Entries: each source has 3–4 files per dictionary; the budget is shared.
    let catalog = limited(Limits { entries: 6, ..LIMITS });
    assert_eq!(catalog.sources[0].status, SourceStatus::Available);
    assert_eq!(catalog.sources[2].status, SourceStatus::LimitReached);
    assert!(!catalog.complete());
    assert_eq!(limited(LIMITS).dictionaries.len(), 4);
    // Depth is per source tree: a deep dictionary marks only its own source.
    install(&t.join("two/deep/er/d.ifo"), "Deep");
    let catalog = limited(Limits { depth: 1, ..LIMITS });
    assert_eq!(statuses(&catalog), [SourceStatus::Available, SourceStatus::Available, SourceStatus::LimitReached]);
    assert_eq!(catalog.dictionaries.len(), 4);
    assert!(limited(LIMITS).complete());
}

#[test]
fn cancellation_never_returns_a_partial_catalog() {
    let t = Temp::new();
    let native = t.join("native");
    install(&native.join("n.ifo"), "Native");
    install(&t.join("ext/e.ifo"), "External");
    let roots = [t.join("ext")];
    assert!(discover_catalog(&native, &roots, &[], &|| false).is_err());
    for after in 1..8 {
        let calls = AtomicUsize::new(0);
        let current = || calls.fetch_add(1, Ordering::Relaxed) < after;
        assert!(discover_catalog(&native, &roots, &[], &current).is_err(), "cancelled after {after} checks");
    }
    assert_eq!(scan(&native, &roots, &[]).dictionaries.len(), 2);
}

#[test]
fn current_worker_discovery_keeps_native_only_behavior() {
    let t = Temp::new();
    let native = t.join("native");
    assert!(discover_current(&native, &[], &|| true).unwrap().is_empty(), "absent folder is empty");
    install(&native.join("n.ifo"), "Native");
    assert_eq!(discover_current(&native, &[], &|| true).unwrap()[0].id, "n.ifo");
    fs::write(t.join("file"), b"").unwrap();
    assert!(discover_current(&t.join("file"), &[], &|| true).is_err());
}
