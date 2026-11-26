//! "What's new" after an update: the release notes built into this build
//! (`dist/release-notes/*.txt`, embedded by `build.rs`) and the rules for
//! when to show them, from the last version the person saw
//! (`verso/update/seen-version`, written by `Updates::saw_whats_new`).
use verso_update::Version;

/// Every `dist/release-notes/<version>.txt` this build was made with.
const BUILT_IN: &[(&str, &str)] = include!(concat!(env!("OUT_DIR"), "/release_notes.rs"));

/// The file under the storage root that holds the last version whose
/// "What's new" was seen (or skipped quietly).
pub const SEEN_PATH: &str = "verso/update/seen-version";

/// At most this many versions' notes on the sheet, newest first.
const MAX_VERSIONS: usize = 5;

/// What the sheet shows after an update.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WhatsNew {
    /// This version.
    pub version: Version,
    /// The version before the update, where known: "From Verso Y".
    pub from: Option<Version>,
    /// The notes of each version since `from`, newest first.
    pub notes: Vec<(Version, &'static str)>,
}

impl WhatsNew {
    /// The notes as one text: each version under a "Verso X" heading when
    /// there are several.
    pub fn text(&self) -> String {
        match self.notes.as_slice() {
            [(_, notes)] => (*notes).to_owned(),
            several => several
                .iter()
                .map(|(version, notes)| format!("## Verso {version}\n{}", notes.trim()))
                .collect::<Vec<_>>()
                .join("\n\n"),
        }
    }
}

/// What to do at start.
#[derive(Debug, PartialEq, Eq)]
pub enum Due {
    /// Nothing changed since the last start.
    Nothing,
    /// Remember this version without showing anything.
    Remember,
    /// Show these notes, newest first; remember once closed.
    Show(Vec<(Version, &'static str)>),
}

/// The built-in notes, by version (unreadable names are left out).
pub fn built_in() -> Vec<(Version, &'static str)> {
    BUILT_IN.iter().filter_map(|(v, notes)| Some((v.parse().ok()?, *notes))).collect()
}

/// The built-in notes for one version, if this build has them.
#[cfg_attr(not(test), allow(dead_code))] // For Settings → Updates, which doesn't open it yet.
pub fn notes_for(version: &Version) -> Option<&'static str> {
    built_in().into_iter().find(|(v, _)| v == version).map(|(_, notes)| notes)
}

/// Whether to show "What's new" for `this` version, given the last one
/// seen, whether this is a development install, and the notes on hand.
pub fn due(this: &Version, seen: Option<&Version>, dev: bool, notes: &[(Version, &'static str)]) -> Due {
    let Some(seen) = seen else { return Due::Remember };
    if seen == this {
        return Due::Nothing;
    }
    if dev || seen > this {
        return Due::Remember;
    }
    let mut shown: Vec<_> = notes.iter().filter(|(v, _)| v > seen && v <= this).cloned().collect();
    shown.sort_by(|a, b| b.0.cmp(&a.0));
    shown.truncate(MAX_VERSIONS);
    if shown.is_empty() { Due::Remember } else { Due::Show(shown) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        s.parse().unwrap()
    }

    #[test]
    fn shows_the_notes_since_the_last_version_seen() {
        let notes: Vec<_> = ["0.1.0", "0.2.0", "0.3.0-beta.1", "0.3.0", "0.4.0", "0.5.0", "0.6.0", "0.7.0"]
            .into_iter()
            .map(|s| (v(s), "notes"))
            .collect();
        let shown = |due: Due| match due {
            Due::Show(notes) => notes.iter().map(|(v, _)| v.to_string()).collect::<Vec<_>>().join(" "),
            other => format!("{other:?}"),
        };
        // (this, seen, dev, expected)
        let table = [
            ("0.3.0", None, false, "Remember"),
            ("0.3.0", Some("0.3.0"), false, "Nothing"),
            ("0.3.0", Some("0.3.0"), true, "Nothing"),
            ("0.2.0", Some("0.3.0"), false, "Remember"),
            ("0.3.0", Some("0.2.0"), true, "Remember"),
            ("0.3.0", Some("0.2.0"), false, "0.3.0 0.3.0-beta.1"),
            ("0.3.0-beta.1", Some("0.2.0"), false, "0.3.0-beta.1"),
            ("0.2.1", Some("0.2.0"), false, "Remember"),
            ("0.7.0", Some("0.1.0"), false, "0.7.0 0.6.0 0.5.0 0.4.0 0.3.0"),
            ("0.9.0", Some("0.6.0"), false, "0.7.0"),
        ];
        for (this, seen, dev, expected) in table {
            let seen = seen.map(v);
            assert_eq!(shown(due(&v(this), seen.as_ref(), dev, &notes)), expected, "{this} after {seen:?}, dev {dev}");
        }
    }

    #[test]
    fn notes_of_several_versions_get_headings() {
        let one = WhatsNew { version: v("0.2.0"), from: None, notes: vec![(v("0.2.0"), "Two.\n")] };
        assert_eq!(one.text(), "Two.\n");
        let two = WhatsNew { notes: vec![(v("0.2.0"), "Two.\n"), (v("0.1.0"), "- One.\n")], ..one };
        assert_eq!(two.text(), "## Verso 0.2.0\nTwo.\n\n## Verso 0.1.0\n- One.");
    }

    #[test]
    fn the_notes_are_built_in() {
        assert!(!BUILT_IN.is_empty() && built_in().len() == BUILT_IN.len(), "every file name is a version");
        let (version, notes) = &built_in()[0];
        assert_eq!(notes_for(version), Some(*notes));
    }
}
