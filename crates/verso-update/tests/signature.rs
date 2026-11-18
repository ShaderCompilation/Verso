//! Signature checks against fixtures signed with the minisign tool (see
//! testdata/README.md).
use verso_update::{Error, verify_manifest};

const MANIFEST: &[u8] = include_bytes!("../testdata/latest.json");
const SIG: &str = include_str!("../testdata/latest.json.minisig");
const SIG_OTHER: &str = include_str!("../testdata/latest.json.other.minisig");
const SIG_COMMENT: &str = include_str!("../testdata/latest.json.comment.minisig");
const SIG_LEGACY: &str = include_str!("../testdata/latest.json.legacy.minisig");
const TEST_KEY: &str = include_str!("../testdata/test.pub");
const OTHER_KEY: &str = include_str!("../testdata/other.pub");

#[test]
fn good_signature() {
    let m = verify_manifest(MANIFEST, SIG, &[TEST_KEY]).unwrap();
    assert_eq!(m.version.to_string(), "0.3.0");
    assert_eq!(m.components.len(), 2);
    // A bare base64 key works as well as a .pub file.
    let bare = TEST_KEY.lines().nth(1).unwrap();
    verify_manifest(MANIFEST, SIG, &[bare]).unwrap();
}

#[test]
fn backup_key() {
    // Either built-in key may sign, in any order.
    verify_manifest(MANIFEST, SIG_OTHER, &[TEST_KEY, OTHER_KEY]).unwrap();
    verify_manifest(MANIFEST, SIG, &[OTHER_KEY, TEST_KEY]).unwrap();
}

#[test]
fn wrong_key() {
    assert_eq!(verify_manifest(MANIFEST, SIG, &[OTHER_KEY]), Err(Error::UnknownKey));
    assert_eq!(verify_manifest(MANIFEST, SIG_OTHER, &[TEST_KEY]), Err(Error::UnknownKey));
    assert_eq!(verify_manifest(MANIFEST, SIG, &[]), Err(Error::NoKeys));
    assert_eq!(verify_manifest(MANIFEST, SIG, &["not a key"]), Err(Error::BadKey));
}

#[test]
fn tampered_manifest() {
    let text = std::str::from_utf8(MANIFEST).unwrap();
    for tampered in [
        text.replace("Faster page turns.", "Faster page turns!"),
        text.replace("\"0.3.0\"", "\"0.4.0\""),
        // Even whitespace: the signature covers the raw bytes.
        text.replacen("\n", "\n ", 1),
        format!("{text}\n"),
    ] {
        assert_eq!(verify_manifest(tampered.as_bytes(), SIG, &[TEST_KEY]), Err(Error::BadSignature));
    }
}

#[test]
fn tampered_signature() {
    // The trusted comment is covered by the global signature.
    let relabelled = SIG.replace("trusted comment: verso 0.3.0", "trusted comment: verso 0.9.0");
    assert_eq!(verify_manifest(MANIFEST, &relabelled, &[TEST_KEY]), Err(Error::BadSignature));
    assert_eq!(verify_manifest(MANIFEST, "garbage", &[TEST_KEY]), Err(Error::BadSignatureFile));
    let mut lines: Vec<String> = SIG.lines().map(str::to_string).collect();
    lines[1] = lines[1].replacen('A', "B", 1);
    assert!(verify_manifest(MANIFEST, &lines.join("\n"), &[TEST_KEY]).is_err());
}

#[test]
fn trusted_comment_must_name_the_version() {
    // A genuine signature whose comment names another release: an old
    // manifest relabelled as new, or the other way round.
    assert_eq!(
        verify_manifest(MANIFEST, SIG_COMMENT, &[TEST_KEY]),
        Err(Error::TrustedComment { expected: "verso 0.3.0".into(), found: "verso 0.2.0".into() })
    );
}

#[test]
fn legacy_signatures_are_refused() {
    assert_eq!(verify_manifest(MANIFEST, SIG_LEGACY, &[TEST_KEY]), Err(Error::BadSignature));
}
