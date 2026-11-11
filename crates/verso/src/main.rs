//! Verso — a calm, lightweight e-reader. See docs/plan.md.
//!
//! Simulator only: `--screenshot out.png` renders without a window, saves a
//! grayscale PNG at device size and exits. `--screen gallery|spike` picks the
//! start screen; `--tap x,y`, `--press x,y` (finger down, not lifted) and
//! `--swipe x1,y1,x2,y2` (all repeatable, played in order) run first.

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
mod screens;
mod ui;

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use app::App;
use geom::Point;
use platform::{Event, RefreshMode, ScriptStep};
use screens::Screen;
use ui::gesture::{GestureConfig, GestureDetector};

/// Events handled at most before drawing a frame.
const MAX_BATCH: usize = 64;

fn main() {
    logging::init();
    if let Err(e) = run() {
        log::error!("{e:#}");
        std::process::exit(1);
    }
}

#[derive(Default)]
struct Options {
    screen: Option<String>,
    screenshot: Option<std::path::PathBuf>,
    script: Vec<ScriptStep>,
}

fn parse_args() -> Result<Options> {
    let mut opts = Options::default();
    let mut args = std::env::args().skip(1);
    let numbers = |s: Option<String>, n: usize, flag: &str| -> Result<Vec<i32>> {
        let s = s.with_context(|| format!("{flag} needs a value"))?;
        let v: Vec<i32> = s
            .split(',')
            .map(|p| p.trim().parse())
            .collect::<Result<_, _>>()
            .with_context(|| format!("{flag} {s:?}: expected {n} comma-separated numbers"))?;
        if v.len() != n {
            bail!("{flag} {s:?}: expected {n} comma-separated numbers");
        }
        Ok(v)
    };
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--screen" => opts.screen = Some(args.next().context("--screen needs a name")?),
            "--screenshot" => opts.screenshot = Some(args.next().context("--screenshot needs a file name")?.into()),
            "--tap" => {
                let v = numbers(args.next(), 2, "--tap")?;
                opts.script.push(ScriptStep::Tap(Point::new(v[0], v[1])));
            }
            "--press" => {
                let v = numbers(args.next(), 2, "--press")?;
                opts.script.push(ScriptStep::Press(Point::new(v[0], v[1])));
            }
            "--swipe" => {
                let v = numbers(args.next(), 4, "--swipe")?;
                opts.script.push(ScriptStep::Swipe(Point::new(v[0], v[1]), Point::new(v[2], v[3])));
            }
            other => bail!("unknown argument {other:?} (see the top of main.rs)"),
        }
    }
    if !opts.script.is_empty() && opts.screenshot.is_none() {
        bail!("--tap/--press/--swipe only work with --screenshot");
    }
    Ok(opts)
}

fn start_screen(name: Option<&str>, theme: &ui::theme::Theme, rect: geom::Rect) -> Result<Box<dyn Screen>> {
    Ok(match name.unwrap_or("gallery") {
        "gallery" => Box::new(screens::gallery::Gallery::new(theme, rect)),
        "spike" => Box::new(screens::spike::Spike::new(theme, rect)),
        other => bail!("unknown screen {other:?}; try gallery or spike"),
    })
}

fn run() -> Result<()> {
    log::info!("Verso {} starting", env!("CARGO_PKG_VERSION"));
    let started = Instant::now();
    let opts = if cfg!(feature = "desktop") { parse_args()? } else { Options::default() };
    let mut platform = match &opts.screenshot {
        Some(path) => platform::open_headless(path, opts.script.clone())?,
        None => platform::open()?,
    };
    let info = platform.info().clone();
    log::info!("screen: {} {}x{} @ {} dpi", info.name, info.width, info.height, info.dpi);

    let theme = ui::theme::Theme::for_dpi(info.dpi);
    let bounds = geom::Rect::new(0, 0, info.width as i32, info.height as i32);
    let mut app = App::new(&info, theme.clone(), start_screen(opts.screen.as_deref(), &theme, bounds)?);
    let mut gestures = GestureDetector::new(GestureConfig::for_dpi(info.dpi));
    app.refresh_all(RefreshMode::Full);
    let mut first_frame = true;
    let mut quit = false;

    loop {
        let updates = app.frame();
        if !updates.is_empty() {
            let t = Instant::now();
            platform.present(app.canvas(), &updates)?;
            log::debug!("presented {} update(s) in {:?}: {updates:?}", updates.len(), t.elapsed());
            if first_frame {
                log::info!("first frame on screen {:?} after start", started.elapsed());
                first_frame = false;
            }
        }
        if quit {
            log::info!("quit requested");
            break;
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

        let deadline = [gestures.next_deadline(), app.deadline()].into_iter().flatten().min();
        let mut timeout = deadline.map(|d| d.saturating_duration_since(Instant::now()));
        // Handle everything that's already waiting before drawing, so a fast
        // drag draws once per batch instead of once per touch report.
        for _ in 0..MAX_BATCH {
            match platform.next_event(timeout)? {
                None => break,
                Some(Event::Touch(t)) => {
                    log::trace!("{t:?}");
                    if let Some(g) = gestures.on_touch(&t) {
                        log::debug!("{g:?}");
                        app.on_gesture(g);
                    }
                }
                Some(Event::Sleep) => {
                    log::info!("sleep");
                    gestures.reset();
                    app.on_sleep();
                }
                Some(Event::Wake) => {
                    log::info!("wake");
                    app.on_wake();
                }
                Some(Event::Quit) => {
                    // Draw what's pending first (screenshots need the last frame).
                    quit = true;
                    break;
                }
            }
            timeout = Some(Duration::ZERO);
        }
        let now = Instant::now();
        if let Some(g) = gestures.on_tick(now) {
            log::debug!("{g:?}");
            app.on_gesture(g);
        }
        app.on_tick(now);
    }

    platform.wait_for_complete()?;
    log::info!("bye");
    Ok(())
}
