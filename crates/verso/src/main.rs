//! Verso — a calm, lightweight e-reader. See docs/plan.md.
//!
//! Simulator only: `--screenshot out.png` renders without a window, saves a
//! grayscale PNG at device size and exits. `--screen books|gallery` picks the
//! start screen; `--tap x,y`, `--press x,y` (finger down, not lifted) and
//! `--swipe x1,y1,x2,y2` (all repeatable, played in order) run first.

#[cfg(all(feature = "desktop", feature = "kindle"))]
compile_error!("Enable exactly one backend: `desktop` (default) or `kindle`.");
#[cfg(not(any(feature = "desktop", feature = "kindle")))]
compile_error!("Enable a backend: `desktop` (default) or `kindle`.");

mod app;
mod dictionary;
mod document;
mod geom;
mod gfx;
mod library;
mod logging;
mod platform;
mod reader;
mod screens;
mod store;
mod ui;

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use app::App;
use geom::Point;
use platform::{Event, RefreshMode, ScriptStep, TextInput};
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
            "--text" => {
                opts.script.push(ScriptStep::Text(TextInput::Insert(args.next().context("--text needs text")?)))
            }
            "--edit" => {
                let key = args.next().context("--edit needs a command")?;
                let input = match key.as_str() {
                    "backspace" => TextInput::Backspace,
                    "delete" => TextInput::Delete,
                    "left" => TextInput::Left,
                    "right" => TextInput::Right,
                    "home" => TextInput::Home,
                    "end" => TextInput::End,
                    "clear" => TextInput::Clear,
                    "submit" => TextInput::Submit,
                    "dismiss" => TextInput::Dismiss,
                    _ => bail!("unknown edit command {key:?}"),
                };
                opts.script.push(ScriptStep::Text(input));
            }
            "--tap" => {
                let v = numbers(args.next(), 2, "--tap")?;
                opts.script.push(ScriptStep::Tap(Point::new(v[0], v[1])));
            }
            "--long-press" => {
                let v = numbers(args.next(), 2, "--long-press")?;
                opts.script.push(ScriptStep::LongPress(Point::new(v[0], v[1])));
            }
            "--hold-drag" => {
                let v = numbers(args.next(), 4, "--hold-drag")?;
                opts.script.push(ScriptStep::HoldDrag(Point::new(v[0], v[1]), Point::new(v[2], v[3])));
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
        bail!("scripted input only works with --screenshot");
    }
    Ok(opts)
}

fn start_screen(
    name: Option<&str>,
    theme: &ui::theme::Theme,
    rect: geom::Rect,
    worker: std::rc::Rc<document::thread::Worker>,
) -> Result<Box<dyn Screen>> {
    Ok(match name.unwrap_or("home") {
        "gallery" => Box::new(screens::gallery::Gallery::new(theme, rect, worker)),
        "home" | "library" | "books" | "reader" => Box::new(screens::books::Books::new(theme, rect, worker)),
        other => bail!("unknown screen {other:?}; try home, library or gallery"),
    })
}

fn run() -> Result<()> {
    if std::env::var("VERSO_SEARCH_BENCH").as_deref() == Ok("1") {
        println!("{}", library::search::benchmark(&store::Store::new().library()));
        return Ok(());
    }
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
    let worker =
        std::rc::Rc::new(document::thread::Worker::new(document::Paths::for_device(), info.dpi, platform.waker()));
    let mut app = App::new(&info, theme.clone(), start_screen(opts.screen.as_deref(), &theme, bounds, worker.clone())?);
    app.frontlight = platform.frontlight();
    app.set_battery(platform.battery());
    if let Some(connected) = platform.usb_connected() {
        app.on_usb(connected);
    }
    let mut gestures = GestureDetector::new(GestureConfig::for_dpi(info.dpi));
    app.refresh_all(RefreshMode::Full);
    let mut first_frame = true;
    let mut quit = false;

    loop {
        if let Some(light) = app.light_request()
            && let Err(e) = platform.set_frontlight(light)
        {
            log::error!("{e:#}");
            app.frontlight = platform.frontlight();
        }
        let updates = app.frame();
        if !updates.is_empty() {
            let t = Instant::now();
            platform.present(app.canvas(), &updates)?;
            let save_started = Instant::now();
            app.after_present();
            log::debug!("after_present: {:?}", save_started.elapsed());
            log::debug!("presented {} update(s) in {:?}: {updates:?}", updates.len(), t.elapsed());
            if first_frame {
                log::info!(
                    "first frame on screen {:?} after start (display request returned; optical completion unmeasured)",
                    started.elapsed()
                );
                first_frame = false;
            }
        }
        if quit || app.wants_exit() {
            if app.on_quit() {
                log::info!("exit requested");
                break;
            }
            quit = false;
            continue;
        }
        if app.has_pending_work() {
            // Its "please wait" frame is on screen now; do the slow part.
            app.run_pending_work();
            continue;
        }

        platform.set_busy(app.busy());
        let deadline = [gestures.next_deadline(), app.deadline()].into_iter().flatten().min();
        let mut timeout = deadline.map(|d| d.saturating_duration_since(Instant::now()));
        // Handle everything that's already waiting before drawing, so a fast
        // drag draws once per batch instead of once per touch report.
        for _ in 0..MAX_BATCH {
            match platform.next_event(timeout)? {
                None => break,
                Some(Event::Background) => app.on_background(),
                Some(Event::TextInput { input, repeat }) => {
                    if !app.on_text_input(&input) {
                        platform.unhandled_text_input(&input, repeat);
                    }
                }
                Some(Event::Touch(t)) => {
                    log::trace!("{t:?}");
                    if let Some(g) = gestures.on_touch(&t) {
                        log::debug!("{g:?}");
                        app.on_gesture(g);
                        // A release after a missed timer classifies as a
                        // long press; deliver its release in this same batch.
                        if matches!(t.phase, platform::TouchPhase::Up)
                            && matches!(g, ui::gesture::Gesture::LongPress(_))
                        {
                            app.on_gesture(ui::gesture::Gesture::End(t.pos));
                        }
                    }
                }
                Some(Event::Sleep) => {
                    log::info!("sleep");
                    gestures.reset();
                    app.on_sleep();
                    let cover = app.draw_sleep_cover(store::Store::new().settings().sleep_screen);
                    platform.prepare_sleep(cover)?;
                    if cover {
                        platform
                            .present(app.canvas(), &[platform::Update { rect: bounds, mode: RefreshMode::Full }])?;
                        platform.wait_for_complete()?;
                    }
                }
                Some(Event::Wake) => {
                    log::info!("wake");
                    platform.resume()?;
                    gestures.reset();
                    app.set_battery(platform.battery());
                    app.on_wake();
                    if let Some(connected) = platform.usb_connected() {
                        app.on_usb(connected);
                    }
                }
                Some(Event::WakeRepaint) => app.schedule_wake_cleanup(),
                Some(Event::PowerChanged) => {
                    app.set_battery(platform.battery());
                    if let Some(connected) = platform.usb_connected()
                        && app.on_usb(connected)
                    {
                        gestures.reset();
                    }
                }
                Some(Event::Usb(connected)) => {
                    gestures.reset();
                    app.set_battery(platform.battery());
                    app.on_usb(platform.usb_connected().unwrap_or(connected));
                }
                Some(Event::Quit) => {
                    if cfg!(feature = "desktop") && opts.screenshot.is_none() {
                        app.request_exit();
                        break;
                    }
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
