//! Builds crengine (KOReader's fork) and the C libraries it needs, straight
//! from the submodules in third_party/, with the `cc` crate. No CMake, Meson or
//! system libraries: the same sources build for the desktop and the Kindle.
//!
//! Which optional crengine features are on is decided in config/crsetup.h;
//! every library enabled there must be compiled here. LunaSVG is the one
//! exception to "from the submodules": it is vendored in lunasvg/ (see its
//! README).

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const LINK_ORDER: &[&str] = &[
    "verso_crengine",
    "verso_lunasvg",
    "verso_plutovg",
    "verso_harfbuzz",
    "verso_freetype",
    "verso_png",
    "verso_jpeg",
    "verso_z",
    "verso_unibreak",
    "verso_utf8proc",
];

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let tp = manifest.join("../../third_party").canonicalize().expect("third_party/ missing");
    let config = manifest.join("config");
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let arm = env::var("CARGO_CFG_TARGET_ARCH").unwrap() == "arm";

    println!("cargo:rerun-if-env-changed=VERSO_NATIVE_CACHE");
    println!("cargo:rerun-if-changed=config");
    println!("cargo:rerun-if-changed=shim");
    println!("cargo:rerun-if-changed=lunasvg");
    for lib in LIBS {
        // Rebuild when a submodule moves to another commit.
        println!("cargo:rerun-if-changed={}", tp.join(lib).join(".git").display());
    }

    let cache_entry = native_cache_entry(&manifest, &tp, arm);
    if let Some(ref entry) = cache_entry
        && restore_cached_libs(entry, &out, LINK_ORDER)
    {
        emit_link_directives(&out, arm);
        return;
    }

    // Generated headers.
    let gen_inc = out.join("include");
    fs::create_dir_all(&gen_inc).unwrap();
    fs::copy(tp.join("libpng/scripts/pnglibconf.h.prebuilt"), gen_inc.join("pnglibconf.h")).unwrap();

    let ctx = Ctx { tp: tp.clone(), config: config.clone(), lunasvg: manifest.join("lunasvg"), gen_inc, arm };

    // Order matters for linking: a library must come before the ones it uses.
    let mut link_order = Vec::new();
    ctx.shim_and_crengine(&mut link_order);
    ctx.lunasvg(&mut link_order);
    ctx.harfbuzz(&mut link_order);
    ctx.freetype(&mut link_order);
    ctx.libpng(&mut link_order);
    ctx.libjpeg(&mut link_order);
    ctx.zlib(&mut link_order);
    ctx.libunibreak(&mut link_order);
    ctx.utf8proc(&mut link_order);
    assert_eq!(link_order, LINK_ORDER);

    if let Some(ref entry) = cache_entry {
        save_cached_libs(entry, &out, LINK_ORDER);
    }

    emit_link_directives(&out, arm);
}

fn emit_link_directives(out: &Path, arm: bool) {
    println!("cargo:rustc-link-search=native={}", out.display());
    for lib in LINK_ORDER {
        println!("cargo:rustc-link-lib=static={lib}");
    }
    // C++ runtime: Zig's libc++ (linked statically) on the Kindle, the system
    // libstdc++ on the desktop.
    println!("cargo:rustc-link-lib={}", if arm { "c++" } else { "stdc++" });
}

/// Content-addressed cache directory under `VERSO_NATIVE_CACHE` so CI can reuse
/// the compiled static archives when submodules and build inputs are unchanged.
fn native_cache_entry(manifest: &Path, tp: &Path, arm: bool) -> Option<PathBuf> {
    let root = env::var_os("VERSO_NATIVE_CACHE").filter(|v| !v.is_empty())?;
    let target = env::var("TARGET").ok()?;
    let opt = env::var("OPT_LEVEL").ok()?;
    let debug = env::var("DEBUG").ok()?;

    let mut h = Fnv128::new();
    h.field(b"crengine-sys-v1");
    h.field(target.as_bytes());
    h.field(opt.as_bytes());
    h.field(debug.as_bytes());
    h.field(env::var("PROFILE").unwrap_or_default().as_bytes());
    for var in ["CFLAGS", "CXXFLAGS", "TARGET_CFLAGS", "TARGET_CXXFLAGS", "ZIG_VERSION"] {
        h.field(var.as_bytes());
        h.field(env::var(var).unwrap_or_default().as_bytes());
    }
    for cpp in [false, true] {
        let mut b = cc::Build::new();
        b.cpp(cpp);
        let out = b.get_compiler().to_command().arg("--version").output().ok()?;
        if !out.status.success() {
            return None;
        }
        h.field(&out.stdout);
        h.field(&out.stderr);
    }
    if arm && let Ok(out) = Command::new("zig").arg("version").output() {
        h.field(&out.stdout);
    }
    for lib in LIBS {
        let out = Command::new("git").args(["-C", tp.join(lib).to_str()?, "rev-parse", "HEAD"]).output().ok()?;
        if !out.status.success() {
            return None;
        }
        h.field(lib.as_bytes());
        h.field(&out.stdout);
    }
    for rel in ["build.rs", "config", "shim", "lunasvg"] {
        hash_tree(&mut h, manifest, &manifest.join(rel))?;
    }
    Some(PathBuf::from(root).join("crengine-sys").join(format!("{target}-O{opt}-d{debug}-{}", h.finish_hex())))
}

fn restore_cached_libs(entry: &Path, out: &Path, libs: &[&str]) -> bool {
    if !entry.join(".complete").is_file() {
        return false;
    }
    for lib in libs {
        if !entry.join(format!("lib{lib}.a")).is_file() {
            return false;
        }
    }
    for lib in libs {
        let name = format!("lib{lib}.a");
        let src = entry.join(&name);
        let dst = out.join(&name);
        let _ = fs::remove_file(&dst);
        if fs::hard_link(&src, &dst).is_err() && fs::copy(&src, &dst).is_err() {
            return false;
        }
    }
    true
}

fn save_cached_libs(entry: &Path, out: &Path, libs: &[&str]) {
    if fs::create_dir_all(entry).is_err() {
        return;
    }
    let pid = std::process::id();
    for lib in libs {
        let name = format!("lib{lib}.a");
        let tmp = entry.join(format!("{name}.tmp.{pid}"));
        let dst = entry.join(&name);
        if fs::copy(out.join(&name), &tmp).is_err() || fs::rename(&tmp, &dst).is_err() {
            let _ = fs::remove_file(&tmp);
            return;
        }
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

const LIBS: &[&str] =
    &["crengine", "freetype", "harfbuzz", "libjpeg-turbo", "libpng", "libunibreak", "utf8proc", "xxhash", "zlib"];

struct Ctx {
    tp: PathBuf,
    config: PathBuf,
    lunasvg: PathBuf,
    gen_inc: PathBuf,
    arm: bool,
}

impl Ctx {
    fn p(&self, rel: &str) -> PathBuf {
        self.tp.join(rel)
    }

    /// A C build with our common settings.
    fn c(&self) -> cc::Build {
        let mut b = cc::Build::new();
        b.warnings(false).cargo_metadata(false).define("NDEBUG", None).flag_if_supported("-fvisibility=hidden");
        if self.arm {
            b.flag("-mcpu=cortex-a9");
        }
        b
    }

    fn cpp(&self) -> cc::Build {
        let mut b = self.c();
        b.cpp(true).cpp_link_stdlib(None).std("c++17");
        b
    }

    fn files(&self, dir: &str, names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(|n| self.p(dir).join(n)).collect()
    }

    fn zlib(&self, link: &mut Vec<&'static str>) {
        let names = [
            "adler32.c",
            "compress.c",
            "crc32.c",
            "deflate.c",
            "infback.c",
            "inffast.c",
            "inflate.c",
            "inftrees.c",
            "trees.c",
            "uncompr.c",
            "zutil.c",
        ];
        self.c()
            .files(self.files("zlib", &names))
            .include(self.p("zlib"))
            .define("_LARGEFILE64_SOURCE", "1")
            .define("HAVE_UNISTD_H", None)
            .compile("verso_z");
        link.push("verso_z");
    }

    fn libpng(&self, link: &mut Vec<&'static str>) {
        let names = [
            "png.c",
            "pngerror.c",
            "pngget.c",
            "pngmem.c",
            "pngpread.c",
            "pngread.c",
            "pngrio.c",
            "pngrtran.c",
            "pngrutil.c",
            "pngset.c",
            "pngtrans.c",
            "pngwio.c",
            "pngwrite.c",
            "pngwtran.c",
            "pngwutil.c",
        ];
        self.c()
            .files(self.files("libpng", &names))
            .include(self.p("libpng"))
            .include(&self.gen_inc)
            .include(self.p("zlib"))
            .define("PNG_ARM_NEON_OPT", "0") // plain C; images are decoded rarely
            .compile("verso_png");
        link.push("verso_png");
    }

    fn libjpeg(&self, link: &mut Vec<&'static str>) {
        let mut b = self.c();
        let src = self.p("libjpeg-turbo/src");
        for f in JPEG_SOURCES {
            b.file(src.join(f));
        }
        b.include(self.config.join("jpeg")).include(&src).compile("verso_jpeg");
        link.push("verso_jpeg");
    }

    fn freetype(&self, link: &mut Vec<&'static str>) {
        let names = [
            "base/ftsystem.c",
            "base/ftinit.c",
            "base/ftdebug.c",
            "base/ftbase.c",
            "base/ftbbox.c",
            "base/ftbdf.c",
            "base/ftbitmap.c",
            "base/ftcid.c",
            "base/ftfstype.c",
            "base/ftgasp.c",
            "base/ftglyph.c",
            "base/ftgxval.c",
            "base/ftmm.c",
            "base/ftotval.c",
            "base/ftpatent.c",
            "base/ftpfr.c",
            "base/ftstroke.c",
            "base/ftsynth.c",
            "base/fttype1.c",
            "base/ftwinfnt.c",
            "autofit/autofit.c",
            "truetype/truetype.c",
            "type1/type1.c",
            "cff/cff.c",
            "cid/type1cid.c",
            "psaux/psaux.c",
            "pshinter/pshinter.c",
            "psnames/psnames.c",
            "sfnt/sfnt.c",
            "smooth/smooth.c",
            "gzip/ftgzip.c",
            "lzw/ftlzw.c",
        ];
        self.c()
            .files(self.files("freetype/src", &names))
            .include(&self.config) // our ftmodule.h
            .include(self.p("freetype/include"))
            .define("FT2_BUILD_LIBRARY", None)
            .define("FT_CONFIG_MODULES_H", "<ftmodule.h>")
            .compile("verso_freetype");
        link.push("verso_freetype");
    }

    fn harfbuzz(&self, link: &mut Vec<&'static str>) {
        let mut b = self.cpp();
        b.file(self.p("harfbuzz/src/harfbuzz.cc"))
            .include(self.p("freetype/include"))
            .define("HAVE_FREETYPE", "1")
            .define("HAVE_FT_GET_VAR_BLEND_COORDINATES", "1")
            .define("HAVE_FT_SET_VAR_BLEND_COORDINATES", "1")
            .define("HAVE_FT_DONE_MM_VAR", "1")
            .define("HAVE_FT_GET_TRANSFORM", "1")
            .flag("-fno-exceptions")
            .flag("-fno-rtti")
            .flag_if_supported("-fno-threadsafe-statics");
        // Features crengine doesn't use (same list as KOReader).
        for d in [
            "HB_DISABLE_DEPRECATED",
            "HB_NO_BEYOND_64K",
            "HB_NO_BUFFER_MESSAGE",
            "HB_NO_BUFFER_SERIALIZE",
            "HB_NO_BUFFER_VERIFY",
            "HB_NO_COLOR",
            "HB_NO_CUBIC_GLYF",
            "HB_NO_LAYOUT_COLLECT_GLYPHS",
            "HB_NO_LAYOUT_FEATURE_PARAMS",
            "HB_NO_LAYOUT_RARELY_USED",
            "HB_NO_LAYOUT_UNUSED",
            "HB_NO_META",
            "HB_NO_OT_FETCH",
            "HB_NO_OT_FONT_GLYPH_NAMES",
            "HB_NO_PAINT",
            "HB_NO_STYLE",
            "HB_NO_VAR_COMPOSITES",
        ] {
            b.define(d, None);
        }
        b.compile("verso_harfbuzz");
        link.push("verso_harfbuzz");
    }

    fn libunibreak(&self, link: &mut Vec<&'static str>) {
        let names = [
            "unibreakbase.c",
            "unibreakdef.c",
            "linebreak.c",
            "linebreakdata.c",
            "linebreakdef.c",
            "eastasianwidthdef.c",
            "emojidef.c",
            "graphemebreak.c",
            "wordbreak.c",
        ];
        self.c().files(self.files("libunibreak/src", &names)).compile("verso_unibreak");
        link.push("verso_unibreak");
    }

    fn utf8proc(&self, link: &mut Vec<&'static str>) {
        self.c().file(self.p("utf8proc/utf8proc.c")).define("UTF8PROC_STATIC", None).compile("verso_utf8proc");
        link.push("verso_utf8proc");
    }

    fn shim_and_crengine(&self, link: &mut Vec<&'static str>) {
        let cre = self.patched_crengine();
        let mut b = self.cpp();
        b.file(PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("shim/verso_cre.cpp"));
        b.file(cre.join("crengine/qimagescale/qimagescale.cpp"));
        for f in CRENGINE_SOURCES {
            b.file(cre.join("crengine/src").join(f));
        }
        b.include(&self.config) // crsetup.h
            .include(cre.join("crengine/include"))
            .include(self.p("freetype/include"))
            .include(self.p("harfbuzz/src"))
            .include(self.p("libunibreak/src"))
            .include(self.p("utf8proc"))
            .include(self.p("xxhash"))
            .include(self.p("zlib"))
            .include(self.p("libpng"))
            .include(&self.gen_inc)
            .include(self.config.join("jpeg"))
            .include(self.p("libjpeg-turbo/src"))
            .include(self.lunasvg.join("include"))
            .include(self.lunasvg.join("xtended"))
            .define("UTF8PROC_STATIC", None)
            .define("XXH_INLINE_ALL", None) // xxhash is header-only this way
            .flag_if_supported("-ftabstop=4")
            .flag_if_supported("-fvisibility-inlines-hidden");
        b.compile("verso_crengine");
        link.push("verso_crengine");
    }

    /// LunaSVG with KOReader's extensions (crengine's SVG decoder, lvimg.cpp)
    /// and plutovg, the rasterizer it draws with.
    fn lunasvg(&self, link: &mut Vec<&'static str>) {
        let pv = self.lunasvg.join("3rdparty/plutovg");
        let mut c = self.c();
        c.files(sources(&pv, "c")).include(&pv).compile("verso_plutovg");

        let mut b = self.cpp();
        for dir in ["source", "xtended"] {
            b.files(sources(&self.lunasvg.join(dir), "cpp"));
        }
        for dir in ["include", "source", "xtended", "3rdparty/plutovg"] {
            b.include(self.lunasvg.join(dir));
        }
        b.compile("verso_lunasvg");
        link.push("verso_lunasvg");
        link.push("verso_plutovg");
    }
}

/// Every file in `dir` with extension `ext`, sorted. Only for vendored trees
/// that hold nothing but what we build.
fn sources(dir: &Path, ext: &str) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == ext))
        .collect();
    v.sort();
    v
}

impl Ctx {
    /// Copies crengine's sources into OUT_DIR and applies our fixes there, so
    /// the submodule itself stays untouched. Returns the copy's root.
    fn patched_crengine(&self) -> PathBuf {
        let src = self.p("crengine");
        let dst = self.gen_inc.parent().unwrap().join("crengine");
        for dir in ["crengine/src", "crengine/include", "crengine/qimagescale"] {
            copy_dir(&src.join(dir), &dst.join(dir));
        }
        for (file, from, to) in CRENGINE_FIXES {
            let path = dst.join(file);
            let text = fs::read_to_string(&path).unwrap();
            assert!(text.contains(from), "crengine fix no longer applies to {file}: {from:?}");
            fs::write(&path, text.replace(from, to)).unwrap();
        }
        dst
    }
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// Small fixes to crengine: (file, text to find, replacement).
const CRENGINE_FIXES: &[(&str, &str, &str)] = &[
    // Verso registers fonts from a list of their faces written on an earlier
    // start, so it doesn't open every font file each time (93 took 0.4–1.3 s
    // on the Kindle). Registering only some fonts instead would change
    // GetFontListHash() and make every cached book lay out again.
    (
        "crengine/include/lvfntman.h",
        "    virtual bool RegisterFont( lString8 name ) = 0;",
        "    virtual bool RegisterFont( lString8 name ) = 0;\n    /// Verso: the faces registered from font file `name`, one line each, for\n    /// RegisterFontFaces(). Empty when none were.\n    virtual lString8 DescribeFontFile( lString8 /*name*/ ) { return lString8::empty_str; }\n    /// Verso: registers the faces DescribeFontFile() described, without\n    /// opening the file. How many, or -1 (and none) for a malformed line.\n    virtual int RegisterFontFaces( lString8 /*name*/, lString8 /*description*/ ) { return -1; }",
    ),
    (
        "crengine/src/lvfntman.cpp",
        "    void GetAvailableFontWeights(LVArray<int>& weights, lString8 typeface) {\n        FONT_MAN_GUARD\n        weights.clear();",
        "    // Verso: see LVFontManager::DescribeFontFile(). A line holds face_index,\n    // italic, CSS family, emojis, math and small caps, then has/min/max for\n    // the wght, opsz, ital, slnt and wdth axes, then the family name.\n    virtual lString8 DescribeFontFile( lString8 name )\n    {\n        FONT_MAN_GUARD\n        LVArray<const LVFontFace*> faces;\n        for (int i = 0; i < _registry.familyCount(); i++) {\n            const LVFontFamily* f = _registry.familyAt(i);\n            for (int j = 0; j < f->faceCount(); j++) {\n                const LVFontFace& face = f->faceAt(j);\n                if (face.documentId == -1 && face.file_path == name)\n                    faces.add(&face);\n            }\n        }\n        // In file order, as RegisterFont() registered them.\n        for (int i = 1; i < faces.length(); i++)\n            for (int j = i; j > 0 && faces[j-1]->face_index > faces[j]->face_index; j--) {\n                const LVFontFace* t = faces[j-1];\n                faces[j-1] = faces[j];\n                faces[j] = t;\n            }\n        lString8 out;\n        for (int i = 0; i < faces.length(); i++) {\n            const LVFontFace& f = *faces[i];\n            if (f.typeface.empty() || f.typeface.pos(cs8(\"\\t\")) >= 0 || f.typeface.pos(cs8(\"\\n\")) >= 0)\n                return lString8::empty_str;\n            char buf[128];\n            snprintf(buf, sizeof(buf), \"%d\\t%d\\t%d\\t%d\\t%d\\t%d\", f.face_index, f.is_italic ? 1 : 0,\n                     (int)f.css_family, f.has_emojis ? 1 : 0, f.has_ot_math ? 1 : 0, f.has_small_caps ? 1 : 0);\n            out << buf;\n            const bool has[5] = { f._has_wght, f._has_opsz, f._has_ital, f._has_slnt, f._has_wdth };\n            const float lo[5] = { f._wght_min, f._opsz_min, f._ital_min, f._slnt_min, f._wdth_min };\n            const float hi[5] = { f._wght_max, f._opsz_max, f._ital_max, f._slnt_max, f._wdth_max };\n            for (int k = 0; k < 5; k++) {\n                snprintf(buf, sizeof(buf), \"\\t%d\\t%.9g\\t%.9g\", has[k] ? 1 : 0, (double)lo[k], (double)hi[k]);\n                out << buf;\n            }\n            out << \"\\t\" << f.typeface << \"\\n\";\n        }\n        return out;\n    }\n\n    // Verso: see LVFontManager::RegisterFontFaces(). Like RegisterFont(), it\n    // stops at the first face that is already registered.\n    virtual int RegisterFontFaces( lString8 name, lString8 description )\n    {\n        FONT_MAN_GUARD\n        static const lUInt32 tags[5] = { LVFONT_TAG_WGHT, LVFONT_TAG_OPSZ, LVFONT_TAG_ITAL, LVFONT_TAG_SLNT, LVFONT_TAG_WDTH };\n        LVPtrVector<LVFontFace, true> faces;\n        const char* p = description.c_str();\n        while (*p) {\n            LVFontFace def;\n            def.file_path = name;\n            char* end;\n            long v[6];\n            for (int k = 0; k < 6; k++) {\n                v[k] = strtol(p, &end, 10);\n                if (end == p || *end != '\\t') return -1;\n                p = end + 1;\n            }\n            def.face_index = (int)v[0];\n            def.is_italic = v[1] != 0;\n            def.css_family = (css_font_family_t)v[2];\n            def.has_emojis = v[3] != 0;\n            def.has_ot_math = v[4] != 0;\n            def.has_small_caps = v[5] != 0;\n            for (int k = 0; k < 5; k++) {\n                long has = strtol(p, &end, 10);\n                if (end == p || *end != '\\t') return -1;\n                p = end + 1;\n                float lo = strtof(p, &end);\n                if (end == p || *end != '\\t') return -1;\n                p = end + 1;\n                float hi = strtof(p, &end);\n                if (end == p || *end != '\\t') return -1;\n                p = end + 1;\n                if (has)\n                    def.setAxisInfo(tags[k], lo, hi);\n            }\n            const char* nl = strchr(p, '\\n');\n            if (!nl || nl == p) return -1;\n            def.typeface = lString8(p, (int)(nl - p));\n            p = nl + 1;\n            faces.add(new LVFontFace(def));\n        }\n        int count = 0;\n        for (int i = 0; i < faces.length(); i++) {\n            if (!tryRegisterFace(*faces[i]))\n                break;\n            count++;\n        }\n        return count;\n    }\n\n    void GetAvailableFontWeights(LVArray<int>& weights, lString8 typeface) {\n        FONT_MAN_GUARD\n        weights.clear();",
    ),
    // The glyph cache item has an `alignas(16)` member, but is allocated with
    // malloc, which only guarantees 8-byte alignment on 32-bit ARM. Clang then
    // uses 16-byte-aligned NEON stores on it and the Kindle crashes (SIGBUS).
    (
        "crengine/include/lvfntman.h",
        "struct LVFontGlyphCacheItem\n{",
        "static inline void *verso_malloc16(size_t size) {\n    void *p = NULL;\n    return posix_memalign(&p, 16, size) == 0 ? p : NULL;\n}\n\nstruct LVFontGlyphCacheItem\n{",
    ),
    ("crengine/include/lvfntman.h", "(LVFontGlyphCacheItem *)malloc(", "(LVFontGlyphCacheItem *)verso_malloc16("),
    // The document cache index (`cr3cache.inx`) is rewritten in place. Verso's
    // background pre-layout process can be killed at any moment, and an index
    // cut short there would drop every cached book at the next start. Write a
    // temporary file and rename it over the index instead.
    (
        "crengine/src/lvtinydom.cpp",
        "            LVStreamRef stream = LVOpenFileStream(filename.c_str(), LVOM_WRITE);\n            if ( !stream )\n                return false;\n            if ( stream->Write( buf.buf(), buf.pos(), NULL )!=LVERR_OK )\n                return false;\n            _oldStreamCRC = newCRC;",
        "            lString32 tmpname = filename + \".tmp\";\n            {\n                LVStreamRef stream = LVOpenFileStream(tmpname.c_str(), LVOM_WRITE);\n                if ( !stream )\n                    return false;\n                if ( stream->Write( buf.buf(), buf.pos(), NULL )!=LVERR_OK )\n                    return false;\n            }\n            if ( !LVRenameFile( tmpname, filename ) )\n                return false;\n            _oldStreamCRC = newCRC;",
    ),
    // crengine skips writing the index when its CRC32 matches the last one
    // written. That CRC covers the index's own trailing CRC, and CRC32 over data
    // followed by its own CRC is the same for every index of one length. So a
    // reopened book moved to the top of the index was never saved, and eviction
    // (oldest first) could drop a book read every day. Leave the trailing CRC
    // out on both sides.
    (
        "crengine/src/lvtinydom.cpp",
        "                _oldStreamCRC = (lUInt32)oldStream->getcrc32();",
        "                _oldStreamCRC = 0;\n                lUInt8 * old = _oldStreamSize > 4 ? (lUInt8 *)malloc( _oldStreamSize ) : NULL;\n                lvsize_t got = 0;\n                if ( old && oldStream->Read( old, _oldStreamSize, &got ) == LVERR_OK && got == _oldStreamSize )\n                    _oldStreamCRC = lStr_crc32( 0, old, (int)_oldStreamSize - 4 );\n                free( old );",
    ),
    (
        "crengine/src/lvtinydom.cpp",
        "        lUInt32 newCRC = buf.getCRC();",
        "        lUInt32 newCRC = lStr_crc32( 0, buf.buf(), buf.pos() - 4 );",
    ),
    // Some repackaged EPUBs leave out the `mimetype` entry. crengine then
    // treats the book as a plain ZIP and shows the first file in it (often a
    // stray text file). Accept a ZIP without `mimetype` as an EPUB when it has
    // the EPUB container file, like the library's metadata reader does.
    (
        "crengine/src/epubfmt.cpp",
        "    if ( mimeType != U\"application/epub+zip\" )\n        return false;",
        "    if ( mimeType.empty() )\n        return !m_arc->OpenStream(U\"META-INF/container.xml\", LVOM_READ).isNull();\n    if ( mimeType != U\"application/epub+zip\" )\n        return false;",
    ),
    // Large images inside an SVG used to cost two full-size copies: libpng
    // decoded the whole image into one buffer, and smooth scaling (always on
    // inside an SVG) kept another to scale from. The 2481x3509 PNG cover of
    // *Go with Domain* took 70 MB that way. A PNG without
    // interlacing is now decoded one line at a time.
    (
        "crengine/src/lvimg.cpp",
        "        png_set_interlace_handling(png_ptr);",
        "        const int verso_png_passes = png_set_interlace_handling(png_ptr);",
    ),
    (
        "crengine/src/lvimg.cpp",
        "        size_t rowbytes = png_get_rowbytes(png_ptr, info_ptr);\n        size_t image_size = height * rowbytes;",
        r#"        if ( verso_png_passes == 1 ) {
            // Verso: one line at a time (see build.rs).
            png_bytep row = (png_bytep) malloc(png_get_rowbytes(png_ptr, info_ptr));
            if ( !row )
                lvpng_error_func(png_ptr, "cannot allocate png row");
            for (size_t y = 0; y < height; y++) {
                png_read_row(png_ptr, row, NULL);
                callback->OnLineDecoded( this, y, reinterpret_cast<lUInt32 *>(row) );
            }
            png_read_end(png_ptr, info_ptr);
            callback->OnEndDecode(this, false);
            free(row);
        } else {
        size_t rowbytes = png_get_rowbytes(png_ptr, info_ptr);
        size_t image_size = height * rowbytes;"#,
    ),
    (
        "crengine/src/lvimg.cpp",
        "        callback->OnEndDecode(this, false);\n        free(storage);\n    }",
        "        callback->OnEndDecode(this, false);\n        free(storage);\n        }\n    }",
    ),
    // Smooth scaling now keeps the image shrunk by the largest whole factor
    // that leaves it at least as large as the target, averaging each block of
    // pixels as the lines come in, and scales from that. The copy is then less
    // than 4 times the target's area, however large the image is.
    (
        "crengine/src/lvdrawbuf.cpp",
        "    lUInt8 * __restrict decoded;\n    bool isNinePatch;",
        r#"    lUInt8 * __restrict decoded;
    // Verso: the shrink factor, the size of the shrunk copy in decoded, the
    // sums of the source lines not yet written to it, and whether the lines
    // have come in order so far (an interlaced GIF sends them out of order).
    int shrink, red_dx, red_dy, next_y;
    bool lines_in_order;
    lUInt32 * __restrict acc;
    // Verso: writes the average of the source lines summed in acc (rows of
    // them) as line ry of the shrunk copy, and clears acc.
    // Divides by multiplying with a 32.32 fixed-point reciprocal: the Kindle
    // build has no hardware divide, and a division per channel made the
    // 2481x3509 cover draw 150 ms slower.
    void flushShrunkLine( int ry, int rows ) {
        lUInt8 * __restrict out = decoded + ry * (red_dx * 4);
        const int last_cols = src_dx - (red_dx - 1) * shrink;
        const lUInt64 inv = ((1ULL << 32) + rows * shrink / 2) / (lUInt64)(rows * shrink);
        const lUInt64 inv_last = ((1ULL << 32) + rows * last_cols / 2) / (lUInt64)(rows * last_cols);
        for (int x = 0; x < red_dx; x++) {
            const lUInt64 r = x == red_dx - 1 ? inv_last : inv;
            for (int c = 0; c < 4; c++)
                out[x * 4 + c] = (lUInt8)((acc[x * 4 + c] * r + (1ULL << 31)) >> 32);
        }
        memset(acc, 0, red_dx * 4 * sizeof(lUInt32));
    }
    bool isNinePatch;"#,
    ),
    (
        "crengine/src/lvdrawbuf.cpp",
        "        if (smoothscale) {\n            // Byte-sized buffer, we're 32bpp, so, 4 bytes per pixel.\n            decoded = new lUInt8[src_dy * (src_dx * 4)];\n        }",
        r#"        // Verso: shrunk by a whole factor while decoding (see build.rs).
        shrink = 1;
        next_y = 0;
        lines_in_order = true;
        acc = NULL;
        if (smoothscale) {
            if (!isNinePatch) {
                shrink = src_dx / dst_dx < src_dy / dst_dy ? src_dx / dst_dx : src_dy / dst_dy;
                if (shrink < 2)
                    shrink = 1;
            }
            red_dx = (src_dx + shrink - 1) / shrink;
            red_dy = (src_dy + shrink - 1) / shrink;
            // Byte-sized buffer, we're 32bpp, so, 4 bytes per pixel.
            decoded = new lUInt8[red_dy * (red_dx * 4)];
            if (shrink > 1)
                acc = new lUInt32[red_dx * 4]();
        }"#,
    ),
    (
        "crengine/src/lvdrawbuf.cpp",
        "        if (decoded)\n            delete[] decoded;",
        "        if (decoded)\n            delete[] decoded;\n        if (acc)\n            delete[] acc;",
    ),
    (
        "crengine/src/lvdrawbuf.cpp",
        "            memcpy(decoded + (y * (src_dx * 4)), data, (src_dx * 4));\n            return true;",
        r#"            if (shrink == 1) {
                memcpy(decoded + (y * (src_dx * 4)), data, (src_dx * 4));
                return true;
            }
            if (lines_in_order && y != next_y) {
                // Out of order: write what is summed so far, then use only
                // every shrink-th line.
                if (next_y % shrink)
                    flushShrunkLine((next_y - 1) / shrink, next_y % shrink);
                lines_in_order = false;
            }
            if (!lines_in_order && y % shrink)
                return true;
            const lUInt8 * __restrict p = (const lUInt8 *)data;
            lUInt32 * __restrict a = acc;
            for (int x = 0; x < src_dx; a += 4) {
                const int end = x + shrink < src_dx ? x + shrink : src_dx;
                for (; x < end; x++, p += 4) {
                    a[0] += p[0];
                    a[1] += p[1];
                    a[2] += p[2];
                    a[3] += p[3];
                }
            }
            if (!lines_in_order) {
                flushShrunkLine(y / shrink, 1);
            } else {
                next_y = y + 1;
                if (next_y % shrink == 0 || next_y == src_dy)
                    flushShrunkLine(y / shrink, y % shrink + 1);
            }
            return true;"#,
    ),
    (
        "crengine/src/lvdrawbuf.cpp",
        "sdata = CRe::qSmoothScaleImage(decoded, src_dx, src_dy, false, dst_dx, dst_dy);",
        "sdata = CRe::qSmoothScaleImage(decoded, red_dx, red_dy, false, dst_dx, dst_dy);",
    ),
    // When its caller gives no target size, an SVG is drawn at the size it
    // declares, which a book can set to anything (10000x10000 would be 400 MB).
    // Draw it at most at the screen's pixel count instead, and stretch the
    // lines back to the declared size, which the caller expects.
    (
        "crengine/include/lvimg.h",
        "LVImageSourceRef LVCreateDummyImageSource",
        "/// Verso: the most pixels an SVG with no target size is drawn at: the\n/// screen's pixel count, set by verso_cre_new() and verso_cre_set_size().\nextern int verso_max_image_pixels;\n\nLVImageSourceRef LVCreateDummyImageSource",
    ),
    (
        "crengine/src/lvimg.cpp",
        "LVImageDecoderCallback::~LVImageDecoderCallback()\n{\n}",
        "LVImageDecoderCallback::~LVImageDecoderCallback()\n{\n}\n\nint verso_max_image_pixels = 1272 * 1696;",
    ),
    (
        "crengine/src/lvimg.cpp",
        "    int render_w = have_target_size ? target_w : _width;\n    int render_h = have_target_size ? target_h : _height;",
        r#"    int render_w = have_target_size ? target_w : _width;
    int render_h = have_target_size ? target_h : _height;
    // Verso: no larger than the screen without a target size (see build.rs).
    const bool capped = !have_target_size && verso_max_image_pixels > 0
                        && (lInt64)render_w * render_h > verso_max_image_pixels;
    if ( capped ) {
        while ( (lInt64)render_w * render_h > verso_max_image_pixels ) {
            render_w = (render_w + 1) / 2;
            render_h = (render_h + 1) / 2;
        }
    }"#,
    ),
    (
        "crengine/src/lvimg.cpp",
        "    lUInt32 * __restrict src = (lUInt32 *)bitmap.data();\n    lUInt32 * __restrict row = new lUInt32 [ render_w ];\n    for (int y=0; y<render_h; y++) {\n        size_t px_count = render_w;\n        lUInt32 * __restrict dst = row;\n        while (px_count--) {",
        r#"    // Verso: a capped SVG is converted at the size it was drawn at, and each
    // line is repeated across the declared size, which the callback expects.
    const int out_w = capped ? _width : render_w;
    const int out_h = capped ? _height : render_h;
    lUInt32 * conv = new lUInt32 [ render_w ];
    lUInt32 * row = capped ? new lUInt32 [ out_w ] : conv;
    int last_sy = -1;
    for (int y=0; y<out_h; y++) {
        const int sy = capped ? (int)((lInt64)y * render_h / out_h) : y;
        if ( sy != last_sy ) {
        last_sy = sy;
        lUInt32 * __restrict src = (lUInt32 *)bitmap.data() + (size_t)sy * render_w;
        size_t px_count = render_w;
        lUInt32 * __restrict dst = conv;
        while (px_count--) {"#,
    ),
    (
        "crengine/src/lvimg.cpp",
        "            src++;\n        }\n        callback->OnLineDecoded( this, y, row );\n    }\n    delete[] row;",
        r#"            src++;
        }
        }
        if ( capped ) {
            int sx = 0, ex = 0;
            for (int x=0; x<out_w; x++) {
                row[x] = conv[sx];
                ex += render_w;
                while ( ex >= out_w ) {
                    ex -= out_w;
                    sx++;
                }
            }
        }
        callback->OnLineDecoded( this, y, row );
    }
    delete[] conv;
    if ( capped )
        delete[] row;"#,
    ),
];

/// crengine's own source list (CRE_SRCS in its CMakeLists.txt).
const CRENGINE_SOURCES: &[&str] = &[
    "chmfmt.cpp",
    "cp_stats.cpp",
    "crtxtenc.cpp",
    "docxfmt.cpp",
    "drawutil.cpp",
    "epubfmt.cpp",
    "fb3fmt.cpp",
    "hist.cpp",
    "hyphman.cpp",
    "lstridmap.cpp",
    "lvdocview.cpp",
    "lvdrawbuf.cpp",
    "lvfnt.cpp",
    "lvfntman.cpp",
    "lvimg.cpp",
    "lvmemman.cpp",
    "lvopc.cpp",
    "lvpagesplitter.cpp",
    "lvrend.cpp",
    "lvstream.cpp",
    "lvstring.cpp",
    "lvstsheet.cpp",
    "lvstyles.cpp",
    "lvtextfm.cpp",
    "lvtinydom.cpp",
    "lvxml.cpp",
    "mathml.cpp",
    "mdfmt.cpp",
    "odtfmt.cpp",
    "odxutil.cpp",
    "pdbfmt.cpp",
    "props.cpp",
    "renderutil.cpp",
    "rtfimp.cpp",
    "textlang.cpp",
    "txtselector.cpp",
    "wordfmt.cpp",
];

/// libjpeg-turbo's JPEG_SOURCES (its CMakeLists.txt), plus arithmetic decoding.
const JPEG_SOURCES: &[&str] = &[
    "jcapimin.c",
    "wrapper/jcapistd-8.c",
    "wrapper/jcapistd-12.c",
    "wrapper/jcapistd-16.c",
    "wrapper/jccoefct-8.c",
    "wrapper/jccoefct-12.c",
    "wrapper/jccolor-8.c",
    "wrapper/jccolor-12.c",
    "wrapper/jccolor-16.c",
    "wrapper/jcdctmgr-8.c",
    "wrapper/jcdctmgr-12.c",
    "wrapper/jcdiffct-8.c",
    "wrapper/jcdiffct-12.c",
    "wrapper/jcdiffct-16.c",
    "jchuff.c",
    "jcicc.c",
    "jcinit.c",
    "jclhuff.c",
    "wrapper/jclossls-8.c",
    "wrapper/jclossls-12.c",
    "wrapper/jclossls-16.c",
    "wrapper/jcmainct-8.c",
    "wrapper/jcmainct-12.c",
    "wrapper/jcmainct-16.c",
    "jcmarker.c",
    "jcmaster.c",
    "jcomapi.c",
    "jcparam.c",
    "jcphuff.c",
    "wrapper/jcprepct-8.c",
    "wrapper/jcprepct-12.c",
    "wrapper/jcprepct-16.c",
    "wrapper/jcsample-8.c",
    "wrapper/jcsample-12.c",
    "wrapper/jcsample-16.c",
    "jctrans.c",
    "jdapimin.c",
    "wrapper/jdapistd-8.c",
    "wrapper/jdapistd-12.c",
    "wrapper/jdapistd-16.c",
    "jdatadst.c",
    "jdatasrc.c",
    "wrapper/jdcoefct-8.c",
    "wrapper/jdcoefct-12.c",
    "wrapper/jdcolor-8.c",
    "wrapper/jdcolor-12.c",
    "wrapper/jdcolor-16.c",
    "wrapper/jddctmgr-8.c",
    "wrapper/jddctmgr-12.c",
    "wrapper/jddiffct-8.c",
    "wrapper/jddiffct-12.c",
    "wrapper/jddiffct-16.c",
    "jdhuff.c",
    "jdicc.c",
    "jdinput.c",
    "jdlhuff.c",
    "wrapper/jdlossls-8.c",
    "wrapper/jdlossls-12.c",
    "wrapper/jdlossls-16.c",
    "wrapper/jdmainct-8.c",
    "wrapper/jdmainct-12.c",
    "wrapper/jdmainct-16.c",
    "jdmarker.c",
    "jdmaster.c",
    "wrapper/jdmerge-8.c",
    "wrapper/jdmerge-12.c",
    "jdphuff.c",
    "wrapper/jdpostct-8.c",
    "wrapper/jdpostct-12.c",
    "wrapper/jdpostct-16.c",
    "wrapper/jdsample-8.c",
    "wrapper/jdsample-12.c",
    "wrapper/jdsample-16.c",
    "jdtrans.c",
    "jerror.c",
    "jfdctflt.c",
    "wrapper/jfdctfst-8.c",
    "wrapper/jfdctfst-12.c",
    "wrapper/jfdctint-8.c",
    "wrapper/jfdctint-12.c",
    "wrapper/jidctflt-8.c",
    "wrapper/jidctflt-12.c",
    "wrapper/jidctfst-8.c",
    "wrapper/jidctfst-12.c",
    "wrapper/jidctint-8.c",
    "wrapper/jidctint-12.c",
    "wrapper/jidctred-8.c",
    "wrapper/jidctred-12.c",
    "jmemmgr.c",
    "jmemnobs.c",
    "jpeg_nbits.c",
    "wrapper/jquant1-8.c",
    "wrapper/jquant1-12.c",
    "wrapper/jquant2-8.c",
    "wrapper/jquant2-12.c",
    "wrapper/jutils-8.c",
    "wrapper/jutils-12.c",
    "wrapper/jutils-16.c",
    "jaricom.c",
    "jdarith.c",
];
