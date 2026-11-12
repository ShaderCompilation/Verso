//! Raw bindings to Verso's C interface over crengine (`shim/verso_cre.h`).
//! Hand-written: the interface is small. See that header for what each
//! function does. Use crengine from one thread at a time.

use std::os::raw::{c_char, c_int};

#[repr(C)]
pub struct VersoCreDoc {
    _private: [u8; 0],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct VersoCreRect {
    pub left: c_int,
    pub top: c_int,
    pub right: c_int,
    pub bottom: c_int,
}

unsafe extern "C" {
    pub fn verso_cre_select(
        doc: *mut VersoCreDoc,
        page: c_int,
        mode: c_int,
        x: c_int,
        y: c_int,
        start: *const c_char,
        end: *const c_char,
    ) -> c_int;
    pub fn verso_cre_selection_text(doc: *mut VersoCreDoc, field: c_int, buf: *mut c_char, len: c_int) -> c_int;
    pub fn verso_cre_selection_rects(doc: *mut VersoCreDoc, rects: *mut VersoCreRect, capacity: c_int) -> c_int;
    pub fn verso_cre_search_start(doc: *mut VersoCreDoc, query: *const c_char) -> c_int;
    pub fn verso_cre_search_step(doc: *mut VersoCreDoc, max_ms: c_int) -> c_int;
    pub fn verso_cre_search_state(doc: *mut VersoCreDoc, searched_pages: *mut c_int) -> c_int;
    pub fn verso_cre_search_hit(
        doc: *mut VersoCreDoc,
        index: c_int,
        field: c_int,
        buf: *mut c_char,
        len: c_int,
    ) -> c_int;
    pub fn verso_cre_init(hyph_dir: *const c_char, cache_dir: *const c_char, cache_max_bytes: c_int) -> c_int;
    pub fn verso_cre_register_font(path: *const c_char) -> c_int;

    pub fn verso_cre_new(width: c_int, height: c_int, dpi: c_int) -> *mut VersoCreDoc;
    pub fn verso_cre_free(doc: *mut VersoCreDoc);

    pub fn verso_cre_set_stylesheet_file(doc: *mut VersoCreDoc, path: *const c_char) -> c_int;
    pub fn verso_cre_set_font_face(doc: *mut VersoCreDoc, face: *const c_char);
    pub fn verso_cre_set_font_size(doc: *mut VersoCreDoc, px: c_int);
    pub fn verso_cre_get_font_size(doc: *mut VersoCreDoc) -> c_int;
    pub fn verso_cre_set_margins(doc: *mut VersoCreDoc, left: c_int, top: c_int, right: c_int, bottom: c_int);
    pub fn verso_cre_set_line_spacing(doc: *mut VersoCreDoc, percent: c_int);
    pub fn verso_cre_set_size(doc: *mut VersoCreDoc, width: c_int, height: c_int);

    pub fn verso_cre_load(doc: *mut VersoCreDoc, path: *const c_char) -> c_int;
    pub fn verso_cre_render(doc: *mut VersoCreDoc);

    pub fn verso_cre_page_count(doc: *mut VersoCreDoc) -> c_int;
    pub fn verso_cre_current_page(doc: *mut VersoCreDoc) -> c_int;
    pub fn verso_cre_go_to_page(doc: *mut VersoCreDoc, page: c_int) -> c_int;

    pub fn verso_cre_draw(doc: *mut VersoCreDoc, buf: *mut u8, width: c_int, height: c_int, stride: c_int) -> c_int;
    pub fn verso_cre_get_prop(doc: *mut VersoCreDoc, key: *const c_char, buf: *mut c_char, buf_len: c_int) -> c_int;
}

unsafe extern "C" {
    pub fn verso_cre_position(doc: *mut VersoCreDoc, buf: *mut c_char, len: c_int) -> c_int;
    pub fn verso_cre_restore(doc: *mut VersoCreDoc, position: *const c_char) -> c_int;
    pub fn verso_cre_progress(doc: *mut VersoCreDoc) -> c_int;
    pub fn verso_cre_typography(
        doc: *mut VersoCreDoc,
        face: *const c_char,
        px: c_int,
        spacing: c_int,
        margin: c_int,
        top: c_int,
        bottom: c_int,
        justify: c_int,
        publisher: c_int,
    ) -> c_int;
    pub fn verso_cre_toc_count(doc: *mut VersoCreDoc) -> c_int;
    pub fn verso_cre_toc_entry(
        doc: *mut VersoCreDoc,
        index: c_int,
        field: c_int,
        buf: *mut c_char,
        len: c_int,
    ) -> c_int;
    pub fn verso_cre_images(doc: *mut VersoCreDoc) -> c_int;
    pub fn verso_cre_embedded_fonts(doc: *mut VersoCreDoc) -> c_int;
    pub fn verso_cre_font_count() -> c_int;
    pub fn verso_cre_font_file(index: c_int, buf: *mut c_char, len: c_int) -> c_int;
    pub fn verso_cre_font_name(index: c_int, buf: *mut c_char, len: c_int) -> c_int;
}
