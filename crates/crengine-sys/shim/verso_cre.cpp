// C interface over crengine for Verso. See verso_cre.h.
// Reference for how crengine is driven: KOReader's cre.cpp (koreader-base).

#include "verso_cre.h"

#include <string.h>
#include <vector>
#include <unordered_map>
#include <algorithm>
#include <chrono>

#include "lvdocview.h"
#include "lvdrawbuf.h"
#include "hyphman.h"
#include "textlang.h"

struct VersoCreDoc {
    LVDocView *view;
    lString8 css;
    std::vector<LVTocItem *> toc;
    std::unordered_map<int, uint64_t> offsets;
    uint64_t text_length = 0;
    lString32 selection_start, selection_end, selection_text;
    std::vector<VersoCreRect> selection_rects;
    int selection_visible = 0, selection_page = 0;
    struct Annotation { int id; ldomXPointerEx start, end; };
    std::vector<Annotation> annotations, pending_annotations;
    std::vector<ldomXPointerEx> annotation_max_end;
    // Book search: a cursor that advances block by block, and the last batch.
    struct Hit { lString32 start, end, before, match, after; int page; };
    lString32 search_pattern;
    ldomXPointerEx search_cursor, search_end;
    int search_page = 0, search_found = 0, search_status = 1;
    bool search_word_start = true;
    std::vector<Hit> search_hits;
};

// Literal matching that ignores case, Unicode encoding/compatibility forms,
// inline markup, soft hyphens, invisible format characters and the various
// space, apostrophe and dash characters books use.
static const lUInt32 SEARCH_FLAGS = LDOM_FIND_TEXT_MATCH_ACROSS_TEXT_NODES
    | LDOM_FIND_TEXT_COLLAPSE_CONSECUTIVE_SPACES | LDOM_FIND_TEXT_NORMALIZE_COMPATIBILITY
    | LDOM_FIND_TEXT_IGNORE_FORMAT_CONTROL_CHARS | LDOM_FIND_TEXT_FOLD_SPACES
    | LDOM_FIND_TEXT_FOLD_APOSTROPHES | LDOM_FIND_TEXT_FOLD_HYPHENS;
static const int SEARCH_PAGES_PER_CHUNK = 4;
static const int SEARCH_CHUNK_RANGES = 1 << 20; // per chunk, before word-start filtering
static const int SEARCH_MAX_HITS = 500;
static const int SNIPPET_BEFORE = 80, SNIPPET_AFTER = 160;

// Runs `body`, turning any C++ exception into `fallback`.
#define GUARD(fallback, body) \
    try { body } catch (...) { return fallback; }

static void set_int(LVDocView *v, const char *name, int value) {
    CRPropRef props = LVCreatePropsContainer();
    props->setInt(name, value);
    v->propsApply(props);
}

static void set_str(LVDocView *v, const char *name, const char *value) {
    CRPropRef props = LVCreatePropsContainer();
    props->setString(name, value);
    v->propsApply(props);
}

static int copy_utf8(const lString32 &value, char *buf, int len) {
    lString8 text = UnicodeToUtf8(value);
    if (buf && len > 0) {
        int n = std::min(text.length(), len - 1);
        memcpy(buf, text.c_str(), n);
        buf[n] = 0;
    }
    return text.length();
}

static void collect_toc(LVTocItem *item, std::vector<LVTocItem *> &out) {
    if (!item) return;
    for (int i = 0; i < item->getChildCount(); ++i) {
        LVTocItem *child = item->getChild(i);
        out.push_back(child);
        collect_toc(child, out);
    }
}

static void collect_offsets(ldomNode *node, VersoCreDoc *doc) {
    if (!node) return;
    if (node->isText()) {
        doc->offsets[node->getDataIndex()] = doc->text_length;
        doc->text_length += node->getText().length();
    } else {
        for (int i = 0; i < node->getChildCount(); ++i)
            collect_offsets(node->getChildNode(i), doc);
    }
}

// Use crengine's Unicode properties and visible DOM traversal, including words
// split by inline markup. Apostrophes join letters, punctuation does not.
static bool word_char(ldomXPointerEx p) {
    if (p.isNull() || !p.isText() || !p.isVisible()) return false;
    lChar32 c = p.getChar();
    if (lGetCharProps(c) & (CH_PROP_ALPHA | CH_PROP_MODIFIER | CH_PROP_DIGIT | CH_PROP_HYPHEN))
        return true;
    if (c != '\'' && c != 0x2019) return false;
    ldomXPointerEx before = p, after = p;
    if (!before.prevVisibleChar(true) || !after.nextVisibleChar(true)) return false;
    if (!after.getChar() && !after.nextVisibleChar(true)) return false;
    return (lGetCharProps(before.getChar()) & CH_PROP_ALPHA)
        && (lGetCharProps(after.getChar()) & CH_PROP_ALPHA);
}

static bool word_at(LVDocView *v, lvPoint point, ldomXRange &range) {
    ldomXPointerEx p = v->getNodeByPoint(point, true, false);
    if (p.isNull() || !p.isText()) return false;
    if (!word_char(p)) {
        // Hit testing can return the boundary just after the touched character.
        if (!p.prevVisibleChar(true) || !word_char(p)) return false;
    }
    ldomXPointerEx start = p, last = p, scan = p;
    while (scan.prevVisibleChar(true) && word_char(scan)) start = scan;
    scan = p;
    while (scan.nextVisibleChar(true)) {
        if (!scan.getChar() && !scan.nextVisibleChar(true)) break;
        if (!word_char(scan)) break;
        last = scan;
    }
    ldomXPointerEx end(last.getNode(), last.getOffset() + 1);
    range = ldomXRange(start, end);
    // Exact word rectangles reject margins, interline whitespace and images.
    LVArray<lvRect> rects;
    range.getSegmentRects(rects);
    for (int i = 0; i < rects.length(); ++i) {
        lvPoint a = rects[i].topLeft(), b = rects[i].bottomRight();
        if (v->docToWindowPoint(a) && v->docToWindowPoint(b, true)
                && point.x >= a.x && point.x < b.x && point.y >= a.y && point.y < b.y)
            return true;
    }
    return false;
}


// The first text position of the block after the one holding `p` (or `p`
// itself at a block start). crengine matches within one rendered block, so
// search chunks end there and never split a possible match.
static ldomXPointerEx block_boundary(ldomXPointerEx p) {
    if (!p.isText() && !p.nextVisibleText()) return ldomXPointerEx();
    ldomXPointerEx before = p;
    if (p.getOffset() == 0 && !before.prevVisibleText(true)) return p;
    ldomNode *block = p.getThisBlockNode();
    while (p.nextVisibleText())
        if (p.getThisBlockNode() != block) return p;
    return ldomXPointerEx();
}

static bool letter_or_digit(lChar32 c) {
    return lGetCharProps(c) & (CH_PROP_ALPHA | CH_PROP_MODIFIER | CH_PROP_DIGIT);
}

// A match must start a word when the query does: "Vin" finds "Vin's" but not
// "moving". Inline markup and soft hyphens inside a word don't break it.
static bool starts_word(ldomXPointerEx p) {
    while (p.prevVisibleChar(true)) {
        lChar32 c = p.getChar();
        if (c != UNICODE_SOFT_HYPHEN_CODE) return !letter_or_digit(c);
    }
    return true;
}

// Up to `want` characters of the same block before/after a text position.
static lString32 block_text_before(ldomXPointerEx p, int want) {
    lString32 out = p.getNode()->getText().substr(0, p.getOffset());
    while (out.length() < want && p.prevVisibleText(true)) {
        lString32 text = p.getNode()->getText();
        text.append(out);
        out = text;
    }
    return out.length() > want ? out.substr(out.length() - want, want) : out;
}
static lString32 block_text_after(ldomXPointerEx p, int want) {
    lString32 text = p.getNode()->getText();
    lString32 out = text.substr(p.getOffset(), text.length() - p.getOffset());
    while (out.length() < want && p.nextVisibleText(true)) out.append(p.getNode()->getText());
    return out.length() > want ? out.substr(0, want) : out;
}

extern "C" {

int verso_cre_search_start(VersoCreDoc *doc, const char *query) {
    GUARD(-1, {
        doc->search_hits.clear();
        doc->search_status = 1;
        doc->search_page = 0;
        doc->search_found = 0;
        ldomDocument *d = doc->view->getDocument();
        lString32 pattern = Utf8ToUnicode(query);
        pattern.trim();
        if (!d || pattern.empty()) return -1;
        ldomXRange all(d->getRootNode(), true);
        doc->search_cursor = all.getStart();
        doc->search_end = all.getEnd();
        if (!doc->search_cursor.isText() && !doc->search_cursor.nextVisibleText()) return 0;
        doc->search_pattern = pattern;
        doc->search_word_start = letter_or_digit(pattern[0]);
        doc->search_status = 0;
        return 0;
    })
}

int verso_cre_search_step(VersoCreDoc *doc, int max_ms) {
    GUARD(-1, {
        doc->search_hits.clear();
        LVDocView *v = doc->view;
        int pages = v->getPageCount();
        auto started = std::chrono::steady_clock::now();
        while (doc->search_status == 0) {
            // Search a few pages at a time, ending each chunk on a block boundary.
            ldomXPointerEx end;
            while (end.isNull() && doc->search_page < pages) {
                doc->search_page = std::min(pages, doc->search_page + SEARCH_PAGES_PER_CHUNK);
                if (doc->search_page >= pages) break;
                LVRef<ldomXRange> page = v->getPageDocumentRange(doc->search_page);
                if (page.isNull()) continue;
                ldomXPointerEx boundary = block_boundary(page->getStart());
                if (boundary.isNull()) doc->search_page = pages;
                else if (boundary.compare(doc->search_cursor) > 0) end = boundary;
            }
            bool last = end.isNull();
            if (last) end = doc->search_end;
            ldomXRange range(doc->search_cursor, end);
            ldomXRangeList found;
            range.findText(doc->search_pattern, true, false, found, SEARCH_CHUNK_RANGES, 0, -1, false, false,
                           SEARCH_FLAGS);
            for (int i = 0; i < found.length(); ++i) {
                if (doc->search_found == SEARCH_MAX_HITS) {
                    doc->search_status = 2;
                    break;
                }
                ldomXRange *r = found[i];
                ldomXPointerEx a = r->getStart();
                ldomXPointerEx b = r->getEnd();
                if (!a.isText() || !b.isText()) continue;
                if (doc->search_word_start && !starts_word(a)) continue;
                VersoCreDoc::Hit hit;
                hit.start = a.toString();
                hit.end = b.toString();
                hit.match = removeSoftHyphens(r->getRangeText(' '));
                hit.before = removeSoftHyphens(block_text_before(a, SNIPPET_BEFORE));
                hit.after = removeSoftHyphens(block_text_after(b, SNIPPET_AFTER));
                hit.page = v->getBookmarkPage(a);
                doc->search_hits.push_back(hit);
                doc->search_found++;
            }
            if (doc->search_status == 0 && last) {
                doc->search_status = 1;
                doc->search_page = pages;
            }
            if (doc->search_status == 0) doc->search_cursor = end;
            auto elapsed = std::chrono::steady_clock::now() - started;
            if (std::chrono::duration_cast<std::chrono::milliseconds>(elapsed).count() >= max_ms) break;
        }
        return (int)doc->search_hits.size();
    })
}

int verso_cre_search_state(VersoCreDoc *doc, int *searched_pages) {
    GUARD(-1, {
        if (searched_pages) *searched_pages = doc->search_page;
        return doc->search_status;
    })
}

int verso_cre_search_hit(VersoCreDoc *doc, int index, int field, char *buf, int len) {
    GUARD(-1, {
        if (index < 0 || index >= (int)doc->search_hits.size()) return -1;
        const VersoCreDoc::Hit &hit = doc->search_hits[index];
        switch (field) {
            case 0: return copy_utf8(hit.start, buf, len);
            case 1: return copy_utf8(hit.end, buf, len);
            case 2: return copy_utf8(hit.before, buf, len);
            case 3: return copy_utf8(hit.match, buf, len);
            case 4: return copy_utf8(hit.after, buf, len);
            case 5: return hit.page;
        }
        return -1;
    })
}

int verso_cre_select(VersoCreDoc *doc, int page, int mode, int x, int y,
                     const char *start, const char *end) {
    GUARD(-1, {
        doc->selection_rects.clear();
        doc->selection_start.clear(); doc->selection_end.clear(); doc->selection_text.clear();
        LVDocView *v = doc->view;
        if (page < 0 || page >= v->getPageCount() || mode < 0 || mode > 4) return -1;
        v->goToPage(page, false, true, false);
        LVRef<ldomXRange> visible = v->getPageDocumentRange(page);
        if (visible.isNull()) return 0;
        ldomXRange range;
        if (mode < 2 && !word_at(v, lvPoint(x, y), range)) return 0;
        if (mode != 0) {
            if (!start || !end) return -1;
            ldomXPointerEx a = v->getDocument()->createXPointer(Utf8ToUnicode(start));
            ldomXPointerEx b = v->getDocument()->createXPointer(Utf8ToUnicode(end));
            if (a.isNull() || b.isNull() || !a.isText() || !b.isText()) return -1;
            if (a.getOffset() < 0 || b.getOffset() < 0 || a.getOffset() > a.getNode()->getText().length() || b.getOffset() > b.getNode()->getText().length()) return -1;
            ldomXRange anchor(a, b); anchor.sort();
            if (mode >= 2) range = anchor;
            else {
                if (anchor.getStart().compare(range.getStart()) < 0) range.setStart(anchor.getStart());
                if (anchor.getEnd().compare(range.getEnd()) > 0) range.setEnd(anchor.getEnd());
            }
        }
        if (range.isNull() || range.getStart().compare(range.getEnd()) >= 0) return 0;
        doc->selection_page = v->getBookmarkPage(range.getStart());
        doc->selection_start = range.getStart().toString();
        doc->selection_end = range.getEnd().toString();
        if (mode == 0 || mode == 2 || mode == 4) doc->selection_text = removeSoftHyphens(range.getRangeText());
        doc->selection_visible = 0;
        if (mode == 4) return 1;
        if (range.getStart().compare(visible->getStart()) >= 0 && range.getStart().compare(visible->getEnd()) < 0) doc->selection_visible |= 1;
        if (range.getEnd().compare(visible->getStart()) > 0 && range.getEnd().compare(visible->getEnd()) <= 0) doc->selection_visible |= 2;
        // Only paint geometry is clipped. Full endpoints/text always survive.
        if (range.getStart().compare(visible->getStart()) < 0) range.setStart(visible->getStart());
        if (range.getEnd().compare(visible->getEnd()) > 0) range.setEnd(visible->getEnd());
        if (range.getStart().compare(range.getEnd()) >= 0) return 1;
        LVArray<lvRect> rects;
        range.getSegmentRects(rects);
        for (int i = 0; i < rects.length(); ++i) {
            lvPoint a = rects[i].topLeft();
            lvPoint b = rects[i].bottomRight();
            if (v->docToWindowPoint(a) && v->docToWindowPoint(b, true) && b.x > a.x && b.y > a.y)
                doc->selection_rects.push_back({a.x, a.y, b.x, b.y});
        }

        return 1;
    })
}
int verso_cre_selection_text(VersoCreDoc *doc, int field, char *buf, int len) {
    GUARD(-1, {
        if (field == 0) return copy_utf8(doc->selection_start, buf, len);
        if (field == 1) return copy_utf8(doc->selection_end, buf, len);
        if (field == 2) return copy_utf8(doc->selection_text, buf, len);
        if (field == 3) return doc->selection_visible;
        if (field == 4) return doc->selection_page;
        return -1;
    })
}
int verso_cre_selection_rects(VersoCreDoc *doc, VersoCreRect *rects, int capacity) {
    GUARD(-1, {
        int n = (int)doc->selection_rects.size();
        if (rects && capacity > 0)
            std::copy_n(doc->selection_rects.begin(), std::min(n, capacity), rects);
        return n;
    })
}


int verso_cre_annotation_clear(VersoCreDoc *doc) {
    GUARD(-1, { doc->pending_annotations.clear(); return 0; })
}
int verso_cre_annotation_add(VersoCreDoc *doc, int id, const char *start, const char *end) {
    GUARD(-1, {
        auto *v = doc->view;
        ldomXPointerEx a = v->getDocument()->createXPointer(Utf8ToUnicode(start));
        ldomXPointerEx b = v->getDocument()->createXPointer(Utf8ToUnicode(end));
        if (a.isNull() || b.isNull() || !a.isText() || !b.isText() || a.compare(b) >= 0) return -1;
        doc->pending_annotations.push_back({id, a, b});
        return v->getBookmarkPage(a);
    })
}
int verso_cre_annotation_finish(VersoCreDoc *doc) {
    GUARD(-1, {
        doc->annotations.swap(doc->pending_annotations);
        doc->pending_annotations.clear();
        std::stable_sort(doc->annotations.begin(), doc->annotations.end(), [](const VersoCreDoc::Annotation &a, const VersoCreDoc::Annotation &b) {
            return a.start.compare(b.start) < 0;
        });
        doc->annotation_max_end.clear();
        for (const auto &a : doc->annotations) {
            auto end = a.end;
            if (!doc->annotation_max_end.empty() && doc->annotation_max_end.back().compare(end) > 0) end = doc->annotation_max_end.back();
            doc->annotation_max_end.push_back(end);
        }
        return 0;
    })
}
int verso_cre_annotation_order(VersoCreDoc *doc, int index) {
    GUARD(-1, { return index >= 0 && index < (int)doc->annotations.size() ? doc->annotations[index].id : -1; })
}
int verso_cre_annotation_page(VersoCreDoc *doc, int page, int *ids, int capacity) {
    GUARD(-1, {
        auto visible = doc->view->getPageDocumentRange(page);
        if (visible.isNull()) return 0;
        const auto &a = doc->annotations;
        // Prefix maximum ends skip every interval wholly before this page,
        // including nested/overlapping ranges. Starts bound the other edge.
        auto first = std::upper_bound(doc->annotation_max_end.begin(), doc->annotation_max_end.end(), visible->getStart(),
            [](const ldomXPointerEx &p, const ldomXPointerEx &q) { return p.compare(q) < 0; });
        int count = 0;
        for (size_t i = first - doc->annotation_max_end.begin(); i < a.size(); ++i) {
            if (a[i].start.compare(visible->getEnd()) >= 0) break;
            if (a[i].end.compare(visible->getStart()) <= 0) continue;
            if (ids && count < capacity) ids[count] = a[i].id;
            count++;
        }
        return count;
    })
}
static lString32 cache_dir_saved;
static lvsize_t cache_max_saved = 0;

int verso_cre_init(const char *hyph_dir, const char *cache_dir, int cache_max_bytes) {
    GUARD(-1, {
        CRLog::setStderrLogger();
        CRLog::setLogLevel(CRLog::LL_ERROR);
        if (!InitFontManager(lString8()))
            return -1;
        if (hyph_dir)
            HyphMan::initDictionaries(Utf8ToUnicode(hyph_dir));
        // Hyphenate with the patterns of each book's (and each element's) language.
        TextLangMan::setEmbeddedLangsEnabled(true);
        TextLangMan::setHyphenationEnabled(true);
        if (cache_dir && cache_max_bytes > 0) {
            cache_dir_saved = Utf8ToUnicode(cache_dir);
            cache_max_saved = (lvsize_t)cache_max_bytes;
            ldomDocCache::init(cache_dir_saved, cache_max_saved);
        }
        return 0;
    })
}

int verso_cre_cache_reload(void) {
    GUARD(-1, {
        if (cache_dir_saved.empty())
            return -1;
        // init() replaces the in-memory index with the one on disk, drops
        // entries whose files are gone and deletes cache files it doesn't list.
        return ldomDocCache::init(cache_dir_saved, cache_max_saved) ? 0 : -1;
    })
}

int verso_cre_register_font(const char *path) {
    GUARD(-1, { return fontMan->RegisterFont(lString8(path)) ? 0 : -1; })
}

int verso_cre_describe_font(const char *path, char *buf, int len) {
    GUARD(-1, {
        lString8 text = fontMan->DescribeFontFile(lString8(path));
        if (buf && len > 0) {
            int n = std::min(text.length(), len - 1);
            memcpy(buf, text.c_str(), n);
            buf[n] = 0;
        }
        return text.length();
    })
}

int verso_cre_register_font_faces(const char *path, const char *description) {
    GUARD(-1, { return fontMan->RegisterFontFaces(lString8(path), lString8(description)); })
}

unsigned int verso_cre_font_list_hash(void) {
    GUARD(0, { return fontMan->GetFontListHash(-1); })
}

VersoCreDoc *verso_cre_new(int width, int height, int dpi) {
    GUARD(nullptr, {
        LVDocView *v = new LVDocView(-1, true);
        v->setViewMode(DVM_PAGES, 1);
        v->setPageHeaderInfo(0); // no crengine status bar; Verso draws its own
        CRPropRef props = LVCreatePropsContainer();
        props->setInt(PROP_RENDER_DPI, dpi);      // CSS units scale with the screen
        props->setInt(PROP_FONT_KERNING, 2);      // HarfBuzz light (see verso_cre_set_shaping)
        props->setInt(PROP_FONT_HINTING, 2);      // autohinter (as KOReader)
        props->setInt(PROP_EMBEDDED_STYLES, 1);
        props->setInt(PROP_EMBEDDED_FONTS, 1);
        props->setInt(PROP_MIN_FILE_SIZE_TO_CACHE, 0); // cache every book (see crsetup.h)
        v->propsApply(props);
        v->Resize(width, height);
        VersoCreDoc *doc = new VersoCreDoc();
        doc->view = v;
        return doc;
    })
}

void verso_cre_free(VersoCreDoc *doc) {
    if (!doc)
        return;
    try {
        doc->view->close();
        delete doc->view;
    } catch (...) {
    }
    delete doc;
}

int verso_cre_set_stylesheet_file(VersoCreDoc *doc, const char *path) {
    GUARD(-1, {
        lString8 css;
        if (!LVLoadStylesheetFile(Utf8ToUnicode(path), css))
            return -1;
        doc->css = css;
        doc->view->setStyleSheet(css, false);
        return 0;
    })
}

void verso_cre_set_shaping(VersoCreDoc *doc, int mode) {
    try {
        set_int(doc->view, PROP_FONT_KERNING, mode);
    } catch (...) {
    }
}

void verso_cre_set_font_face(VersoCreDoc *doc, const char *face) {
    try {
        set_str(doc->view, PROP_FONT_FACE, face);
    } catch (...) {
    }
}

void verso_cre_set_font_size(VersoCreDoc *doc, int px) {
    try {
        LVDocView *v = doc->view;
        if (!v->getDocument()) {
            v->setFontSize(px);
            return;
        }
        // Re-layout keeps the page *number*; keep the reading position instead.
        ldomXPointer pos = v->getBookmark();
        v->setFontSize(px);
        v->Render();
        if (!pos.isNull()) {
            v->goToBookmark(pos);
            v->SetPos(pos.toPoint().y); // goToBookmark is lazy (as in KOReader's cre.cpp)
        }
    } catch (...) {
    }
}

int verso_cre_get_font_size(VersoCreDoc *doc) {
    GUARD(0, { return doc->view->getFontSize(); })
}

void verso_cre_set_margins(VersoCreDoc *doc, int left, int top, int right, int bottom) {
    try {
        lvRect rc(left, top, right, bottom);
        doc->view->setPageMargins(rc);
    } catch (...) {
    }
}

void verso_cre_set_line_spacing(VersoCreDoc *doc, int percent) {
    try {
        doc->view->setDefaultInterlineSpace(percent);
    } catch (...) {
    }
}

void verso_cre_set_size(VersoCreDoc *doc, int width, int height) {
    try {
        doc->view->Resize(width, height);
    } catch (...) {
    }
}

int verso_cre_load(VersoCreDoc *doc, const char *path) {
    GUARD(-1, {
        if (!doc->view->LoadDocument(path) || !doc->view->getDocument())
            return -1;
        // Default hyphenation language: the book's own (e.g. "ru", "en-GB").
        lString32 lang = doc->view->getLanguage();
        if (lang.empty())
            lang = U"en";
        TextLangMan::setMainLang(lang);
        set_str(doc->view, PROP_TEXTLANG_MAIN_LANG, UnicodeToUtf8(lang).c_str());
        return 0;
    })
}

void verso_cre_render(VersoCreDoc *doc) {
    try {
        doc->view->Render();
    } catch (...) {
    }
}

int verso_cre_page_count(VersoCreDoc *doc) {
    GUARD(0, { return doc->view->getPageCount(); })
}

int verso_cre_current_page(VersoCreDoc *doc) {
    GUARD(0, { return doc->view->getCurPage(); })
}

int verso_cre_go_to_page(VersoCreDoc *doc, int page) {
    GUARD(-1, { return doc->view->goToPage(page, false, true, false) ? 0 : -1; })
}

int verso_cre_draw(VersoCreDoc *doc, uint8_t *buf, int width, int height, int stride) {
    GUARD(-1, {
        doc->view->Resize(width, height);
        doc->view->Render();
        if (stride == width) {
            LVGrayDrawBuf draw(width, height, 8, buf);
            doc->view->Draw(draw, false);
        } else {
            std::vector<uint8_t> tmp((size_t)width * height);
            LVGrayDrawBuf draw(width, height, 8, tmp.data());
            doc->view->Draw(draw, false);
            for (int y = 0; y < height; y++)
                memcpy(buf + (size_t)y * stride, tmp.data() + (size_t)y * width, width);
        }
        return 0;
    })
}

int verso_cre_get_prop(VersoCreDoc *doc, const char *key, char *buf, int buf_len) {
    GUARD(0, {
        lString32 value;
        if (!strcmp(key, "title"))
            value = doc->view->getTitle();
        else if (!strcmp(key, "authors"))
            value = doc->view->getAuthors();
        else if (!strcmp(key, "language"))
            value = doc->view->getLanguage();
        lString8 utf8 = UnicodeToUtf8(value);
        int len = utf8.length();
        if (buf && buf_len > 0) {
            int n = len < buf_len - 1 ? len : buf_len - 1;
            memcpy(buf, utf8.c_str(), n);
            buf[n] = 0;
        }
        return len;
    })
}


int verso_cre_position(VersoCreDoc *doc, char *buf, int len) {
    GUARD(-1, { return copy_utf8(doc->view->getBookmark().toString(), buf, len); })
}
int verso_cre_restore(VersoCreDoc *doc, const char *position) {
    GUARD(-1, {
        ldomXPointer pos = doc->view->getDocument()->createXPointer(Utf8ToUnicode(position));
        if (pos.isNull() || pos.toPoint().y < 0) return -1;
        doc->view->goToBookmark(pos);
        doc->view->SetPos(pos.toPoint().y);
        return 0;
    })
}
// Characters of text before `pos`, counting every text node in document
// order. Text offsets depend only on the book, not the layout. A pointer at
// an element counts from the first text inside or after it. -1 if unknown.
static long long text_offset(VersoCreDoc *doc, const ldomXPointer &pos) {
    if (doc->offsets.empty()) collect_offsets(doc->view->getDocument()->getRootNode(), doc);
    if (pos.isNull() || !doc->text_length) return -1;
    ldomXPointerEx p(pos);
    int offset = std::max(0, pos.getOffset());
    if (!p.isText()) {
        if (!p.nextText()) return (long long)doc->text_length;
        offset = 0;
    }
    auto it = doc->offsets.find(p.getNode()->getDataIndex());
    if (it == doc->offsets.end()) return -1;
    return (long long)std::min<uint64_t>(it->second + offset, doc->text_length);
}
int verso_cre_progress(VersoCreDoc *doc) {
    GUARD(0, {
        long long offset = text_offset(doc, doc->view->getBookmark());
        if (offset < 0) return 0;
        return std::min(10000, (int)(offset * 10000 / (long long)doc->text_length));
    })
}
long long verso_cre_text_offset(VersoCreDoc *doc, const char *position) {
    GUARD(-1, {
        if (!position) return text_offset(doc, doc->view->getBookmark());
        ldomXPointer pos = doc->view->getDocument()->createXPointer(Utf8ToUnicode(position));
        return text_offset(doc, pos);
    })
}
long long verso_cre_text_length(VersoCreDoc *doc) {
    GUARD(-1, {
        if (doc->offsets.empty()) collect_offsets(doc->view->getDocument()->getRootNode(), doc);
        return (long long)doc->text_length;
    })
}
int verso_cre_typography(VersoCreDoc *doc, const char *face, int px, int spacing,
                         int margin, int top, int bottom, int justify, int publisher) {
    GUARD(-1, {
        LVDocView *v = doc->view;
        ldomXPointer pos = v->getDocument() ? v->getBookmark() : ldomXPointer();
        CRPropRef props = LVCreatePropsContainer();
        props->setString(PROP_FONT_FACE, face);
        props->setInt(PROP_EMBEDDED_FONTS, publisher);
        v->propsApply(props);
        v->setFontSize(px);
        v->setDefaultInterlineSpace(spacing);
        v->setPageMargins(lvRect(margin, top, margin, bottom));
        lString8 css = doc->css;
        if (!publisher) {
            lString8 escaped;
            for (const char *p = face; *p; ++p) {
                if (*p == '\'' || *p == '\\') escaped << "\\";
                escaped << lString8(p, 1);
            }
            css << "\n* { font-family: '" << escaped << "' !important; }\n";
        }
        css << (justify ? "\np, li { text-align: justify !important; }" : "\np, li { text-align: left !important; }");
        v->setStyleSheet(css, false);
        if (v->getDocument()) {
            v->Render();
            if (!pos.isNull()) {
                v->goToBookmark(pos);
                v->SetPos(pos.toPoint().y);
            }
        }
        doc->offsets.clear(); doc->text_length = 0;
        doc->toc.clear();
        doc->search_hits.clear(); doc->search_status = 1;
        return 0;
    })
}
int verso_cre_toc_count(VersoCreDoc *doc) {
    GUARD(-1, {
        doc->toc.clear(); collect_toc(doc->view->getToc(), doc->toc);
        return (int)doc->toc.size();
    })
}
int verso_cre_toc_entry(VersoCreDoc *doc, int index, int field, char *buf, int len) {
    GUARD(-1, {
        if (index < 0 || index >= (int)doc->toc.size()) return -1;
        LVTocItem *item = doc->toc[index];
        if (field == 0) return copy_utf8(item->getName(), buf, len);
        if (field == 1) return copy_utf8(item->getXPointer().toString(), buf, len);
        if (field == 2) return doc->view->getBookmarkPage(item->getXPointer());
        return item->getLevel();
    })
}
int verso_cre_images(VersoCreDoc *doc) {
    GUARD(0, { return doc->view->getCurrentPageImageCount(); })
}
int verso_cre_embedded_fonts(VersoCreDoc *doc) {
    GUARD(0, { return doc->view->getDocument()->getEmbeddedFontList().length(); })
}
int verso_cre_font_count(void) {
    GUARD(0, { lString32Collection faces; fontMan->getFaceList(faces); return faces.length(); })
}
int verso_cre_font_name(int index, char *buf, int len) {
    GUARD(-1, {
        lString32Collection faces; fontMan->getFaceList(faces);
        if (index < 0 || index >= faces.length()) return -1;
        return copy_utf8(faces[index], buf, len);
    })
}

int verso_cre_font_file(int index, char *buf, int len) {
    GUARD(-1, {
        lString32Collection faces; fontMan->getFaceList(faces);
        if (index < 0 || index >= faces.length()) return -1;
        lString8 file; int face_index; int family; bool math; bool emojis;
        if (!fontMan->getFontFileNameAndFaceIndex(faces[index], false, false, file, face_index, family, math, emojis)) return -1;
        return copy_utf8(Utf8ToUnicode(file), buf, len);
    })
}
} // extern "C"
