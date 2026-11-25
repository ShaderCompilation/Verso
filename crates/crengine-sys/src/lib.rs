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

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct VersoCrePageInfo {
    pub page: c_int,
    pub progress: c_int,
    pub text_offset: std::os::raw::c_longlong,
    pub has_images: c_int,
    pub position_len: c_int,
    pub link_rects: c_int,
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
    pub fn verso_cre_annotation_clear(doc: *mut VersoCreDoc) -> c_int;
    pub fn verso_cre_annotation_check(doc: *mut VersoCreDoc, start: *const c_char, end: *const c_char) -> c_int;
    pub fn verso_cre_annotation_checked_text(doc: *mut VersoCreDoc, buf: *mut c_char, len: c_int) -> c_int;
    pub fn verso_cre_annotation_keep(doc: *mut VersoCreDoc, id: c_int, painted: c_int, last: *mut c_int) -> c_int;
    pub fn verso_cre_annotation_kept(doc: *mut VersoCreDoc, index: c_int, page: *mut c_int, last: *mut c_int) -> c_int;
    pub fn verso_cre_annotation_finish(doc: *mut VersoCreDoc) -> c_int;
    pub fn verso_cre_annotation_order(doc: *mut VersoCreDoc, index: c_int) -> c_int;
    pub fn verso_cre_annotation_page(doc: *mut VersoCreDoc, page: c_int, ids: *mut c_int, capacity: c_int) -> c_int;
    pub fn verso_cre_page_opening(doc: *mut VersoCreDoc, start: *const c_char, max_chars: c_int) -> c_int;
    pub fn verso_cre_page_opening_text(doc: *mut VersoCreDoc, field: c_int, buf: *mut c_char, len: c_int) -> c_int;
    pub fn verso_cre_bookmark_check(doc: *mut VersoCreDoc, start: *const c_char, end: *const c_char) -> c_int;
    pub fn verso_cre_selection_text(doc: *mut VersoCreDoc, field: c_int, buf: *mut c_char, len: c_int) -> c_int;
    pub fn verso_cre_page_links(
        doc: *mut VersoCreDoc,
        page: c_int,
        rects: *mut VersoCreRect,
        links: *mut c_int,
        capacity: c_int,
    ) -> c_int;
    pub fn verso_cre_link(doc: *mut VersoCreDoc, page: c_int, link: c_int) -> c_int;
    pub fn verso_cre_link_text(doc: *mut VersoCreDoc, field: c_int, buf: *mut c_char, len: c_int) -> c_int;
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
    pub fn verso_cre_set_logger(logger: extern "C" fn(*const c_char));
    pub fn verso_cre_init(hyph_dir: *const c_char, cache_dir: *const c_char, cache_max_bytes: c_int) -> c_int;
    pub fn verso_cre_cache_reload() -> c_int;
    pub fn verso_cre_register_font(path: *const c_char) -> c_int;
    pub fn verso_cre_describe_font(path: *const c_char, buf: *mut c_char, len: c_int) -> c_int;
    pub fn verso_cre_register_font_faces(path: *const c_char, description: *const c_char) -> c_int;
    pub fn verso_cre_font_list_hash() -> std::os::raw::c_uint;

    pub fn verso_cre_new(width: c_int, height: c_int, dpi: c_int) -> *mut VersoCreDoc;
    pub fn verso_cre_set_shaping(doc: *mut VersoCreDoc, mode: c_int);
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
    pub fn verso_cre_draw_rgba(
        doc: *mut VersoCreDoc,
        buf: *mut u8,
        width: c_int,
        height: c_int,
        stride: c_int,
    ) -> c_int;
    pub fn verso_cre_get_prop(doc: *mut VersoCreDoc, key: *const c_char, buf: *mut c_char, buf_len: c_int) -> c_int;
}

unsafe extern "C" {
    pub fn verso_cre_position(doc: *mut VersoCreDoc, buf: *mut c_char, len: c_int) -> c_int;
    pub fn verso_cre_page_info(
        doc: *mut VersoCreDoc,
        skip_pos: *const c_char,
        pos_buf: *mut c_char,
        pos_len: c_int,
        out: *mut VersoCrePageInfo,
    ) -> c_int;
    pub fn verso_cre_page_position(doc: *mut VersoCreDoc, buf: *mut c_char, len: c_int) -> c_int;
    pub fn verso_cre_restore(doc: *mut VersoCreDoc, position: *const c_char) -> c_int;
    pub fn verso_cre_progress(doc: *mut VersoCreDoc) -> c_int;
    pub fn verso_cre_text_offset(doc: *mut VersoCreDoc, position: *const c_char) -> std::os::raw::c_longlong;
    pub fn verso_cre_text_length(doc: *mut VersoCreDoc) -> std::os::raw::c_longlong;
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
    pub fn verso_cre_fonts_clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;
    use std::sync::{Mutex, Once};

    static INIT: Once = Once::new();
    static CRE_LOCK: Mutex<()> = Mutex::new(());

    fn init_cre() {
        INIT.call_once(|| unsafe {
            assert_eq!(verso_cre_init(std::ptr::null(), std::ptr::null(), 0), 0);
            let font = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts/Literata-Regular.ttf");
            let cfont = CString::new(font.to_str().unwrap()).unwrap();
            assert_eq!(verso_cre_register_font(cfont.as_ptr()), 0);
        });
    }

    fn base64(data: &[u8]) -> String {
        const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
        for chunk in data.chunks(3) {
            let b0 = chunk[0] as u32;
            let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
            let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
            let n = (b0 << 16) | (b1 << 8) | b2;
            out.push(T[((n >> 18) & 63) as usize] as char);
            out.push(T[((n >> 12) & 63) as usize] as char);
            out.push(if chunk.len() > 1 { T[((n >> 6) & 63) as usize] as char } else { '=' });
            out.push(if chunk.len() > 2 { T[(n & 63) as usize] as char } else { '=' });
        }
        out
    }

    fn png_crc32(data: &[u8]) -> u32 {
        let mut crc: u32 = 0xffff_ffff;
        for &b in data {
            crc ^= b as u32;
            for _ in 0..8 {
                crc = if crc & 1 != 0 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
            }
        }
        !crc
    }

    fn png_chunk(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(12 + payload.len());
        out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(payload);
        let crc = png_crc32(&out[4..]);
        out.extend_from_slice(&crc.to_be_bytes());
        out
    }

    /// PNG with a valid IHDR (so libpng reaches row/image allocation) and a
    /// corrupt IDAT payload (so libpng longjmps during decode).
    fn corrupt_png(width: u32, height: u32, interlace: u8) -> Vec<u8> {
        let mut ihdr = Vec::with_capacity(13);
        ihdr.extend_from_slice(&width.to_be_bytes());
        ihdr.extend_from_slice(&height.to_be_bytes());
        ihdr.extend_from_slice(&[8, 2, 0, 0, interlace]); // 8-bit RGB
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend_from_slice(&png_chunk(b"IHDR", &ihdr));
        png.extend_from_slice(&png_chunk(b"IDAT", b"\x78\x9c\xff\xff\xff\xff"));
        png.extend_from_slice(&png_chunk(b"IEND", &[]));
        png
    }

    /// Minimal valid 1x1 RGB PNG (stored zlib block, single white pixel).
    fn valid_png_1x1() -> Vec<u8> {
        let mut ihdr = Vec::with_capacity(13);
        ihdr.extend_from_slice(&1u32.to_be_bytes());
        ihdr.extend_from_slice(&1u32.to_be_bytes());
        ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
        // zlib stream for 4 raw bytes: [0x00 (filter), 0xff, 0xff, 0xff].
        // adler32([0, 255, 255, 255]) = s1=766 (0x02fe), s2=1534 (0x05fe).
        let zlib = [0x78, 0x01, 0x01, 0x04, 0x00, 0xfb, 0xff, 0x00, 0xff, 0xff, 0xff, 0x05, 0xfe, 0x02, 0xfe];
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend_from_slice(&png_chunk(b"IHDR", &ihdr));
        png.extend_from_slice(&png_chunk(b"IDAT", &zlib));
        png.extend_from_slice(&png_chunk(b"IEND", &[]));
        png
    }

    /// Corrupt JPEG: valid SOI + APP0, then an invalid marker that forces
    /// libjpeg's error_exit -> longjmp.
    fn corrupt_jpeg() -> Vec<u8> {
        vec![
            0xff, 0xd8, // SOI
            0xff, 0xc0, 0x00, 0x0b, 0x08, 0x00, 0x10, 0x00, 0x10, 0x01, 0x01, 0x11, 0x00, // SOF0 16x16
            0xff, 0xda, 0x00, 0x08, 0x01, 0x01, 0x00, 0x00, 0x3f, 0x00, // SOS (missing DHT -> longjmp)
            0x00, 0xff, 0xd9,
        ]
    }

    #[test]
    fn register_font_faces_rejects_out_of_range_css_family() {
        let _guard = CRE_LOCK.lock().unwrap();
        init_cre();
        let path = CString::new("/nonexistent/test-font.ttf").unwrap();
        let line = |family: i64, name: &str| {
            CString::new(format!("0\t0\t{family}\t0\t0\t0\t0\t0\t0\t0\t0\t0\t0\t0\t0\t0\t0\t0\t0\t0\t0\t{name}\n"))
                .unwrap()
        };

        for bad in [-1, 9, 999_999] {
            let desc = line(bad, "BadFamily");
            let rc = unsafe { verso_cre_register_font_faces(path.as_ptr(), desc.as_ptr()) };
            assert_eq!(rc, -1, "expected -1 for out-of-range css_font_family_t={bad}");
        }

        for (good, name) in [(0, "ValidInheritFamily"), (8, "ValidFangsongFamily")] {
            let desc = line(good, name);
            let rc = unsafe { verso_cre_register_font_faces(path.as_ptr(), desc.as_ptr()) };
            assert!(rc >= 0, "expected valid css_font_family_t={good} to be accepted, got {rc}");
        }
    }

    #[test]
    fn corrupt_and_extreme_images_do_not_crash() {
        let _guard = CRE_LOCK.lock().unwrap();
        init_cre();

        let svg_zero = br#"<svg xmlns="http://www.w3.org/2000/svg" width="0" height="0"/>"#;
        let svg_huge = br##"<svg xmlns="http://www.w3.org/2000/svg" width="4000" height="4000"><rect width="4000" height="4000" fill="#333"/></svg>"##;

        let fb2 = format!(
            concat!(
                r#"<?xml version="1.0" encoding="utf-8"?>"#,
                r#"<FictionBook xmlns="http://www.gribuser.ru/xml/fictionbook/2.0" xmlns:l="http://www.w3.org/1999/xlink">"#,
                r#"<body><section>"#,
                r##"<image l:href="#bad.jpg"/>"##,
                r##"<image l:href="#bad.png"/>"##,
                r##"<image l:href="#bad_interlaced.png"/>"##,
                r##"<image l:href="#good.png"/>"##,
                r##"<image l:href="#zero.svg"/>"##,
                r##"<image l:href="#huge.svg"/>"##,
                r#"</section></body>"#,
                r#"<binary id="bad.jpg" content-type="image/jpeg">{}</binary>"#,
                r#"<binary id="bad.png" content-type="image/png">{}</binary>"#,
                r#"<binary id="bad_interlaced.png" content-type="image/png">{}</binary>"#,
                r#"<binary id="good.png" content-type="image/png">{}</binary>"#,
                r#"<binary id="zero.svg" content-type="image/svg+xml">{}</binary>"#,
                r#"<binary id="huge.svg" content-type="image/svg+xml">{}</binary>"#,
                r#"</FictionBook>"#,
            ),
            base64(&corrupt_jpeg()),
            base64(&corrupt_png(300, 300, 0)),
            base64(&corrupt_png(300, 300, 1)),
            base64(&valid_png_1x1()),
            base64(svg_zero),
            base64(svg_huge),
        );

        let path = std::env::temp_dir().join(format!("verso-cre-img-test-{}.fb2", std::process::id()));
        std::fs::write(&path, fb2).unwrap();
        let cpath = CString::new(path.to_str().unwrap()).unwrap();

        unsafe {
            let doc = verso_cre_new(200, 300, 96);
            assert!(!doc.is_null());
            assert_eq!(verso_cre_load(doc, cpath.as_ptr()), 0);
            verso_cre_render(doc);

            let pages = verso_cre_page_count(doc);
            assert!(pages >= 1);
            let mut gray = vec![0u8; 200 * 300];
            let mut rgba = vec![0u8; 200 * 300 * 4];
            for p in 0..pages {
                assert_eq!(verso_cre_go_to_page(doc, p), 0);
                assert_eq!(verso_cre_draw(doc, gray.as_mut_ptr(), 200, 300, 200), 0);
                assert_eq!(verso_cre_draw_rgba(doc, rgba.as_mut_ptr(), 200, 300, 200 * 4), 0);
            }
            verso_cre_free(doc);
        }

        let _ = std::fs::remove_file(&path);
    }
}
