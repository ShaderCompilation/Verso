//! `verso-mdns [--watch PID [--watch-name NAME]] [--firewall INTERFACE] LABEL`
//!
//! Advertises `LABEL.local` until the watched process exits or a signal
//! arrives. Verso starts it with the SSH server; see the library docs.

fn main() {
    let mut options = verso_mdns::Options { label: String::new(), watch: None, watch_name: None, firewall: None };
    let mut args = std::env::args().skip(1);
    let mut label = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--watch" => options.watch = args.next().and_then(|pid| pid.parse().ok()).or_else(|| usage()),
            "--watch-name" => options.watch_name = Some(args.next().unwrap_or_else(|| usage())),
            "--firewall" => options.firewall = Some(args.next().unwrap_or_else(|| usage())),
            _ if label.is_none() && !arg.starts_with('-') => label = Some(arg),
            _ => usage(),
        }
    }
    options.label = label.filter(|l| verso_mdns::valid_label(l)).unwrap_or_else(|| usage());
    std::process::exit(verso_mdns::run(options));
}

fn usage() -> ! {
    eprintln!(
        "usage: verso-mdns [--watch PID [--watch-name NAME]] [--firewall INTERFACE] LABEL\n\
         LABEL: 1-63 lowercase letters, digits and hyphens"
    );
    std::process::exit(2);
}
