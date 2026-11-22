//! Prints the stems Hunspell finds for each word.
//!
//!     cargo run -p hunspell-sys --example stem -- de_DE.aff de_DE.dic Häuser Kinder

use std::path::Path;
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        eprintln!("usage: stem <file.aff> <file.dic> [word ...]");
        std::process::exit(2);
    }
    let start = Instant::now();
    let dict = match hunspell_sys::Hunspell::open(Path::new(&args[0]), Path::new(&args[1])) {
        Ok(dict) => dict,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    };
    println!(
        "hunspell {} · {} · opened in {:.1} ms",
        hunspell_sys::library_version(),
        dict.encoding(),
        start.elapsed().as_secs_f64() * 1000.0
    );
    for word in &args[2..] {
        println!("{word}\t{}", dict.stem(word).join(", "));
    }
}
