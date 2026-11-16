//! verso-mdns: lets computers on the same Wi-Fi find the Kindle by name,
//! `verso.local`, instead of an IP address that changes with DHCP.
//!
//! A minimal mDNS responder (RFC 6762) for one name with one IPv4 address.
//! Verso starts it next to the SSH server and it exits when that server does.
//! It is its own small binary rather than a mode of `verso`, because the
//! Kindle's busybox `pidof`/`killall` match processes by executable name: a
//! second `verso` process would make the launcher think Verso is already
//! running. See docs/device-tools.md and docs/research/mdns.md.
//!
//! The library half is what Verso needs to talk to a running responder: the
//! file paths and name rules.

mod dns;
mod responder;
#[cfg(target_os = "linux")]
mod run;

#[cfg(target_os = "linux")]
pub use run::{Options, run};

/// Holds the running responder's PID, locked while it runs, so there is only ever one.
pub const PID_FILE: &str = "/tmp/verso-mdns.pid";
/// The name the responder won, e.g. `verso` or `verso-2` after a conflict.
pub const NAME_FILE: &str = "/tmp/verso-mdns.name";
/// The responder's process name, as `/proc/<pid>/comm` shows it.
pub const PROCESS_NAME: &str = "verso-mdns";
pub const DEFAULT_LABEL: &str = "verso";

/// A name we can advertise as `<label>.local`: 1–63 lowercase letters,
/// digits and hyphens, not starting or ending with a hyphen.
pub fn valid_label(label: &str) -> bool {
    (1..=63).contains(&label.len())
        && label.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !label.starts_with('-')
        && !label.ends_with('-')
}

/// The name to try after a conflict: `verso` → `verso-2` → `verso-3`.
pub fn next_label(label: &str) -> String {
    let (base, n) = match label.rsplit_once('-') {
        Some((base, n)) if !base.is_empty() && !n.starts_with('0') => match n.parse::<u32>() {
            Ok(n) if n >= 2 => (base, n.saturating_add(1)),
            _ => (label, 2),
        },
        _ => (label, 2),
    };
    let suffix = format!("-{n}");
    let base = base[..base.len().min(63 - suffix.len())].trim_end_matches('-');
    format!("{base}{suffix}")
}

/// The running responder's PID, if there is one.
pub fn running_pid() -> Option<i32> {
    let pid: i32 = std::fs::read_to_string(PID_FILE).ok()?.trim().parse().ok()?;
    let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    (comm.trim() == PROCESS_NAME).then_some(pid)
}

/// The name the running responder has won, if it has won one yet.
pub fn won_label() -> Option<String> {
    running_pid()?;
    let label = std::fs::read_to_string(NAME_FILE).ok()?.trim().to_owned();
    valid_label(&label).then_some(label)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels() {
        assert!(valid_label("verso"));
        assert!(valid_label("verso-2"));
        assert!(valid_label(&"a".repeat(63)));
        for bad in ["", "-verso", "verso-", "Verso", "ver so", "vérso", "verso.local", &"a".repeat(64)] {
            assert!(!valid_label(bad), "{bad:?}");
        }
    }

    #[test]
    fn next_labels() {
        assert_eq!(next_label("verso"), "verso-2");
        assert_eq!(next_label("verso-2"), "verso-3");
        assert_eq!(next_label("verso-9"), "verso-10");
        assert_eq!(next_label("verso-1"), "verso-1-2");
        assert_eq!(next_label("my-kindle"), "my-kindle-2");
        assert_eq!(next_label("verso-02"), "verso-02-2");
        let long = "a".repeat(63);
        assert_eq!(next_label(&long), format!("{}-2", "a".repeat(61)));
        assert!(valid_label(&next_label(&format!("{}-b", "a".repeat(61)))));
    }
}
