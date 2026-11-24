//! Compiles a minimal FBInk (no fonts/images; we draw everything ourselves)
//! straight from the submodule. Only meaningful for e-reader (ARM) targets.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    if target_arch != "arm" {
        // Nothing to build for desktop targets; the crate is empty there.
        return;
    }

    // Build-script binaries can survive a repository move in target/.
    // Cargo supplies the current location when running the script.
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let root = manifest.join("../../third_party/FBInk");
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    println!("cargo:rerun-if-env-changed=VERSO_NATIVE_CACHE");
    for f in ["fbink.c", "fbink.h", "cutef8/utf8.c", "cutef8/dfa.c"] {
        println!("cargo:rerun-if-changed={}", root.join(f).display());
    }

    let version = Command::new("git")
        .args(["-C", root.to_str().unwrap(), "describe", "--tags", "--always"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".into());

    let cache_entry = native_cache_entry(&manifest, &root, &version);
    if let Some(ref entry) = cache_entry
        && restore_cached_lib(entry, &out, "fbink")
    {
        println!("cargo:rustc-link-search=native={}", out.display());
        println!("cargo:rustc-link-lib=static=fbink");
        return;
    }

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
        .flag_if_supported("-mcpu=cortex-a9")
        .warnings(false)
        .opt_level(2)
        .compile("fbink");

    if let Some(ref entry) = cache_entry {
        save_cached_lib(entry, &out, "fbink");
    }
}

fn native_cache_entry(manifest: &Path, fbink: &Path, version: &str) -> Option<PathBuf> {
    let root = env::var_os("VERSO_NATIVE_CACHE").filter(|v| !v.is_empty())?;
    let target = env::var("TARGET").ok()?;
    let opt = env::var("OPT_LEVEL").ok()?;
    let debug = env::var("DEBUG").ok()?;

    let mut h = Fnv128::new();
    h.field(b"fbink-sys-v1");
    h.field(target.as_bytes());
    h.field(opt.as_bytes());
    h.field(debug.as_bytes());
    h.field(env::var("PROFILE").unwrap_or_default().as_bytes());
    h.field(version.as_bytes());
    for var in ["CFLAGS", "TARGET_CFLAGS", "ZIG_VERSION"] {
        h.field(var.as_bytes());
        h.field(env::var(var).unwrap_or_default().as_bytes());
    }
    let ver = cc::Build::new().get_compiler().to_command().arg("--version").output().ok()?;
    if !ver.status.success() {
        return None;
    }
    h.field(&ver.stdout);
    h.field(&ver.stderr);
    if let Ok(out) = Command::new("zig").arg("version").output() {
        h.field(&out.stdout);
    }
    let head = Command::new("git").args(["-C", fbink.to_str()?, "rev-parse", "HEAD"]).output().ok()?;
    if !head.status.success() {
        return None;
    }
    h.field(&head.stdout);
    h.field(&fs::read(manifest.join("build.rs")).ok()?);
    Some(PathBuf::from(root).join("fbink-sys").join(format!("{target}-O{opt}-d{debug}-{}", h.finish_hex())))
}

fn restore_cached_lib(entry: &Path, out: &Path, lib: &str) -> bool {
    let name = format!("lib{lib}.a");
    let src = entry.join(&name);
    if !entry.join(".complete").is_file() || !src.is_file() {
        return false;
    }
    let dst = out.join(&name);
    let _ = fs::remove_file(&dst);
    fs::hard_link(&src, &dst).is_ok() || fs::copy(&src, &dst).is_ok()
}

fn save_cached_lib(entry: &Path, out: &Path, lib: &str) {
    if fs::create_dir_all(entry).is_err() {
        return;
    }
    let pid = std::process::id();
    let name = format!("lib{lib}.a");
    let tmp = entry.join(format!("{name}.tmp.{pid}"));
    let dst = entry.join(&name);
    if fs::copy(out.join(&name), &tmp).is_err() || fs::rename(&tmp, &dst).is_err() {
        let _ = fs::remove_file(&tmp);
        return;
    }
    let tmp = entry.join(format!(".complete.tmp.{pid}"));
    if fs::write(&tmp, b"ok\n").is_ok() && fs::rename(&tmp, entry.join(".complete")).is_err() {
        let _ = fs::remove_file(&tmp);
    }
}

struct Fnv128(u128);

impl Fnv128 {
    fn new() -> Self {
        Self(0x6c62272e07bb014262b821756295c58d)
    }

    fn field(&mut self, bytes: &[u8]) {
        self.update(&(bytes.len() as u64).to_le_bytes());
        self.update(bytes);
    }

    fn update(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 ^= u128::from(b);
            self.0 = self.0.wrapping_mul(0x0000000001000000000000000000013b);
        }
    }

    fn finish_hex(&self) -> String {
        format!("{:032x}", self.0)
    }
}
