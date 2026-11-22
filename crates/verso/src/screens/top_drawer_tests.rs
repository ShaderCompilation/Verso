use super::*;
use crate::geom::Point;
use crate::gfx::Canvas;
use crate::gfx::{IconRenderer, TextRenderer};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

fn mm(t: &Theme, px: i32) -> f32 {
    px as f32 * 25.4 / t.dpi
}

fn rows(l: &DrawerLayout) -> Vec<Vec<Rect>> {
    let mut rows: Vec<Vec<Rect>> = Vec::new();
    for &r in &l.tiles {
        match rows.last_mut() {
            Some(row) if row[0].y == r.y => row.push(r),
            _ => rows.push(vec![r]),
        }
    }
    rows
}

fn check_layout(t: &Theme, l: &DrawerLayout, screen: Rect) {
    let last_divider = *l.dividers.last().unwrap();
    for (i, a) in l.tiles.iter().enumerate() {
        assert!(a.x >= l.status.x && a.right() <= l.status.right(), "{a:?} inside the margins");
        assert!(a.y > last_divider && a.bottom() < l.handle.y, "{a:?} between divider and handle");
        assert_eq!(a.h, t.mm(13.4));
        for b in &l.tiles[i + 1..] {
            assert!(a.intersect(b).is_none(), "{a:?} overlaps {b:?}");
        }
    }
    for row in rows(l) {
        let (min, max) = (row.iter().map(|r| r.w).min().unwrap(), row.iter().map(|r| r.w).max().unwrap());
        assert!(max - min <= 1, "equal tiles in a row: {row:?}");
        assert_eq!(row.first().unwrap().x, l.status.x, "rows start at the margin");
        assert!((row.last().unwrap().right() - l.status.right()).abs() <= 1, "rows end at the margin");
    }
    for s in &l.sliders {
        assert!(s.y > l.dividers[0] && s.bottom() < last_divider);
    }
    assert_eq!(l.panel.y, screen.y);
    assert!(l.panel.bottom() <= screen.bottom());
    assert!(l.handle.bottom() < l.panel.bottom());
    assert_eq!(l.handle.center().x, screen.center().x);
}

#[test]
fn paperwhite_portrait_grids_have_the_same_height() {
    let t = Theme::for_dpi(300);
    let screen = Rect::new(0, 0, 1272, 1696);
    let five = DrawerLayout::new(&t, screen, 2, 5, false, false);
    let six = DrawerLayout::new(&t, screen, 2, 6, false, false);
    check_layout(&t, &five, screen);
    check_layout(&t, &six, screen);
    assert_eq!(five.panel, six.panel, "SSH doesn't change the drawer's height");
    let h = mm(&t, five.panel.h);
    assert!((62.0..=66.0).contains(&h), "about 65 mm, not {h}");
    assert!(five.panel.h * 2 < screen.h, "over half the page stays visible below");
    let r5 = rows(&five);
    assert_eq!(r5.iter().map(Vec::len).collect::<Vec<_>>(), [3, 2]);
    assert!(r5[1][0].w > r5[0][0].w, "the second row has two wider tiles");
    let r6 = rows(&six);
    assert_eq!(r6.iter().map(Vec::len).collect::<Vec<_>>(), [3, 3]);
    assert_eq!(r6[0].iter().map(|r| r.x).collect::<Vec<_>>(), r6[1].iter().map(|r| r.x).collect::<Vec<_>>());
    // Stacked sliders, about 8.5 mm apart.
    assert_eq!(five.sliders.len(), 2);
    assert_eq!(five.sliders[0].x, five.sliders[1].x);
    let pitch = mm(&t, five.sliders[1].y - five.sliders[0].y);
    assert!((8.0..=9.0).contains(&pitch), "{pitch}");
    assert_eq!(five.dividers.len(), 2);
    assert!((7.5..=8.6).contains(&mm(&t, five.dividers[0])), "status strip about 8 mm");
}

#[test]
fn paperwhite_landscape_puts_sliders_side_by_side_and_tiles_in_one_row() {
    let t = Theme::for_dpi(300);
    let screen = Rect::new(0, 0, 1696, 1272);
    for tiles in [5, 6] {
        let l = DrawerLayout::new(&t, screen, 2, tiles, false, false);
        check_layout(&t, &l, screen);
        assert_eq!(rows(&l).len(), 1, "{tiles} tiles in one row");
        assert!(mm(&t, l.tiles[0].w) >= MIN_ROW_TILE_MM);
        assert_eq!(l.sliders[0].y, l.sliders[1].y, "light and warmth side by side");
        assert!(l.sliders[0].right() < l.sliders[1].x);
        let h = mm(&t, l.panel.h);
        assert!((36.0..=41.0).contains(&h), "about 40 mm, not {h}");
    }
    // Without warmth, light takes the whole row.
    let l = DrawerLayout::new(&t, screen, 1, 6, false, false);
    assert_eq!(l.sliders.len(), 1);
    assert_eq!(l.sliders[0].w, l.status.w);
}

#[test]
fn small_screens_scale_and_narrow_landscape_rows_wrap() {
    let t = Theme::for_dpi(167);
    let portrait = Rect::new(0, 0, 600, 800);
    for (tiles, shape) in [(5, vec![3, 2]), (6, vec![3, 3])] {
        let l = DrawerLayout::new(&t, portrait, 2, tiles, false, false);
        check_layout(&t, &l, portrait);
        assert_eq!(rows(&l).iter().map(Vec::len).collect::<Vec<_>>(), shape);
        assert!(l.panel.h * 100 / portrait.h < 60, "{}", l.panel.h);
    }
    let landscape = Rect::new(0, 0, 800, 600);
    let six = DrawerLayout::new(&t, landscape, 2, 6, false, false);
    check_layout(&t, &six, landscape);
    assert_eq!(rows(&six).len(), 2, "six tiles would be under 19 mm wide: wrap to 3 × 2");
    let five = DrawerLayout::new(&t, landscape, 2, 5, false, false);
    check_layout(&t, &five, landscape);
    assert_eq!(rows(&five).len(), 1, "five tiles still fit one row");
    assert!(mm(&t, five.tiles[0].w) >= MIN_ROW_TILE_MM);
    assert!(six.panel.h * 100 / landscape.h < 65);
    // 6" at 300 dpi.
    let t = Theme::for_dpi(300);
    let screen = Rect::new(0, 0, 1072, 1448);
    check_layout(&t, &DrawerLayout::new(&t, screen, 1, 6, false, false), screen);
}

#[test]
fn no_frontlight_reading_leaves_out_the_slider_rows() {
    let t = Theme::for_dpi(300);
    let screen = Rect::new(0, 0, 1272, 1696);
    let none = DrawerLayout::new(&t, screen, 0, 5, false, false);
    check_layout(&t, &none, screen);
    assert!(none.sliders.is_empty());
    assert_eq!(none.dividers.len(), 1);
    assert!(none.panel.h < DrawerLayout::new(&t, screen, 1, 5, false, false).panel.h);
}

pub(crate) fn status(running: bool, wifi: bool) -> ssh::Status {
    ssh::Status {
        running,
        unavailable: None,
        address: wifi.then(|| "10.0.0.2".to_owned()),
        hostname: (running && wifi).then(|| "verso-2".to_owned()),
    }
}

fn net(wifi: Option<bool>, running: bool) -> NetState {
    NetState {
        wifi,
        probing: false,
        wifi_pending: None,
        ssh: status(running, wifi == Some(true)),
        ssh_pending: None,
        finished: 0,
    }
}

const LIGHT: Frontlight = Frontlight { brightness: 12, warmth: Some(6) };

fn drawer(developer: bool, net: NetState) -> (Theme, TopDrawer) {
    let t = Theme::for_dpi(300);
    let d = TopDrawer::new(
        &t,
        Rect::new(0, 0, 1272, 1696),
        Orientation::Portrait,
        developer,
        Some(LIGHT),
        net,
        None,
        Instant::now(),
    );
    (t, d)
}

fn tap(d: &mut TopDrawer, p: Point) -> Option<DrawerEvent> {
    assert_eq!(d.on_gesture(&Gesture::Down(p)), None);
    d.on_gesture(&Gesture::Tap(p))
}

fn tap_tile(d: &mut TopDrawer, tile: Tile) -> Option<DrawerEvent> {
    let p = tile_at(d, tile);
    tap(d, p)
}

fn tile_at(d: &TopDrawer, tile: Tile) -> Point {
    let i = d.tile_kinds().iter().position(|&k| k == tile).expect("tile shown");
    d.layout().tiles[i].center()
}

#[test]
fn ssh_tile_shows_in_developer_mode_or_while_the_server_runs() {
    let (t, mut d) = drawer(false, net(Some(true), false));
    assert_eq!(d.tile_kinds(), [Tile::Wifi, Tile::Orientation, Tile::Refresh, Tile::Transfer, Tile::Settings]);
    assert_eq!(d.tile_view(Tile::Transfer).2, "Wi-Fi transfer");
    assert_eq!(d.tile_view(Tile::Transfer).3, "Books & files");
    let (_, dev) = drawer(true, net(Some(true), false));
    assert_eq!(dev.tile_kinds().len(), 6);
    assert_eq!(dev.tile_kinds()[1], Tile::Ssh);
    assert_eq!(dev.tile_view(Tile::Transfer).2, "Transfer");
    // A server started elsewhere shows the tile, which then stays after it stops.
    let height = d.rect().h;
    d.set_net(&t, net(Some(true), true), Instant::now());
    assert_eq!(d.tile_kinds().len(), 6);
    assert_eq!(d.rect().h, height);
    d.set_net(&t, net(Some(true), false), Instant::now());
    assert_eq!(d.tile_kinds().len(), 6, "turning SSH off doesn't make its tile vanish");
}

#[test]
fn tile_states_follow_wifi_ssh_and_orientation() {
    let (_, mut d) = drawer(true, net(None, false));
    d.net.probing = true;
    assert_eq!(d.tile_view(Tile::Wifi).0, TileState::Off);
    assert_eq!(d.tile_view(Tile::Wifi).3, "Checking…");
    d.net.probing = false;
    assert_eq!(d.tile_view(Tile::Wifi).3, "Unknown");
    d.net = net(Some(false), false);
    assert_eq!((d.tile_view(Tile::Wifi).0, d.tile_view(Tile::Wifi).3.as_str()), (TileState::Off, "Off"));
    d.net.wifi_pending = Some(true);
    assert_eq!((d.tile_view(Tile::Wifi).0, d.tile_view(Tile::Wifi).3.as_str()), (TileState::Pending, "Turning on…"));
    d.net = net(Some(true), false);
    assert_eq!((d.tile_view(Tile::Wifi).0, d.tile_view(Tile::Wifi).3.as_str()), (TileState::On, "On"));
    d.net.wifi_pending = Some(false);
    assert_eq!(d.tile_view(Tile::Wifi).3, "Turning off…");
    assert_eq!((d.tile_view(Tile::Ssh).0, d.tile_view(Tile::Ssh).3.as_str()), (TileState::Off, "Off"));
    d.net.ssh_pending = Some(true);
    assert_eq!((d.tile_view(Tile::Ssh).0, d.tile_view(Tile::Ssh).3.as_str()), (TileState::Pending, "Starting…"));
    d.net = net(Some(true), true);
    assert_eq!((d.tile_view(Tile::Ssh).0, d.tile_view(Tile::Ssh).3.as_str()), (TileState::On, "On · :2222"));
    d.net.ssh_pending = Some(false);
    assert_eq!(d.tile_view(Tile::Ssh).3, "Stopping…");
    assert_eq!(d.tile_view(Tile::Orientation), (TileState::Off, Icon::Portrait, "Orientation", "Portrait".into()));
    d.orientation = Orientation::Landscape;
    assert_eq!(d.tile_view(Tile::Orientation), (TileState::On, Icon::Landscape, "Orientation", "Landscape".into()));
    assert_eq!(d.tile_view(Tile::Refresh).0, TileState::Off);
}

#[test]
fn status_strip_names_the_ssh_address_while_it_runs() {
    let (_, mut d) = drawer(true, net(Some(true), false));
    assert_eq!(d.net_note(), None);
    d.net = net(Some(true), true);
    assert_eq!(d.net_note().as_deref(), Some("verso-2.local · 10.0.0.2"));
    d.net.ssh.hostname = None;
    assert_eq!(d.net_note().as_deref(), Some("10.0.0.2"));
    d.net.ssh.address = None;
    assert_eq!(d.net_note().as_deref(), Some("Waiting for Wi-Fi…"));
    d.net.wifi = Some(false);
    assert_eq!(d.net_note().as_deref(), Some("Wi-Fi is off"));
}

#[test]
fn tiles_ask_for_their_actions_and_pending_tiles_ignore_taps() {
    let (_, mut d) = drawer(true, net(Some(false), false));
    assert_eq!(tap_tile(&mut d, Tile::Wifi), Some(DrawerEvent::Wifi(true)));
    assert_eq!(tap_tile(&mut d, Tile::Ssh), Some(DrawerEvent::Ssh(true)));
    assert_eq!(tap_tile(&mut d, Tile::Orientation), Some(DrawerEvent::Orientation(Orientation::Landscape)));
    assert_eq!(tap_tile(&mut d, Tile::Refresh), Some(DrawerEvent::Refresh));
    assert_eq!(tap_tile(&mut d, Tile::Transfer), Some(DrawerEvent::Transfer));
    assert_eq!(tap_tile(&mut d, Tile::Settings), Some(DrawerEvent::Settings));
    d.net.wifi_pending = Some(true);
    d.net.ssh_pending = Some(true);
    assert_eq!(tap_tile(&mut d, Tile::Wifi), None, "a switch in flight ignores taps");
    assert_eq!(tap_tile(&mut d, Tile::Ssh), None);
    d.net = net(None, false);
    d.net.probing = true;
    assert_eq!(tap_tile(&mut d, Tile::Wifi), None, "still checking");
    d.net.probing = false;
    assert_eq!(tap_tile(&mut d, Tile::Wifi), Some(DrawerEvent::Probe), "unreadable: read again");
    d.net = net(Some(true), true);
    assert_eq!(tap_tile(&mut d, Tile::Wifi), Some(DrawerEvent::Wifi(false)));
    assert_eq!(tap_tile(&mut d, Tile::Ssh), Some(DrawerEvent::Ssh(false)));
    d.net = net(Some(true), false);
    d.net.ssh.unavailable = Some("not installed");
    assert_eq!(tap_tile(&mut d, Tile::Ssh), None);
    // Sliding off a tile cancels it.
    let p = tile_at(&d, Tile::Refresh);
    d.on_gesture(&Gesture::Down(p));
    d.on_gesture(&Gesture::Move(Point::new(p.x + 200, p.y)));
    assert_eq!(d.on_gesture(&Gesture::Cancel(Point::new(p.x + 200, p.y))), None);
}

#[test]
fn swipe_up_and_taps_below_close_but_taps_on_the_panel_do_not() {
    let (_, mut d) = drawer(false, net(Some(true), false));
    let panel = d.rect();
    assert_eq!(tap(&mut d, Point::new(600, panel.bottom() + 200)), Some(DrawerEvent::Close));
    let (status, handle) = (d.layout().status.center().y, d.layout().handle.y);
    assert_eq!(tap(&mut d, Point::new(600, status)), None);
    assert_eq!(tap(&mut d, Point::new(600, handle)), None);
    let up = |from: Point| Gesture::Swipe { dir: Direction::Up, from, to: Point::new(from.x, 10) };
    for from in [Point::new(600, panel.bottom() - 20), Point::new(600, panel.bottom() + 300)] {
        d.on_gesture(&Gesture::Down(from));
        assert_eq!(d.on_gesture(&up(from)), Some(DrawerEvent::Close), "from {from:?}");
    }
    let from = Point::new(600, 50);
    d.on_gesture(&Gesture::Down(from));
    let down = Gesture::Swipe { dir: Direction::Down, from, to: Point::new(600, 900) };
    assert_eq!(d.on_gesture(&down), None, "pulling down again does nothing");
}

#[test]
fn light_sliders_drag_and_report_the_frontlight() {
    let (t, mut d) = drawer(false, net(Some(true), false));
    let light = row_parts(&t, d.layout().sliders[0], false).slider;
    let warmth = row_parts(&t, d.layout().sliders[1], false).slider;
    let right = Point::new(light.right() - 2, light.center().y);
    assert_eq!(
        d.on_gesture(&Gesture::Down(right)),
        Some(DrawerEvent::Light(Frontlight { brightness: 24, warmth: Some(6) }))
    );
    let left = Point::new(light.x + 2, light.center().y);
    assert_eq!(
        d.on_gesture(&Gesture::Move(left)),
        Some(DrawerEvent::Light(Frontlight { brightness: 0, warmth: Some(6) }))
    );
    // Lifting with an upward flick finishes the drag; it doesn't close.
    let lift = Gesture::Swipe { dir: Direction::Up, from: right, to: Point::new(left.x, 10) };
    assert_eq!(d.on_gesture(&lift), Some(DrawerEvent::Light(Frontlight { brightness: 0, warmth: Some(6) })));
    let mid = Point::new(warmth.center().x, warmth.center().y);
    assert_eq!(
        d.on_gesture(&Gesture::Down(mid)),
        Some(DrawerEvent::Light(Frontlight { brightness: 0, warmth: Some(12) }))
    );
    assert_eq!(
        d.on_gesture(&Gesture::Tap(mid)),
        Some(DrawerEvent::Light(Frontlight { brightness: 0, warmth: Some(12) }))
    );
}

#[test]
fn late_frontlight_reading_adds_the_sliders_once() {
    let t = Theme::for_dpi(300);
    let screen = Rect::new(0, 0, 1272, 1696);
    let mut d =
        TopDrawer::new(&t, screen, Orientation::Portrait, false, None, net(Some(true), false), None, Instant::now());
    assert!(d.layout().sliders.is_empty());
    let short = d.rect().h;
    assert!(d.set_frontlight(&t, LIGHT));
    assert_eq!(d.layout().sliders.len(), 2);
    assert!(d.rect().h > short);
    assert!(!d.set_frontlight(&t, Frontlight { brightness: 1, warmth: None }));
    assert_eq!(d.light.as_ref().unwrap().value, 12.0);
}

#[test]
fn drawing_shows_on_pending_and_pressed_tiles() {
    let (t, mut d) = drawer(true, net(Some(true), true));
    d.net.ssh_pending = Some(false);
    let mut canvas = Canvas::new(1272, 1696);
    canvas.fill(0x77);
    let (mut text, mut icons, mut hints) = (TextRenderer::new(), IconRenderer::default(), Vec::new());
    let refresh = tile_at(&d, Tile::Refresh);
    d.on_gesture(&Gesture::Down(refresh));
    {
        let mut cx = DrawCx::new(&mut canvas, &mut text, &mut icons, &t, &mut hints);
        d.draw(&mut cx);
    }
    let px = |p: Point| canvas.gray_data()[p.y as usize * 1272 + p.x as usize];
    let corner = |tile: Tile| {
        let i = d.tile_kinds().iter().position(|&k| k == tile).unwrap();
        let r = d.layout().tiles[i];
        Point::new(r.x + t.mm(2.0), r.y + t.mm(1.0))
    };
    assert_eq!(px(corner(Tile::Wifi)), INK, "Wi-Fi on: solid black");
    assert_eq!(px(corner(Tile::Ssh)), FILL, "SSH stopping: light gray");
    assert_eq!(px(corner(Tile::Orientation)), PAPER, "portrait: white");
    assert_eq!(px(corner(Tile::Refresh)), INK, "pressed: inverted");
    assert!(hints.iter().any(|h| h.mode == RefreshMode::Fast && h.rect.contains(refresh)));
    assert_eq!(px(Point::new(600, d.rect().bottom() + 10)), 0x77, "nothing below the drawer");
    assert_eq!(px(Point::new(600, d.rect().bottom() - 2)), INK, "bottom border");
}

#[test]
fn incomplete_ssh_status_is_reread_for_a_while() {
    let ops = Arc::new(FakeNet::new(true, true));
    let worker = DrawerWorker::new(ops.clone(), Waker::new(|| {}));
    let mut state = net(Some(true), true);
    state.ssh.address = None;
    state.ssh.hostname = None;
    let (t, mut d) = drawer(true, state);
    let now = Instant::now();
    d.ssh_read_at = now;
    d.ssh_recheck_until = now + SSH_RECHECK_FOR;
    let due = d.ssh_recheck_due().expect("rechecking");
    assert!(due <= now + SSH_RECHECK);
    assert!(d.deadline().unwrap() <= due);
    assert!(d.on_tick(due, &worker), "the address arrived");
    assert_eq!(d.net().ssh.hostname.as_deref(), Some("verso-2"));
    assert!(d.ssh_recheck_due().is_none(), "complete: no more re-reads");
    // Past the window it stops re-reading, until a switch finishes.
    d.net.ssh.address = None;
    d.ssh_read_at = d.ssh_recheck_until;
    assert!(d.ssh_recheck_due().is_none());
    let mut finished = d.net.clone();
    finished.finished += 1;
    d.set_net(&t, finished, now);
    assert!(d.ssh_recheck_due().is_some());
}

/// Wi-Fi and SSH in memory. With a gate, each switch waits for a permit.
pub(crate) struct FakeNet {
    wifi: Mutex<bool>,
    ssh: Mutex<bool>,
    pub fail_stop: AtomicBool,
    pub fail_wifi: AtomicBool,
    pub calls: Mutex<Vec<String>>,
    gate: Mutex<Option<mpsc::Receiver<()>>>,
}

impl FakeNet {
    pub fn new(wifi: bool, ssh: bool) -> Self {
        Self {
            wifi: Mutex::new(wifi),
            ssh: Mutex::new(ssh),
            fail_stop: AtomicBool::new(false),
            fail_wifi: AtomicBool::new(false),
            calls: Mutex::new(Vec::new()),
            gate: Mutex::new(None),
        }
    }
    /// From now on each switch waits for one message on the returned sender.
    pub fn gated(&self) -> mpsc::Sender<()> {
        let (tx, rx) = mpsc::channel();
        *self.gate.lock().unwrap() = Some(rx);
        tx
    }
    fn pass(&self, call: String) {
        if let Some(rx) = &*self.gate.lock().unwrap() {
            rx.recv_timeout(Duration::from_secs(10)).expect("permit");
        }
        self.calls.lock().unwrap().push(call);
    }
    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

impl NetOps for FakeNet {
    fn wifi_enabled(&self) -> anyhow::Result<bool> {
        Ok(*self.wifi.lock().unwrap())
    }
    fn set_wifi(&self, on: bool) -> anyhow::Result<()> {
        self.pass(format!("wifi {on}"));
        anyhow::ensure!(!self.fail_wifi.load(Ordering::SeqCst), "wifid said no");
        *self.wifi.lock().unwrap() = on;
        Ok(())
    }
    fn ssh_status(&self) -> ssh::Status {
        status(*self.ssh.lock().unwrap(), *self.wifi.lock().unwrap())
    }
    fn ssh_start(&self, hostname: &str) -> anyhow::Result<()> {
        self.pass(format!("ssh start {hostname}"));
        *self.ssh.lock().unwrap() = true;
        Ok(())
    }
    fn ssh_stop(&self) -> anyhow::Result<()> {
        self.pass("ssh stop".into());
        anyhow::ensure!(!self.fail_stop.load(Ordering::SeqCst), "dropbear won't stop");
        *self.ssh.lock().unwrap() = false;
        Ok(())
    }
}

/// A waker that counts and lets the test wait for the next wake.
fn waker() -> (Waker, Arc<AtomicUsize>, mpsc::Receiver<()>) {
    let (tx, rx) = mpsc::channel();
    let tx = Mutex::new(tx);
    let count = Arc::new(AtomicUsize::new(0));
    let counted = count.clone();
    let waker = Waker::new(move || {
        counted.fetch_add(1, Ordering::SeqCst);
        let _ = tx.lock().unwrap().send(());
    });
    (waker, count, rx)
}

fn woken(rx: &mpsc::Receiver<()>) {
    rx.recv_timeout(Duration::from_secs(10)).expect("the worker wakes the UI");
}

#[test]
fn worker_probes_then_turns_wifi_off_after_stopping_ssh() {
    let ops = Arc::new(FakeNet::new(true, true));
    let (waker, wakes, rx) = waker();
    let mut worker = DrawerWorker::new(ops.clone(), waker);
    let state = worker.state();
    assert_eq!((state.wifi, state.ssh.running), (None, true), "SSH is read at once, Wi-Fi in the background");
    worker.probe();
    assert!(worker.state().probing && worker.busy());
    woken(&rx);
    let state = worker.state();
    assert_eq!((state.wifi, state.probing, state.finished), (Some(true), false, 1));
    assert!(!worker.busy());
    let gate = ops.gated();
    assert!(worker.set_wifi(false));
    assert_eq!(worker.state().wifi_pending, Some(false));
    assert!(!worker.set_wifi(true), "one Wi-Fi switch at a time");
    worker.probe();
    assert!(!worker.state().probing, "no reading while switching");
    gate.send(()).unwrap();
    gate.send(()).unwrap();
    woken(&rx);
    assert_eq!(ops.calls(), ["ssh stop", "wifi false"]);
    let state = worker.state();
    assert_eq!((state.wifi, state.wifi_pending, state.ssh.running), (Some(false), None, false));
    assert!(worker.take_errors().is_empty());
    assert_eq!(wakes.load(Ordering::SeqCst), 2);
}

#[test]
fn worker_leaves_wifi_on_when_ssh_wont_stop_and_reports_failures() {
    let ops = Arc::new(FakeNet::new(true, true));
    ops.fail_stop.store(true, Ordering::SeqCst);
    let (waker, _, rx) = waker();
    let mut worker = DrawerWorker::new(ops.clone(), waker);
    worker.set_wifi(false);
    woken(&rx);
    assert_eq!(ops.calls(), ["ssh stop"], "Wi-Fi is left alone");
    let state = worker.state();
    assert_eq!((state.wifi, state.ssh.running, state.wifi_pending), (Some(true), true, None));
    assert_eq!(worker.take_errors(), ["Couldn't stop SSH. Wi-Fi left on."]);
    assert!(worker.take_errors().is_empty(), "each failure is shown once");
    ops.fail_wifi.store(true, Ordering::SeqCst);
    *ops.ssh.lock().unwrap() = false;
    worker.set_wifi(false);
    woken(&rx);
    assert_eq!(worker.take_errors(), ["Couldn't turn Wi-Fi off"]);
    assert_eq!(worker.state().wifi, Some(true));
}

#[test]
fn worker_runs_switches_in_order_and_keeps_them_pending_until_done() {
    let ops = Arc::new(FakeNet::new(true, false));
    let (waker, _, rx) = waker();
    let mut worker = DrawerWorker::new(ops.clone(), waker);
    let gate = ops.gated();
    assert!(worker.set_ssh(true, "verso-2".into()));
    assert!(!worker.set_ssh(false, "verso-2".into()), "one SSH switch at a time");
    assert!(worker.set_wifi(false));
    let state = worker.state();
    assert_eq!((state.ssh_pending, state.wifi_pending), (Some(true), Some(false)));
    // While the first switch waits, nothing has changed.
    std::thread::sleep(Duration::from_millis(30));
    assert!(ops.calls().is_empty());
    gate.send(()).unwrap();
    woken(&rx);
    let state = worker.state();
    assert_eq!((state.ssh_pending, state.ssh.running), (None, true));
    assert_eq!(state.ssh.hostname.as_deref(), Some("verso-2"));
    assert_eq!(state.wifi_pending, Some(false), "Wi-Fi waits its turn");
    gate.send(()).unwrap();
    gate.send(()).unwrap();
    woken(&rx);
    assert_eq!(ops.calls(), ["ssh start verso-2", "ssh stop", "wifi false"]);
    let state = worker.state();
    assert_eq!((state.wifi, state.ssh.running, state.finished), (Some(false), false, 2));
}

#[test]
fn dropping_the_worker_lets_a_switch_in_progress_finish() {
    let ops = Arc::new(FakeNet::new(false, false));
    let mut worker = DrawerWorker::new(ops.clone(), Waker::new(|| {}));
    let gate = ops.gated();
    worker.set_wifi(true);
    let release = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        gate.send(()).unwrap();
    });
    drop(worker);
    release.join().unwrap();
    assert_eq!(ops.calls(), ["wifi true"]);
    assert!(ops.wifi_enabled().unwrap());
}

#[test]
fn claimed_hostname_is_remembered() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let store = Store::at(dir.path().to_path_buf());
    let before = store.settings().hostname;
    remember_hostname(&store, &status(true, false));
    assert_eq!(store.settings().hostname, before, "no name yet: nothing saved");
    remember_hostname(&store, &status(true, true));
    assert_eq!(store.settings().hostname, "verso-2");
    Ok(())
}

#[test]
fn in_book_drawer_header_geometry_and_actions_in_portrait_landscape_and_small() {
    let book = || Some(TopDrawerBook { title: "Ender's Game".into() });
    for (dpi, screen, orientation, head_range) in [
        (300, Rect::new(0, 0, 1272, 1696), Orientation::Portrait, 11.0..=12.0),
        (300, Rect::new(0, 0, 1696, 1272), Orientation::Landscape, 9.3..=10.0),
        (167, Rect::new(0, 0, 600, 800), Orientation::Portrait, 11.0..=12.0),
    ] {
        let t = Theme::for_dpi(dpi);
        let mut d =
            TopDrawer::new(&t, screen, orientation, false, Some(LIGHT), net(Some(true), false), book(), Instant::now());
        check_layout(&t, d.layout(), screen);
        let head_mm = mm(&t, d.layout().status.h);
        assert!(head_range.contains(&head_mm), "{screen:?}: head {head_mm} mm");
        assert!(d.layout().ssh_note.is_none());
        let back = d.back_rect().expect("back button");
        let search = d.search_rect().expect("search button");
        assert!(back.right() < search.x && search.right() < d.layout().status.right());
        assert!(back.y >= 0 && back.bottom() <= d.layout().dividers[0]);
        assert!(search.y >= 0 && search.bottom() <= d.layout().dividers[0]);
        assert_eq!(tap(&mut d, back.center()), Some(DrawerEvent::CloseBook));
        assert_eq!(tap(&mut d, search.center()), Some(DrawerEvent::Search));
        // Over half the page stays visible in Paperwhite portrait and landscape.
        if dpi == 300 {
            assert!(d.rect().h * 2 < screen.h, "{screen:?}: panel {}", d.rect().h);
        }
        // Enabling SSH in developer mode adds the compact subline below the header.
        let before_h = d.rect().h;
        d.set_net(&t, net(Some(true), true), Instant::now());
        let note = d.layout().ssh_note.expect("SSH subline appears while running");
        assert!(note.y >= d.layout().status.bottom() && note.bottom() <= d.layout().dividers[0]);
        assert!(d.rect().h > before_h);
        // Drawing succeeds cleanly with book header, battery, clock, and SSH subline.
        let mut canvas = Canvas::new(screen.w as u32, screen.h as u32);
        let (mut text, mut icons, mut hints) = (TextRenderer::new(), IconRenderer::default(), Vec::new());
        let mut cx = DrawCx::new(&mut canvas, &mut text, &mut icons, &t, &mut hints);
        cx.battery = Some(crate::platform::Battery { percent: 82, charging: false });
        d.draw(&mut cx);
    }
    // Home/Library drawer has no back/search buttons.
    let (_, home) = drawer(false, net(Some(true), false));
    assert!(home.back_rect().is_none() && home.search_rect().is_none());
}
