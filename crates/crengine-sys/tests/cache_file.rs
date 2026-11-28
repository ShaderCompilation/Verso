//! crengine's document cache across repeated layouts of one book. Its own
//! test binary: crengine's logger and cache folder are per process.

use std::ffi::{CStr, CString, c_char};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use crengine_sys::*;

static ERRORS: Mutex<Vec<String>> = Mutex::new(Vec::new());
static ROUND: AtomicUsize = AtomicUsize::new(0);

extern "C" fn logger(message: *const c_char) {
    let message = unsafe { CStr::from_ptr(message) }.to_string_lossy().into_owned();
    ERRORS.lock().unwrap().push(format!("layout {}: {message}", ROUND.load(Ordering::Relaxed)));
}

fn c(s: &str) -> CString {
    CString::new(s).unwrap()
}

/// A generated book: varied paragraph lengths give layout blocks of varying
/// sizes, so each layout at a new size frees and reallocates blocks in the file.
fn book() -> String {
    let words = ["river", "lantern", "harbour", "quietly", "morning", "stone", "letter", "window", "silver", "orchard"];
    let mut html = String::from("<html><head><title>Churn</title></head><body>");
    let mut seed = 1u32;
    for chapter in 0..6 {
        html.push_str(&format!("<h1>Chapter {chapter}</h1>"));
        for _ in 0..50 {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            let len = 20 + (seed >> 16) as usize % 160;
            html.push_str("<p>");
            for w in 0..len {
                html.push_str(words[(w * 7 + seed as usize) % words.len()]);
                html.push(' ');
            }
            html.push_str("</p>");
        }
    }
    html.push_str("</body></html>");
    html
}

#[test]
fn relayouts_keep_the_cache_file_valid() {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    let dir = std::env::temp_dir().join(format!("verso-cache-file-test-{}", std::process::id()));
    let cache = dir.join("cache");
    std::fs::create_dir_all(&cache).unwrap();
    let path = dir.join("book.html");
    std::fs::write(&path, book()).unwrap();
    unsafe {
        verso_cre_set_logger(logger);
        let hyph = format!("{root}/third_party/crengine/cr3gui/data/hyph");
        assert_eq!(verso_cre_init(c(&hyph).as_ptr(), c(cache.to_str().unwrap()).as_ptr(), 64 << 20), 0);
        assert_eq!(verso_cre_register_font(c(&format!("{root}/assets/fonts/Literata-Regular.ttf")).as_ptr()), 0);
        // Before the `writeIndex` fix in build.rs, the 10th layout found a damaged file.
        for round in 0..24 {
            ROUND.store(round, Ordering::Relaxed);
            let doc = verso_cre_new(600, 800, 300);
            verso_cre_set_font_face(doc, c("Literata").as_ptr());
            verso_cre_set_font_size(doc, [30, 34, 38, 42, 46, 50, 36, 44][round % 8]);
            assert_eq!(verso_cre_load(doc, c(path.to_str().unwrap()).as_ptr()), 0);
            verso_cre_render(doc);
            verso_cre_free(doc);
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    let errors = ERRORS.lock().unwrap();
    let damaged: Vec<_> = errors.iter().filter(|m| m.contains("CRC doesn't match")).collect();
    assert!(damaged.is_empty(), "cache file damaged: {damaged:?}");
}
