# Kindle Notes

Hardware, firmware and integration facts for running Verso on jailbroken
Kindles. Facts observed on our test device (Kindle Paperwhite 6, 12th gen 2024,
firmware 5.18.5.0.1, kernel `5.15.41-lab126`) are marked **(measured)**;
behaviour on other models is **(to verify)**.

---

## 1. Target ABI, firmware and models

- **Build target:** `armv7-unknown-linux-gnueabihf`, glibc **2.20** (floor for
  i.MX armhf models like Oasis 3), `-C target-cpu=cortex-a9` (`.cargo/config.toml`
  and `crengine-sys` `-mcpu=cortex-a9`). Cortex-A9 enables NEON/VFPv3-D32
  without emitting VFPv4 or hardware integer division that i.MX6SLL models
  (PW4, Basic 3) lack. `.cargo/arm-glibc-relplt.ld` keeps `.rel.plt` immediately
  after `.rel.dyn` so LLD does not place `.ARM.exidx` between them, avoiding an
  instant `SIGSEGV` in glibc ≤ 2.22 `ld.so` under Zig's default `-z now` (glibc
  bug #14341).
- **Hard-float firmware only (5.16.3+):** requires `/lib/ld-linux-armhf.so.3`.
  `verso.sh` checks for it before pausing the Kindle UI; if absent, it prints
  via `eips` (avoiding apostrophes, which `eips` cannot draw), saves a problem
  report and exits without altering the UI. Older soft-float Kindles are not
  supported.
- **Self-contained binary:** system libraries under `/usr/lib` are not relied on
  because firmware updates can change them; crengine, FreeType, HarfBuzz,
  Hunspell, image codecs and FBInk are statically linked (with Zig's static
  `libc++`).
- **Tested device (measured):** Paperwhite 6 (*Sangria*, platform *Bellatrix4*,
  MediaTek MT8113, ~957 MB RAM / ~620 MB free with Kindle UI, 1272×1696 @ 300
  dpi Y8, `hwtcon_v2`, white+warm 0..=24).
- **CPU (measured):** two cores, `ondemand` governor at 600 MHz–2 GHz. A page
  turn's short burst mostly runs at 600–1000 MHz, so CPU-bound steps (the
  framebuffer copy, the landscape turn) slow down when less work comes before
  them, and timings swing between sessions: compare builds in one session.
- **Model table ([kindle_profiles.rs](../crates/verso/src/platform/kindle_profiles.rs)):**
  19 Kindle models keyed by `FBInkState.device_id` with lights, ranges, page
  buttons and EPDC (`Imx` up to 2019 vs `Mtk` 2021+). Untested or unknown models
  run with a one-time notice; unknown IDs probe `flIntensity` /
  `currentAmberLevel`. Unsupported framebuffers or missing touchscreens draw a
  centred FBInk message and exit **`88`** (`EXIT_UNSUPPORTED`); the launcher
  saves a problem report, waits 8 s and restores Amazon's UI.

---

## 2. Display and FBInk

- Verso opens `/dev/fb0` through FBInk, writes directly into `fbink_get_fb_pointer`
  and submits dirty rects with `fbink_refresh(fd, y, x, w, h, &cfg)`.
- **Trust FBInk's viewport, not marketing specs:** PW6 specs list 1264×1680,
  while the real visible framebuffer is **1272×1696** with stride 1272 and
  virtual height 3392 **(measured)**. Init FBInk with `no_viewport = true` and
  subtract `view_vert_offset` from `view_vert_origin`: FBInk otherwise bakes
  bitmap-row centering into `view_vert_origin` (`+4` px on 1448 px panels, `+8`
  on 1648/1680/2480 px), pushing full refreshes past `yres` (MTK returns `EBADF`).
- **Pixel formats:** accepts 8-bit (`Y8`, honouring `inverted_grayscale`) and
  32-bit (`RGBA`/`RGB32`, `BGRA`/`BGR32`). On 32-bit panels (Colorsoft),
  `Writer` swizzles R↔B row by row straight from the canvas and never reads
  back from write-combining `/dev/fb0`; landscape first turns the frame into a
  cached-RAM shadow buffer.
- **MediaTek `hwtcon_v2` (measured):** `is_mtk = 1`, `canRotate = 0` (landscape
  is rotated in software), `hasEclipseWfm = 1`, `canHWInvert = 1`.
- **Waveforms & completion:** monochrome uses `DU` (`Fast`), `GL16` (`Ui`),
  `REAGL` (`Page`) and flashing `GC16` (`Full`). Color panels use `GLRC16`
  (`Page`) and `GCC16` (`Full`) with `CFA_DEFAULT` **(to verify)**. Full flashes
  take ~200–400 ms, varying by session **(measured)**, and are awaited lazily via `fbink_get_last_marker()` /
  `fbink_wait_for_complete()` before the next framebuffer write, sleep or exit.
- **ARM alignment gotcha (measured):** crengine's glyph cache uses `alignas(16)`
  items that upstream allocates with 8-byte-aligned `malloc`, causing `SIGBUS`
  on NEON stores under Zig/Clang. `crates/crengine-sys/build.rs` patches a copy
  to use `posix_memalign`.

---

## 3. Input (evdev)

- Scans `/dev/input/event*` (`O_NONBLOCK | O_CLOEXEC`) for `ABS_MT_POSITION_X/Y`
  (PW6: `/dev/input/event1` `pt_mt`, raw `0..1271 × 0..1695`, no axis swap or
  mirroring **(measured)**).
- **MT Protocol B (measured):** uses `ABS_MT_SLOT`, `ABS_MT_TRACKING_ID` (`-1`
  on lift), `ABS_MT_POSITION_X/Y`, `SYN_REPORT` and `SYN_DROPPED`. PW6 emits no
  `BTN_TOUCH` and no single-touch `ABS_X/Y`.
- **Exclusive grab (`EVIOCGRAB`):** prevents Amazon's paused UI from queueing
  taps; the kernel releases it automatically on crash. Re-grabbing an
  already-held fd returns **`EBUSY`** **(measured)**, so `TouchInput` tracks
  grab state explicitly. The grab is released for Amazon's native screensaver
  and reacquired on wake; book-cover sleep keeps it.
- **Coordinate resync (measured):** evdev suppresses unchanged axes even across
  `EVIOCGRAB`. On startup, wake and `SYN_DROPPED`, `reset_contacts()` queries
  `EVIOCGMTSLOTS` and `EVIOCGABS(ABS_MT_SLOT)` so the first tap after wake does
  not land at zero on an unchanged axis.
- **Page-turn buttons:** probed only on profiles with `page_buttons` (or
  unknown models). Maps `KEY_PAGEUP` (104) / `KEY_F23` (193) → previous and
  `KEY_PAGEDOWN` (109) / `KEY_YEN` (124) → next, swapped in landscape **(to
  verify on Oasis 3)**. Any device that also exposes `KEY_POWER` (116) is
  **never grabbed**, so `powerd` keeps the power button.
- **Remotes** (rules in `platform/remote.rs`): the Kindle Click's virtual
  `kindle-input-pt` is absent on PW6 fw 5.18.5.0.1 **(measured)**, present on
  Basic 5 fw 5.20.1. Remotes are never grabbed; on PW6 Xorg doesn't open
  them **(measured)**, but HID Passthrough's udev rule can make it **(to verify)**.
  They come and go under any `eventN`;
  inotify on `/dev/input` (tmpfs) sees udev add them **(measured)**. uinput is at
  `/dev/input/uinput` on PW6, not `/dev/uinput` **(measured)**.

---

## 4. Launcher and Amazon UI coexistence

[verso.sh](../dist/kindle/verso/verso.sh) runs alongside Amazon's framework
rather than stopping it:

- **Self-copy & lock:** copies itself to `/tmp/verso-launcher-$$.sh` and
  `exec`s it so deploys or in-app updates overwriting `/mnt/us/verso/verso.sh`
  (FUSE) cannot break teardown. Uses an atomic `/tmp` hard-link lock and refuses
  to start if Verso or KOReader (`reader.lua`) is running or `powerd state !=
  active`.
- **Log:** `verso.log` times are UTC. `verso/running` (boot id, start time)
  exists during a session; the next launch logs one left without an end, and
  whether the Kindle restarted.
- **Pause order** (flags set *before* acting so `trap restore EXIT` always
  undoes them): save `/dev/fb0` to `/tmp/verso-fb.dump`, disable `pillow`
  (`com.lab126.pillow disableEnablePillow disable`), `SIGSTOP` `awesome`,
  `SIGSTOP` `JunoStatusBarDriver` (separate clock/status-bar X window
  `com.lab126.gtkstatusbar` on PW6 **(measured)**; `/proc/comm` truncated to
  `JunoStatusBarDr`), `SIGSTOP` `volumd` (keeps `/mnt/us` mounted if USB is
  plugged in).
- **Restore order** (ignores signals): `SIGCONT` `JunoStatusBarDriver` and
  `awesome`, restore `/tmp/verso-fb.dump`, re-enable `pillow`, ask `appmgrd` for
  `app://com.lab126.booklet.home`, clean up any stray `preventScreenSaver` or
  port-80 firewall markers, finish all log writes, **then `SIGCONT volumd`
  last** (a pending USB plug may unmount `/mnt/us` immediately).
- **Native sleep handoff:** Verso sends `SIGUSR1` to resume `awesome` for
  Amazon's screensaver and `SIGUSR2` to pause it on wake, waiting for
  `$VERSO_SYSTEM_ACK`. *(Known issue: native Amazon wake has reproduced a
  stale Home frame over the reader until the next turn; book-cover sleep
  restores cleanly **(measured)**.)*
- **Exit codes & updates:** `0` normal, `85` install staged update, `86` go
  back to `update/previous/`, `87` restart without saving (protocol 2, after
  developer live-data edits), `88` unsupported hardware. Updates use empty
  renamed journal files (`journal-install` / `rollback` / `cleanup`) + `sync`
  and a 120 s trial waiting for `/tmp/verso-started`; crash, hang or two
  interrupted starts roll back automatically. Non-zero/signal/panic exits write
  a crash report via `tools/report.sh`.
- **Direct library scriptlet (`documents/Verso.sh`, measured):** `SH_Integration`
  (PW6 5.18.5.0.1) scans only the first 6 lines (`# Name:`, `# Author:`,
  `# DontUseFBInk`, `# Icon: data:image/…` base64 only), unloads via `kill -9`
  on pause and loops `sleep(1)` → `waitpid` → `popAppHistory`. `Verso.sh`
  detaches via `setsid`, exits `0` and waits for `sh_integration_launcher` to
  exit and `activeApp` to clear (+200 ms) before `exec`ing `verso.sh`, avoiding
  both mid-restore `SIGKILL` and frozen-`awesome` `popAppHistory` wedges.
- **KPM `.kpkg` (5.19.4+):** KUAL is broken on 5.19.4+ and `.sh` indexing can
  be unreliable there. Releases ship `verso-<version>.kpkg` (manifest v2,
  `kindlehf`, `[major, minor, patch]`) with POSIX `sh` hooks (`launch.sh`
  shares the `SH_Integration` handoff; `uninstall.sh` keeps `verso/data/` and `verso/ssh/`).

---

## 5. Power, frontlight, battery and USB

- **Power events:** child `lipc-wait-event -m com.lab126.powerd`
  (`PR_SET_PDEATHSIG(SIGTERM)`) streams `goingToScreenSaver`,
  `outOfScreenSaver`, `exitingScreenSaver`, `wakeupFromSuspend`, `charging`,
  `notCharging` and `battLevelChanged`.
- **Late wake banner cleanup:** every wake schedules a forced `Ui` rewrite of
  the panel's bottom 10% after **1.5 s** (rescheduled by `exitingScreenSaver` /
  `wakeupFromSuspend`) to clear Amazon's post-hibernation banner. Short wake
  verified **(measured)**; overnight hibernation remains **(to verify)**.
- **Frontlight:** controlled through `lipc` (`com.lab126.powerd flIntensity`
  and `currentAmberLevel`, 0..=24 **(measured)**) rather than writing sysfs
  directly, so Verso does not fight `powerd`. Read off-thread at startup.
- **Keep-awake gotcha (measured):** `preventScreenSaver=1` (used during update
  downloads, guarded by `update/awake`) makes `powerd` ignore the power button.
  Wi-Fi transfer therefore calls `touchScreenSaverTimeout 1` every 60 s instead,
  keeping the power button and magnetic cover working.
- **Battery & USB (measured):** reads `/sys/class/power_supply/` (<1 ms vs
  ~29 ms per `lipc-get-prop`): first `Battery` supply (`bd71827_bat`) for
  capacity/charging, first `Mains`/`USB`/`USB_DCP`/`USB_CDP` supply
  (`bd71827_ac`) for cable presence. Ignores `Wireless` and `USB_WET`
  (`max20342_moisture`). `USB_DCP` (wall charger) never triggers the computer
  prompt. Battery `status` turns `Charging` ~0.2 s *after* powerd's `charging`,
  and unplugging reads `Mains` online for ~0.1 s **(measured)**, so reads repeat
  after each cable event. Without sysfs: `battStateInfo`, `battLevel`, `isCharging`.

---

## 6. Wi-Fi, firewall, mDNS, SSH and diagnostics

- **Wi-Fi:** enables `com.lab126.cmd wirelessEnable` *before*
  `com.lab126.wifid enable` (reversed when disabling), then polls `wifid` up to
  2 s because it applies asynchronously **(measured)**. Reads `wlan0` IP via
  `getifaddrs` so gateway-less networks work. After wake, Wi-Fi takes ~50–60 s
  to reconnect **(measured)**.
- **Firewall:** Kindle drops inbound TCP/UDP by default. Verso adds/removes
  `iptables` `INPUT`/`OUTPUT` `conntrack` rules for port 2222 (SSH) and port 80
  (Wi-Fi transfer, cleaned up by `verso.sh` via `/tmp/verso-transfer-firewall`
  on crash), and `verso-mdns` manages UDP 5353 on `wlan0`.
- **Dropbear keys workaround (measured):** `/mnt/us` (FUSE `0777`), `/tmp` and
  `/tmp/root` all fail Dropbear's strict ancestor permission check. Verso copies
  `verso/ssh/authorized_keys` into a `0700` `/tmp/verso-ssh` folder and runs
  upstream Dropbear detached with `-F -D .` (relative path checks only `.`),
  sharing `/tmp/dropbear_koreader.pid` on port 2222 so Verso and KOReader see
  and can stop each other's server.
- **Hardware diagnostics & crash context:** [collect-diagnostics.sh](../scripts/collect-diagnostics.sh)
  is pure POSIX `sh` + BusyBox `awk`, bounds platform queries to 3 s, works
  without installing Verso, and exposes `--identity` (extracting only the
  non-PII 2–3 char model code from `/proc/usid` and the single firmware string
  from `/etc/prettyversion.txt`, never full serials). `verso.sh` logs that
  environment stamp, run duration and byte count, and on fatal-signal crashes
  appends filtered `dmesg` fault/OOM lines plus both ends (40 lines each) of an
  `LD_DEBUG=files,libs` probe when Verso died with 0 bytes logged. `logging.rs`
  logs fatal signals (registers, a raw backtrace) and dies with the first one,
  even if the handler faults. Never call glibc's `backtrace()` there: it loads
  the Kindle's libgcc_s, whose ARM unwinder crashes in our LLVM libunwind.
