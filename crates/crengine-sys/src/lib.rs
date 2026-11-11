//! Raw bindings to Verso's C interface over crengine (`shim/verso_cre.h`).
//! Hand-written: the interface is small. See that header for what each
//! function does. Use crengine from one thread at a time.

use std::os::raw::{c_char, c_int};

#[repr(C)]
pub struct VersoCreDoc {
    _private: [u8; 0],
}

unsafe extern "C" {
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
