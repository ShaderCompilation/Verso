//! The update check's decisions, without the network: when an automatic
//! check is due, what a verified manifest offers this device, and the saved
//! result (`state.json`).
use crate::{Installed, Manifest, Version};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Where the last check's result lives, relative to the storage root.
pub const STATE_PATH: &str = "verso/update/state.json";
/// The component holding the app itself. Its version is the release version.
pub const APP: &str = "app";
/// Automatic checks run at most this often (seconds, whole hours)…
pub const CHECK_EVERY: u64 = 6 * 60 * 60;
/// …and after a failed one, wait at least this long before trying again.
pub const RETRY_AFTER: u64 = 60 * 60;

/// The last check, as saved in `state.json`. Times are Unix seconds.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    /// The last check that reached the server and got a verified answer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked: Option<u64>,
    /// The last check that tried the network, whether or not it worked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempted: Option<u64>,
    /// Why the last attempt failed, in plain words. Cleared by a success.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The newest release a verified manifest has named. A signed but older
    /// manifest served later (a replay) is refused.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub newest_seen: Option<Version>,
    /// What the last successful check found to install.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub available: Option<Offer>,
    /// Versions the person chose to skip, or that were rolled back.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<Version>,
    /// When the person chose "Later": the notice stays hidden until a check
    /// that succeeds after this time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub postponed: Option<u64>,
}

/// An update this device can install.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Offer {
    pub version: Version,
    /// `YYYY-MM-DD`.
    pub released: String,
    pub notes: String,
    /// The components to download, in manifest order.
    pub components: Vec<String>,
    /// Their archives' total size in bytes.
    pub download: u64,
    /// Their files' total size once unpacked.
    pub unpacked: u64,
}

/// What a verified manifest means for this device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The release isn't newer than what's installed.
    UpToDate,
    Available(Offer),
}

/// A verified manifest this device won't act on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// Older than a release seen before: an old manifest served again.
    Stale { offered: Version, seen: Version },
    /// A changed component needs an app newer than the one it would run with.
    NeedsNewerApp { component: String, min_app: Version },
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::Stale { offered, seen } => write!(f, "offered {offered}, older than {seen} seen before"),
            Refusal::NeedsNewerApp { component, min_app } => {
                write!(f, "component {component:?} needs Verso {min_app} or newer")
            }
        }
    }
}

impl std::error::Error for Refusal {}

/// Whether an automatic check should run at `now`: no successful check in
/// the last [`CHECK_EVERY`] and no attempt in the last [`RETRY_AFTER`]. A
/// time in the future (the clock was set back) doesn't count as recent.
pub fn auto_check_due(state: &State, now: u64) -> bool {
    let recent = |time: Option<u64>, within: u64| time.is_some_and(|t| t <= now && now - t < within);
    !recent(state.checked, CHECK_EVERY) && !recent(state.attempted, RETRY_AFTER)
}

/// Decides what a verified `manifest` offers. `installed` is the device's
/// `installed.json` (None when there is none: every required component
/// counts as changed), `app` the running app's version.
pub fn evaluate(
    manifest: &Manifest,
    installed: Option<&Installed>,
    app: &Version,
    newest_seen: Option<&Version>,
) -> Result<Outcome, Refusal> {
    let current = installed.map_or(app, |i| &i.version);
    // Never offer the installed release or an older one: going back is only
    // the launcher's local rollback.
    if manifest.version <= *current {
        return Ok(Outcome::UpToDate);
    }
    if let Some(seen) = newest_seen
        && manifest.version < *seen
    {
        return Err(Refusal::Stale { offered: manifest.version.clone(), seen: seen.clone() });
    }
    let changed: Vec<_> = manifest
        .components
        .iter()
        .filter(|c| match installed.and_then(|i| i.component(&c.id)) {
            Some(had) => had.hash != c.hash,
            // Optional components are updated only once installed.
            None => c.required,
        })
        .collect();
    // Changed components are switched over together, so they run with the
    // new app if it is among them and with the running one otherwise.
    let app_after = if changed.iter().any(|c| c.id == APP) { &manifest.version } else { app };
    if let Some(c) = changed.iter().find(|c| c.min_app.as_ref().is_some_and(|min| min > app_after)) {
        return Err(Refusal::NeedsNewerApp { component: c.id.clone(), min_app: c.min_app.clone().unwrap() });
    }
    Ok(Outcome::Available(Offer {
        version: manifest.version.clone(),
        released: manifest.released.clone(),
        notes: manifest.notes.clone(),
        components: changed.iter().map(|c| c.id.clone()).collect(),
        download: changed.iter().map(|c| c.archive.size).sum(),
        unpacked: changed.iter().flat_map(|c| &c.files).map(|f| f.size).sum(),
    }))
}

impl State {
    /// Records a verified manifest's outcome at `now`. Skipped versions at
    /// or below `current` (the installed release) are forgotten.
    pub fn record_success(&mut self, manifest_version: &Version, outcome: Outcome, current: &Version, now: u64) {
        self.checked = Some(now);
        self.attempted = Some(now);
        self.error = None;
        if self.newest_seen.as_ref().is_none_or(|seen| manifest_version > seen) {
            self.newest_seen = Some(manifest_version.clone());
        }
        self.available = match outcome {
            Outcome::UpToDate => None,
            Outcome::Available(offer) => Some(offer),
        };
        self.skipped.retain(|v| v > current);
    }

    /// Records a check that tried the network and failed. The previous
    /// offer stays: it was verified, and a flaky network doesn't undo it.
    pub fn record_failure(&mut self, error: String, now: u64) {
        self.attempted = Some(now);
        self.error = Some(error);
    }

    /// Hides `version`'s notice until a newer version appears.
    pub fn skip(&mut self, version: &Version) {
        if !self.skipped.contains(version) {
            self.skipped.push(version.clone());
        }
    }

    pub fn is_skipped(&self, version: &Version) -> bool {
        self.skipped.contains(version)
    }

    /// "Later": hides the notice until the next successful check.
    pub fn postpone(&mut self, now: u64) {
        self.postponed = Some(now);
    }

    /// "Later" was chosen after the last successful check.
    pub fn is_postponed(&self) -> bool {
        self.postponed.is_some_and(|later| self.checked.is_none_or(|checked| checked <= later))
    }

    /// An update newer than `current`, whether or not its notice is hidden
    /// (Settings shows a dot until it is installed).
    pub fn update(&self, current: &Version) -> Option<&Offer> {
        self.available.as_ref().filter(|o| o.version > *current)
    }

    /// The update to tell the person about: the offer, unless it was
    /// skipped, postponed until the next check, or is no longer newer than
    /// `current` (it was installed since).
    pub fn notice(&self, current: &Version) -> Option<&Offer> {
        self.update(current).filter(|o| !self.is_skipped(&o.version) && !self.is_postponed())
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Archive, Component, FORMAT, FileEntry, MODE_EXEC, MODE_FILE, component_hash, sha256_hex};

    fn v(s: &str) -> Version {
        s.parse().unwrap()
    }

    fn component(id: &str, install: &str, content: &[u8], size: u64) -> Component {
        let files = vec![FileEntry {
            path: format!("{id}.bin"),
            size: content.len() as u64 * 1000,
            sha256: sha256_hex(content),
            mode: if id == APP { MODE_EXEC } else { MODE_FILE },
        }];
        Component {
            id: id.into(),
            hash: component_hash(install, &files),
            required: true,
            min_app: None,
            install: install.into(),
            description: String::new(),
            archive: Archive { url: format!("{id}.tar.gz"), size, sha256: sha256_hex(id.as_bytes()) },
            files,
        }
    }

    /// A release whose components have content `<id><tag>`.
    fn release(version: &str, tags: &[(&str, &str)]) -> Manifest {
        let components = tags
            .iter()
            .enumerate()
            .map(|(i, (id, tag))| component(id, "verso", format!("{id}{tag}").as_bytes(), 100 * (i as u64 + 1)))
            .collect();
        let m = Manifest {
            format: FORMAT,
            version: v(version),
            released: "2026-12-01".into(),
            notes: format!("Verso {version}"),
            components,
        };
        m.validate().unwrap();
        m
    }

    fn installed_from(m: &Manifest) -> Installed {
        let ids: Vec<&str> = m.components.iter().map(|c| c.id.as_str()).collect();
        Installed::from_manifest(m, &ids, false)
    }

    fn offer(outcome: Result<Outcome, Refusal>) -> Offer {
        match outcome {
            Ok(Outcome::Available(offer)) => offer,
            other => panic!("expected an offer, got {other:?}"),
        }
    }

    const EVERY: u64 = CHECK_EVERY;
    const HOUR: u64 = RETRY_AFTER;

    #[test]
    fn schedules_checks_and_retries_failures_after_an_hour() {
        let now = 1_800_000_000;
        assert!(auto_check_due(&State::default(), now), "never checked");
        let ok = |checked: u64| State { checked: Some(checked), attempted: Some(checked), ..State::default() };
        assert!(!auto_check_due(&ok(now), now));
        assert!(!auto_check_due(&ok(now - EVERY + 1), now));
        assert!(auto_check_due(&ok(now - EVERY), now));
        // A failure after an old success: retry after an hour, not a whole interval.
        let failed = |at: u64| State { attempted: Some(at), error: Some("x".into()), ..ok(now - 2 * EVERY) };
        assert!(!auto_check_due(&failed(now - HOUR + 1), now));
        assert!(auto_check_due(&failed(now - HOUR), now));
        // Times in the future: the clock went back, so check.
        assert!(auto_check_due(&ok(now + 10), now));
        assert!(auto_check_due(&State { attempted: Some(now + 10), ..ok(now + 10) }, now));
    }

    #[test]
    fn offers_only_changed_components() {
        let old = release("0.2.0", &[("app", "1"), ("fonts", "1"), ("wordforms-ru", "1")]);
        let new = release("0.3.0", &[("app", "2"), ("fonts", "1"), ("wordforms-ru", "2"), ("scriptlet", "1")]);
        let installed = installed_from(&old);
        // Existing installed.json without a newly added required component
        // (`scriptlet`) is offered it alongside changed components.
        let o = offer(evaluate(&new, Some(&installed), &v("0.2.0"), None));
        assert_eq!(o.version, v("0.3.0"));
        assert_eq!(o.components, ["app", "wordforms-ru", "scriptlet"]);
        assert_eq!(o.download, 100 + 300 + 400);
        assert_eq!(o.unpacked, 4000 + 13000 + 10000);
        assert_eq!(o.notes, "Verso 0.3.0");
        // No installed.json: every required component.
        let o = offer(evaluate(&new, None, &v("0.2.0"), None));
        assert_eq!(o.components, ["app", "fonts", "wordforms-ru", "scriptlet"]);
    }

    #[test]
    fn optional_components_update_only_when_installed() {
        let old = release("0.2.0", &[("app", "1"), ("wordforms-de", "1")]);
        let mut new = release("0.3.0", &[("app", "2"), ("wordforms-de", "2")]);
        new.components[1].required = false;
        let without = Installed::from_manifest(&old, &["app"], false);
        assert_eq!(offer(evaluate(&new, Some(&without), &v("0.2.0"), None)).components, ["app"]);
        let with = installed_from(&old);
        assert_eq!(offer(evaluate(&new, Some(&with), &v("0.2.0"), None)).components, ["app", "wordforms-de"]);
    }

    #[test]
    fn refuses_downgrades_and_replays() {
        let m = |version: &str| release(version, &[("app", version)]);
        let installed = installed_from(&m("0.3.0"));
        // The installed release, or an older one, is never offered.
        for version in ["0.3.0", "0.2.9", "0.3.0-rc.1", "0.1.0"] {
            assert_eq!(evaluate(&m(version), Some(&installed), &v("0.3.0"), None), Ok(Outcome::UpToDate), "{version}");
        }
        // Without installed.json the running app's version counts.
        assert_eq!(evaluate(&m("0.3.0"), None, &v("0.3.0"), None), Ok(Outcome::UpToDate));
        // Newer than installed, but older than a release seen before.
        assert_eq!(
            evaluate(&m("0.3.1"), Some(&installed), &v("0.3.0"), Some(&v("0.3.2"))),
            Err(Refusal::Stale { offered: v("0.3.1"), seen: v("0.3.2") })
        );
        // The release seen before, served again, is still offered.
        assert_eq!(offer(evaluate(&m("0.3.2"), Some(&installed), &v("0.3.0"), Some(&v("0.3.2")))).version, v("0.3.2"));
    }

    #[test]
    fn minimum_app_version() {
        let old = release("0.2.0", &[("app", "1"), ("wordforms-ru", "1")]);
        let installed = installed_from(&old);
        // Data that needs the new app, shipped with it: fine.
        let mut new = release("0.3.0", &[("app", "2"), ("wordforms-ru", "2")]);
        new.components[1].min_app = Some(v("0.3.0"));
        assert_eq!(offer(evaluate(&new, Some(&installed), &v("0.2.0"), None)).components, ["app", "wordforms-ru"]);
        // Data that needs an app newer than the one it would run with.
        let mut data_only = release("0.3.0", &[("app", "1"), ("wordforms-ru", "2")]);
        data_only.components[1].min_app = Some(v("0.3.0"));
        assert_eq!(
            evaluate(&data_only, Some(&installed), &v("0.2.0"), None),
            Err(Refusal::NeedsNewerApp { component: "wordforms-ru".into(), min_app: v("0.3.0") })
        );
        // The same data-only release is fine for an app already new enough.
        data_only.components[1].min_app = Some(v("0.2.0"));
        assert_eq!(offer(evaluate(&data_only, Some(&installed), &v("0.2.0"), None)).components, ["wordforms-ru"]);
        // A release whose own app claims to need a newer one is broken.
        let mut broken = release("0.3.0", &[("app", "2")]);
        broken.components[0].min_app = Some(v("0.4.0"));
        assert!(matches!(evaluate(&broken, Some(&installed), &v("0.2.0"), None), Err(Refusal::NeedsNewerApp { .. })));
    }

    #[test]
    fn skipped_versions_hide_the_notice_until_a_newer_one() {
        let installed = v("0.2.0");
        let m = |version: &str| release(version, &[("app", version)]);
        let mut state = State::default();
        let check = |state: &mut State, version: &str| {
            let manifest = m(version);
            let outcome = evaluate(&manifest, None, &installed, state.newest_seen.as_ref()).unwrap();
            state.record_success(&manifest.version, outcome, &installed, 1000);
        };
        check(&mut state, "0.3.0");
        assert_eq!(state.notice(&installed).unwrap().version, v("0.3.0"));
        state.skip(&v("0.3.0"));
        state.skip(&v("0.3.0"));
        assert_eq!(state.skipped, [v("0.3.0")]);
        assert!(state.notice(&installed).is_none());
        check(&mut state, "0.3.0");
        assert!(state.notice(&installed).is_none(), "still skipped after another check");
        check(&mut state, "0.3.1");
        assert_eq!(state.notice(&installed).unwrap().version, v("0.3.1"));
        // Once a newer release is installed, old skips are forgotten.
        let manifest = m("0.3.1");
        state.record_success(&manifest.version, Outcome::UpToDate, &v("0.3.1"), 2000);
        assert!(state.skipped.is_empty());
        assert!(state.notice(&v("0.3.1")).is_none());
    }

    #[test]
    fn later_hides_the_notice_until_the_next_successful_check() {
        let installed = v("0.2.0");
        let manifest = release("0.3.0", &[("app", "2")]);
        let mut state = State::default();
        let check = |state: &mut State, now: u64| {
            let outcome = evaluate(&manifest, None, &installed, state.newest_seen.as_ref()).unwrap();
            state.record_success(&manifest.version, outcome, &installed, now);
        };
        check(&mut state, 1000);
        state.postpone(1500);
        assert!(state.notice(&installed).is_none());
        assert_eq!(state.update(&installed).unwrap().version, v("0.3.0"), "the Settings dot stays");
        // A failed check doesn't bring it back; a successful one does.
        state.record_failure("x".into(), 2000);
        assert!(state.notice(&installed).is_none());
        check(&mut state, 3000);
        assert_eq!(state.notice(&installed).unwrap().version, v("0.3.0"));
        // Skipped stays hidden, Later or not; installed means no dot.
        state.skip(&v("0.3.0"));
        assert!(state.notice(&installed).is_none());
        assert!(state.update(&v("0.3.0")).is_none());
        let json = serde_json::to_vec(&state).unwrap();
        assert_eq!(State::parse(&json).unwrap(), state);
    }

    #[test]
    fn failures_keep_the_last_offer_and_successes_clear_errors() {
        let installed = v("0.2.0");
        let manifest = release("0.3.0", &[("app", "2")]);
        let mut state = State::default();
        let outcome = evaluate(&manifest, None, &installed, None).unwrap();
        state.record_success(&manifest.version, outcome, &installed, 1000);
        state.record_failure("Couldn't reach the update server.".into(), 5000);
        assert_eq!((state.checked, state.attempted), (Some(1000), Some(5000)));
        assert!(state.error.is_some());
        assert_eq!(state.notice(&installed).unwrap().version, v("0.3.0"));
        // Installed since: no notice even before the next check.
        assert!(state.notice(&v("0.3.0")).is_none());
        state.record_success(&manifest.version, Outcome::UpToDate, &v("0.3.0"), 9000);
        assert_eq!(state.error, None);
        assert_eq!(state.newest_seen, Some(v("0.3.0")));
    }

    #[test]
    fn state_round_trips_and_tolerates_old_or_partial_files() {
        let mut state = State { checked: Some(1), attempted: Some(2), error: Some("x".into()), ..State::default() };
        state.skip(&v("0.3.0"));
        let json = serde_json::to_vec(&state).unwrap();
        assert_eq!(State::parse(&json).unwrap(), state);
        assert_eq!(State::parse(b"{}").unwrap(), State::default());
        assert_eq!(State::parse(br#"{"checked":5,"future":true}"#).unwrap().checked, Some(5));
        assert!(State::parse(b"{broken").is_err());
    }
}
