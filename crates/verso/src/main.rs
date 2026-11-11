//! Verso — a calm, lightweight e-reader. See docs/plan.md.

#[cfg(all(feature = "desktop", feature = "kindle"))]
compile_error!("Enable exactly one backend: `desktop` (default) or `kindle`.");
#[cfg(not(any(feature = "desktop", feature = "kindle")))]
compile_error!("Enable a backend: `desktop` (default) or `kindle`.");

mod app;
mod document;
mod geom;
mod gfx;
mod logging;
mod platform;
mod ui;

use std::time::Instant;

use anyhow::Result;

use app::App;
use platform::{Event, RefreshMode};
use ui::gesture::{GestureConfig, GestureDetector};

fn main() {
    logging::init();
    if let Err(e) = run() {
        log::error!("{e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    log::info!("Verso {} starting", env!("CARGO_PKG_VERSION"));
    let started = Instant::now();
    let mut platform = platform::open()?;
    let info = platform.info().clone();
    log::info!("screen: {} {}x{} @ {} dpi", info.name, info.width, info.height, info.dpi);

    let mut app = App::new(&info);
    let mut gestures = GestureDetector::new(GestureConfig::for_dpi(info.dpi));
    app.redraw_all(RefreshMode::Full);
    let mut first_frame = true;

    loop {
        let updates = app.take_updates();
        if !updates.is_empty() {
            let t = Instant::now();
            platform.present(app.canvas(), &updates)?;
            log::debug!("presented {} update(s) in {:?}: {updates:?}", updates.len(), t.elapsed());
            if first_frame {
                log::info!("first frame on screen {:?} after start", started.elapsed());
                first_frame = false;
            }
        }
        if app.wants_exit() {
            log::info!("exit requested");
            break;
        }
        if app.has_pending_work() {
            // Its "please wait" frame is on screen now; do the slow part.
            app.run_pending_work();
            continue;
        }

        let timeout = gestures.next_deadline().map(|d| d.saturating_duration_since(Instant::now()));
        let gesture = match platform.next_event(timeout)? {
            None => gestures.on_tick(Instant::now()),
            Some(Event::Touch(t)) => {
                log::trace!("{t:?}");
                gestures.on_touch(&t)
            }
            Some(Event::Sleep) => {
                log::info!("sleep");
                gestures.reset();
                app.on_sleep();
                None
            }
            Some(Event::Wake) => {
                log::info!("wake");
                app.on_wake();
                None
            }
            Some(Event::Quit) => {
                log::info!("quit requested");
                break;
            }
        };
        if let Some(g) = gesture {
            log::debug!("{g:?}");
            app.on_gesture(g);
        }
    }

    platform.wait_for_complete()?;
    log::info!("bye");
    Ok(())
}
