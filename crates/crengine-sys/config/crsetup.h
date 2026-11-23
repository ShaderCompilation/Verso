/* crengine build configuration for Verso (replaces crengine's generated
 * crsetup.h from crsetup.h.cmake). Keep in sync with build.rs: every USE_*
 * library set to 1 here must be compiled there. */
#ifndef CRSETUP_H_INCLUDED
#define CRSETUP_H_INCLUDED

#define LINUX 1
#define _LINUX 1

/* Compression. */
#define USE_ZLIB                             1
#define USE_ZSTD                             0

/* Documents cache. Every book is cached, however small: a 140 KB EPUB can be
   a 240-page novel that takes ~0.6 s to lay out. Files of this size or smaller
   never read the cache. The size at which a cache is *written* is the
   crengine.cache.filesize.min property (default 300000 bytes), which the shim
   sets to 0; this threshold is only its unused fallback. */
#define DOCUMENT_CACHING_MIN_SIZE            0
#define DOCUMENT_CACHING_SIZE_THRESHOLD      0

/* Document formats (EPUB, FB2, HTML, TXT, RTF... are always on). */
#define CHM_SUPPORT_ENABLED                  0
#define ENABLE_ANTIWORD                      0
#define USE_MD4C                             0

/* Images. GIF uses crengine's own decoder. SVG uses LunaSVG, vendored in
   ../lunasvg. */
#define ARBITRARY_IMAGE_SCALE_ENABLED        1
#define MAX_IMAGE_SCALE_MUL                  2
#define USE_GIF                              1
#define USE_LIBJPEG                          1
#define USE_LIBPNG                           1
#define USE_LIBWEBP                          0
#define USE_LUNASVG                          1
#define USE_NANOSVG                          0
#define USE_STB_IMAGE                        0

/* Miscellaneous. */
#define MATHML_SUPPORT                       1
#define USE_SRELL_REGEX                      0

/* Output buffer. */
#define COLOR_BACKBUFFER                     1
#define CR_INTERNAL_PAGE_ORIENTATION         1
#define GRAY_BACKBUFFER_BITS                 2
#define GRAY_INVERSE                         0

/* Streams (glibc has off64_t). */
#define HAVE_OFF64_T                         1
#define HAVE_STAT64                          1
#ifndef _LARGEFILE64_SOURCE
# define _LARGEFILE64_SOURCE                 1
#endif
#define LVLONG_FILE_SUPPORT                  0
#define USE_ANSI_FILES                       0
#define FILE_STREAM_BUFFER_SIZE              0x20000   // 128.0 KiB
#define ZIP_STREAM_BUFFER_SIZE               0x40000   // 256.0 KiB

/* System. */
#define CR_USE_THREADS                       0
#define LDOM_USE_OWN_MEM_MAN                 1

/* Text. */
#define USE_LIMITED_FONT_SIZES_SET           0
#define USE_BITMAP_FONTS                     0
#define USE_WIN32_FONTS                      0
#define USE_GLYPHCACHE_HASHTABLE             0
#define GLYPH_CACHE_SIZE                     0x40000   // 256.0 KiB
#define ALLOW_KERNING                        1
#define USE_FONTCONFIG                       0
#define USE_FREETYPE                         1
#define USE_FRIBIDI                          0
#define USE_HARFBUZZ                         1
#define USE_LIBUNIBREAK                      1
#define USE_UTF8PROC                         1

/* Disable unused code. */
#define CR_ENABLE_PAGE_IMAGE_CACHE           0

#endif /* CRSETUP_H_INCLUDED */
