//! Logging setup. The app logs to stderr; on the Kindle the launcher script
//! sends stderr to `/mnt/us/verso/verso.log`, so crash output lands there too.
//! Set `RUST_LOG` (e.g. `RUST_LOG=debug`) to see more.

pub fn init() {
    let default = if cfg!(debug_assertions) { "info,verso=debug" } else { "info" };
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(default))
        .format_timestamp_millis()
        .init();

    // Panics abort in release builds; make sure the reason reaches the log first.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log::error!("panic: {info}");
        default_hook(info);
    }));
}
