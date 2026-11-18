//! Builds the release public keys in `keys/*.pub` into the crate. Missing
//! keys give an empty list: such a build can't verify any release.
use std::{env, fmt::Write, fs, path::PathBuf};

fn main() {
    // Resolved at run time, so a moved checkout reads its own keys.
    let dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("keys");
    println!("cargo:rerun-if-changed={}", dir.display());
    let mut keys = Vec::new();
    if let Ok(entries) = fs::read_dir(&dir) {
        for entry in entries {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "pub") {
                let name = path.file_stem().unwrap().to_string_lossy().into_owned();
                keys.push((name, fs::read_to_string(&path).unwrap()));
            }
        }
    }
    // The main key first, then the rest by name.
    keys.sort_by(|a, b| (a.0 != "main", &a.0).cmp(&(b.0 != "main", &b.0)));
    let mut out = String::from("pub const RELEASE_KEYS: &[(&str, &str)] = &[\n");
    for (name, key) in keys {
        writeln!(out, "    ({name:?}, {key:?}),").unwrap();
    }
    out.push_str("];\n");
    fs::write(PathBuf::from(env::var("OUT_DIR").unwrap()).join("release_keys.rs"), out).unwrap();
}
