//! What Verso and its launcher (`dist/kindle/verso/verso.sh`) agree on:
//! exit codes, environment variables and the files the launcher writes
//! while it switches an update in or goes back. The launcher does the
//! switch-over with renames, from the plan staged with the update
//! ([`crate::stage::PLAN_FILE`]). Paths are relative to the storage root
//! (`/mnt/us`). See docs/updates.md.
use crate::Version;
use serde::Deserialize;
use std::path::Path;

/// The launcher protocol this build's launcher speaks. The launcher
/// exports it as [`PROTOCOL_ENV`]; an app run by an older launcher (or by
/// hand) sees a lower number, or none.
///
/// 1: [`EXIT_INSTALL`], [`EXIT_GO_BACK`], [`STARTED_ENV`], trial and rollback.
pub const PROTOCOL: u32 = 1;
pub const PROTOCOL_ENV: &str = "VERSO_LAUNCHER_PROTOCOL";
/// The file to create once this version has shown that it works; set only
/// by a launcher that checks it.
pub const STARTED_ENV: &str = "VERSO_STARTED_MARKER";

/// Exit code: install the staged update and start Verso again, without
/// bringing back the Kindle UI.
pub const EXIT_INSTALL: i32 = 85;
/// Exit code: go back to the previous version (`update/previous/`) and
/// start Verso again.
pub const EXIT_GO_BACK: i32 = 86;

/// One of these empty files is present while the launcher moves files,
/// naming the job; the next launch finishes it. The launcher renames one
/// to the next, so the journal can't be half written.
pub const JOURNAL_PATHS: [&str; 3] =
    ["verso/update/journal-install", "verso/update/journal-rollback", "verso/update/journal-cleanup"];
/// The replaced files, at their paths under `files/`, and the plan that
/// replaced them (`plan.txt`), kept until the next update.
pub const PREVIOUS_DIR: &str = "verso/update/previous";
pub const PREVIOUS_PLAN: &str = "verso/update/previous/plan.txt";
/// Present while the version in [`PREVIOUS_PLAN`] is on trial: how many
/// times it was started without creating the started marker.
pub const TRIAL_PATH: &str = "verso/update/trial";
/// Why the launcher went back, for the restored version to tell.
pub const ROLLBACK_PATH: &str = "verso/update/rollback.json";

/// The protocol of the launcher running this process; 0 when it didn't
/// say (an older launcher, the simulator, or started by hand).
pub fn protocol() -> u32 {
    std::env::var(PROTOCOL_ENV).ok().and_then(|v| v.trim().parse().ok()).unwrap_or(0)
}

/// The version the previous files belong to, and the version that
/// replaced them, while "go back" is possible.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Previous {
    /// The version installed now (the plan's `version`).
    pub current: Version,
    /// The version going back would restore; None when it wasn't recorded.
    pub previous: Option<Version>,
}

/// Reads [`PREVIOUS_PLAN`] under `root`: None when there are no previous
/// files to go back to.
pub fn previous(root: &Path) -> Option<Previous> {
    if JOURNAL_PATHS.iter().any(|j| root.join(j).exists()) {
        return None; // Mid-switch: the next launch finishes it first.
    }
    let text = std::fs::read_to_string(root.join(PREVIOUS_PLAN)).ok()?;
    parse_plan_header(&text)
}

/// The versions in a plan's header (see [`crate::stage::Staged::plan_text`]).
pub fn parse_plan_header(text: &str) -> Option<Previous> {
    let mut lines = text.lines().map(|l| l.split_once('\t').unwrap_or((l, "")));
    if lines.next()? != ("verso-plan", "1") {
        return None;
    }
    let (mut current, mut previous) = (None, None);
    for (key, value) in lines {
        match key {
            "version" => current = value.parse().ok(),
            "from" => previous = value.parse().ok(),
            "remove" | "install" => break,
            _ => {}
        }
    }
    Some(Previous { current: current?, previous })
}

/// Why the launcher went back, as `rollback.json` records it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reason {
    /// The new version exited with this status before it had started.
    Exit(i32),
    /// It was killed by this signal (a crash, or an abort after a panic).
    Signal(i32),
    /// It hadn't started after the launcher's time limit.
    Hang,
    /// Two launches in a row ended before it had started (power loss or a
    /// forced restart).
    Interrupted,
    /// Switching its files in failed; nothing of it ran.
    Install,
    /// The person asked to go back.
    Requested,
    /// A reason from a newer launcher.
    Other(String),
}

/// `rollback.json`: written by the launcher after going back, kept until
/// the restored version has told the person.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rollback {
    /// The version that was removed, and the one restored.
    pub from: Option<Version>,
    pub to: Option<Version>,
    pub reason: Reason,
    /// Seconds since the Unix epoch.
    pub time: Option<u64>,
    /// The last lines of `verso.log` from the failed start.
    pub log: Vec<String>,
}

#[derive(Deserialize)]
struct RawRollback {
    #[serde(default)]
    from: String,
    #[serde(default)]
    to: String,
    #[serde(default)]
    reason: String,
    #[serde(default)]
    status: Option<i32>,
    #[serde(default)]
    time: Option<u64>,
    #[serde(default)]
    log: Vec<String>,
}

impl Rollback {
    /// Parses what the launcher wrote. The log lines come from a shell, so
    /// bytes that aren't UTF-8 are replaced rather than refused.
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        let text = String::from_utf8_lossy(bytes);
        let raw: RawRollback = serde_json::from_str(&text).ok()?;
        let reason = match (raw.reason.as_str(), raw.status) {
            ("exit", Some(status)) if status > 128 => Reason::Signal(status - 128),
            ("exit", Some(status)) => Reason::Exit(status),
            ("hang", _) => Reason::Hang,
            ("interrupted", _) => Reason::Interrupted,
            ("install", _) => Reason::Install,
            ("requested", _) => Reason::Requested,
            (other, _) => Reason::Other(other.to_owned()),
        };
        Some(Rollback { from: raw.from.parse().ok(), to: raw.to.parse().ok(), reason, time: raw.time, log: raw.log })
    }

    /// Reads [`ROLLBACK_PATH`] under `root`.
    pub fn read(root: &Path) -> Option<Self> {
        Self::parse(&std::fs::read(root.join(ROLLBACK_PATH)).ok()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        s.parse().unwrap()
    }

    #[test]
    fn reads_the_plan_header() {
        let plan = "verso-plan\t1\nversion\t0.3.0\nfrom\t0.2.1\nremove\tverso/x\ninstall\ta\tb\n";
        assert_eq!(parse_plan_header(plan), Some(Previous { current: v("0.3.0"), previous: Some(v("0.2.1")) }));
        let no_from = "verso-plan\t1\nversion\t0.3.0\ninstall\ta\tb\n";
        assert_eq!(parse_plan_header(no_from), Some(Previous { current: v("0.3.0"), previous: None }));
        assert_eq!(parse_plan_header("verso-plan\t2\nversion\t0.3.0\n"), None, "unknown format");
        assert_eq!(parse_plan_header("verso-plan\t1\nfrom\t0.2.1\n"), None, "no version");
        assert_eq!(parse_plan_header(""), None);
    }

    #[test]
    fn previous_needs_the_plan_and_no_journal() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(previous(dir.path()), None);
        std::fs::create_dir_all(dir.path().join(PREVIOUS_DIR)).unwrap();
        std::fs::write(dir.path().join(PREVIOUS_PLAN), "verso-plan\t1\nversion\t0.3.0\nfrom\t0.2.1\n").unwrap();
        assert_eq!(previous(dir.path()).unwrap().previous, Some(v("0.2.1")));
        std::fs::write(dir.path().join(JOURNAL_PATHS[1]), "").unwrap();
        assert_eq!(previous(dir.path()), None, "a switch-over in progress");
    }

    #[test]
    fn reads_what_the_launcher_writes() {
        // As verso.sh writes it, with a byte that isn't UTF-8 in the log.
        let mut bytes = br#"{"format":1,"from":"0.3.0","to":"0.2.1","reason":"exit","status":139,"time":1763500000,"log":["thread 'main' panicked at src/main.rs:3:5:","boom \"quoted\" \\ back"#.to_vec();
        bytes.push(0xff);
        bytes.extend_from_slice(br#""]}"#);
        let r = Rollback::parse(&bytes).unwrap();
        assert_eq!(r.from, Some(v("0.3.0")));
        assert_eq!(r.to, Some(v("0.2.1")));
        assert_eq!(r.reason, Reason::Signal(11));
        assert_eq!(r.time, Some(1763500000));
        assert_eq!(r.log[0], "thread 'main' panicked at src/main.rs:3:5:");
        assert_eq!(r.log[1], "boom \"quoted\" \\ back\u{fffd}");

        let reason = |json: &str| Rollback::parse(json.as_bytes()).unwrap().reason;
        assert_eq!(reason(r#"{"reason":"exit","status":101}"#), Reason::Exit(101));
        assert_eq!(reason(r#"{"reason":"hang"}"#), Reason::Hang);
        assert_eq!(reason(r#"{"reason":"interrupted"}"#), Reason::Interrupted);
        assert_eq!(reason(r#"{"reason":"install"}"#), Reason::Install);
        assert_eq!(reason(r#"{"reason":"requested"}"#), Reason::Requested);
        assert_eq!(reason(r#"{"reason":"later"}"#), Reason::Other("later".into()));
        let unknown = Rollback::parse(br#"{"reason":"hang","to":""}"#).unwrap();
        assert_eq!((unknown.from, unknown.to), (None, None));
        assert!(Rollback::parse(b"not json").is_none());
    }
}
