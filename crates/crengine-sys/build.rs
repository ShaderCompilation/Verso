//! Builds crengine (KOReader's fork) and the C libraries it needs, straight
//! from the submodules in third_party/, with the `cc` crate. No CMake, Meson or
//! system libraries: the same sources build for the desktop and the Kindle.
//!
//! Which optional crengine features are on is decided in config/crsetup.h;
//! every library enabled there must be compiled here.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let tp = manifest.join("../../third_party").canonicalize().expect("third_party/ missing");
    let config = manifest.join("config");
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let arm = env::var("CARGO_CFG_TARGET_ARCH").unwrap() == "arm";

    println!("cargo:rerun-if-changed=config");
    println!("cargo:rerun-if-changed=shim");
    for lib in LIBS {
        // Rebuild when a submodule moves to another commit.
        println!("cargo:rerun-if-changed={}", tp.join(lib).join(".git").display());
    }

    // Generated headers.
    let gen_inc = out.join("include");
    fs::create_dir_all(&gen_inc).unwrap();
    fs::copy(tp.join("libpng/scripts/pnglibconf.h.prebuilt"), gen_inc.join("pnglibconf.h")).unwrap();

    let ctx = Ctx { tp: tp.clone(), config: config.clone(), gen_inc, arm };

    // Order matters for linking: a library must come before the ones it uses.
    let mut link_order = Vec::new();
    ctx.shim_and_crengine(&mut link_order);
    ctx.harfbuzz(&mut link_order);
    ctx.freetype(&mut link_order);
    ctx.libpng(&mut link_order);
    ctx.libjpeg(&mut link_order);
    ctx.zlib(&mut link_order);
    ctx.libunibreak(&mut link_order);
    ctx.utf8proc(&mut link_order);

    println!("cargo:rustc-link-search=native={}", out.display());
    for lib in link_order {
        println!("cargo:rustc-link-lib=static={lib}");
    }
    // C++ runtime: Zig's libc++ (linked statically) on the Kindle, the system
    // libstdc++ on the desktop.
    println!("cargo:rustc-link-lib={}", if arm { "c++" } else { "stdc++" });
}

const LIBS: &[&str] =
    &["crengine", "freetype", "harfbuzz", "libjpeg-turbo", "libpng", "libunibreak", "utf8proc", "xxhash", "zlib"];

struct Ctx {
    tp: PathBuf,
    config: PathBuf,
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
            b.flag("-mcpu=cortex-a7");
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
            .define("UTF8PROC_STATIC", None)
            .define("XXH_INLINE_ALL", None) // xxhash is header-only this way
            .flag_if_supported("-ftabstop=4")
            .flag_if_supported("-fvisibility-inlines-hidden");
        b.compile("verso_crengine");
        link.push("verso_crengine");
    }
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
    // The glyph cache item has an `alignas(16)` member, but is allocated with
    // malloc, which only guarantees 8-byte alignment on 32-bit ARM. Clang then
    // uses 16-byte-aligned NEON stores on it and the Kindle crashes (SIGBUS).
    (
        "crengine/include/lvfntman.h",
        "struct LVFontGlyphCacheItem\n{",
        "static inline void *verso_malloc16(size_t size) {\n    void *p = NULL;\n    return posix_memalign(&p, 16, size) == 0 ? p : NULL;\n}\n\nstruct LVFontGlyphCacheItem\n{",
    ),
    ("crengine/include/lvfntman.h", "(LVFontGlyphCacheItem *)malloc(", "(LVFontGlyphCacheItem *)verso_malloc16("),
    // Some repackaged EPUBs leave out the `mimetype` entry. crengine then
    // treats the book as a plain ZIP and shows the first file in it (often a
    // stray text file). Accept a ZIP without `mimetype` as an EPUB when it has
    // the EPUB container file, like the library's metadata reader does.
    (
        "crengine/src/epubfmt.cpp",
        "    if ( mimeType != U\"application/epub+zip\" )\n        return false;",
        "    if ( mimeType.empty() )\n        return !m_arc->OpenStream(U\"META-INF/container.xml\", LVOM_READ).isNull();\n    if ( mimeType != U\"application/epub+zip\" )\n        return false;",
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
