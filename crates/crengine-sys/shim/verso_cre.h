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
typedef struct VersoCreRect { int left, top, right, bottom; } VersoCreRect;

/* Resolve a word (mode 0), union it with an anchor (1), or extract a range (2).
 * Explicitly visits page, even after speculative rendering. Stores a snapshot
 * until the next call. Returns 1 for a selection, 0 for no text, -1 on error. */
int verso_cre_select(VersoCreDoc *doc, int page, int mode, int x, int y,
                     const char *start, const char *end);
/* Snapshot fields: 0=start, 1=exclusive end, 2=plain text. */
int verso_cre_selection_text(VersoCreDoc *doc, int field, char *buf, int len);
/* Returns total count, copies at most capacity screen rectangles. */
int verso_cre_selection_rects(VersoCreDoc *doc, VersoCreRect *rects, int capacity);

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

/* Reader API. String getters return full UTF-8 byte length, excluding NUL. */
int verso_cre_position(VersoCreDoc *doc, char *buf, int len);
int verso_cre_restore(VersoCreDoc *doc, const char *position);
int verso_cre_progress(VersoCreDoc *doc);
int verso_cre_typography(VersoCreDoc *doc, const char *face, int px, int spacing,
                         int margin, int top, int bottom, int justify, int publisher);
int verso_cre_toc_count(VersoCreDoc *doc);
int verso_cre_toc_entry(VersoCreDoc *doc, int index, int field, char *buf, int len);
int verso_cre_images(VersoCreDoc *doc);
int verso_cre_embedded_fonts(VersoCreDoc *doc);
int verso_cre_font_count(void);
int verso_cre_font_name(int index, char *buf, int len);
int verso_cre_font_file(int index, char *buf, int len);

#ifdef __cplusplus
}
#endif

#endif /* VERSO_CRE_H */
