//! Verso's in-app updates, the shared part: release manifests, the record of
//! what is installed, version ordering, component hashes and signature
//! checks. The app reads and verifies with it; `verso-release` (the `release`
//! feature) writes releases. See docs/updates.md.
pub mod check;
mod manifest;
#[cfg(feature = "release")]
pub mod release;
mod sign;
mod version;

pub use manifest::{
    Archive, Component, FORMAT, FileEntry, INSTALLED_PATH, Installed, InstalledComponent, MAX_MANIFEST_BYTES,
    MAX_NOTES, MAX_SIGNATURE_BYTES, MODE_EXEC, MODE_FILE, Manifest, component_hash, is_safe_path, sha256_hex,
};
pub use sign::{release_key_names, release_keys, trusted_comment, verify_manifest};
pub use version::{ParseVersionError, Version};

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// This build has no release keys, so it can't check any release.
    NoKeys,
    /// A built-in key couldn't be read.
    BadKey,
    /// The signature file isn't a minisign signature.
    BadSignatureFile,
    /// Signed, but not by a key this build trusts.
    UnknownKey,
    /// The signature doesn't match the manifest: it was changed or damaged.
    BadSignature,
    /// The signed trusted comment doesn't name the manifest's version.
    TrustedComment { expected: String, found: String },
    /// A newer manifest format than this build understands.
    UnsupportedFormat(u32),
    /// The manifest is malformed or unsafe.
    Manifest(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NoKeys => write!(f, "this build has no release keys"),
            Error::BadKey => write!(f, "a built-in release key is unreadable"),
            Error::BadSignatureFile => write!(f, "the signature file is damaged"),
            Error::UnknownKey => write!(f, "the release is signed with an unknown key"),
            Error::BadSignature => write!(f, "the release doesn't match its signature"),
            Error::TrustedComment { expected, found } => {
                write!(f, "the signature is for {found:?}, not {expected:?}")
            }
            Error::UnsupportedFormat(n) => write!(f, "release format {n} needs a newer Verso"),
            Error::Manifest(msg) => write!(f, "the release manifest is invalid: {msg}"),
        }
    }
}

impl std::error::Error for Error {}
