//! Problem and crash reports.
//!
//! Reports are built by `verso/tools/report.sh` (POSIX sh + BusyBox awk) so the
//! same script works from Settings, from KUAL when Verso can't start, and from
//! the launcher after a crash.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::update::Paths;

/// A crash report written since the user last dismissed one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CrashNotice {
    /// File name under `verso/reports/`, e.g. `crash-20261122-123000.txt`.
    pub filename: String,
}

impl CrashNotice {
    /// Display path relative to the Kindle's USB storage root.
    pub fn display_path(&self) -> String {
        format!("verso/reports/{}", self.filename)
    }
}

/// Where crash and problem reports live (`<storage_root>/verso/reports`).
pub fn reports_dir(data: &Path) -> PathBuf {
    Paths::for_device(data).root.join("verso/reports")
}

/// The marker file recording the newest crash report already seen by the user.
fn seen_path(data: &Path) -> PathBuf {
    data.join("crash-seen.txt")
}

/// Returns the newest crash report if the user hasn't dismissed it yet.
pub fn newest_unseen_crash(data: &Path) -> Option<CrashNotice> {
    let dir = reports_dir(data);
    let entries = std::fs::read_dir(&dir).ok()?;
    let mut crashes: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            if name.starts_with("crash-") && name.ends_with(".txt") && e.file_type().ok()?.is_file() {
                Some(name)
            } else {
                None
            }
        })
        .collect();
    // Filenames are crash-YYYYMMDD-HHMMSS[_NN].txt; reverse lexical order is newest first.
    crashes.sort();
    let newest = crashes.pop()?;
    let seen = std::fs::read_to_string(seen_path(data)).ok().map(|s| s.trim().to_owned());
    if seen.as_deref() == Some(newest.as_str()) {
        return None;
    }
    Some(CrashNotice { filename: newest })
}

/// Marks `filename` as seen so the Home notice isn't shown again for this crash.
pub fn dismiss_crash(data: &Path, filename: &str) -> Result<()> {
    let path = seen_path(data);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("txt.tmp");
    std::fs::write(&tmp, format!("{filename}\n"))?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Locates `report.sh`: installed under `<storage_root>/verso/tools/report.sh`,
/// with a checkout fallback on desktop so the simulator can run it too.
fn report_script(storage_root: &Path) -> Option<(PathBuf, Option<PathBuf>)> {
    let installed = storage_root.join("verso/tools/report.sh");
    if installed.is_file() {
        return Some((installed, None));
    }
    #[cfg(feature = "desktop")]
    {
        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let script = repo_root.join("dist/kindle/verso/tools/report.sh");
        let collector = repo_root.join("scripts/collect-diagnostics.sh");
        if script.is_file() {
            return Some((script, collector.is_file().then_some(collector)));
        }
    }
    None
}

/// Runs `report.sh report` and returns the created report's filename (under
/// `verso/reports/`).
pub fn create_report(data: &Path) -> Result<String> {
    let storage_root = Paths::for_device(data).root;
    let (script, collector) = report_script(&storage_root)
        .context("report.sh not found; reinstall Verso to restore verso/tools/report.sh")?;
    let mut cmd = Command::new("sh");
    cmd.arg(&script).arg("report").env("VERSO_STORAGE_ROOT", &storage_root).env("VERSO_DATA", data);
    if let Some(c) = collector {
        cmd.env("VERSO_COLLECTOR", c);
    }
    let out = cmd.output().with_context(|| format!("running {}", script.display()))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!("report.sh failed ({}): {}", out.status, err.trim());
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let created =
        stdout.lines().last().map(str::trim).filter(|s| !s.is_empty()).context("report.sh printed no path")?;
    let name = Path::new(created)
        .file_name()
        .and_then(|n| n.to_str())
        .map(str::to_owned)
        .context("invalid report filename")?;
    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn script_path() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../dist/kindle/verso/tools/report.sh")
    }

    fn write_fake_collector(dir: &Path) -> PathBuf {
        let p = dir.join("collect-diagnostics.sh");
        fs::write(&p, "#!/bin/sh\necho 'Verso device diagnostics - format 1'\necho 'Fake hardware ok'\n").unwrap();
        p
    }

    #[test]
    fn problem_report_has_all_sections_in_order_and_redacts_books() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let app = root.join("verso");
        let data = app.join("data");
        let update = app.join("update");
        let prev = update.join("previous");
        fs::create_dir_all(&data).unwrap();
        fs::create_dir_all(&prev).unwrap();

        fs::write(app.join("installed.json"), r#"{"format":1,"version":"0.1.0","dev":true,"components":[]}"#).unwrap();
        fs::write(
            data.join("settings.json"),
            r#"{
                "sleep_screen": "cover",
                "library_folders": ["/mnt/us/documents", "/mnt/us/Secret Shelf"],
                "page_flash": 6,
                "hostname": "my-private-kindle",
                "translate_provider": "deepl",
                "deepl_api_key": "12345678-1234-1234-1234-1234567890ab:fx",
                "orientation": "portrait"
            }"#,
        )
        .unwrap();
        fs::write(prev.join("plan.txt"), "verso-plan\t1\nversion\t0.1.0\nfrom\t0.0.9\n").unwrap();
        fs::write(
            update.join("rollback.json"),
            r#"{"format":1,"from":"0.2.0","to":"0.1.0","reason":"exit","status":139,"time":1700000000,"log":["reading /mnt/us/Secret Shelf/Sub","opening /mnt/us/Secret Shelf/Private Book.epub"]}"#,
        )
        .unwrap();

        let old_log = (1..=600)
            .map(|i| format!("[old {i}] reading /mnt/us/documents/Tolkien/The Hobbit.epub"))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(app.join("verso.log.old"), format!("{old_log}\n")).unwrap();

        let cur_log = [
            "[2026-11-22T08:00:00.000Z INFO  verso] === Verso session ===",
            "    version: 0.1.0 (abc1234)",
            "    device: Paperwhite 6 (bellatrix, mt8110, id=0x1234)",
            "    === end session ===",
            "session frontlight: flIntensity=yes, currentAmberLevel=yes",
            "2026-11-22T08:00:01Z launcher: launched from library (waited 3 of 50 times for Home)",
            "2026-11-22T08:00:01Z launcher: pausing the Kindle UI",
            "[2026-11-22T08:00:02.000Z INFO  verso] frame check 2.0 s after the launch frame: framebuffer intact",
            "[2026-11-22T08:00:03.000Z INFO  verso] screen: Library",
            "2026-11-22T08:00:04Z launcher: Verso ended: bye (code 0) (after 3s, 100B logged)",
            "deleted /mnt/us/documents/Tolkien/The Hobbit.epub",
            "deleting /mnt/us/Secret Shelf/Война и мир.epub: Permission denied",
            "open book removed: Some(\"/mnt/us/documents/Tolkien/The Hobbit.epub\")",
            "moved /mnt/us/documents/A.epub to /mnt/us/documents/B.epub",
            "opened /mnt/us/documents/Gwynne, John - Malice [Gwynne, John] (2024).epub: parse 186 ms",
            "ERROR ldomDocCache::openExisting - File Gwynne_John_Malice.epub.571d7876.0.cr3 is not found",
            "book \"The Secret Title\", authors \"Jane Doe\", language \"EN-US\"",
            "library folders changed: [\"/mnt/us/Secret Shelf\"]",
        ]
        .join("\n");
        // Lines and what the report shows of them, earlier versions' included.
        let redacted = [
            // Where a path ends: quotes, colons, commas and arrows don't end it.
            ("opened /mnt/us/documents/The Hitchhiker's Guide.epub: parse 1 ms", "opened <book 6>: parse 1 ms"),
            ("opened /mnt/us/documents/The Hitchhiker's Guide.epub: bad zip", "opened <book 6>: bad zip"),
            (r#"removed Some("/mnt/us/documents/O'Brian")"#, r#"removed Some("<book 7>")"#),
            (r#"removed Some("/mnt/us/documents/Sci-Fi: Best, of")"#, r#"removed Some("<book 8>")"#),
            (r#"removed Some("/mnt/us/documents/Say \"Hi\".epub")"#, r#"removed Some("<book 9>")"#),
            ("read /mnt/us/documents/Sci-Fi: Two: Denied (os error 13)", "read <book 10>: Denied (os error 13)"),
            ("moved /mnt/us/documents/D1 to /mnt/us/documents/D2", "moved <book 11> to <book 12>"),
            ("deleted /mnt/us/documents/Own.epub/Chapter.txt (files)", "deleted <book 13>"),
            // Library folders themselves, and one whose name only starts like one.
            (
                r#"collection switch Some("/mnt/us/Secret Shelf"): 3 ms"#,
                r#"collection switch Some("<library folder>"): 3 ms"#,
            ),
            (r#"collection switch Some("/mnt/us/Cafe\u{301}"): 3 ms"#, r#"collection switch Some("<book 14>"): 3 ms"#),
            (
                "Couldn't read /mnt/us/Secret Shelf: Denied (os error 13)",
                "Couldn't read <library folder>: Denied (os error 13)",
            ),
            (
                "Couldn't read /mnt/us/documents: Denied (os error 13)",
                "Couldn't read /mnt/us/documents: Denied (os error 13)",
            ),
            ("opened /mnt/us/Secret Shelf 2/Two.epub: parse", "opened <book 15>: parse"),
            // Names outside library folders.
            ("web app: received Up.epub → /mnt/us/documents/Up.epub (id 1)", "web app: received <book 16> (id 1)"),
            (
                "web app: received A → B → C.epub → /mnt/us/documents/C.epub (id 2)",
                "web app: received <book 17> (id 2)",
            ),
            ("web app: received /mnt/us/Outside/Title.epub (files)", "web app: received <book 18> (files)"),
            ("web app: moved /mnt/us/Outside/E1 → /mnt/us/Outside/E2", "web app: moved <book 19> → <book 20>"),
            (
                "web app: deleted /mnt/us/verso/data/dictionaries/x.ifo",
                "web app: deleted /mnt/us/verso/data/dictionaries/x.ifo",
            ),
            ("loading /mnt/us/fonts/Font.ttf: bad", "loading /mnt/us/fonts/Font.ttf: bad"),
            ("[x] listing /mnt/us/Browsed: Denied (os error 13)", "[x] listing <book 21>: Denied (os error 13)"),
            (
                "web app: writing highlights zip entry Zip (1).md: full",
                "web app: writing highlights zip entry <redacted>: full",
            ),
            (
                "web app: refusing Ref in X.epub in /mnt/us/documents: not",
                "web app: refusing a name in /mnt/us/documents: not",
            ),
            ("File ABC_.epub_noname.571d7876.0.cr3 is not", "File <cache> is not"),
            // The device's name and addresses.
            ("mDNS: advertising my-private-kindle.local", "mDNS: advertising <redacted>"),
            (
                r#"Wi-Fi transfer: Ready at Some(192.168.1.23) as Some("my-private-kindle")"#,
                "Wi-Fi transfer: Ready at <redacted> as <redacted>",
            ),
            ("web app: preparing a connection from 192.168.1.23:5000: bad", "web app: preparing a connection: bad"),
            // Stored API keys and DeepL authorization headers never leak.
            (
                "Authorization: DeepL-Auth-Key 99999999-aaaa-bbbb-cccc-ddddeeeeffff:fx",
                "Authorization: DeepL-Auth-Key <redacted>",
            ),
            ("debug key 12345678-1234-1234-1234-1234567890ab:fx leaked", "debug key <redacted> leaked"),
            // Markdown (.md and .markdown) book paths and cache files.
            ("opened /mnt/us/documents/Field Notes.md: parse 3 ms", "opened <book 22>: parse 3 ms"),
            ("opened /mnt/us/documents/Long Essay.markdown: parse 5 ms", "opened <book 23>: parse 5 ms"),
            ("writing /mnt/us/documents/Draft.md.part: full", "writing <book 24>: full"),
            ("moved /mnt/us/documents/A.md to /mnt/us/documents/B.markdown", "moved <book 25> to <book 26>"),
            ("deleted /mnt/us/documents/Own.md/note.txt (files)", "deleted <book 27>"),
            ("File Field_Notes.md.571d7876.0.cr3 is not", "File <cache> is not"),
        ];
        let lines: String = redacted.iter().map(|(line, _)| format!("{line}\n")).collect();
        fs::write(app.join("verso.log"), format!("{cur_log}\n{lines}")).unwrap();

        // Also create a crash report first so section 3 includes it.
        let collector = write_fake_collector(root);
        let crash_out = Command::new("sh")
            .arg(script_path())
            .args(["crash", "crash (SIGSEGV, code 139)"])
            .env("VERSO_STORAGE_ROOT", root)
            .env("VERSO_DATA", &data)
            .output()
            .unwrap();
        assert!(crash_out.status.success(), "{}", String::from_utf8_lossy(&crash_out.stderr));

        let out_file = root.join("out-report.txt");
        let rep = Command::new("sh")
            .arg(script_path())
            .args(["report", "--output", out_file.to_str().unwrap()])
            .env("VERSO_STORAGE_ROOT", root)
            .env("VERSO_DATA", &data)
            .env("VERSO_COLLECTOR", &collector)
            .output()
            .unwrap();
        assert!(rep.status.success(), "{}", String::from_utf8_lossy(&rep.stderr));

        let text = fs::read_to_string(&out_file).unwrap();

        // 1. All 5 sections present and in exact order.
        let sessions = text.find("== Sessions (newest last) ==").expect("sessions");
        let s1 = text.find("== 1. Verso version and settings ==").expect("section 1");
        let s2 = text.find("== 2. Update state ==").expect("section 2");
        let s3 = text.find("== 3. Crash reports ==").expect("section 3");
        let s4 = text.find("== 4. Logs (up to 1000 lines, book names redacted) ==").expect("section 4");
        let s5 = text.find("== 5. Hardware diagnostics ==").expect("section 5");
        assert!(sessions < s1 && s1 < s2 && s2 < s3 && s3 < s4 && s4 < s5, "sections out of order");

        // The summary has the sessions' key lines and nothing else.
        let summary: Vec<_> = text[sessions..s1].lines().filter(|l| !l.is_empty()).skip(1).collect();
        assert_eq!(summary.len(), 3, "{summary:?}");
        for (line, part) in summary.iter().zip(["launched from library", "frame check", "Verso ended: bye"]) {
            assert!(line.contains(part), "{line}");
        }

        // 2. Version, commit, settings summary without library folders, hostname or API keys.
        assert!(text.contains("Installed version: 0.1.0"));
        assert!(text.contains("Last session version: 0.1.0 (abc1234)"));
        assert!(text.contains("sleep_screen: \"cover\""));
        assert!(text.contains("page_flash: 6"));
        assert!(text.contains("translate_provider: \"deepl\""));
        assert!(text.contains("deepl_api_key: <redacted>"));
        assert!(!text.contains("12345678-1234-1234-1234-1234567890ab"), "DeepL API key leaked into report:\n{text}");
        assert!(!text.contains("99999999-aaaa-bbbb"), "DeepL-Auth-Key header leaked into report:\n{text}");
        assert!(!text.contains("Secret Shelf"), "library folder leaked into report:\n{text}");
        assert!(!text.contains("my-private-kindle"), "hostname leaked into report");

        // 3. Update state present.
        assert!(text.contains("Previous version (available to go back): 0.0.9"));
        assert!(text.contains("\"reason\":\"exit\""));
        assert!(!text.contains("Private Book.epub"), "book in rollback.json leaked");
        assert!(text.contains("\"opening <book 2>"), "a folder's path ended past its own log line:\n{text}");

        // 4. Crash report listed and included.
        assert!(text.contains("--- Newest crash report: crash-"));
        assert!(text.contains("Ended: crash (SIGSEGV, code 139)"));
        assert!(text.contains("session frontlight: flIntensity=yes, currentAmberLevel=yes"));

        // 5. Book names redacted consistently; same book keeps same number.
        assert!(!text.contains("The Hobbit.epub"), "book name leaked");
        assert!(!text.contains("Война и мир.epub"), "Cyrillic book name leaked");
        assert!(!text.contains("A.epub") && !text.contains("B.epub"), "moved book names leaked");
        assert!(!text.contains("Gwynne"), "comma/paren/cache book name leaked:\n{text}");
        assert!(text.contains("deleted <book 1>"));
        assert!(text.contains("open book removed: Some(\"<book 1>\")"));
        assert!(text.contains("deleting <book 2>: Permission denied"));
        assert!(text.contains("moved <book 3> to <book 4>"));
        assert!(text.contains("opened <book 5>: parse 186 ms"));
        assert!(text.contains("File <cache> is not found"));
        assert!(!text.contains("The Secret Title") && !text.contains("Jane Doe"), "metadata log leaked:\n{text}");
        assert!(text.contains("book <redacted>, language \"EN-US\""));
        assert!(text.contains("library folders changed: [<redacted>]"));

        let shown: String = redacted.iter().map(|(_, shown)| format!("{shown}\n")).collect();
        let start = text[s4..].find("opened <book 6>").map(|i| s4 + i).expect("first line of the table");
        assert!(text[start..].starts_with(&shown), "expected:\n{shown}\ngot:\n{}", &text[start..s5]);

        // 6. Diagnostics output included.
        assert!(text.contains("Fake hardware ok"));

        // A crash report of an earlier version, with part of a title left.
        let old = "Verso crash report - format 1\nEnded: old\n== Last 300 log lines ==\nopened <book 1>'s Guide.epub\n";
        fs::write(app.join("reports/crash-29990101-000000.txt"), old).unwrap();
        let rep = Command::new("sh")
            .arg(script_path())
            .args(["report", "--output", out_file.to_str().unwrap()])
            .env("VERSO_STORAGE_ROOT", root)
            .env("VERSO_DATA", &data)
            .env("VERSO_COLLECTOR", &collector)
            .output()
            .unwrap();
        assert!(rep.status.success());
        let text = fs::read_to_string(&out_file).unwrap();
        assert!(text.contains("Ended: old") && !text.contains("'s Guide"), "old crash report:\n{text}");

        // 7. Original log on device still has real book names.
        let raw_log = fs::read_to_string(app.join("verso.log")).unwrap();
        assert!(raw_log.contains("The Hobbit.epub"));
        assert!(raw_log.contains("Война и мир.epub"));
    }

    #[test]
    fn crash_reports_rotate_to_newest_five_and_unseen_notice_tracks_dismissal() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let app = root.join("verso");
        let data = app.join("data");
        fs::create_dir_all(&data).unwrap();
        fs::write(app.join("verso.log"), "=== Verso session ===\nversion: 0.1.0 (deadbee)\n=== end session ===\n")
            .unwrap();

        let mut created = Vec::new();
        for i in 1..=7 {
            let out = Command::new("sh")
                .arg(script_path())
                .args(["crash", &format!("crash #{i}")])
                .env("VERSO_STORAGE_ROOT", root)
                .env("VERSO_DATA", &data)
                .output()
                .unwrap();
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
            created.push(String::from_utf8_lossy(&out.stdout).trim().to_owned());
        }

        let reports: Vec<_> = fs::read_dir(app.join("reports"))
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().into_string().unwrap())
            .filter(|n| n.starts_with("crash-") && n.ends_with(".txt"))
            .collect();
        assert_eq!(reports.len(), 5, "should keep only 5 crash reports: {reports:?}");

        // Newest unseen crash is detected, then dismissed.
        // Point VERSO_STORAGE_ROOT via direct helper test using reports dir.
        let mut sorted = reports.clone();
        sorted.sort();
        let newest = sorted.last().unwrap().clone();
        // Verify newest file contains crash #7.
        let body = fs::read_to_string(app.join("reports").join(&newest)).unwrap();
        assert!(body.contains("Ended: crash #7"), "newest crash body:\n{body}");
        assert!(body.contains("version: 0.1.0 (deadbee)"));

        // Verify seen marker logic directly on a data dir whose device root has reports.
        let dev_reports = data.join("device/verso/reports");
        fs::create_dir_all(&dev_reports).unwrap();
        fs::write(dev_reports.join("crash-20261122-120000.txt"), "old").unwrap();
        fs::write(dev_reports.join("crash-20261122-120005.txt"), "new").unwrap();
        let notice = newest_unseen_crash(&data).expect("unseen crash");
        assert_eq!(notice.filename, "crash-20261122-120005.txt");
        dismiss_crash(&data, &notice.filename).unwrap();
        assert_eq!(newest_unseen_crash(&data), None);

        // A newer crash shows the notice again.
        fs::write(dev_reports.join("crash-20261122-120010.txt"), "newer").unwrap();
        let notice2 = newest_unseen_crash(&data).expect("newer unseen crash");
        assert_eq!(notice2.filename, "crash-20261122-120010.txt");
    }

    #[test]
    fn diagnostics_and_crash_report_identify_kindle_model_without_leaking_serial() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let app = root.join("verso");
        let data = app.join("data");
        fs::create_dir_all(&data).unwrap();
        fs::write(app.join("installed.json"), r#"{"format":1,"version":"0.1.0-beta.5","dev":false}"#).unwrap();
        fs::write(app.join("verso.log"), "2026-11-24 19:47:47 launcher: starting Verso (0.1.0-beta.5)\n").unwrap();

        let usid = root.join("usid");
        let pv = root.join("prettyversion.txt");
        // Modern base32 serial: "11L" -> 0x434 (Kindle Oasis 3); trailing digits must never appear.
        fs::write(&usid, "G0011L9876543210\n").unwrap();
        fs::write(&pv, "Kindle 5.18.2.1.1 (4358970010)\n").unwrap();

        let collector = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/collect-diagnostics.sh");

        let id_out = Command::new("sh")
            .arg(&collector)
            .arg("--identity")
            .env("VERSO_USID_FILE", &usid)
            .env("VERSO_PRETTYVERSION_FILE", &pv)
            .output()
            .unwrap();
        assert!(id_out.status.success(), "{}", String::from_utf8_lossy(&id_out.stderr));
        let id_text = String::from_utf8_lossy(&id_out.stdout);
        assert!(id_text.contains("Kindle model: Kindle Oasis 3 (2019) (code 11L, id=0x434)"), "{id_text}");
        assert!(id_text.contains("Kindle firmware: Kindle 5.18.2.1.1 (4358970010)"), "{id_text}");
        assert!(!id_text.contains("9876543210"), "serial digits leaked:\n{id_text}");

        // Legacy hex serial: B0[17]... -> 0x17 (Kindle Paperwhite 2).
        let legacy_usid = root.join("usid-legacy");
        fs::write(&legacy_usid, "B017987654321012\n").unwrap();
        // A placeholder the firmware never filled in is dropped, the version kept.
        fs::write(&pv, "Kindle 5.18.5.0.1 (~~otaVersion~~)\n").unwrap();
        let leg_out = Command::new("sh")
            .arg(&collector)
            .arg("--identity")
            .env("VERSO_USID_FILE", &legacy_usid)
            .env("VERSO_PRETTYVERSION_FILE", &pv)
            .output()
            .unwrap();
        assert!(leg_out.status.success());
        let leg_text = String::from_utf8_lossy(&leg_out.stdout);
        assert!(leg_text.contains("Kindle model: Kindle Paperwhite 2 (2013) (code 17, id=0x17)"), "{leg_text}");
        assert!(leg_text.contains("Kindle firmware: Kindle 5.18.5.0.1\n"), "{leg_text}");
        assert!(!leg_text.contains("987654321012"), "legacy serial digits leaked:\n{leg_text}");
        fs::write(&pv, "Kindle 5.18.2.1.1 (4358970010)\n").unwrap();

        // Crash report embeds Installed version + Device identity even with no session header.
        let crash = Command::new("sh")
            .arg(script_path())
            .args(["crash", "crash (SIGSEGV, code 139)"])
            .env("VERSO_STORAGE_ROOT", root)
            .env("VERSO_DATA", &data)
            .env("VERSO_COLLECTOR", &collector)
            .env("VERSO_USID_FILE", &usid)
            .env("VERSO_PRETTYVERSION_FILE", &pv)
            .output()
            .unwrap();
        assert!(crash.status.success(), "{}", String::from_utf8_lossy(&crash.stderr));
        let crash_path = String::from_utf8_lossy(&crash.stdout).trim().to_owned();
        let crash_text = fs::read_to_string(&crash_path).unwrap();
        assert!(crash_text.contains("Installed version: 0.1.0-beta.5"), "{crash_text}");
        assert!(crash_text.contains("Kindle model: Kindle Oasis 3 (2019) (code 11L, id=0x434)"), "{crash_text}");
        assert!(crash_text.contains("Kindle firmware: Kindle 5.18.2.1.1 (4358970010)"), "{crash_text}");
        assert!(!crash_text.contains("9876543210"), "serial digits leaked into crash report:\n{crash_text}");
    }
}
