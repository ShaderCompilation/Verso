//! Renders one page of a book to a PGM image, to test the engine without the app.
//!
//!   cargo run -p crengine-sys --example render_page -- BOOK.epub OUT.pgm [PAGE] [FONT_FILE...]
//!
//! Prints load, layout and draw times, and peak memory.

use std::ffi::CString;
use std::time::Instant;

use crengine_sys::*;

fn c(s: &str) -> CString {
    CString::new(s).unwrap()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (book, out) = (&args[0], &args[1]);
    let page: i32 = args.get(2).and_then(|p| p.parse().ok()).unwrap_or(5);
    let fonts = &args[3.min(args.len())..];
    // crengine's CSS and hyphenation files; override with VERSO_CRE_DATA on the device.
    let data = std::env::var("VERSO_CRE_DATA")
        .unwrap_or_else(|_| concat!(env!("CARGO_MANIFEST_DIR"), "/../../third_party/crengine/cr3gui/data").into());
    let (w, h, dpi) = (1272, 1696, 300);
    unsafe {
        assert_eq!(verso_cre_init(c(&format!("{data}/hyph")).as_ptr(), std::ptr::null(), 0), 0);
        for f in fonts {
            assert_eq!(verso_cre_register_font(c(f).as_ptr()), 0, "font {f}");
        }
        let doc = verso_cre_new(w, h, dpi);
        assert!(!doc.is_null());
        verso_cre_set_stylesheet_file(doc, c(&format!("{data}/epub.css")).as_ptr());
        verso_cre_set_font_size(doc, 42);
        verso_cre_set_margins(doc, 90, 90, 90, 90);
        let t = Instant::now();
        assert_eq!(verso_cre_load(doc, c(book).as_ptr()), 0, "load failed");
        let t_load = t.elapsed();
        let t = Instant::now();
        verso_cre_render(doc);
        let t_render = t.elapsed();
        let mut lang = [0 as std::os::raw::c_char; 64];
        verso_cre_get_prop(doc, c("language").as_ptr(), lang.as_mut_ptr(), 64);
        let lang = std::ffi::CStr::from_ptr(lang.as_ptr()).to_string_lossy();
        println!("pages: {}, lang: {lang}, load {t_load:?}, layout {t_render:?}", verso_cre_page_count(doc));
        verso_cre_go_to_page(doc, page);
        let mut buf = vec![0u8; (w * h) as usize];
        let t = Instant::now();
        assert_eq!(verso_cre_draw(doc, buf.as_mut_ptr(), w, h, w), 0);
        println!("draw page {page}: {:?}", t.elapsed());
        let mut pgm = format!("P5\n{w} {h}\n255\n").into_bytes();
        pgm.extend_from_slice(&buf);
        std::fs::write(out, pgm).unwrap();
        verso_cre_free(doc);
    }
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        for l in status.lines().filter(|l| l.starts_with("VmHWM") || l.starts_with("VmRSS")) {
            println!("{l}");
        }
    }
}
