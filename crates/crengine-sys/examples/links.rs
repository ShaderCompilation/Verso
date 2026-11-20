//! Lists every internal link in a book and how Verso treats a tap on it:
//! a footnote shown in a sheet, or a jump. For tuning the footnote rules.
//!
//!   cargo run -p crengine-sys --example links -- BOOK.epub [FONT_FILE...]
//!
//! Prints one line per link: page, kind, the rule that decided, the link
//! text and the start of the note's HTML.

use std::ffi::CString;
use std::os::raw::c_int;

use crengine_sys::*;

fn c(s: &str) -> CString {
    CString::new(s).unwrap()
}

unsafe fn field(doc: *mut VersoCreDoc, f: c_int) -> String {
    let len = unsafe { verso_cre_link_text(doc, f, std::ptr::null_mut(), 0) };
    let mut buf = vec![0u8; len.max(0) as usize + 1];
    unsafe { verso_cre_link_text(doc, f, buf.as_mut_ptr().cast(), buf.len() as c_int) };
    buf.pop();
    String::from_utf8_lossy(&buf).into_owned()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let book = &args[0];
    let data = std::env::var("VERSO_CRE_DATA")
        .unwrap_or_else(|_| concat!(env!("CARGO_MANIFEST_DIR"), "/../../third_party/crengine/cr3gui/data").into());
    let (w, h, dpi) = (1272, 1696, 300);
    unsafe {
        assert_eq!(verso_cre_init(c(&format!("{data}/hyph")).as_ptr(), std::ptr::null(), 0), 0);
        for f in &args[1..] {
            assert_eq!(verso_cre_register_font(c(f).as_ptr()), 0, "font {f}");
        }
        let doc = verso_cre_new(w, h, dpi);
        assert!(!doc.is_null());
        verso_cre_set_stylesheet_file(doc, c(&format!("{data}/epub.css")).as_ptr());
        verso_cre_set_font_size(doc, 42);
        verso_cre_set_margins(doc, 90, 90, 90, 90);
        assert_eq!(verso_cre_load(doc, c(book).as_ptr()), 0, "load failed");
        verso_cre_render(doc);
        let (mut notes, mut jumps) = (0, 0);
        for page in 0..verso_cre_page_count(doc) {
            let n = verso_cre_page_links(doc, page, std::ptr::null_mut(), std::ptr::null_mut(), 0);
            let mut rects = vec![VersoCreRect::default(); n.max(0) as usize];
            let mut ids = vec![0; n.max(0) as usize];
            verso_cre_page_links(doc, page, rects.as_mut_ptr(), ids.as_mut_ptr(), n);
            ids.dedup();
            for id in ids {
                let kind = verso_cre_link(doc, page, id);
                let kind = match kind {
                    1 => {
                        notes += 1;
                        "NOTE"
                    }
                    2 => {
                        jumps += 1;
                        "jump"
                    }
                    0 => "none",
                    _ => "ERR",
                };
                let note: String = field(doc, 1).chars().take(160).collect();
                println!(
                    "p{:<4} {kind} {:<10} {:?} -> {} | {note}",
                    page + 1,
                    field(doc, 3),
                    field(doc, 2),
                    field(doc, 0)
                );
            }
        }
        println!("notes {notes}, jumps {jumps}");
        verso_cre_free(doc);
    }
}
