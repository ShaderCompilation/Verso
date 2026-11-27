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

/// Text settings for `verso_cre_typography` (see `verso_cre.h`).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VersoCreTypography {
    pub face: *const c_char,
    pub px: c_int,
    pub line_spacing: c_int,
    pub margin: c_int,
    pub top: c_int,
    pub bottom: c_int,
    pub alignment: c_int,
    pub publisher: c_int,
    pub weight: c_int,
    pub gamma_index: c_int,
    pub word_spacing: c_int,
    pub paragraphs: c_int,
    pub hyphenate: c_int,
    pub book_styles: c_int,
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
    pub fn verso_cre_hyphenate(lang: *const c_char, word: *const c_char, buf: *mut c_char, len: c_int) -> c_int;
    pub fn verso_cre_cache_reload() -> c_int;
    pub fn verso_cre_register_font(path: *const c_char) -> c_int;
    pub fn verso_cre_describe_font(path: *const c_char, buf: *mut c_char, len: c_int) -> c_int;
    pub fn verso_cre_register_font_faces(path: *const c_char, description: *const c_char) -> c_int;
    pub fn verso_cre_font_list_hash() -> std::os::raw::c_uint;

    pub fn verso_cre_new(width: c_int, height: c_int, dpi: c_int) -> *mut VersoCreDoc;
    pub fn verso_cre_set_shaping(doc: *mut VersoCreDoc, mode: c_int);
    pub fn verso_cre_free(doc: *mut VersoCreDoc);

    pub fn verso_cre_set_stylesheet_file(doc: *mut VersoCreDoc, path: *const c_char) -> c_int;
    pub fn verso_cre_append_stylesheet(doc: *mut VersoCreDoc, css: *const c_char) -> c_int;
    pub fn verso_cre_set_font_face(doc: *mut VersoCreDoc, face: *const c_char);
    pub fn verso_cre_set_font_size(doc: *mut VersoCreDoc, px: c_int);
    pub fn verso_cre_get_font_size(doc: *mut VersoCreDoc) -> c_int;
    pub fn verso_cre_set_margins(doc: *mut VersoCreDoc, left: c_int, top: c_int, right: c_int, bottom: c_int);
    pub fn verso_cre_set_line_spacing(doc: *mut VersoCreDoc, percent: c_int);
    pub fn verso_cre_set_size(doc: *mut VersoCreDoc, width: c_int, height: c_int);
    pub fn verso_cre_set_hanging_punctuation(doc: *mut VersoCreDoc, on: c_int);

    pub fn verso_cre_load(doc: *mut VersoCreDoc, path: *const c_char) -> c_int;
    pub fn verso_cre_load_html(
        doc: *mut VersoCreDoc,
        html: *const c_char,
        html_len: c_int,
        base_dir: *const c_char,
        file_name: *const c_char,
        lang: *const c_char,
    ) -> c_int;
    pub fn verso_cre_render(doc: *mut VersoCreDoc);

    pub fn verso_cre_page_count(doc: *mut VersoCreDoc) -> c_int;
    pub fn verso_cre_current_page(doc: *mut VersoCreDoc) -> c_int;
    pub fn verso_cre_go_to_page(doc: *mut VersoCreDoc, page: c_int) -> c_int;

    pub fn verso_cre_draw(
        doc: *mut VersoCreDoc,
        buf: *mut u8,
        width: c_int,
        height: c_int,
        stride: c_int,
        dark: c_int,
    ) -> c_int;
    pub fn verso_cre_draw_rgba(
        doc: *mut VersoCreDoc,
        buf: *mut u8,
        width: c_int,
        height: c_int,
        stride: c_int,
        dark: c_int,
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
    pub fn verso_cre_typography(doc: *mut VersoCreDoc, t: *const VersoCreTypography) -> c_int;
    pub fn verso_cre_gamma(gamma_index: c_int) -> c_int;
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
            // The patterns folder as the app passes it: no trailing slash.
            let hyph =
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../third_party/crengine/cr3gui/data/hyph");
            let hyph = CString::new(hyph.to_str().unwrap()).unwrap();
            assert_eq!(verso_cre_init(hyph.as_ptr(), std::ptr::null(), 0), 0);
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
    fn hyphenation_uses_each_languages_patterns() {
        let _guard = CRE_LOCK.lock().unwrap();
        init_cre();
        let hyphenate = |lang: &str, word: &str| {
            let (lang, word) = (CString::new(lang).unwrap(), CString::new(word).unwrap());
            let mut buf = [0u8; 64];
            let n = unsafe { verso_cre_hyphenate(lang.as_ptr(), word.as_ptr(), buf.as_mut_ptr().cast(), 64) };
            String::from_utf8(buf[..n as usize].to_vec()).unwrap()
        };
        // Guessed (algorithmic) breaks give "lig-htho-u-se".
        assert_eq!(hyphenate("en", "lighthouse"), "light-house");
        assert_eq!(hyphenate("en", "responsibilities"), "re-spon-si-bil-i-ties");
        assert_eq!(hyphenate("ru", "достопримечательность"), "до-сто-при-ме-ча-тель-ность");
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
                assert_eq!(verso_cre_draw(doc, gray.as_mut_ptr(), 200, 300, 200, 0), 0);
                assert_eq!(verso_cre_draw_rgba(doc, rgba.as_mut_ptr(), 200, 300, 200 * 4, 0), 0);
            }
            verso_cre_free(doc);
        }

        let _ = std::fs::remove_file(&path);
    }

    fn valid_rgb_png(width: u32, height: u32, rgb: [u8; 3]) -> Vec<u8> {
        let mut row = Vec::with_capacity(1 + width as usize * 3);
        row.push(0); // filter: None
        for _ in 0..width {
            row.extend_from_slice(&rgb);
        }
        let mut raw = Vec::with_capacity(row.len() * height as usize);
        for _ in 0..height {
            raw.extend_from_slice(&row);
        }
        assert!(raw.len() <= 65535);
        let len = raw.len() as u16;
        let mut zlib = vec![0x78, 0x01, 0x01];
        zlib.extend_from_slice(&len.to_le_bytes());
        zlib.extend_from_slice(&(!len).to_le_bytes());
        zlib.extend_from_slice(&raw);
        let mut s1 = 1u32;
        let mut s2 = 0u32;
        for &b in &raw {
            s1 = (s1 + b as u32) % 65521;
            s2 = (s2 + s1) % 65521;
        }
        zlib.extend_from_slice(&((s2 << 16) | s1).to_be_bytes());
        let mut ihdr = Vec::with_capacity(13);
        ihdr.extend_from_slice(&width.to_be_bytes());
        ihdr.extend_from_slice(&height.to_be_bytes());
        ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend_from_slice(&png_chunk(b"IHDR", &ihdr));
        png.extend_from_slice(&png_chunk(b"IDAT", &zlib));
        png.extend_from_slice(&png_chunk(b"IEND", &[]));
        png
    }

    #[test]
    fn dark_mode_preserves_images_and_flips_colored_text_luma() {
        let _guard = CRE_LOCK.lock().unwrap();
        init_cre();

        let dir = std::env::temp_dir().join(format!("verso-cre-dark-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let swatch = [30u8, 150, 210];
        std::fs::write(dir.join("swatch.png"), valid_rgb_png(40, 40, swatch)).unwrap();

        let html = concat!(
            r#"<?xml version="1.0" encoding="utf-8"?>"#,
            r#"<html xmlns="http://www.w3.org/1999/xhtml"><head><style>"#,
            r#"html, body { margin: 0; padding: 0; background: #ffffff; }"#,
            r#"div { margin: 0; padding: 0; font-size: 14px; line-height: 16px; }"#,
            r#"</style></head><body>"#,
            r##"<div style="background-color:#231f20;color:#231f20">a</div>"##,
            r##"<div style="background-color:#003388;color:#003388">a</div>"##,
            r##"<div style="background-color:#990000;color:#990000">a</div>"##,
            r##"<div style="background-color:#FFAA00;color:#FFAA00">a</div>"##,
            r##"<div style="background-color:#000001;color:#000001">a</div>"##,
            r##"<div style="background-color:#FEFFFF;color:#FEFFFF">a</div>"##,
            r##"<div style="background-color:#808080;color:#808080">a</div>"##,
            r#"<div style="background-color:rgba(0,0,1,0.25);color:transparent">a</div>"#,
            r##"<div style="background-color:#2060a0"><div style="background-color:rgba(180,40,20,0.0);color:transparent">a</div></div>"##,
            r#"<div><img src="swatch.png" width="40" height="40"/></div>"#,
            r#"</body></html>"#,
        );

        let css =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../third_party/crengine/cr3gui/data/epub.css");
        let ccss = CString::new(css.to_str().unwrap()).unwrap();
        let cdir = CString::new(dir.to_str().unwrap()).unwrap();
        let cname = CString::new("dark.xhtml").unwrap();
        let clang = CString::new("en").unwrap();

        let luma = |r: u8, g: u8, b: u8| (77 * r as i32 + 150 * g as i32 + 29 * b as i32 + 128) >> 8;
        unsafe {
            let (w, h) = (240, 800);
            let doc = verso_cre_new(w, h, 96);
            assert!(!doc.is_null());
            assert_eq!(verso_cre_set_stylesheet_file(doc, ccss.as_ptr()), 0);
            verso_cre_set_margins(doc, 0, 0, 0, 0);
            assert_eq!(
                verso_cre_load_html(
                    doc,
                    html.as_ptr().cast(),
                    html.len() as c_int,
                    cdir.as_ptr(),
                    cname.as_ptr(),
                    clang.as_ptr()
                ),
                0
            );
            verso_cre_render(doc);
            assert_eq!(verso_cre_page_count(doc), 1);

            // Test both fast (stride == width / width*4) and fallback (padded stride) drawbuf branches.
            for extra in [0i32, 16] {
                let gstride = w + extra;
                let mut g_light = vec![0u8; (gstride * h) as usize];
                let mut g_dark = vec![0u8; (gstride * h) as usize];
                assert_eq!(verso_cre_draw(doc, g_light.as_mut_ptr(), w, h, gstride, 0), 0);
                assert_eq!(verso_cre_draw(doc, g_dark.as_mut_ptr(), w, h, gstride, 1), 0);

                let cstride = w * 4 + extra * 4;
                let mut c_light = vec![0u8; (cstride * h) as usize];
                let mut c_dark = vec![0u8; (cstride * h) as usize];
                assert_eq!(verso_cre_draw_rgba(doc, c_light.as_mut_ptr(), w, h, cstride, 0), 0);
                assert_eq!(verso_cre_draw_rgba(doc, c_dark.as_mut_ptr(), w, h, cstride, 1), 0);

                let at = |buf: &[u8], x: i32, y: i32, flip: bool| -> [u8; 3] {
                    let i = (y * cstride + x * 4) as usize;
                    assert_eq!(buf[i + 3], 255);
                    let f = if flip { 0xff } else { 0 };
                    [buf[i] ^ f, buf[i + 1] ^ f, buf[i + 2] ^ f]
                };
                let find_rgb = |target: [u8; 3]| -> (i32, i32) {
                    for y in 0..h {
                        for x in 0..w {
                            if at(&c_light, x, y, false) == target {
                                return (x, y);
                            }
                        }
                    }
                    panic!("missing swatch {target:?} in light render");
                };

                // RGB & gray image pixels survive flip identically.
                let (ix, iy) = find_rgb(swatch);
                assert_eq!(at(&c_dark, ix, iy, true), swatch, "color image differs (extra={extra})");
                let gpx_l = g_light[(iy * gstride + ix) as usize];
                let gpx_d = g_dark[(iy * gstride + ix) as usize] ^ 0xff;
                assert_eq!(gpx_d, gpx_l, "gray image differs after flip (extra={extra})");

                // Rich black (#231f20): flips to light on both gray (plain flip) and color (patched).
                let (rx, ry) = find_rgb([0x23, 0x1f, 0x20]);
                assert!(g_dark[(ry * gstride + rx) as usize] ^ 0xff > 210);
                let rich = at(&c_dark, rx, ry, true);
                assert!(luma(rich[0], rich[1], rich[2]) >= 220, "rich black stayed dark: {rich:?}");

                // Colored blocks keep hue ordering and land at luma 255 - Y within rounding.
                let (nx, ny) = find_rgb([0x00, 0x33, 0x88]);
                let navy = at(&c_dark, nx, ny, true);
                assert!(navy[2] > navy[1] && navy[1] > navy[0], "navy hue lost: {navy:?}");
                assert!((luma(navy[0], navy[1], navy[2]) - (255 - luma(0x00, 0x33, 0x88))).abs() <= 2);

                let (kx, ky) = find_rgb([0x99, 0x00, 0x00]);
                let red = at(&c_dark, kx, ky, true);
                assert!(red[0] > red[1] && red[0] > red[2], "red hue lost: {red:?}");
                assert!((luma(red[0], red[1], red[2]) - (255 - luma(0x99, 0x00, 0x00))).abs() <= 2);

                let (gx, gy) = find_rgb([0xFF, 0xAA, 0x00]);
                let gold = at(&c_dark, gx, gy, true);
                assert!(gold[0] > gold[1] && gold[1] > gold[2], "gold hue lost: {gold:?}");
                assert!((luma(gold[0], gold[1], gold[2]) - (255 - luma(0xFF, 0xAA, 0x00))).abs() <= 2);

                // Near-extreme non-grays (#000001 / #FEFFFF) handle Y=0 / Y=255 without div-by-zero.
                let (zx, zy) = find_rgb([0x00, 0x00, 0x01]);
                assert_eq!(at(&c_dark, zx, zy, true), [255, 255, 255]);
                let (fx, fy) = find_rgb([0xFE, 0xFF, 0xFF]);
                assert_eq!(at(&c_dark, fx, fy, true), [0, 0, 0]);

                // Pure gray (#808080) skips pre-inversion -> panel flips 0x80 to 0x7f.
                let (mx, my) = find_rgb([0x80, 0x80, 0x80]);
                assert_eq!(at(&c_dark, mx, my, true), [0x7f, 0x7f, 0x7f]);

                // Semi-transparent rgba(0,0,1,0.25) over white blends with preserved alpha onto paper.
                let (ax, ay) = (0..h)
                    .flat_map(|y| (0..w).map(move |x| (x, y)))
                    .find(|&(x, y)| {
                        let p = at(&c_light, x, y, false);
                        p[0] == p[1] && p[1] == p[2] && (185..=198).contains(&p[0])
                    })
                    .expect("missing semi-transparent swatch");
                let semi = at(&c_dark, ax, ay, true);
                assert!((57..=70).contains(&semi[0]) && semi[0] == semi[1] && semi[1] == semi[2], "semi={semi:?}");

                // Fully transparent child over #2060a0 leaves #2060a0 intact (flipped luma, blue hue).
                let (tx, ty) = find_rgb([0x20, 0x60, 0xa0]);
                let trans = at(&c_dark, tx, ty, true);
                assert!(trans[2] > trans[1] && trans[1] > trans[0], "transparent clobbered parent: {trans:?}");
            }

            verso_cre_free(doc);
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}
