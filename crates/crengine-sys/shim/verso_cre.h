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
typedef struct VersoCrePageInfo {
    int page;
    int progress;
    long long text_offset;
    int has_images;
    int position_len;
    int link_rects;
} VersoCrePageInfo;

/* Resolve a word (0), extend geometry (1), extract a full range and clipped
 * geometry (2), or geometry only (3).
 * Explicitly visits page, even after speculative rendering. Stores a snapshot
 * until the next call. Returns 1 for a selection, 0 for no text, -1 on error. */
int verso_cre_select(VersoCreDoc *doc, int page, int mode, int x, int y,
                     const char *start, const char *end);
/* Snapshot fields: 0=start, 1=exclusive end, 2=plain text,
 * 3=visible endpoint bits (start=1, end=2), 4=start page. */
int verso_cre_selection_text(VersoCreDoc *doc, int field, char *buf, int len);
/* Returns total count, copies at most capacity screen rectangles. */
int verso_cre_selection_rects(VersoCreDoc *doc, VersoCreRect *rects, int capacity);

/* Links. List the internal links on page (0-based) for hit-testing taps:
 * the rectangles of each link's text in window coordinates, and each
 * rectangle's link number in links. Call with rects NULL first: it lists
 * the page and returns the rectangle count; then call with buffers to copy
 * at most capacity. Links out of the book are left out. -1 on error. */
int verso_cre_page_links(VersoCreDoc *doc, int page, VersoCreRect *rects, int *links, int capacity);
/* Resolves link number link of page: 1 a footnote (show it in place),
 * 2 another place in the book (jump there), 0 no target, -1 on error.
 * Stores a snapshot until the next call. */
int verso_cre_link(VersoCreDoc *doc, int page, int link);
/* Snapshot fields: 0=target position, 1=the note as simple HTML (footnotes
 * only), 2=link text without brackets, 3=the rule that decided (for logs). */
int verso_cre_link_text(VersoCreDoc *doc, int field, char *buf, int len);

/* Book search. Start a literal, case-insensitive search from the beginning,
 * keeping matches that start a word when the query starts with a letter or
 * digit; each step searches whole blocks for about max_ms and keeps only that step's
 * hits. Returns the number of new hits, or -1 on error. */
int verso_cre_search_start(VersoCreDoc *doc, const char *query);
int verso_cre_search_step(VersoCreDoc *doc, int max_ms);
/* 0 = searching, 1 = finished, 2 = stopped at the hit limit. */
int verso_cre_search_state(VersoCreDoc *doc, int *searched_pages);
/* Hit fields: 0=start, 1=exclusive end, 2=text before, 3=match, 4=text after
 * (UTF-8, returns full length), 5=page number (returned directly). */
int verso_cre_search_hit(VersoCreDoc *doc, int index, int field, char *buf, int len);

/* Where crengine's error messages go, instead of stderr. Call before
 * verso_cre_init. */
void verso_cre_set_logger(void (*logger)(const char *message));

/* Global setup, once per process. `hyph_dir` holds crengine's *.pattern
 * files (may be NULL: no hyphenation). `cache_dir` enables crengine's
 * on-disk document cache (may be NULL). Returns 0 on success. */
int verso_cre_init(const char *hyph_dir, const char *cache_dir, int cache_max_bytes);

/* Reads the document cache index again from disk, after another process
 * (Verso's background pre-layout) changed it. Call with no document open
 * that was opened before the other process ran. Returns 0 on success. */
int verso_cre_cache_reload(void);

/* Makes a font file available to all documents. Returns 0 on success. */
int verso_cre_register_font(const char *path);
/* The faces registered from font file `path`, as text for
   verso_cre_register_font_faces (copied into buf like the other getters).
   Returns the text's length, or -1. */
int verso_cre_describe_font(const char *path, char *buf, int len);
/* Registers faces described by verso_cre_describe_font without opening the
   file. Returns how many, or -1 for a malformed description. */
int verso_cre_register_font_faces(const char *path, const char *description);
/* Hash of every registered font; part of each cached layout's key. */
unsigned int verso_cre_font_list_hash(void);

/* A document view of `width` x `height` pixels. NULL on failure. */
VersoCreDoc *verso_cre_new(int width, int height, int dpi);
/* Text shaping (crengine's kerning mode): 1 FreeType kerning, 2 HarfBuzz
   light (default), 3 full HarfBuzz with ligatures. Set before loading. */
void verso_cre_set_shaping(VersoCreDoc *doc, int mode);
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
/* Draws the current page as 32-bit opaque RGBA (`[R, G, B, 255]`), `stride`
 * bytes per row, into a buffer of at least stride * height bytes. */
int verso_cre_draw_rgba(VersoCreDoc *doc, uint8_t *buf, int width, int height, int stride);

/* Book metadata as UTF-8, written into `buf` (always NUL-terminated).
 * `key`: "title", "authors" or "language". Returns the full length. */
int verso_cre_get_prop(VersoCreDoc *doc, const char *key, char *buf, int buf_len);

/* Reader API. String getters return full UTF-8 byte length, excluding NUL. */
int verso_cre_position(VersoCreDoc *doc, char *buf, int len);
/* Snapshot of the current page's metadata in a single getBookmark() + single
 * page-range scan: page number, position XPointer, progress, text offset,
 * image presence, and internal link rectangles (cached for verso_cre_page_links
 * with non-NULL rects). When `skip_pos` is non-NULL and matches the current
 * position while text offsets are not yet built, skips counting offsets
 * (setting progress = -1, text_offset = -1). Returns 0 on success, -1 on error. */
int verso_cre_page_info(VersoCreDoc *doc, const char *skip_pos, char *pos_buf, int pos_len,
                        VersoCrePageInfo *out);
/* Copies the cached XPointer from the last verso_cre_page_info call. */
int verso_cre_page_position(VersoCreDoc *doc, char *buf, int len);
int verso_cre_restore(VersoCreDoc *doc, const char *position);
int verso_cre_progress(VersoCreDoc *doc);
/* Characters of book text before `position` (an XPointer, e.g. a TOC entry's),
 * or before the current page when NULL. Independent of layout. -1 if unknown. */
long long verso_cre_text_offset(VersoCreDoc *doc, const char *position);
/* Characters of text in the whole book. */
long long verso_cre_text_length(VersoCreDoc *doc);
int verso_cre_typography(VersoCreDoc *doc, const char *face, int px, int spacing,
                         int margin, int top, int bottom, int justify, int publisher);
int verso_cre_toc_count(VersoCreDoc *doc);
int verso_cre_toc_entry(VersoCreDoc *doc, int index, int field, char *buf, int len);
int verso_cre_images(VersoCreDoc *doc);
int verso_cre_embedded_fonts(VersoCreDoc *doc);
int verso_cre_font_count(void);
int verso_cre_font_name(int index, char *buf, int len);
int verso_cre_font_file(int index, char *buf, int len);
void verso_cre_fonts_clear(void);


int verso_cre_annotation_clear(VersoCreDoc *doc);
// Checks a saved range: both ends must be valid text positions, start before
// end. Returns the length of its UTF-8 text (soft hyphens removed), or -1.
// Remembers the range and text for the two calls below.
int verso_cre_annotation_check(VersoCreDoc *doc, const char *start, const char *end);
// Copies the last checked text; returns its full length.
int verso_cre_annotation_checked_text(VersoCreDoc *doc, char *buf, int len);
// Adds the last checked range as annotation `id`; returns its page, or -1.
// `last` (may be NULL) gets the page a painted range ends on, else the same page.
// Unpainted entries (page bookmarks) count for reading order only:
// verso_cre_annotation_page never lists them.
int verso_cre_annotation_keep(VersoCreDoc *doc, int id, int painted, int *last);
int verso_cre_annotation_finish(VersoCreDoc *doc);
int verso_cre_annotation_order(VersoCreDoc *doc, int index);
// The `index`-th finished annotation in reading order: returns its id (-1
// past the end) and its pages in the current layout, as
// verso_cre_annotation_keep reports them. Kept ranges are DOM positions, so
// they stay valid across relayouts of the same open book.
int verso_cre_annotation_kept(VersoCreDoc *doc, int index, int *page, int *last);
int verso_cre_annotation_page(VersoCreDoc *doc, int page, int *ids, int capacity);

/* Page bookmarks. The current page's opening words: from where the page
 * starts (the reading position's XPointer) to the end of the last whole word
 * within `max_chars` characters and the page. With a non-empty `start` (a
 * saved XPointer, such as an imported bookmark's), the words from there to the
 * end of the page holding it instead, or -1 when it isn't in this book.
 * Returns the text's UTF-8 length, 0 for a page without text (empty end and
 * text), or -1. */
int verso_cre_page_opening(VersoCreDoc *doc, const char *start, int max_chars);
/* The last opening's field 0 = start, 1 = end, 2 = text; full UTF-8 length. */
int verso_cre_page_opening_text(VersoCreDoc *doc, int field, char *buf, int len);
/* Checks a saved bookmark like verso_cre_annotation_check: the start must still
 * exist; a non-empty end must be a text position after it. An empty end checks
 * only the start, and the checked text is empty. Returns the checked text's
 * length, or -1. The range can then be kept with verso_cre_annotation_keep. */
int verso_cre_bookmark_check(VersoCreDoc *doc, const char *start, const char *end);

#ifdef __cplusplus
}
#endif

#endif /* VERSO_CRE_H */
