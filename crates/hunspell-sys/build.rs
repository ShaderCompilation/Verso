//! Builds Hunspell's library (`src/hunspell/*.cxx` from the
//! third_party/hunspell submodule) and Verso's small C++ shim with the `cc`
//! crate, the same way crengine-sys builds crengine: the same sources for the
//! desktop and the Kindle, no autotools.

use std::env;
use std::path::PathBuf;

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
    let arm = env::var("CARGO_CFG_TARGET_ARCH").unwrap() == "arm";

    println!("cargo:rerun-if-changed=shim");
    // Rebuild when the submodule moves to another commit.
    println!("cargo:rerun-if-changed={}", hunspell.join(".git").display());

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

    // C++ runtime: Zig's libc++ (linked statically) on the Kindle, the system
    // libstdc++ on the desktop, as in crengine-sys.
    println!("cargo:rustc-link-lib={}", if arm { "c++" } else { "stdc++" });
}
