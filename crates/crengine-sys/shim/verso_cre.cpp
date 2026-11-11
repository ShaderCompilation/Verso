// C interface over crengine for Verso. See verso_cre.h.
// Reference for how crengine is driven: KOReader's cre.cpp (koreader-base).

#include "verso_cre.h"

#include <string.h>
#include <vector>

#include "lvdocview.h"
#include "lvdrawbuf.h"
#include "hyphman.h"
#include "textlang.h"

struct VersoCreDoc {
    LVDocView *view;
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
        return new VersoCreDoc{v};
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
        doc->view->setFontSize(px);
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

} // extern "C"
