//! Release signatures: `latest.json` is signed with minisign, and Verso
//! checks it against the public keys built into the binary.
use crate::{Error, Manifest, Version};
use minisign_verify::{PublicKey, Signature};

// `RELEASE_KEYS`: every `keys/*.pub` file, as (file stem, contents). Written by
// build.rs.
include!(concat!(env!("OUT_DIR"), "/release_keys.rs"));

/// The minisign public keys (the contents of `.pub` files) this build trusts:
/// the main release key and the offline backup key.
pub fn release_keys() -> Vec<&'static str> {
    RELEASE_KEYS.iter().map(|(_, key)| *key).collect()
}

/// The names of the built-in keys (`main`, `backup`), for messages.
pub fn release_key_names() -> Vec<&'static str> {
    RELEASE_KEYS.iter().map(|(name, _)| *name).collect()
}

/// The trusted comment a release's signature must carry. It is covered by the
/// signature, so an old manifest can't be presented as a newer release.
pub fn trusted_comment(version: &Version) -> String {
    format!("verso {version}")
}

/// Checks `signature` (the text of `latest.json.minisig`) over the raw
/// manifest bytes with `keys` (`.pub` file contents or bare base64 keys), and
/// only then parses and validates the manifest.
pub fn verify_manifest(manifest: &[u8], signature: &str, keys: &[&str]) -> Result<Manifest, Error> {
    if keys.is_empty() {
        return Err(Error::NoKeys);
    }
    let signature = Signature::decode(signature).map_err(|_| Error::BadSignatureFile)?;
    let mut signed = false;
    for key in keys {
        let key = PublicKey::decode(key).or_else(|_| PublicKey::from_base64(key.trim())).map_err(|_| Error::BadKey)?;
        match key.verify(manifest, &signature, false) {
            Ok(()) => {
                signed = true;
                break;
            }
            Err(minisign_verify::Error::UnexpectedKeyId) => continue,
            Err(_) => return Err(Error::BadSignature),
        }
    }
    if !signed {
        return Err(Error::UnknownKey);
    }
    let parsed = Manifest::parse(manifest)?;
    let expected = trusted_comment(&parsed.version);
    if signature.trusted_comment() != expected {
        return Err(Error::TrustedComment { expected, found: signature.trusted_comment().to_string() });
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_keys_parse() {
        for (name, key) in RELEASE_KEYS {
            assert!(PublicKey::decode(key).is_ok(), "keys/{name}.pub isn't a minisign public key");
        }
    }
}
