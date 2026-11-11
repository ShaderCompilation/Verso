// C interface over crengine for Verso. See verso_cre.h.
// Reference for how crengine is driven: KOReader's cre.cpp (koreader-base).

#include "verso_cre.h"

#include <string.h>
#include <vector>
#include <unordered_map>
#include <algorithm>

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

};

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

extern "C" {

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
        if (cache_dir && cache_max_bytes > 0)
            ldomDocCache::init(Utf8ToUnicode(cache_dir), (lvsize_t)cache_max_bytes);
        return 0;
    })
}

int verso_cre_register_font(const char *path) {
    GUARD(-1, { return fontMan->RegisterFont(lString8(path)) ? 0 : -1; })
}

VersoCreDoc *verso_cre_new(int width, int height, int dpi) {
    GUARD(nullptr, {
        LVDocView *v = new LVDocView(-1, true);
        v->setViewMode(DVM_PAGES, 1);
        v->setPageHeaderInfo(0); // no crengine status bar; Verso draws its own
        CRPropRef props = LVCreatePropsContainer();
        props->setInt(PROP_RENDER_DPI, dpi);      // CSS units scale with the screen
        props->setInt(PROP_FONT_KERNING, 3);      // HarfBuzz shaping (ligatures, kerning)
        props->setInt(PROP_FONT_HINTING, 2);      // autohinter (as KOReader)
        props->setInt(PROP_EMBEDDED_STYLES, 1);
        props->setInt(PROP_EMBEDDED_FONTS, 1);
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
int verso_cre_progress(VersoCreDoc *doc) {
    GUARD(0, {
        if (doc->offsets.empty()) collect_offsets(doc->view->getDocument()->getRootNode(), doc);
        ldomXPointer pos = doc->view->getBookmark();
        if (pos.isNull() || !doc->text_length) return 0;
        ldomNode *node = pos.getNode();
        while (node && !node->isText() && node->getChildCount()) node = node->getChildNode(0);
        if (!node) return 0;
        auto it = doc->offsets.find(node->getDataIndex());
        if (it == doc->offsets.end()) return 0;
        uint64_t offset = it->second + std::max(0, pos.getOffset());
        return std::min(10000, (int)(offset * 10000 / doc->text_length));
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
