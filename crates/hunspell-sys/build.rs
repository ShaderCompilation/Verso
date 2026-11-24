//! Builds Hunspell's library (`src/hunspell/*.cxx` from the
//! third_party/hunspell submodule) and Verso's small C++ shim with the `cc`
//! crate, the same way crengine-sys builds crengine: the same sources for the
//! desktop and the Kindle, no autotools.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const SOURCES: &[&str] = &[
    "affentry.cxx",
    "affixmgr.cxx",
    "csutil.cxx",
    "filemgr.cxx",
    "hashmgr.cxx",
    "hunspell.cxx",
    "hunspelltrace.cxx",
    "hunzip.cxx",
    "phonet.cxx",
    "replist.cxx",
    "suggestmgr.cxx",
];

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let hunspell = manifest.join("../../third_party/hunspell").canonicalize().expect("third_party/hunspell missing");
    let src = hunspell.join("src/hunspell");
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let arm = env::var("CARGO_CFG_TARGET_ARCH").unwrap() == "arm";

    println!("cargo:rerun-if-env-changed=VERSO_NATIVE_CACHE");
    println!("cargo:rerun-if-changed=shim");
    // Rebuild when the submodule moves to another commit.
    println!("cargo:rerun-if-changed={}", hunspell.join(".git").display());

    let cache_entry = native_cache_entry(&manifest, &hunspell, arm);
    if let Some(ref entry) = cache_entry
        && restore_cached_lib(entry, &out, "hunspell")
    {
        println!("cargo:rustc-link-search=native={}", out.display());
        println!("cargo:rustc-link-lib=static=hunspell");
        println!("cargo:rustc-link-lib={}", if arm { "c++" } else { "stdc++" });
        return;
    }

    let mut b = cc::Build::new();
    b.cpp(true)
        .cpp_link_stdlib(None)
        .std("c++14")
        .warnings(false)
        .define("NDEBUG", None)
        // A static library: no export attributes on the C API.
        .define("HUNSPELL_STATIC", None)
        .define("BUILDING_LIBHUNSPELL", None)
        .flag_if_supported("-fvisibility=hidden")
        .include(&src);
    if arm {
        b.flag("-mcpu=cortex-a9");
    }
    for name in SOURCES {
        b.file(src.join(name));
    }
    b.file(manifest.join("shim/verso_hunspell.cxx"));
    b.compile("hunspell");

    if let Some(ref entry) = cache_entry {
        save_cached_lib(entry, &out, "hunspell");
    }

    // C++ runtime: Zig's libc++ (linked statically) on the Kindle, the system
    // libstdc++ on the desktop, as in crengine-sys.
    println!("cargo:rustc-link-lib={}", if arm { "c++" } else { "stdc++" });
}

fn native_cache_entry(manifest: &Path, hunspell: &Path, arm: bool) -> Option<PathBuf> {
    let root = env::var_os("VERSO_NATIVE_CACHE").filter(|v| !v.is_empty())?;
    let target = env::var("TARGET").ok()?;
    let opt = env::var("OPT_LEVEL").ok()?;
    let debug = env::var("DEBUG").ok()?;

    let mut h = Fnv128::new();
    h.field(b"hunspell-sys-v2");
    h.field(target.as_bytes());
    h.field(opt.as_bytes());
    h.field(debug.as_bytes());
    h.field(env::var("PROFILE").unwrap_or_default().as_bytes());
    for var in ["CXXFLAGS", "TARGET_CXXFLAGS", "ZIG_VERSION"] {
        h.field(var.as_bytes());
        h.field(env::var(var).unwrap_or_default().as_bytes());
    }
    let mut b = cc::Build::new();
    b.cpp(true);
    let compiler = b.get_compiler();
    if let Some(name) = compiler.path().file_name() {
        h.field(name.to_string_lossy().as_bytes());
    }
    let ver = compiler.to_command().arg("--version").output().ok()?;
    if !ver.status.success() {
        return None;
    }
    h.field(&ver.stdout);
    h.field(&ver.stderr);
    if arm && let Ok(out) = Command::new("zig").arg("version").output() {
        h.field(&out.stdout);
    }
    let head = Command::new("git").args(["-C", hunspell.to_str()?, "rev-parse", "HEAD"]).output().ok()?;
    if !head.status.success() {
        return None;
    }
    h.field(&head.stdout);
    for rel in ["build.rs", "shim"] {
        hash_tree(&mut h, manifest, &manifest.join(rel))?;
    }
    Some(PathBuf::from(root).join("hunspell-sys").join(format!("{target}-O{opt}-d{debug}-{}", h.finish_hex())))
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

fn hash_tree(h: &mut Fnv128, base: &Path, path: &Path) -> Option<()> {
    if path.is_file() {
        let rel = path.strip_prefix(base).ok()?;
        h.field(rel.to_string_lossy().replace('\\', "/").as_bytes());
        h.field(&fs::read(path).ok()?);
        return Some(());
    }
    let mut entries: Vec<PathBuf> = fs::read_dir(path).ok()?.filter_map(|e| Some(e.ok()?.path())).collect();
    entries.sort();
    for entry in entries {
        hash_tree(h, base, &entry)?;
    }
    Some(())
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
