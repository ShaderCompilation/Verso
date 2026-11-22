//! Runs a quality list (assets/wordforms/hunspell-check/<lang>.tsv) against a dictionary.
//!
//!     cargo run -p hunspell-sys --release --example check -- de_DE.aff de_DE.dic \
//!         assets/wordforms/hunspell-check/de.tsv
//!
//! Each line is `form<TAB>expected lemmas, best first<TAB>note`. A form is
//! found when Hunspell gives any stem, right when a stem is an expected lemma,
//! and right first when the first stem is the first expected lemma.

use std::path::Path;
use std::time::Instant;

/// The process's resident memory in KiB (Linux only).
fn rss_kib() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    status.lines().find_map(|l| l.strip_prefix("VmRSS:")?.trim().strip_suffix("kB")?.trim().parse().ok())
}

/// The process's peak resident memory in KiB (Linux only).
fn peak_kib() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    status.lines().find_map(|l| l.strip_prefix("VmHWM:")?.trim().strip_suffix("kB")?.trim().parse().ok())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() != 3 {
        eprintln!("usage: check <file.aff> <file.dic> <list.tsv>");
        std::process::exit(2);
    }
    let list = std::fs::read_to_string(&args[2]).unwrap_or_else(|e| {
        eprintln!("error: {}: {e}", args[2]);
        std::process::exit(1);
    });
    let rss_before = rss_kib();
    let start = Instant::now();
    let dict = hunspell_sys::Hunspell::open(Path::new(&args[0]), Path::new(&args[1])).unwrap_or_else(|e| {
        eprintln!("error: {e}");
        std::process::exit(1);
    });
    let opened = start.elapsed().as_secs_f64() * 1000.0;
    let rss_after_open = rss_kib();
    println!(
        "opened in {opened:.0} ms ({}); RSS {} KiB before, {} KiB after open",
        dict.encoding(),
        rss_before.map_or("?".into(), |k| k.to_string()),
        rss_after_open.map_or("?".into(), |k| k.to_string())
    );
    let (mut total, mut found, mut right, mut first) = (0, 0, 0, 0);
    let mut stem_time = 0.0;
    for line in list.lines().filter(|l| !l.starts_with('#') && !l.trim().is_empty()) {
        let mut cols = line.split('\t');
        let form = cols.next().unwrap_or_default();
        let expected: Vec<&str> = cols.next().unwrap_or_default().split(", ").collect();
        let note = cols.next().unwrap_or_default();
        let t = Instant::now();
        let stems = dict.stem(form);
        stem_time += t.elapsed().as_secs_f64();
        total += 1;
        let is_found = !stems.is_empty();
        let is_right = stems.iter().any(|s| expected.contains(&s.as_str()));
        let is_first = stems.first().is_some_and(|s| *s == expected[0]);
        found += is_found as u32;
        right += is_right as u32;
        first += is_first as u32;
        let mark = if is_first {
            "ok   "
        } else if is_right {
            "right"
        } else if is_found {
            "WRONG"
        } else {
            "none "
        };
        println!("{mark}  {form:<20} -> {:<40} expected {}  {note}", stems.join(", "), expected.join(", "));
    }
    let (rss_after_stems, peak) = (rss_kib(), peak_kib());
    drop(dict);
    println!(
        "RSS {} KiB after the stems (peak {} KiB), {} KiB after closing",
        rss_after_stems.map_or("?".into(), |k| k.to_string()),
        peak.map_or("?".into(), |k| k.to_string()),
        rss_kib().map_or("?".into(), |k| k.to_string())
    );
    let pct = |n: u32| 100.0 * f64::from(n) / f64::from(total.max(1));
    println!(
        "{total} forms: found {found} ({:.0}%), right {right} ({:.0}%), right first {first} ({:.0}%); stem {:.2} ms each",
        pct(found),
        pct(right),
        pct(first),
        stem_time * 1000.0 / f64::from(total.max(1))
    );
}
