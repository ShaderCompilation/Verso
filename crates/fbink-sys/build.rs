//! Compiles a minimal FBInk (no fonts/images; we draw everything ourselves)
//! straight from the submodule. Only meaningful for e-reader (ARM) targets.

use std::path::PathBuf;

fn main() {
    let target_arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    if target_arch != "arm" {
        // Nothing to build for desktop targets; the crate is empty there.
        return;
    }

    // Build-script binaries can survive a repository move in target/.
    // Cargo supplies the current location when running the script.
    let root = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("../../third_party/FBInk");
    for f in ["fbink.c", "fbink.h", "cutef8/utf8.c", "cutef8/dfa.c"] {
        println!("cargo:rerun-if-changed={}", root.join(f).display());
    }

    let version = std::process::Command::new("git")
        .args(["-C", root.to_str().unwrap(), "describe", "--tags", "--always"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".into());

    cc::Build::new()
        .files([root.join("fbink.c"), root.join("cutef8/utf8.c"), root.join("cutef8/dfa.c")])
        .include(&root)
        .define("NDEBUG", None)
        .define("_REENTRANT", "1")
        .define("_GNU_SOURCE", None)
        .define("FBINK_FOR_KINDLE", None)
        .define("FBINK_MINIMAL", None)
        .define("FBINK_WITH_INPUT", None)
        .define("FBINK_VERSION", format!("\"{version}\"").as_str())
        .flag_if_supported("-mcpu=cortex-a7")
        .warnings(false)
        .opt_level(2)
        .compile("fbink");
}
