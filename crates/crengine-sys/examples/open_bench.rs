//! Times one book open the way the app does it: parse, layout, first draw,
//! progress, the next page and close (which writes the cache). Used for
//! docs/archive/book-open-2026-11-17.md.
//!
//!   open_bench CACHE_DIR BOOK.epub [PAGE]
//!
//! Env: VERSO_CRE_DATA (epub.css, hyph), VERSO_FONT_DIRS (colon-separated),
//! VERSO_BENCH_PGM (write the first page as a PGM image), VERSO_CRE_SHAPING
//! (1 FreeType, 2 HarfBuzz light, 3 full HarfBuzz; default 2).

use std::ffi::CString;
use std::time::Instant;

use crengine_sys::*;

fn c(s: &str) -> CString {
    CString::new(s).unwrap()
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (cache, book) = (&args[0], &args[1]);
    let page: i32 = args.get(2).and_then(|p| p.parse().ok()).unwrap_or(0);
    let data = std::env::var("VERSO_CRE_DATA").unwrap_or_else(|_| "/mnt/us/verso/data/cre".into());
    let font_dirs =
        std::env::var("VERSO_FONT_DIRS").unwrap_or_else(|_| "/mnt/us/verso/fonts:/usr/java/lib/fonts".into());
    // Portrait Paperwhite, Literata 10 pt, normal spacing, smallest margins, justified, footer on.
    let (w, h, dpi) = (1272, 1696, 300);
    let mm = |v: f32| (v * dpi as f32 / 25.4).round() as i32;
    let px = (10.0f32 * dpi as f32 / 72.0).round() as i32;
    unsafe {
        assert_eq!(verso_cre_init(c(&format!("{data}/hyph")).as_ptr(), c(cache).as_ptr(), 64 << 20), 0);
        for dir in font_dirs.split(':') {
            for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                let p = e.path();
                let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
                if ext == "ttf" || ext == "otf" {
                    verso_cre_register_font(c(p.to_str().unwrap()).as_ptr());
                }
            }
        }
        let doc = verso_cre_new(w, h, dpi);
        assert!(!doc.is_null());
        if let Some(mode) = std::env::var("VERSO_CRE_SHAPING").ok().and_then(|m| m.parse().ok()) {
            verso_cre_set_shaping(doc, mode);
        }
        verso_cre_set_stylesheet_file(doc, c(&format!("{data}/epub.css")).as_ptr());
        let face = c("Literata");
        verso_cre_set_font_face(doc, face.as_ptr());
        verso_cre_set_font_size(doc, px);
        assert_eq!(verso_cre_typography(doc, face.as_ptr(), px, 100, mm(4.0), mm(6.0), mm(12.0), 1, 0), 0);
        let t = Instant::now();
        assert_eq!(verso_cre_load(doc, c(book).as_ptr()), 0, "load failed");
        let parse = ms(t);
        let t = Instant::now();
        verso_cre_render(doc);
        let layout = ms(t);
        let pages = verso_cre_page_count(doc);
        verso_cre_go_to_page(doc, page.min(pages - 1));
        let mut buf = vec![255u8; (w * h) as usize];
        let t = Instant::now();
        assert_eq!(verso_cre_draw(doc, buf.as_mut_ptr(), w, h, w), 0);
        let draw = ms(t);
        if let Ok(out) = std::env::var("VERSO_BENCH_PGM") {
            let mut pgm = format!("P5\n{w} {h}\n255\n").into_bytes();
            pgm.extend_from_slice(&buf);
            std::fs::write(out, pgm).unwrap();
        }
        let mut pos = vec![0 as std::os::raw::c_char; 4096];
        let t = Instant::now();
        verso_cre_position(doc, pos.as_mut_ptr(), 4096);
        let position = ms(t);
        let t = Instant::now();
        let p1 = verso_cre_progress(doc);
        let progress1 = ms(t);
        let t = Instant::now();
        verso_cre_progress(doc);
        let progress2 = ms(t);
        // The neighbour page, as the app renders next.
        verso_cre_go_to_page(doc, (page + 1).min(pages - 1));
        let t = Instant::now();
        verso_cre_draw(doc, buf.as_mut_ptr(), w, h, w);
        let draw2 = ms(t);
        let t = Instant::now();
        verso_cre_free(doc);
        let close = ms(t);
        println!(
            "pages={pages} parse={parse:.0} layout={layout:.0} draw={draw:.0} position={position:.1} \
             progress_first={progress1:.0} progress_again={progress2:.1} draw_next={draw2:.0} close={close:.0} \
             progress={p1}"
        );
    }
}
