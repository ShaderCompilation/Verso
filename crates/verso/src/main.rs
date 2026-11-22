//! Verso — a calm, lightweight e-reader. See docs/plan.md.
//!
//! Simulator only: `--screenshot out.png` renders without a window, saves a
//! grayscale PNG at device size and exits. `--screen books|settings|transfer|gallery` picks the
//! start screen; `--tap x,y`, `--press x,y` (finger down, not lifted) and
//! `--swipe x1,y1,x2,y2` and `--key next|prev` (a page-turn button; all
//! repeatable, played in order) run first.
//! `--device basic-167|basic-300|pw5|pw6|oasis3|colorsoft|scribe` picks the
//! simulated Kindle, with or without a window (platform/sim_device.rs).

#[cfg(all(feature = "desktop", feature = "kindle"))]
compile_error!("Enable exactly one backend: `desktop` (default) or `kindle`.");
#[cfg(not(any(feature = "desktop", feature = "kindle")))]
compile_error!("Enable a backend: `desktop` (default) or `kindle`.");

mod annotations;
mod app;
mod clock;
mod dictionary;
mod document;
mod geom;
mod gfx;
mod library;
mod logging;
mod page_layout;
mod perf;
mod platform;
mod reader;
mod report;
mod screens;
mod store;
mod transfer;
mod ui;
mod update;

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
    if std::env::args().nth(1).as_deref() == Some(document::prelayout::ARG) {
        logging::init_tagged("prelayout");
        if let Err(e) = document::prelayout::run_child() {
            log::error!("prelayout: {e:#}");
            std::process::exit(1);
        }
        return;
    }
    logging::init();
    match run() {
        Ok(0) => {}
        // Everything is saved and dropped; the launcher takes it from here.
        Ok(code) => std::process::exit(code),
        Err(e) => {
            log::error!("{e:#}");
            std::process::exit(1);
        }
    }
}

#[derive(Default)]
struct Options {
    screen: Option<String>,
    screenshot: Option<std::path::PathBuf>,
    script: Vec<ScriptStep>,
    device: Option<String>,
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
            "--device" => opts.device = Some(args.next().context("--device needs a name, e.g. pw6")?),
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
            "--key" => {
                let key = args.next().context("--key needs next or prev")?;
                opts.script.push(ScriptStep::Key(match key.as_str() {
                    "next" => platform::KeyAction::PageNext,
                    "prev" => platform::KeyAction::PagePrev,
                    _ => bail!("unknown key {key:?}: expected next or prev"),
                }));
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
    store: store::Store,
) -> Result<Box<dyn Screen>> {
    Ok(match name.unwrap_or("home") {
        "gallery" => Box::new(screens::gallery::Gallery::new(theme, rect, worker)),
        "home" | "library" | "books" | "reader" => {
            Box::new(screens::books::Books::with_store(theme, rect, worker, store))
        }
        "settings" => Box::new(screens::system::SystemSettings::new(theme, rect, store, worker.waker())),
        "transfer" => Box::new(screens::transfer::TransferScreen::new(theme, rect, store, worker.waker())),
        other => bail!("unknown screen {other:?}; try home, library, settings, transfer or gallery"),
    })
}

/// The screens after turning the display, showing what they showed before.
fn relayout_screens(
    name: Option<&str>,
    theme: &ui::theme::Theme,
    rect: geom::Rect,
    worker: std::rc::Rc<document::thread::Worker>,
    store: store::Store,
    state: &screens::RelayoutState,
) -> Vec<Box<dyn Screen>> {
    match name.unwrap_or("home") {
        "home" | "library" | "books" | "reader" => screens::books::relayout_stack(state, theme, rect, worker, store),
        "settings" => {
            vec![Box::new(screens::system::SystemSettings::new(theme, rect, store, worker.waker()).on_display_tab())]
        }
        _ => vec![start_screen(name, theme, rect, worker, store).expect("checked at startup")],
    }
}

/// Runs Verso; the exit code for the launcher (0, or one a screen asked for).
fn run() -> Result<i32> {
    if std::env::var("VERSO_SEARCH_BENCH").as_deref() == Ok("1") {
        println!("{}", library::search::benchmark(&store::Store::new().library()));
        return Ok(0);
    }
    if std::env::var("VERSO_PREVIEW_BENCH").as_deref() == Ok("1") {
        let store = store::Store::new();
        println!("{}", library::previews::benchmark(&store, &store.library(), &store.library_folders()));
        return Ok(0);
    }
    if std::env::var("VERSO_FONT_BENCH").as_deref() == Ok("1") {
        // Starts the book engine the way the book thread does: with the font
        // list from the last start, or writing it when there is none.
        let started = Instant::now();
        let engine = document::CreEngine::new(document::Paths::for_device(), 300)?;
        println!(
            "fonts: {} faces, engine ready in {:?}, font hash {:08x}",
            engine.fonts().len(),
            started.elapsed(),
            engine.font_hash()
        );
        return Ok(0);
    }
    if std::env::var("VERSO_LANGUAGE_BENCH").as_deref() == Ok("1") {
        println!("{}", library::language::benchmark(&store::Store::new().library()));
        return Ok(0);
    }
    if std::env::var("VERSO_COVER_BENCH").as_deref() == Ok("1") {
        let store = store::Store::new();
        println!("{}", library::covers::benchmark(&store, &store.library()));
        return Ok(0);
    }
    log::info!("Verso {} ({}) starting", env!("CARGO_PKG_VERSION"), env!("VERSO_GIT_COMMIT"));
    let started = Instant::now();
    let opts = if cfg!(feature = "desktop") { parse_args()? } else { Options::default() };
    let opened = match &opts.screenshot {
        Some(path) => platform::open_headless(path, opts.script.clone(), opts.device.as_deref()),
        None => platform::open(opts.device.as_deref()),
    };
    let mut platform = match opened {
        Ok(platform) => platform,
        // Already said on screen; the launcher saves a problem report.
        Err(e) if e.downcast_ref::<platform::Unsupported>().is_some() => {
            log::error!("{e:#}");
            return Ok(verso_update::launcher::EXIT_UNSUPPORTED);
        }
        Err(e) => return Err(e),
    };
    let platform_us = started.elapsed().as_micros();
    // One Store for every screen, so they share preferences and the dictionary service.
    let store = store::Store::new();
    let orientation = store.settings().orientation;
    if orientation != platform::Orientation::Portrait {
        platform.set_orientation(orientation)?;
    }
    let info = platform.info().clone();
    let store = store.with_color(info.color);
    let mut session = format!(
        "=== Verso session ===\nversion: {} ({})\norientation: {:?}\nscreen: {} {}x{} @ {} dpi",
        env!("CARGO_PKG_VERSION"),
        env!("VERSO_GIT_COMMIT"),
        orientation,
        info.name,
        info.width,
        info.height,
        info.dpi,
    );
    for line in platform.session_details() {
        session.push('\n');
        session.push_str(&line);
    }
    session.push_str("\n=== end session ===");
    log::info!("{session}");

    let theme = ui::theme::Theme::for_dpi(info.dpi);
    let bounds = geom::Rect::new(0, 0, info.width as i32, info.height as i32);
    let worker = std::rc::Rc::new(document::thread::Worker::new(
        document::Paths::for_device(),
        info.dpi,
        info.color,
        platform.waker(),
    ));
    let t_screen = Instant::now();
    let screen = start_screen(opts.screen.as_deref(), &theme, bounds, worker.clone(), store.clone())?;
    let screen_us = t_screen.elapsed().as_micros();
    let t_app = Instant::now();
    let mut app = App::new(&info, theme.clone(), screen);
    let app_us = t_app.elapsed().as_micros();
    let t_setup = Instant::now();
    app.set_orientation(orientation);
    app.set_services(app::Services::device(store.clone(), platform.waker()));
    // After a rollback, the restored version says why with its first frame.
    if let Some(rollback) = store.updates().rollback() {
        log::info!("showing the rollback note: {:?}", rollback.reason);
        app.show_rollback(screens::update::RollbackSheet::new(&theme, bounds, store.updates(), &rollback));
    }
    let model = platform.model();
    app.set_light_range(model.light_range);
    if screens::untested::due(&store.settings(), &model) {
        log::info!("showing the untested-model notice: {}", model.name);
        app.show_notice(app::Notice::Untested(screens::untested::UntestedSheet::new(
            &theme,
            bounds,
            store.clone(),
            &model,
        )));
    }
    if let Some(light) = platform.initial_frontlight() {
        app.frontlight = light;
    }
    app.set_battery(platform.battery());
    if let Some(connected) = platform.usb_connected() {
        app.on_usb(connected);
    }
    let setup_us = t_setup.elapsed().as_micros();
    log::debug!(
        "startup stage: init platform_us={platform_us} screen_us={screen_us} app_us={app_us} setup_us={setup_us}"
    );
    let mut gestures = GestureDetector::new(GestureConfig::for_dpi(info.dpi));
    app.refresh_all(RefreshMode::Full);
    let mut first_frame = true;
    let mut quit = false;
    let mut perf = perf::SessionPerf::default();
    // When the last finger lifted: a page turn is timed from there.
    let mut last_release: Option<Instant> = None;

    loop {
        if let Some(light) = app.light_request()
            && let Err(e) = platform.set_frontlight(light)
        {
            log::error!("{e:#}");
            app.frontlight = platform.frontlight();
        }
        let frame_started = Instant::now();
        let updates = app.frame();
        let frame_us = frame_started.elapsed().as_micros();
        if !updates.is_empty() {
            let t = Instant::now();
            platform.present(app.canvas(), &updates)?;
            let present_us = t.elapsed().as_micros();
            let save_started = Instant::now();
            app.after_present();
            log::debug!("after_present: {:?}", save_started.elapsed());
            log::debug!("presented {} update(s) in {:?}: {updates:?}", updates.len(), t.elapsed());
            if app.take_page_turn()
                && let Some(released) = last_release.take()
            {
                perf.page_turn(released.elapsed());
            }
            if first_frame {
                log::debug!("startup stage: first frame draw_us={frame_us} present_us={present_us}");
                perf.first_frame(started.elapsed());
                log::info!(
                    "first frame on screen {:?} after start (display request returned; optical completion unmeasured)",
                    started.elapsed()
                );
                first_frame = false;
                // Fonts load now rather than alongside the first frame.
                worker.start();
                // An SSH server left running (or started by KOReader) gets its
                // name back. After the first frame, so startup isn't delayed.
                let settings = store.settings();
                platform::ssh::advertise(&settings.hostname);
                // A version on trial counts as started a little later.
                update::mark_started_soon();
                // An update download cut short may have left the Kindle
                // kept awake (a file check; rare lipc call).
                store.updates().restore_awake_after_crash();
                // Wi-Fi transfer may have left port 80 open (a file check).
                transfer::cleanup_after_crash(&transfer::Device::current(store.root()));
                // A version the launcher went back from isn't offered again.
                store.updates().note_rollback();
                // The update check reads its files and Wi-Fi on its own thread.
                if settings.check_updates {
                    store.updates().check_on_start(&platform.waker());
                }
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
        if let Some((orientation, state)) = app.prepare_orientation() {
            platform.set_orientation(orientation)?;
            let info = platform.info().clone();
            log::info!("screen turned: {}x{}, {orientation:?}, {state:?}", info.width, info.height);
            gestures.reset();
            let name = opts.screen.as_deref();
            app.relayout(&info, orientation, |theme, rect| {
                relayout_screens(name, theme, rect, worker.clone(), store.clone(), &state)
            });
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
        // drag or burst of taps draws once per batch instead of once per report.
        app.begin_batch();
        for _ in 0..MAX_BATCH {
            match platform.next_event(timeout)? {
                None => break,
                Some(Event::Background) => app.on_background(),
                Some(Event::TextInput { input, repeat }) => {
                    if !app.on_text_input(&input) {
                        platform.unhandled_text_input(&input, repeat);
                    }
                }
                Some(Event::Key(key)) => {
                    log::debug!("{key:?}");
                    // A button press is timed like a lifted finger.
                    last_release = Some(Instant::now());
                    app.on_key(key);
                }
                Some(Event::Touch(t)) => {
                    log::trace!("{t:?}");
                    if t.phase == platform::TouchPhase::Up {
                        last_release = Some(t.time);
                    }
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
                    app.finish_batch();
                    gestures.reset();
                    app.on_sleep();
                    let cover = app.draw_sleep_cover(store.settings().sleep_screen);
                    platform.prepare_sleep(cover)?;
                    if cover {
                        let rect = app.canvas().bounds();
                        platform.present(app.canvas(), &[platform::Update { rect, mode: RefreshMode::Full }])?;
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
                    app.set_battery(platform.battery());
                    if app.on_usb(platform.usb_connected().unwrap_or(connected)) {
                        gestures.reset();
                    }
                }
                Some(Event::Frontlight(light)) => app.set_initial_frontlight(light),
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
        app.finish_batch();
        app.on_tick(now);
    }

    platform.wait_for_complete()?;
    if !first_frame {
        // Closed cleanly before the started delay ran out: it works.
        update::mark_started();
    }
    log::info!("{}", perf.summary(perf::peak_rss_kib()));
    let code = app.exit_code();
    log::info!("bye{}", if code == 0 { String::new() } else { format!(" (exit code {code} for the launcher)") });
    Ok(code)
}
