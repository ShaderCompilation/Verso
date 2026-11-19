//! Logging setup. The app logs to stderr; on the Kindle the launcher script
//! sends stderr to `/mnt/us/verso/verso.log`, so crash output lands there too.
//! Set `RUST_LOG` (e.g. `RUST_LOG=debug`) to see more.

pub fn init() {
    builder().init();
    panic_hook();
}

/// For a helper process sharing the app's log: each line names it.
pub fn init_tagged(tag: &'static str) {
    builder()
        .format(move |buf, record| {
            use std::io::Write;
            writeln!(
                buf,
                "[{} {:<5} {} {tag}] {}",
                buf.timestamp_millis(),
                record.level(),
                record.target(),
                record.args()
            )
        })
        .init();
    panic_hook();
}

fn builder() -> env_logger::Builder {
    let default = if cfg!(debug_assertions) { "info,verso=debug" } else { "info" };
    let mut builder = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(default));
    builder.format_timestamp_millis();
    builder
}

fn panic_hook() {
    // Panics abort in release builds; make sure the reason reaches the log first.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log::error!("panic: {info}");
        default_hook(info);
    }));
}
