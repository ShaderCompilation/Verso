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
#include "fb2def.h"

struct VersoCreDoc {
    LVDocView *view;
    lString8 css;
    std::vector<LVTocItem *> toc;
    std::unordered_map<int, uint64_t> offsets;
    uint64_t text_length = 0;
    lString32 selection_start, selection_end, selection_text;
    std::vector<VersoCreRect> selection_rects;
    int selection_visible = 0, selection_page = 0;
    // Bookmarks are kept for reading order only: `painted` is false.
    struct Annotation { int id; ldomXPointerEx start, end; bool painted; };
    std::vector<Annotation> annotations, pending_annotations;
    std::vector<ldomXPointerEx> annotation_max_end;
    // The range the last annotation_check accepted, and its text.
    ldomXPointerEx checked_start, checked_end;
    lString8 checked_text;
    // Book search: a cursor that advances block by block, and the last batch.
    struct Hit { lString32 start, end, before, match, after; int page; };
    lString32 search_pattern;
    ldomXPointerEx search_cursor, search_end;
    int search_page = 0, search_found = 0, search_status = 1;
    bool search_word_start = true;
    std::vector<Hit> search_hits;
    // Internal links on the last page listed, and the last link resolved.
    std::vector<VersoCreRect> link_rects;
    std::vector<int> link_ids;
    lString32 link_target, link_label;
    lString8 link_note, link_rule;
    // Cached UTF-8 XPointer from the last verso_cre_page_info call.
    lString8 page_position;
    // The last verso_cre_page_opening result.
    lString32 opening_start, opening_end, opening_text;
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

static int copy_lstring8(const lString8 &text, char *buf, int len) {
    if (buf && len > 0) {
        int n = std::min(text.length(), len - 1);
        memcpy(buf, text.c_str(), n);
        buf[n] = 0;
    }
    return text.length();
}

static int copy_utf8(const lString32 &value, char *buf, int len) {
    return copy_lstring8(UnicodeToUtf8(value), buf, len);
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

static bool space_char(lChar32 c) {
    return c == ' ' || c == '\t' || c == '\n' || c == '\r' || c == 0xA0 || c == 0x2009 || c == 0x200B;
}

static bool expand_word(ldomXPointerEx p, ldomXRange &range) {
    if (!word_char(p)) return false;
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
    return true;
}

static bool word_at(LVDocView *v, lvPoint point, ldomXRange &range) {
    ldomXPointerEx p = v->getNodeByPoint(point, true, false);
    if (p.isNull() || !p.isText()) return false;
    if (!word_char(p)) {
        // Hit testing can return the boundary just after the touched character.
        if (!p.prevVisibleChar(true) || !word_char(p)) return false;
    }
    if (!expand_word(p, range)) return false;
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

// Forgiving hit-test for dragging an active selection: crengine snaps points
// in margins, interline gaps and the empty space past a short line to the
// nearest visible text position on the page.
static bool extend_at(LVDocView *v, lvPoint point, ldomXRange &range) {
    ldomXPointerEx p = v->getNodeByPoint(point, false, true);
    if (p.isNull() || !p.isText()) return false;
    if (expand_word(p, range)) return true;
    lChar32 c = p.getChar();
    if (!c || space_char(c)) {
        ldomXPointerEx scan = p;
        while (scan.prevVisibleChar(true)) {
            lChar32 sc = scan.getChar();
            if (!sc || space_char(sc)) continue;
            if (expand_word(scan, range)) return true;
            range = ldomXRange(scan, ldomXPointerEx(scan.getNode(), scan.getOffset() + 1));
            return true;
        }
        scan = p;
        while (scan.nextVisibleChar(true)) {
            lChar32 sc = scan.getChar();
            if (!sc || space_char(sc)) continue;
            if (expand_word(scan, range)) return true;
            range = ldomXRange(scan, ldomXPointerEx(scan.getNode(), scan.getOffset() + 1));
            return true;
        }
        return false;
    }
    range = ldomXRange(p, ldomXPointerEx(p.getNode(), p.getOffset() + 1));
    return true;
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

// ---- Links and footnotes ----

// The longest note shown in a sheet; a longer target is a place to jump to.
static const int NOTE_MAX_CHARS = 6000;

// The element an internal link points at. crengine rewrites EPUB links to
// "#id" (ids prefixed per file) in its single DOM; other links have no target.
static ldomNode *link_target(ldomNode *a) {
    lString32 href = a->getAttributeValue(attr_href);
    if (href.length() < 2 || href[0] != '#') return NULL;
    ldomDocument *d = a->getDocument();
    lString32 id = href.substr(1);
    return d->getNodeById(d->getAttrValueIndex(id.c_str()));
}

static bool same(ldomNode *a, ldomNode *b) {
    return a && b && a->getDataIndex() == b->getDataIndex();
}

static bool inline_node(ldomNode *n) {
    if (!n->isElement()) return true;
    lvdom_element_render_method m = n->getRendMethod();
    return m == erm_inline || m == erm_invisible;
}

// The nearest block (paragraph, list item, section) holding n.
static ldomNode *block_of(ldomNode *n) {
    while (n && inline_node(n)) n = n->getParentNode();
    return n;
}

static bool has_token(lString32 value, const char *const *words) {
    value.lowercase();
    int start = 0;
    for (int i = 0; i <= value.length(); ++i) {
        if (i < value.length() && value[i] != ' ' && value[i] != '\t' && value[i] != '\n') continue;
        if (i > start) {
            lString32 token = value.substr(start, i - start);
            for (const char *const *w = words; *w; ++w)
                if (token == Utf8ToUnicode(*w)) return true;
        }
        start = i + 1;
    }
    return false;
}

static const char *const NOTEREF_TYPES[] = {"noteref", "note", "doc-noteref", NULL};
static const char *const NOTE_TYPES[] = {"footnote", "endnote", "rearnote", "note", "footnotes", "endnotes",
    "rearnotes", "doc-footnote", "doc-endnote", "doc-endnotes", NULL};

// EPUB3 epub:type / ARIA role on the link, FB2's <a type="note">.
static bool marked_source(ldomNode *a) {
    return has_token(a->getAttributeValue(attr_type), NOTEREF_TYPES)
        || has_token(a->getAttributeValue(attr_role), NOTEREF_TYPES);
}

// The same markup on the target or a container of it, or FB2's notes body.
static bool marked_target(ldomNode *t) {
    for (ldomNode *n = t; n && !n->isRoot(); n = n->getParentNode()) {
        if (!n->isElement()) continue;
        if (has_token(n->getAttributeValue(attr_type), NOTE_TYPES)
                || has_token(n->getAttributeValue(attr_role), NOTE_TYPES))
            return true;
        lUInt16 id = n->getNodeId();
        if (id == el_body) {
            lString32 name = n->getAttributeValue(attr_name);
            name.lowercase();
            return name == cs32("notes") || name == cs32("comments");
        }
        if (id == el_DocFragment) break;
    }
    return false;
}

static bool label_trim(lChar32 c) {
    return c == ' ' || c == '\t' || c == '\n' || c == '\r' || c == 0xA0 || c == '[' || c == ']' || c == '('
        || c == ')' || c == '{' || c == '}';
}

// Text without soft hyphens, surrounding spaces and brackets: "[12]" is "12".
static lString32 bare(lString32 s) {
    s = removeSoftHyphens(s);
    int a = 0, b = s.length();
    while (a < b && label_trim(s[a])) a++;
    while (b > a && label_trim(s[b - 1])) b--;
    return s.substr(a, b - a);
}

static ldomNode *first_text(ldomNode *n) {
    if (!n->isElement()) return n;
    for (int i = 0; i < n->getChildCount(); ++i)
        if (ldomNode *t = first_text(n->getChildNode(i))) return t;
    return NULL;
}

static int vertical(ldomNode *n) {
    css_style_ref_t st = n->getStyle();
    if (st.isNull() || st->vertical_align.type != css_val_unspecified) return 0;
    if (st->vertical_align.value == css_va_super) return 1;
    if (st->vertical_align.value == css_va_sub) return 2;
    return 0;
}

// Set as a superscript: <sup>, or vertical-align: super from the book's CSS.
static bool raised(ldomNode *n) {
    for (; n && inline_node(n); n = n->getParentNode()) {
        if (!n->isElement()) continue;
        if (n->getNodeId() == el_sup || vertical(n) == 1) return true;
    }
    return false;
}

// What a footnote marker looks like: "12", "*", "†", or anything short raised.
static bool note_label(const lString32 &label, bool superscript) {
    if (label.empty() || label.length() > 6) return false;
    if (superscript) return true;
    bool digits = true, symbols = true;
    for (int i = 0; i < label.length(); ++i) {
        lChar32 c = label[i];
        digits = digits && c >= '0' && c <= '9';
        symbols = symbols && (c == '*' || c == '#' || c == 0x2020 || c == 0x2021 || c == 0xA7 || c == 0xB6
            || c == 0x2016);
    }
    return digits || symbols;
}

// Whether link points at the marker a, or an inline container of it up to
// its paragraph: a footnote's way back.
static bool points_at(ldomNode *link, ldomNode *a) {
    ldomNode *t = link_target(link);
    if (!t) return false;
    ldomNode *stop = block_of(a);
    for (ldomNode *n = a; n; n = n->getParentNode()) {
        if (same(n, t)) return true;
        if (same(n, stop)) break;
    }
    return false;
}

static bool links_back(ldomNode *n, ldomNode *a) {
    if (!n->isElement()) return false;
    if (n->getNodeId() == el_a && points_at(n, a)) return true;
    for (int i = 0; i < n->getChildCount(); ++i)
        if (links_back(n->getChildNode(i), a)) return true;
    return false;
}

static bool heading(ldomNode *n) {
    lUInt16 id = n->getNodeId();
    return (id >= el_h1 && id <= el_h6) || id == el_title;
}

// A heading other than the note's own number ("12" over note 12).
static bool other_heading(ldomNode *n, const lString32 &label) {
    if (!n->isElement()) return false;
    if (heading(n)) return bare(n->getText()) != label;
    for (int i = 0; i < n->getChildCount(); ++i)
        if (other_heading(n->getChildNode(i), label)) return true;
    return false;
}

static lString32 block_text(ldomNode *n) {
    lString32 text = removeSoftHyphens(n->getText(' ', NOTE_MAX_CHARS + 1));
    text.trim();
    return text;
}

// The block a link target stands for: the paragraph, list item or section
// holding it, the next block after an empty anchor block, or a whole file's
// body for a link to a file.
static ldomNode *note_block(ldomNode *t) {
    if (t->isElement() && t->getNodeId() == el_DocFragment) {
        for (int i = 0; i < t->getChildCount(); ++i) {
            ldomNode *c = t->getChildNode(i);
            if (c->isElement() && c->getNodeId() == el_body) return c;
        }
        return t;
    }
    ldomNode *b = block_of(t);
    for (int tries = 0; b && tries < 2 && block_text(b).empty(); ++tries) {
        ldomNode *parent = b->getParentNode();
        if (!parent) break;
        ldomNode *next = NULL;
        for (int i = b->getNodeIndex() + 1; i < parent->getChildCount() && !next; ++i) {
            ldomNode *c = parent->getChildNode(i);
            if (c->isElement()) next = c;
        }
        if (!next) break;
        b = next;
    }
    return b;
}

static void escape(const lString32 &text, lString8 &out) {
    lString8 s = UnicodeToUtf8(removeSoftHyphens(text));
    for (int i = 0; i < s.length(); ++i) {
        char c = s[i];
        if (c == '&') out << "&amp;";
        else if (c == '<') out << "&lt;";
        else if (c == '>') out << "&gt;";
        else out.append(1, c);
    }
}

// The note as simple HTML for Verso's text renderer: blocks, line breaks,
// bold, italic, super/subscript (from the book's computed styles). Images,
// hidden elements, the link back to the marker and a heading that only
// repeats the marker are left out.
static void write_note(ldomNode *n, ldomNode *a, const lString32 &label, lString8 &out, bool italic, bool bold) {
    if (!n->isElement()) {
        escape(n->getText(), out);
        return;
    }
    lvdom_element_render_method m = n->getRendMethod();
    if (m == erm_invisible) return;
    lUInt16 id = n->getNodeId();
    if (id == el_img || id == el_image) return;
    if (id == el_br) {
        out << "<br>";
        return;
    }
    if (id == el_a && points_at(n, a)) return;
    if (heading(n) && bare(n->getText()) == label) return;
    css_style_ref_t st = n->getStyle();
    bool it = !st.isNull() && st->font_style == css_fs_italic;
    bool bo = !st.isNull() && st->font_weight >= 600 && st->font_weight < 1000;
    int script = vertical(n);
    if (id == el_sup) script = 1;
    if (id == el_sub) script = 2;
    const char *block = m == erm_inline ? NULL : heading(n) ? "h4" : id == el_blockquote ? "blockquote" : "div";
    if (block) out << "<" << block << ">";
    if (it && !italic) out << "<i>";
    if (bo && !bold) out << "<b>";
    if (script) out << (script == 1 ? "<sup>" : "<sub>");
    for (int i = 0; i < n->getChildCount(); ++i) write_note(n->getChildNode(i), a, label, out, it, bo);
    if (script) out << (script == 1 ? "</sup>" : "</sub>");
    if (bo && !bold) out << "</b>";
    if (it && !italic) out << "</i>";
    if (block) out << "</" << block << ">";
}

// 1: show the target as a footnote; 2: jump to it. why names the rule.
static int classify(ldomNode *a, ldomNode *t, ldomNode *note, const lString32 &label, const char *&why) {
    if (!note) { why = "no block"; return 2; }
    lString32 text = block_text(note);
    if (text.empty()) { why = "empty"; return 2; }
    if (text.length() > NOTE_MAX_CHARS) { why = "long"; return 2; }
    if (marked_source(a) || marked_target(t)) { why = "marked"; return 1; }
    if (heading(note) || other_heading(note, label)) { why = "heading"; return 2; }
    // A link that is a whole paragraph is navigation: a contents entry.
    ldomNode *source = block_of(a);
    if (source && bare(source->getText()) == label) { why = "navigation"; return 2; }
    // Notes follow their markers; a link back up is the way back.
    if (ldomXPointerEx(t, 0).compare(ldomXPointerEx(a, 0)) < 0) { why = "backward"; return 2; }
    ldomNode *first = first_text(a);
    if (note_label(label, first && raised(first))) { why = "label"; return 1; }
    if (links_back(note, a)) { why = "backlink"; return 1; }
    why = "plain";
    return 2;
}

// Walks a page's DOM range once to check for images and/or collect internal
// links in document order (matching LVDocView::getCurrentPageLinks, including
// an <a> wrapping the page's first text node).
static void scan_page_range(const LVRef<ldomXRange> &range, bool *has_images, std::vector<ldomNode *> *anchors) {
    if (has_images) *has_images = false;
    if (anchors) anchors->clear();
    if (range.isNull()) return;
    class PageScanner : public ldomNodeCallback {
        bool *has_images;
        std::vector<ldomNode *> *anchors;
        std::vector<ldomNode *> seen;
        bool checked_first_text = false;
        void add_anchor(ldomNode *elem) {
            if (!elem || !elem->hasChildren()) return;
            for (ldomNode *prev : seen)
                if (prev == elem) return;
            seen.push_back(elem);
            anchors->push_back(link_target(elem) ? elem : NULL);
        }
    public:
        PageScanner(bool *img, std::vector<ldomNode *> *a) : has_images(img), anchors(a) {}
        virtual void onText(ldomXRange *r) {
            if (!anchors || checked_first_text) return;
            ldomNode *node = r->getStart().getNode();
            while (node && !node->isElement()) node = node->getParentNode();
            while (node && node->getNodeId() != el_a) node = node->getParentNode();
            if (node) add_anchor(node);
            checked_first_text = true;
        }
        virtual bool onElement(ldomXPointerEx *ptr) {
            ldomNode *elem = ptr->getNode();
            lUInt16 id = elem->getNodeId();
            if (has_images && (id == el_img || id == el_image))
                *has_images = true;
            if (anchors && id == el_a)
                add_anchor(elem);
            return true;
        }
    };
    PageScanner scanner(has_images, anchors);
    range->forEach(&scanner);
}

static void collect_link_rects(LVDocView *v, const std::vector<ldomNode *> &anchors,
                               std::vector<VersoCreRect> &rects, std::vector<int> &ids) {
    rects.clear();
    ids.clear();
    for (size_t i = 0; i < anchors.size(); ++i) {
        if (!anchors[i]) continue;
        LVArray<lvRect> segments;
        ldomXRange(anchors[i], true).getSegmentRects(segments);
        for (int s = 0; s < segments.length(); ++s) {
            lvPoint a = segments[s].topLeft();
            lvPoint b = segments[s].bottomRight();
            if (v->docToWindowPoint(a) && v->docToWindowPoint(b, true) && b.x > a.x && b.y > a.y) {
                VersoCreRect r;
                r.left = a.x; r.top = a.y; r.right = b.x; r.bottom = b.y;
                rects.push_back(r);
                ids.push_back((int)i);
            }
        }
    }
}

// The links on the current page that point inside the book.
static void current_links(LVDocView *v, std::vector<ldomNode *> &out) {
    scan_page_range(v->getPageDocumentRange(-1), nullptr, &out);
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
        if (page < 0 || page >= v->getPageCount() || mode < 0 || mode > 3) return -1;
        if (v->getCurPage() != page)
            v->goToPage(page, false, true, false);
        LVRef<ldomXRange> visible = v->getPageDocumentRange(page);
        if (visible.isNull()) return 0;
        ldomXRange range;
        if (mode == 0 && !word_at(v, lvPoint(x, y), range)) return 0;
        if (mode == 1 && !extend_at(v, lvPoint(x, y), range)) return 0;
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
        if (mode == 0 || mode == 2) doc->selection_text = removeSoftHyphens(range.getRangeText());
        doc->selection_visible = 0;
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
int verso_cre_annotation_check(VersoCreDoc *doc, const char *start, const char *end) {
    GUARD(-1, {
        doc->checked_start = doc->checked_end = ldomXPointerEx();
        doc->checked_text.clear();
        if (!start || !end) return -1;
        auto *d = doc->view->getDocument();
        ldomXPointerEx a = d->createXPointer(Utf8ToUnicode(start));
        ldomXPointerEx b = d->createXPointer(Utf8ToUnicode(end));
        if (a.isNull() || b.isNull() || !a.isText() || !b.isText()) return -1;
        if (a.getOffset() < 0 || b.getOffset() < 0 || a.getOffset() > a.getNode()->getText().length() || b.getOffset() > b.getNode()->getText().length()) return -1;
        if (a.compare(b) >= 0) return -1;
        doc->checked_text = UnicodeToUtf8(removeSoftHyphens(ldomXRange(a, b).getRangeText()));
        doc->checked_start = a;
        doc->checked_end = b;
        return doc->checked_text.length();
    })
}
int verso_cre_annotation_checked_text(VersoCreDoc *doc, char *buf, int len) {
    GUARD(-1, {
        const lString8 &text = doc->checked_text;
        if (buf && len > 0) {
            int n = std::min(text.length(), len - 1);
            memcpy(buf, text.c_str(), n);
            buf[n] = 0;
        }
        return text.length();
    })
}
// The page an annotation starts on; `last` gets the page a highlight ends on
// (the start page for a bookmark). An end with no place on screen counts as
// running to the last page, so callers never miss a page it may paint.
static int annotation_pages(LVDocView *v, const VersoCreDoc::Annotation &a, int *last) {
    int page = v->getBookmarkPage(a.start);
    if (last) {
        int end = a.painted ? v->getBookmarkPage(a.end) : page;
        if (end == 0 && a.painted && ldomXPointer(a.end).toPoint().y < 0) end = v->getPageCount() - 1;
        *last = std::max(page, end);
    }
    return page;
}
int verso_cre_annotation_keep(VersoCreDoc *doc, int id, int painted, int *last) {
    GUARD(-1, {
        if (doc->checked_start.isNull()) return -1;
        doc->pending_annotations.push_back({id, doc->checked_start, doc->checked_end, painted != 0});
        return annotation_pages(doc->view, doc->pending_annotations.back(), last);
    })
}
int verso_cre_annotation_kept(VersoCreDoc *doc, int index, int *page, int *last) {
    GUARD(-1, {
        if (index < 0 || index >= (int)doc->annotations.size()) return -1;
        const auto &a = doc->annotations[index];
        int first = annotation_pages(doc->view, a, last);
        if (page) *page = first;
        return a.id;
    })
}
int verso_cre_annotation_finish(VersoCreDoc *doc) {
    GUARD(-1, {
        doc->annotations.swap(doc->pending_annotations);
        doc->pending_annotations.clear();
        std::stable_sort(doc->annotations.begin(), doc->annotations.end(), [](const VersoCreDoc::Annotation &a, const VersoCreDoc::Annotation &b) {
            return a.start.compare(b.start) < 0;
        });
        // Prefix maximum of painted ends. An unpainted entry carries the
        // previous maximum (or its own start, which keeps the sequence sorted).
        doc->annotation_max_end.clear();
        for (const auto &a : doc->annotations) {
            auto end = a.painted ? a.end : a.start;
            if (!doc->annotation_max_end.empty() && (!a.painted || doc->annotation_max_end.back().compare(end) > 0)) end = doc->annotation_max_end.back();
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
            if (!a[i].painted || a[i].end.compare(visible->getStart()) <= 0) continue;
            if (ids && count < capacity) ids[count] = a[i].id;
            count++;
        }
        return count;
    })
}
// The first text position at or after `p`: a page start can point at an
// element, and its text begins in the first text node inside or after it.
static ldomXPointerEx first_text(ldomXPointerEx p) {
    if (p.isNull() || p.isText()) return p;
    if (!p.nextText()) return ldomXPointerEx();
    return p;
}
static bool is_space(lChar32 c) {
    return c <= 0x20 || c == 0xA0 || c == 0x1680 || (c >= 0x2000 && c <= 0x200A) || c == 0x2028 || c == 0x2029
        || c == 0x202F || c == 0x205F || c == 0x3000;
}
static lString8 range_text(const ldomXPointerEx &a, const ldomXPointerEx &b) {
    return UnicodeToUtf8(removeSoftHyphens(ldomXRange(a, b).getRangeText()));
}
int verso_cre_page_opening(VersoCreDoc *doc, const char *start, int max_chars) {
    GUARD(-1, {
        LVDocView *v = doc->view;
        doc->opening_start.clear();
        doc->opening_end.clear();
        doc->opening_text.clear();
        ldomXPointer bm;
        int page;
        if (start && *start) {
            // A saved position (an imported bookmark): it must still be a
            // laid-out position in this book, checked as bookmark_check does.
            ldomXPointerEx a = v->getDocument()->createXPointer(Utf8ToUnicode(start));
            if (a.isNull() || a.toPoint().y < 0) return -1;
            if (a.isText() && (a.getOffset() < 0 || a.getOffset() > a.getNode()->getText().length())) return -1;
            bm = a;
            page = v->getBookmarkPage(bm);
            if (page < 0 || page >= v->getPageCount()) return -1;
        } else {
            bm = v->getBookmark();
            page = v->getCurPage();
        }
        doc->opening_start = bm.toString();
        auto visible = v->getPageDocumentRange(page);
        if (bm.isNull() || visible.isNull()) return 0;
        // The range's end can be the first line of the next page (crengine
        // counts a line starting at the page's bottom edge), so the next
        // page's start bounds the page when it comes first.
        ldomXPointerEx page_end = visible->getEnd();
        if (page + 1 < v->getPageCount()) {
            auto next = v->getPageDocumentRange(page + 1);
            if (!next.isNull() && !next->getStart().isNull() && ldomXPointerEx(next->getStart()).compare(page_end) < 0)
                page_end = next->getStart();
        }
        ldomXPointerEx first = first_text(ldomXPointerEx(bm));
        if (first.isNull() || first.compare(page_end) >= 0) return 0;
        // Walk the page's text a character at a time, remembering where the
        // last word ended. A new block counts as a space between words.
        ldomXPointerEx p = first;
        ldomXPointerEx word_end;
        ldomXPointerEx end;
        int count = 0;
        bool seen = false;
        bool in_word = false;
        ldomNode *block = p.getThisBlockNode();
        for (;;) {
            ldomNode *node = p.getNode();
            const lString32 text = node->getText();
            int limit = page_end.getNode() == node ? std::min(page_end.getOffset(), (int)text.length()) : text.length();
            int i = std::max(0, p.getOffset());
            for (; i < limit && count < max_chars; ++i) {
                lChar32 c = text[i];
                if (c == 0xAD) continue;
                ++count;
                if (is_space(c)) {
                    if (in_word) { word_end = p; word_end.setOffset(i); }
                    in_word = false;
                } else {
                    seen = in_word = true;
                }
            }
            end = p;
            end.setOffset(i);
            if (count >= max_chars || i < (int)text.length()) {
                // At the limit or the page end. Unless the next character is a
                // space, the cut may split a word (or a word hyphenated across
                // pages): end on the last word boundary when there is one.
                bool mid_word = in_word && (i >= (int)text.length() || !is_space(text[i]));
                if (mid_word && !word_end.isNull()) end = word_end;
                break;
            }
            ldomXPointerEx next = p;
            if (!next.nextText() || next.compare(page_end) >= 0) break;
            if (next.getThisBlockNode() != block) {
                if (in_word) word_end = end;
                in_word = false;
                ++count;
                block = next.getThisBlockNode();
            }
            p = next;
            if (count >= max_chars) {
                if (!word_end.isNull()) end = word_end;
                break;
            }
        }
        if (!seen || end.isNull() || first.compare(end) >= 0) return 0;
        doc->opening_end = end.toString();
        doc->opening_text = removeSoftHyphens(ldomXRange(first, end).getRangeText());
        return UnicodeToUtf8(doc->opening_text).length();
    })
}
int verso_cre_page_opening_text(VersoCreDoc *doc, int field, char *buf, int len) {
    GUARD(-1, {
        if (field == 0) return copy_utf8(doc->opening_start, buf, len);
        if (field == 1) return copy_utf8(doc->opening_end, buf, len);
        if (field == 2) return copy_utf8(doc->opening_text, buf, len);
        return -1;
    })
}
int verso_cre_bookmark_check(VersoCreDoc *doc, const char *start, const char *end) {
    GUARD(-1, {
        doc->checked_start = doc->checked_end = ldomXPointerEx();
        doc->checked_text.clear();
        if (!start || !*start || !end) return -1;
        auto *d = doc->view->getDocument();
        ldomXPointerEx a = d->createXPointer(Utf8ToUnicode(start));
        if (a.isNull() || a.toPoint().y < 0) return -1;
        if (a.isText() && (a.getOffset() < 0 || a.getOffset() > a.getNode()->getText().length())) return -1;
        ldomXPointerEx b = a;
        if (*end) {
            b = d->createXPointer(Utf8ToUnicode(end));
            if (b.isNull() || !b.isText() || b.getOffset() < 0 || b.getOffset() > b.getNode()->getText().length()) return -1;
            ldomXPointerEx text = first_text(a);
            if (text.isNull() || text.compare(b) >= 0) return -1;
            doc->checked_text = range_text(text, b);
        }
        doc->checked_start = a;
        doc->checked_end = b;
        return doc->checked_text.length();
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
        verso_max_image_pixels = width * height; // caps SVGs without a target size
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
        verso_max_image_pixels = width * height;
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

static void cre32_to_rgba_row(const lUInt32 *src, uint8_t *dst, int width) {
    for (int x = 0; x < width; x++) {
        lUInt32 px = src[x];
        uint8_t *p = dst + (size_t)x * 4;
        p[0] = (uint8_t)((px >> 16) & 0xFF);
        p[1] = (uint8_t)((px >> 8) & 0xFF);
        p[2] = (uint8_t)(px & 0xFF);
        p[3] = 0xFF;
    }
}

int verso_cre_draw_rgba(VersoCreDoc *doc, uint8_t *buf, int width, int height, int stride) {
    GUARD(-1, {
        if (!buf || width <= 0 || height <= 0 || stride < width * 4)
            return -1;
        doc->view->Resize(width, height);
        doc->view->Render();
        if (stride == width * 4 && (((uintptr_t)buf) & 3) == 0) {
            LVColorDrawBuf draw(width, height, buf, 32);
            doc->view->Draw(draw, false);
            for (int y = 0; y < height; y++) {
                uint8_t *row = buf + (size_t)y * stride;
                cre32_to_rgba_row((const lUInt32 *)row, row, width);
            }
        } else {
            LVColorDrawBuf draw(width, height, 32);
            doc->view->Draw(draw, false);
            for (int y = 0; y < height; y++) {
                const lUInt32 *src = (const lUInt32 *)draw.GetScanLine(y);
                cre32_to_rgba_row(src, buf + (size_t)y * stride, width);
            }
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
int verso_cre_page_position(VersoCreDoc *doc, char *buf, int len) {
    GUARD(-1, { return copy_lstring8(doc->page_position, buf, len); })
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
int verso_cre_page_info(VersoCreDoc *doc, const char *skip_pos, char *pos_buf, int pos_len,
                        VersoCrePageInfo *out) {
    GUARD(-1, {
        if (!doc || !out) return -1;
        LVDocView *v = doc->view;
        int page = v->getCurPage();
        ldomXPointer bm = v->getBookmark();
        doc->page_position = UnicodeToUtf8(bm.toString());
        out->page = page;
        out->position_len = copy_lstring8(doc->page_position, pos_buf, pos_len);
        if (page == 0) {
            out->progress = 0;
            out->text_offset = 0;
        } else if (skip_pos && doc->offsets.empty()
                   && strcmp(doc->page_position.c_str(), skip_pos) == 0) {
            out->progress = -1;
            out->text_offset = -1;
        } else {
            long long off = text_offset(doc, bm);
            out->text_offset = off;
            out->progress = (off >= 0 && doc->text_length > 0)
                ? std::min(10000, (int)(off * 10000 / (long long)doc->text_length))
                : 0;
        }
        bool has_images = false;
        std::vector<ldomNode *> anchors;
        scan_page_range(v->getPageDocumentRange(-1), &has_images, &anchors);
        out->has_images = has_images ? 1 : 0;
        collect_link_rects(v, anchors, doc->link_rects, doc->link_ids);
        out->link_rects = (int)doc->link_rects.size();
        return 0;
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
struct CachedFont {
    lString8 name;
    lString8 file;
    bool has_file;
};
static std::vector<CachedFont> cached_fonts;

int verso_cre_font_count(void) {
    GUARD(0, {
        cached_fonts.clear();
        lString32Collection faces;
        fontMan->getFaceList(faces);
        cached_fonts.reserve(faces.length());
        for (int i = 0; i < faces.length(); ++i) {
            CachedFont entry;
            entry.name = UnicodeToUtf8(faces[i]);
            lString8 file;
            int face_index;
            int family;
            bool math;
            bool emojis;
            entry.has_file = fontMan->getFontFileNameAndFaceIndex(
                faces[i], false, false, file, face_index, family, math, emojis);
            if (entry.has_file) entry.file = file;
            cached_fonts.push_back(std::move(entry));
        }
        return (int)cached_fonts.size();
    })
}
int verso_cre_font_name(int index, char *buf, int len) {
    GUARD(-1, {
        if (index < 0 || index >= (int)cached_fonts.size()) return -1;
        return copy_lstring8(cached_fonts[index].name, buf, len);
    })
}

int verso_cre_font_file(int index, char *buf, int len) {
    GUARD(-1, {
        if (index < 0 || index >= (int)cached_fonts.size() || !cached_fonts[index].has_file) return -1;
        return copy_lstring8(cached_fonts[index].file, buf, len);
    })
}

void verso_cre_fonts_clear(void) {
    try {
        cached_fonts.clear();
        cached_fonts.shrink_to_fit();
    } catch (...) {
    }
}
int verso_cre_page_links(VersoCreDoc *doc, int page, VersoCreRect *rects, int *links, int capacity) {
    GUARD(-1, {
        LVDocView *v = doc->view;
        if (!rects) {
            doc->link_rects.clear();
            doc->link_ids.clear();
            if (page < 0 || page >= v->getPageCount()) return -1;
            if (v->getCurPage() != page)
                v->goToPage(page, false, true, false);
            std::vector<ldomNode *> anchors;
            current_links(v, anchors);
            collect_link_rects(v, anchors, doc->link_rects, doc->link_ids);
        } else {
            int n = std::min((int)doc->link_rects.size(), capacity);
            std::copy_n(doc->link_rects.begin(), n, rects);
            if (links) std::copy_n(doc->link_ids.begin(), n, links);
        }
        return (int)doc->link_rects.size();
    })
}

int verso_cre_link(VersoCreDoc *doc, int page, int link) {
    GUARD(-1, {
        doc->link_target.clear(); doc->link_label.clear(); doc->link_note.clear(); doc->link_rule.clear();
        LVDocView *v = doc->view;
        if (page < 0 || page >= v->getPageCount()) return -1;
        if (v->getCurPage() != page)
            v->goToPage(page, false, true, false);
        std::vector<ldomNode *> anchors;
        current_links(v, anchors);
        if (link < 0 || link >= (int)anchors.size()) return -1;
        ldomNode *a = anchors[link];
        if (!a) return 0;
        ldomNode *t = link_target(a);
        if (!t) return 0;
        doc->link_target = ldomXPointer(t, 0).toString();
        lString32 label = bare(a->getText());
        doc->link_label = label;
        ldomNode *note = note_block(t);
        const char *why = "";
        int kind = classify(a, t, note, label, why);
        doc->link_rule = why;
        if (kind == 1) write_note(note, a, label, doc->link_note, false, false);
        return kind;
    })
}

int verso_cre_link_text(VersoCreDoc *doc, int field, char *buf, int len) {
    GUARD(-1, {
        if (field == 0) return copy_utf8(doc->link_target, buf, len);
        if (field == 2) return copy_utf8(doc->link_label, buf, len);
        const lString8 &text = field == 1 ? doc->link_note : doc->link_rule;
        if (field != 1 && field != 3) return -1;
        if (buf && len > 0) {
            int n = std::min(text.length(), len - 1);
            memcpy(buf, text.c_str(), n);
            buf[n] = 0;
        }
        return text.length();
    })
}
} // extern "C"
