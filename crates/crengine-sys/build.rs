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
    h.field(b"crengine-sys-v2");
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
        let compiler = b.get_compiler();
        if let Some(name) = compiler.path().file_name() {
            h.field(name.to_string_lossy().as_bytes());
        }
        let out = compiler.to_command().arg("--version").output().ok()?;
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
        "    // Verso: see LVFontManager::DescribeFontFile(). A line holds face_index,\n    // italic, CSS family, emojis, math and small caps, then has/min/max for\n    // the wght, opsz, ital, slnt and wdth axes, then the family name.\n    virtual lString8 DescribeFontFile( lString8 name )\n    {\n        FONT_MAN_GUARD\n        LVArray<const LVFontFace*> faces;\n        for (int i = 0; i < _registry.familyCount(); i++) {\n            const LVFontFamily* f = _registry.familyAt(i);\n            for (int j = 0; j < f->faceCount(); j++) {\n                const LVFontFace& face = f->faceAt(j);\n                if (face.documentId == -1 && face.file_path == name)\n                    faces.add(&face);\n            }\n        }\n        // In file order, as RegisterFont() registered them.\n        for (int i = 1; i < faces.length(); i++)\n            for (int j = i; j > 0 && faces[j-1]->face_index > faces[j]->face_index; j--) {\n                const LVFontFace* t = faces[j-1];\n                faces[j-1] = faces[j];\n                faces[j] = t;\n            }\n        lString8 out;\n        for (int i = 0; i < faces.length(); i++) {\n            const LVFontFace& f = *faces[i];\n            if (f.typeface.empty() || f.typeface.pos(cs8(\"\\t\")) >= 0 || f.typeface.pos(cs8(\"\\n\")) >= 0)\n                return lString8::empty_str;\n            char buf[128];\n            snprintf(buf, sizeof(buf), \"%d\\t%d\\t%d\\t%d\\t%d\\t%d\", f.face_index, f.is_italic ? 1 : 0,\n                     (int)f.css_family, f.has_emojis ? 1 : 0, f.has_ot_math ? 1 : 0, f.has_small_caps ? 1 : 0);\n            out << buf;\n            const bool has[5] = { f._has_wght, f._has_opsz, f._has_ital, f._has_slnt, f._has_wdth };\n            const float lo[5] = { f._wght_min, f._opsz_min, f._ital_min, f._slnt_min, f._wdth_min };\n            const float hi[5] = { f._wght_max, f._opsz_max, f._ital_max, f._slnt_max, f._wdth_max };\n            for (int k = 0; k < 5; k++) {\n                snprintf(buf, sizeof(buf), \"\\t%d\\t%.9g\\t%.9g\", has[k] ? 1 : 0, (double)lo[k], (double)hi[k]);\n                out << buf;\n            }\n            out << \"\\t\" << f.typeface << \"\\n\";\n        }\n        return out;\n    }\n\n    // Verso: see LVFontManager::RegisterFontFaces(). Like RegisterFont(), it\n    // stops at the first face that is already registered.\n    virtual int RegisterFontFaces( lString8 name, lString8 description )\n    {\n        FONT_MAN_GUARD\n        static const lUInt32 tags[5] = { LVFONT_TAG_WGHT, LVFONT_TAG_OPSZ, LVFONT_TAG_ITAL, LVFONT_TAG_SLNT, LVFONT_TAG_WDTH };\n        LVPtrVector<LVFontFace, true> faces;\n        const char* p = description.c_str();\n        while (*p) {\n            LVFontFace def;\n            def.file_path = name;\n            char* end;\n            long v[6];\n            for (int k = 0; k < 6; k++) {\n                v[k] = strtol(p, &end, 10);\n                if (end == p || *end != '\\t') return -1;\n                p = end + 1;\n            }\n            if (v[2] < css_ff_inherit || v[2] > css_ff_fangsong) return -1;\n            def.face_index = (int)v[0];\n            def.is_italic = v[1] != 0;\n            def.css_family = (css_font_family_t)v[2];\n            def.has_emojis = v[3] != 0;\n            def.has_ot_math = v[4] != 0;\n            def.has_small_caps = v[5] != 0;\n            for (int k = 0; k < 5; k++) {\n                long has = strtol(p, &end, 10);\n                if (end == p || *end != '\\t') return -1;\n                p = end + 1;\n                float lo = strtof(p, &end);\n                if (end == p || *end != '\\t') return -1;\n                p = end + 1;\n                float hi = strtof(p, &end);\n                if (end == p || *end != '\\t') return -1;\n                p = end + 1;\n                if (has)\n                    def.setAxisInfo(tags[k], lo, hi);\n            }\n            const char* nl = strchr(p, '\\n');\n            if (!nl || nl == p) return -1;\n            def.typeface = lString8(p, (int)(nl - p));\n            p = nl + 1;\n            faces.add(new LVFontFace(def));\n        }\n        int count = 0;\n        for (int i = 0; i < faces.length(); i++) {\n            if (!tryRegisterFace(*faces[i]))\n                break;\n            count++;\n        }\n        return count;\n    }\n\n    void GetAvailableFontWeights(LVArray<int>& weights, lString8 typeface) {\n        FONT_MAN_GUARD\n        weights.clear();",
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
        "#include <stdlib.h>\n#include <string.h>",
        "#include <stdlib.h>\n#include <string.h>\n#include <limits.h>",
    ),
    (
        "crengine/src/lvtinydom.cpp",
        "                _oldStreamCRC = (lUInt32)oldStream->getcrc32();",
        "                _oldStreamCRC = 0;\n                lUInt8 * old = _oldStreamSize > 4 && _oldStreamSize <= (lUInt32)INT_MAX ? (lUInt8 *)malloc( _oldStreamSize ) : NULL;\n                lvsize_t got = 0;\n                if ( old && oldStream->Read( old, _oldStreamSize, &got ) == LVERR_OK && got == _oldStreamSize )\n                    _oldStreamCRC = lStr_crc32( 0, old, (int)_oldStreamSize - 4 );\n                free( old );",
    ),
    (
        "crengine/src/lvtinydom.cpp",
        "        lUInt32 newCRC = buf.getCRC();",
        "        lUInt32 newCRC = lStr_crc32( 0, buf.buf(), buf.pos() - 4 );",
    ),
    // crengine writes the index from a copy taken before the write. When the
    // index has grown, the write moves it into a free block, and the saved
    // copy still lists that block as free: a later layout puts data there and
    // the next index write overwrites it ("packed data CRC doesn't match", and
    // the book is laid out from scratch). Copy and write again until the index
    // stays where it is.
    (
        "crengine/src/lvtinydom.cpp",
        r#"    CacheFileItem * index = new CacheFileItem[count]();
    int sz = count * sizeof(CacheFileItem);
    for ( int i = 0; i < count; i++ ) {
        memcpy( &index[i], _index[i], sizeof(CacheFileItem) );
        if (index[i]._dataType == CBT_INDEX) {
            index[i]._dataHash = 0;
            index[i]._packedHash = 0;
            index[i]._dataSize = 0;
        }
    }
    bool res = write(CBT_INDEX, 0, (const lUInt8*)index, sz, false);
    delete[] index;
"#,
        r#"    bool res = false;
    for ( int attempt = 0; attempt < 8; attempt++ ) {
        count = _index.length();
        CacheFileItem * before = findBlock(CBT_INDEX, 0);
        CacheFileItem * index = new CacheFileItem[count]();
        int sz = count * sizeof(CacheFileItem);
        for ( int i = 0; i < count; i++ ) {
            memcpy( &index[i], _index[i], sizeof(CacheFileItem) );
            if (index[i]._dataType == CBT_INDEX) {
                index[i]._dataHash = 0;
                index[i]._packedHash = 0;
                index[i]._dataSize = 0;
            }
        }
        res = write(CBT_INDEX, 0, (const lUInt8*)index, sz, false);
        delete[] index;
        if ( !res || ( findBlock(CBT_INDEX, 0) == before && _index.length() == count ) )
            break;
    }
"#,
    ),
    // Files saved before the fix above can list the index's own block as free.
    // Never reuse free space that overlaps the index.
    (
        "crengine/src/lvtinydom.cpp",
        r#"    CacheFileItem * indexitem = findBlock(CBT_INDEX, 0);
    if ( !indexitem ) {
        CRLog::error("CacheFile::readIndex: index block info doesn't match header");
        return false;
    }
"#,
        r#"    CacheFileItem * indexitem = findBlock(CBT_INDEX, 0);
    if ( !indexitem ) {
        CRLog::error("CacheFile::readIndex: index block info doesn't match header");
        return false;
    }
    for ( int i = _freeIndex.length() - 1; i >= 0; i-- ) {
        CacheFileItem * item = _freeIndex[i];
        if ( item->_blockFilePos < indexitem->_blockFilePos + indexitem->_blockSize
                && indexitem->_blockFilePos < item->_blockFilePos + item->_blockSize )
            _freeIndex.remove( i );
    }
"#,
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
    // Headers needed by our lvimg.cpp fixes (SIZE_MAX, std::vector).
    (
        "crengine/src/lvimg.cpp",
        "#include <stdlib.h>\n#include <stdio.h>\n#include <string.h>",
        "#include <stdlib.h>\n#include <stdio.h>\n#include <string.h>\n#include <stdint.h>\n#include <vector>",
    ),
    // Fix setjmp/longjmp UB and error-path leaks in JPEG decoding: arm setjmp
    // before jpeg_create_decompress, qualify locals mutated across setjmp as
    // volatile, and copy out a non-volatile scanline pointer for libjpeg.
    (
        "crengine/src/lvimg.cpp",
        "        /* Now we can initialize the JPEG decompression object. */\n        jpeg_create_decompress(&cinfo);\n\n        lUInt8 * buffer = NULL;\n\n        if (setjmp(jerr.setjmp_buffer)) {\n        \tCRLog::error(\"JPEG setjmp error handling\");\n\t    /* If we get here, the JPEG code has signaled an error.\n\t     * We need to clean up the JPEG object, close the input file, and return.\n\t     */\n            delete[] buffer;\n        \tCRLog::debug(\"JPEG decoder cleanup\");\n            cr_jpeg_src_free (&cinfo);\n            jpeg_destroy_decompress(&cinfo);\n            return false;\n\t     }",
        r#"        lUInt8 * volatile buffer = NULL;
        volatile bool cinfo_created = false;

        if (setjmp(jerr.setjmp_buffer)) {
            CRLog::error("JPEG setjmp error handling");
            /* If we get here, the JPEG code has signaled an error.
             * We need to clean up the JPEG object, close the input file, and return.
             */
            delete[] buffer;
            CRLog::debug("JPEG decoder cleanup");
            if (cinfo_created) {
                cr_jpeg_src_free (&cinfo);
                jpeg_destroy_decompress(&cinfo);
            }
            return false;
        }

        /* Now we can initialize the JPEG decompression object. */
        jpeg_create_decompress(&cinfo);
        cinfo_created = true;"#,
    ),
    (
        "crengine/src/lvimg.cpp",
        "                buffer = new lUInt8 [ cinfo.output_width << 2U ];",
        "                buffer = new lUInt8 [ (size_t)cinfo.output_width << 2U ];",
    ),
    (
        "crengine/src/lvimg.cpp",
        "                    (void) jpeg_read_scanlines(&cinfo, &buffer, 1);\n\n                    // CRe wants inverted alpha, but libjpeg-turbo doesn't guarantee that the alpha byte will be zero...\n                    const unsigned char* const end = buffer + (cinfo.output_width << 2U);\n                    // Start at the first alpha byte, then loop over each subsequent alpha byte, pixel-by-pixel\n                    for (unsigned char* p = buffer + 3U; p <= end; p += 4U) {\n                        *p = 0x00;\n                    }\n                    callback->OnLineDecoded( this, y, reinterpret_cast<lUInt32 *>(buffer) );\n\n                }\n                callback->OnEndDecode(this, false);",
        r#"                    lUInt8 * scanline = buffer;
                    (void) jpeg_read_scanlines(&cinfo, &scanline, 1);

                    // CRe wants inverted alpha, but libjpeg-turbo doesn't guarantee that the alpha byte will be zero...
                    const unsigned char* const end = scanline + ((size_t)cinfo.output_width << 2U);
                    // Start at the first alpha byte, then loop over each subsequent alpha byte, pixel-by-pixel
                    for (unsigned char* p = scanline + 3U; p < end; p += 4U) {
                        *p = 0x00;
                    }
                    callback->OnLineDecoded( this, y, reinterpret_cast<lUInt32 *>(scanline) );

                }
                delete[] buffer;
                buffer = NULL;
                callback->OnEndDecode(this, false);"#,
    ),
    // Large images inside an SVG used to cost two full-size copies: libpng
    // decoded the whole image into one buffer, and smooth scaling (always on
    // inside an SVG) kept another to scale from. The 2481x3509 PNG cover of
    // *Go with Domain* took 70 MB that way. A PNG without
    // interlacing is now decoded one line at a time. Also fix setjmp/longjmp
    // UB (volatile info_ptr and decode buffer), 32-bit size_t overflows, and
    // OOM/error-path leaks in LVPngImageSource::Decode.
    (
        "crengine/src/lvimg.cpp",
        "    png_structp png_ptr = NULL;\n    png_infop info_ptr = NULL;\n    _stream->SetPos( 0 );\n    png_ptr = png_create_read_struct(PNG_LIBPNG_VER_STRING,\n        (png_voidp)this, lvpng_error_func, lvpng_warning_func);\n    if ( !png_ptr )\n        return false;\n\n    if (setjmp(png_jmpbuf(png_ptr))) {\n        _width = 0;\n        _height = 0;\n        if (png_ptr)\n        {\n            png_destroy_read_struct(&png_ptr, &info_ptr, NULL);\n        }\n        if (callback)\n            callback->OnEndDecode(this, true); // error!\n        return false;\n    }",
        r#"    png_structp png_ptr = NULL;
    png_infop volatile info_ptr = NULL;
    unsigned char * volatile verso_buf = NULL;
    _stream->SetPos( 0 );
    png_ptr = png_create_read_struct(PNG_LIBPNG_VER_STRING,
        (png_voidp)this, lvpng_error_func, lvpng_warning_func);
    if ( !png_ptr )
        return false;

    if (setjmp(png_jmpbuf(png_ptr))) {
        _width = 0;
        _height = 0;
        free(verso_buf);
        verso_buf = NULL;
        if (png_ptr)
        {
            png_infop ip = info_ptr;
            png_destroy_read_struct(&png_ptr, ip ? &ip : NULL, NULL);
        }
        if (callback)
            callback->OnEndDecode(this, true); // error!
        return false;
    }"#,
    ),
    (
        "crengine/src/lvimg.cpp",
        "        png_set_interlace_handling(png_ptr);",
        "        const int verso_png_passes = png_set_interlace_handling(png_ptr);",
    ),
    (
        "crengine/src/lvimg.cpp",
        "        size_t rowbytes = png_get_rowbytes(png_ptr, info_ptr);\n        size_t image_size = height * rowbytes;\n        unsigned char * storage = NULL;\n        unsigned char * __restrict image = NULL;\n        png_bytep * __restrict row_pointers = NULL;\n\n        // NOTE: Stash *both* the array of row pointers *and* the image data in a single allocation.\n        //       This implies some alignment trickery to ensure both pointers are aligned as malloc would.\n        //       c.f., the comments for LVFontGlyphCacheItem in lvfntman.h\n        // To that effect, compute the size of the array of row pointers, and align it on a 16-byte boundary.\n        size_t image_offset = ALIGN((height * sizeof(*row_pointers)), 16);\n        // And we can now alloc the whole thing...\n        storage = (unsigned char *) malloc(image_offset + image_size);",
        r#"        if ( verso_png_passes == 1 ) {
            // Verso: one line at a time (see build.rs).
            verso_buf = (unsigned char *) malloc(png_get_rowbytes(png_ptr, info_ptr));
            if ( !verso_buf )
                lvpng_error_func(png_ptr, "cannot allocate png row");
            png_bytep row = verso_buf;
            for (size_t y = 0; y < height; y++) {
                png_read_row(png_ptr, row, NULL);
                callback->OnLineDecoded( this, y, reinterpret_cast<lUInt32 *>(row) );
            }
            png_read_end(png_ptr, info_ptr);
            free(verso_buf);
            verso_buf = NULL;
            callback->OnEndDecode(this, false);
        } else {
        size_t rowbytes = png_get_rowbytes(png_ptr, info_ptr);
        if ( height > 0 && rowbytes > SIZE_MAX / (size_t)height )
            lvpng_error_func(png_ptr, "png image too large");
        size_t image_size = (size_t)height * rowbytes;
        unsigned char * storage = NULL;
        unsigned char * __restrict image = NULL;
        png_bytep * __restrict row_pointers = NULL;

        // NOTE: Stash *both* the array of row pointers *and* the image data in a single allocation.
        //       This implies some alignment trickery to ensure both pointers are aligned as malloc would.
        //       c.f., the comments for LVFontGlyphCacheItem in lvfntman.h
        // To that effect, compute the size of the array of row pointers, and align it on a 16-byte boundary.
        if ( (size_t)height > (SIZE_MAX - 15) / sizeof(*row_pointers) )
            lvpng_error_func(png_ptr, "png image too large");
        size_t image_offset = ALIGN(((size_t)height * sizeof(*row_pointers)), 16);
        if ( image_size > SIZE_MAX - image_offset )
            lvpng_error_func(png_ptr, "png image too large");
        // And we can now alloc the whole thing...
        storage = (unsigned char *) malloc(image_offset + image_size);
        if ( !storage )
            lvpng_error_func(png_ptr, "cannot allocate png image");
        verso_buf = storage;"#,
    ),
    (
        "crengine/src/lvimg.cpp",
        "        callback->OnEndDecode(this, false);\n        free(storage);\n    }\n    png_destroy_read_struct(&png_ptr, &info_ptr, NULL);",
        "        free(verso_buf);\n        verso_buf = NULL;\n        callback->OnEndDecode(this, false);\n        }\n    }\n    png_infop ip = info_ptr;\n    png_destroy_read_struct(&png_ptr, ip ? &ip : NULL, NULL);",
    ),
    // Smooth scaling now keeps the image shrunk by the largest whole factor
    // that leaves it at least as large as the target, averaging each block of
    // pixels as the lines come in, and scales from that. The copy is then less
    // than 4 times the target's area, however large the image is.
    // Also guard LVColorDrawBuf::Draw against non-positive dimensions (like
    // LVGrayDrawBuf::Draw), guard overflow/bounds in smoothscale, and bail
    // out of OnEndDecode when decoding reported errors.
    (
        "crengine/src/lvdrawbuf.cpp",
        "#include <stdlib.h>\n#include <stdio.h>\n#include <string.h>\n#include \"../include/lvdrawbuf.h\"",
        "#include <stdlib.h>\n#include <stdio.h>\n#include <string.h>\n#include <stdint.h>\n#include \"../include/lvdrawbuf.h\"",
    ),
    (
        "crengine/src/lvdrawbuf.cpp",
        "void LVColorDrawBuf::Draw( LVImageSourceRef img, int x, int y, int width, int height, bool dither )\n{\n    //fprintf( stderr, \"LVColorDrawBuf::Draw( img(%d, %d), %d, %d, %d, %d\\n\", img->GetWidth(), img->GetHeight(), x, y, width, height );\n    LVImageScaledDrawCallback drawcb( this, img, x, y, width, height, dither, _invertImages, _smoothImages );",
        "void LVColorDrawBuf::Draw( LVImageSourceRef img, int x, int y, int width, int height, bool dither )\n{\n    //fprintf( stderr, \"LVColorDrawBuf::Draw( img(%d, %d), %d, %d, %d, %d\\n\", img->GetWidth(), img->GetHeight(), x, y, width, height );\n    if ( width<=0 || height<=0 )\n        return;\n    LVImageScaledDrawCallback drawcb( this, img, x, y, width, height, dither, _invertImages, _smoothImages );",
    ),
    (
        "crengine/src/lvdrawbuf.cpp",
        "    lUInt8 * __restrict decoded;\n    bool isNinePatch;",
        r#"    lUInt8 * __restrict decoded;
    // Verso: the shrink factor, the size of the shrunk copy in decoded, the
    // sums of the source lines not yet written to it, and whether the lines
    // have come in order so far (an interlaced GIF sends them out of order).
    int shrink, red_dx, red_dy, next_y;
    bool lines_in_order;
    lUInt64 * __restrict acc;
    // Verso: writes the average of the source lines summed in acc (rows of
    // them) as line ry of the shrunk copy, and clears acc.
    // Divides by multiplying with a 32.32 fixed-point reciprocal: the Kindle
    // build has no hardware divide, and a division per channel made the
    // 2481x3509 cover draw 150 ms slower.
    void flushShrunkLine( int ry, int rows ) {
        if (!decoded || !acc || ry < 0 || ry >= red_dy || rows <= 0)
            return;
        lUInt8 * __restrict out = decoded + (size_t)ry * ((size_t)red_dx * 4);
        const int last_cols = src_dx - (int)((lUInt64)(red_dx - 1) * (lUInt64)shrink);
        const lUInt64 denom = (lUInt64)rows * (lUInt64)shrink;
        const lUInt64 denom_last = last_cols > 0 ? (lUInt64)rows * (lUInt64)last_cols : denom;
        if (denom == 0 || denom_last == 0)
            return;
        const lUInt64 inv = ((1ULL << 32) + denom / 2) / denom;
        const lUInt64 inv_last = ((1ULL << 32) + denom_last / 2) / denom_last;
        for (int x = 0; x < red_dx; x++) {
            const lUInt64 r = x == red_dx - 1 ? inv_last : inv;
            const size_t base = (size_t)x * 4;
            for (int c = 0; c < 4; c++)
                out[base + c] = (lUInt8)((acc[base + c] * r + (1ULL << 31)) >> 32);
        }
        memset(acc, 0, (size_t)red_dx * 4 * sizeof(lUInt64));
    }
    bool isNinePatch;"#,
    ),
    (
        "crengine/src/lvdrawbuf.cpp",
        "        if (smoothscale) {\n            // Byte-sized buffer, we're 32bpp, so, 4 bytes per pixel.\n            decoded = new lUInt8[src_dy * (src_dx * 4)];\n        }",
        r#"        // Verso: shrunk by a whole factor while decoding (see build.rs).
        shrink = 1;
        red_dx = 0;
        red_dy = 0;
        next_y = 0;
        lines_in_order = true;
        acc = NULL;
        if (smoothscale) {
            if (dst_dx <= 0 || dst_dy <= 0 || src_dx <= 0 || src_dy <= 0) {
                smoothscale = false;
            } else {
                if (!isNinePatch) {
                    shrink = src_dx / dst_dx < src_dy / dst_dy ? src_dx / dst_dx : src_dy / dst_dy;
                    if (shrink < 2)
                        shrink = 1;
                }
                red_dx = (int)(((lUInt64)src_dx + (lUInt64)shrink - 1) / (lUInt64)shrink);
                red_dy = (int)(((lUInt64)src_dy + (lUInt64)shrink - 1) / (lUInt64)shrink);
                // Byte-sized buffer, we're 32bpp, so, 4 bytes per pixel.
                const bool acc_ok = shrink <= 1 || (size_t)red_dx <= SIZE_MAX / (4 * sizeof(lUInt64));
                if (acc_ok && (size_t)red_dx <= SIZE_MAX / 4
                    && (size_t)red_dy <= SIZE_MAX / ((size_t)red_dx * 4)) {
                    decoded = new lUInt8[(size_t)red_dy * ((size_t)red_dx * 4)];
                    if (shrink > 1)
                        acc = new lUInt64[(size_t)red_dx * 4]();
                } else {
                    smoothscale = false;
                }
            }
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
        r#"            if (y < 0 || y >= src_dy || !decoded)
                return true;
            if (shrink == 1) {
                memcpy(decoded + ((size_t)y * ((size_t)src_dx * 4)), data, (size_t)src_dx * 4);
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
            lUInt64 * __restrict a = acc;
            for (int x = 0; x < src_dx; a += 4) {
                const int end = shrink < src_dx - x ? x + shrink : src_dx;
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
        "    virtual void OnEndDecode( LVImageSource * obj, bool )\n    {\n        // If we're not smooth scaling, we're done!\n        if (!smoothscale) {\n            return;\n        }",
        "    virtual void OnEndDecode( LVImageSource * obj, bool errors )\n    {\n        // If we're not smooth scaling, or decoding failed, we're done!\n        if (!smoothscale || errors || !decoded) {\n            return;\n        }",
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
    if ( render_w <= 0 || render_h <= 0 )
        return false;
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
        "    render_w = bitmap.width();\n    render_h = bitmap.height();\n\n    callback->OnStartDecode(this);\n    lUInt32 * __restrict src = (lUInt32 *)bitmap.data();\n    lUInt32 * __restrict row = new lUInt32 [ render_w ];\n    for (int y=0; y<render_h; y++) {\n        size_t px_count = render_w;\n        lUInt32 * __restrict dst = row;\n        while (px_count--) {",
        r#"    render_w = bitmap.width();
    render_h = bitmap.height();
    if ( render_w <= 0 || render_h <= 0 )
        return false;
    // Verso: a capped SVG is converted at the size it was drawn at, and each
    // line is repeated across the declared size, which the callback expects.
    const int out_w = capped ? _width : render_w;
    const int out_h = capped ? _height : render_h;
    if ( out_w <= 0 || out_h <= 0 )
        return false;

    callback->OnStartDecode(this);
    std::vector<lUInt32> conv_buf( (size_t)render_w );
    std::vector<lUInt32> row_buf;
    if ( capped )
        row_buf.resize( (size_t)out_w );
    lUInt32 * conv = conv_buf.data();
    lUInt32 * row = capped ? row_buf.data() : conv;
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
            int sx = 0;
            lInt64 ex = 0;
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
    }"#,
    ),
    // Stream fixes in lvstream.cpp:
    // - Pass missing format argument to both "File is too big to open %s" logs.
    // - Use stat64 when HAVE_STAT64 && _LINUX in LVOpenDirectory and check the
    //   return value so failed stat / 64-bit inodes never read uninitialized st.
    // - Return LVERR_FAIL instead of calling exit(-1) in LVCachedStream::Read.
    (
        "crengine/src/lvstream.cpp",
        "CRLog::error( \"File is too big to open %s\");",
        "CRLog::error( \"File is too big to open %s\", fn8.c_str() );",
    ),
    (
        "crengine/src/lvstream.cpp",
        "                struct stat st;\n                stat( fpath.c_str(), &st );\n                if ( S_ISDIR(st.st_mode) ) {",
        r#"#if defined(HAVE_STAT64) && defined(_LINUX)
                struct stat64 st;
                if ( stat64( fpath.c_str(), &st ) != 0 )
                    continue;
#else
                struct stat st;
                if ( stat( fpath.c_str(), &st ) != 0 )
                    continue;
#endif
                if ( S_ISDIR(st.st_mode) ) {"#,
    ),
    (
        "crengine/src/lvstream.cpp",
        "                    if ( !res )\n                    {\n                        fprintf( stderr, \"cannot fill fragment %d .. %d\\n\", fillStart, fillEnd );\n                        exit(-1);\n                    }",
        "                    if ( !res )\n                    {\n                        fprintf( stderr, \"cannot fill fragment %d .. %d\\n\", fillStart, fillEnd );\n                        delete[] flags;\n                        return LVERR_FAIL;\n                    }",
    ),
    // Verso's Alignment setting (0 = book, 1 = left, 2 = justify) overrides
    // body text alignment without clobbering center/right headers or images
    // wrapped in <p>. Stored in _docFlags so crengine's render context and
    // .cr3 cache automatically track it.
    (
        "crengine/include/lvtinydom.h",
        "#define DOC_FLAG_NONLINEAR_PAGEBREAK   16",
        "#define DOC_FLAG_NONLINEAR_PAGEBREAK   16\n#define DOC_FLAG_VERSO_ALIGN_LEFT      32\n#define DOC_FLAG_VERSO_ALIGN_JUSTIFY   64",
    ),
    (
        "crengine/include/lvdocviewprops.h",
        "#define PROP_NONLINEAR_PAGEBREAK     \"crengine.doc.nonlinear.pagebreak.force\"",
        "#define PROP_NONLINEAR_PAGEBREAK     \"crengine.doc.nonlinear.pagebreak.force\"\n#define PROP_VERSO_TEXT_ALIGN        \"verso.doc.text.align\"",
    ),
    (
        "crengine/src/lvdocview.cpp",
        "    m_doc->setDocFlag(DOC_FLAG_NONLINEAR_PAGEBREAK, m_props->getBoolDef(\n            PROP_NONLINEAR_PAGEBREAK, false));",
        "    m_doc->setDocFlag(DOC_FLAG_NONLINEAR_PAGEBREAK, m_props->getBoolDef(\n            PROP_NONLINEAR_PAGEBREAK, false));\n    {\n        int versoAlign = m_props->getIntDef(PROP_VERSO_TEXT_ALIGN, 0);\n        m_doc->setDocFlag(DOC_FLAG_VERSO_ALIGN_LEFT, versoAlign == 1);\n        m_doc->setDocFlag(DOC_FLAG_VERSO_ALIGN_JUSTIFY, versoAlign == 2);\n    }",
    ),
    (
        "crengine/src/lvdocview.cpp",
        "        } else if (name == PROP_FOOTNOTES) {\n            bool value = props->getBoolDef(PROP_FOOTNOTES, true);\n            if (m_doc) // not when noDefaultDocument=true\n                getDocument()->setDocFlag(DOC_FLAG_ENABLE_FOOTNOTES, value);\n            REQUEST_RENDER(\"propsApply footnotes\")",
        "        } else if (name == PROP_FOOTNOTES) {\n            bool value = props->getBoolDef(PROP_FOOTNOTES, true);\n            if (m_doc) // not when noDefaultDocument=true\n                getDocument()->setDocFlag(DOC_FLAG_ENABLE_FOOTNOTES, value);\n            REQUEST_RENDER(\"propsApply footnotes\")\n        } else if (name == PROP_VERSO_TEXT_ALIGN) {\n            int value = props->getIntDef(PROP_VERSO_TEXT_ALIGN, 0);\n            if (m_doc) {\n                getDocument()->setDocFlag(DOC_FLAG_VERSO_ALIGN_LEFT, value == 1);\n                getDocument()->setDocFlag(DOC_FLAG_VERSO_ALIGN_JUSTIFY, value == 2);\n            }\n            REQUEST_RENDER(\"propsApply verso text align\")",
    ),
    (
        "crengine/src/lvrend.cpp",
        "    UPDATE_STYLE_FIELD( text_align, css_ta_inherit );\n    UPDATE_STYLE_FIELD( text_align_last, css_ta_inherit );",
        "    UPDATE_STYLE_FIELD( text_align, css_ta_inherit );\n    UPDATE_STYLE_FIELD( text_align_last, css_ta_inherit );\n    if ( doc->getDocFlag(DOC_FLAG_VERSO_ALIGN_LEFT) ) {\n        if ( pstyle->text_align == css_ta_justify )\n            pstyle->text_align = css_ta_start;\n        if ( pstyle->text_align_last == css_ta_justify )\n            pstyle->text_align_last = css_ta_auto;\n    } else if ( doc->getDocFlag(DOC_FLAG_VERSO_ALIGN_JUSTIFY) ) {\n        if ( (nodeElementId == el_p || nodeElementId == el_li)\n             && pstyle->white_space != css_ws_pre && pstyle->white_space != css_ws_pre_wrap\n             && (pstyle->text_align == css_ta_start || pstyle->text_align == css_ta_left\n                 || pstyle->text_align == css_ta_html_align_left) )\n            pstyle->text_align = css_ta_justify;\n    }",
    ),
    // Dark mode: preserve hue and flip luma (255 - Y) for non-grayscale
    // colors so dark publisher blacks (#231f20) flip to light and colors keep
    // their designed contrast and hue after the final panel flip.
    (
        "crengine/include/lvdrawbuf.h",
        "inline lUInt32 invertNonGrayscaleColor( lUInt32 color ) {\n    if ( (color & 0xFFFFFFFE) == 0xFFFFFFFE || (color & 0xFF000000) == 0xFF000000 )\n        return color;\n    const lUInt8 b = color & 0xFF;\n    const lUInt8 g = (color >> 8) & 0xFF;\n    const lUInt8 r = (color >> 16) & 0xFF;\n    if ( r == g && g == b )\n        return color;\n    return color ^ 0x00FFFFFF;\n}",
        r#"inline lUInt32 invertNonGrayscaleColor( lUInt32 color ) {
    if ( (color & 0xFFFFFFFE) == 0xFFFFFFFE || (color & 0xFF000000) == 0xFF000000 )
        return color;
    const int b = (int)(color & 0xFF);
    const int g = (int)((color >> 8) & 0xFF);
    const int r = (int)((color >> 16) & 0xFF);
    if ( r == g && g == b )
        return color;
    const int y = (77 * r + 150 * g + 29 * b + 128) >> 8;
    const int target = 255 - y;
    int nr, ng, nb;
    if ( y == 0 ) {
        nr = ng = nb = 255;
    } else if ( y == 255 ) {
        nr = ng = nb = 0;
    } else if ( target > y ) {
        const int num = target - y;
        const int den = 255 - y;
        nr = r + ((255 - r) * num + den / 2) / den;
        ng = g + ((255 - g) * num + den / 2) / den;
        nb = b + ((255 - b) * num + den / 2) / den;
    } else {
        nr = (r * target + y / 2) / y;
        ng = (g * target + y / 2) / y;
        nb = (b * target + y / 2) / y;
    }
    const lUInt32 out = ((lUInt32)(nr & 0xFF) << 16) | ((lUInt32)(ng & 0xFF) << 8) | (lUInt32)(nb & 0xFF);
    return (color & 0xFF000000) | (out ^ 0x00FFFFFF);
}"#,
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
