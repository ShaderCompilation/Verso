/* A small C interface over crengine's C++ API, so Rust can call it without
 * C++ bindings. Every function catches C++ exceptions; none ever throws.
 * Not thread-safe: use crengine from one thread at a time. */
#ifndef VERSO_CRE_H
#define VERSO_CRE_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct VersoCreDoc VersoCreDoc;

/* Global setup, once per process. `hyph_dir` holds crengine's *.pattern
 * files (may be NULL: no hyphenation). `cache_dir` enables crengine's
 * on-disk document cache (may be NULL). Returns 0 on success. */
int verso_cre_init(const char *hyph_dir, const char *cache_dir, int cache_max_bytes);

/* Makes a font file available to all documents. Returns 0 on success. */
int verso_cre_register_font(const char *path);

/* A document view of `width` x `height` pixels. NULL on failure. */
VersoCreDoc *verso_cre_new(int width, int height, int dpi);
void verso_cre_free(VersoCreDoc *doc);

/* Settings. Apply before verso_cre_load where possible: changing them after
 * loading re-lays out the book on the next render/draw. */
int verso_cre_set_stylesheet_file(VersoCreDoc *doc, const char *path);
void verso_cre_set_font_face(VersoCreDoc *doc, const char *face);
/* After loading, this re-lays out the book and keeps the reading position. */
void verso_cre_set_font_size(VersoCreDoc *doc, int px);
int verso_cre_get_font_size(VersoCreDoc *doc);
void verso_cre_set_margins(VersoCreDoc *doc, int left, int top, int right, int bottom);
void verso_cre_set_line_spacing(VersoCreDoc *doc, int percent);
void verso_cre_set_size(VersoCreDoc *doc, int width, int height);

/* Opens a book and picks the hyphenation language from its metadata.
 * Returns 0 on success. */
int verso_cre_load(VersoCreDoc *doc, const char *path);

/* Lays out the book (if anything changed since the last call). */
void verso_cre_render(VersoCreDoc *doc);

int verso_cre_page_count(VersoCreDoc *doc);
/* 0-based. */
int verso_cre_current_page(VersoCreDoc *doc);
int verso_cre_go_to_page(VersoCreDoc *doc, int page);

/* Draws the current page as 8-bit gray (0 = black), `stride` bytes per row,
 * into a buffer of at least stride * height bytes. Returns 0 on success. */
int verso_cre_draw(VersoCreDoc *doc, uint8_t *buf, int width, int height, int stride);

/* Book metadata as UTF-8, written into `buf` (always NUL-terminated).
 * `key`: "title", "authors" or "language". Returns the full length. */
int verso_cre_get_prop(VersoCreDoc *doc, const char *key, char *buf, int buf_len);

#ifdef __cplusplus
}
#endif

#endif /* VERSO_CRE_H */
