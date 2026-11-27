#!/usr/bin/env python3
"""Isolated Kindle redraw comparison. See README.md before running."""
import argparse
import hashlib
import json
import re
import shlex
import subprocess
import time
from pathlib import Path

APP = '/mnt/us/verso/redraw-check'
SSH = ['ssh', '-o', 'ConnectTimeout=5', 'kindle']
Q = shlex.quote


def remote(command, data=None):
    return subprocess.run([*SSH, command], input=data, stdout=subprocess.PIPE,
                          stderr=subprocess.PIPE, check=True, timeout=55).stdout.decode().strip()


def upload(path, data):
    remote('cat > ' + Q(path), data)


def copy_dictionary_cache(source, state):
    """Seed an isolated state with prepared dictionary indexes. A fresh state
    otherwise builds every index in the background after the book opens (seconds
    of CPU on the dictionary thread), which only happens once on a real device."""
    assert re.fullmatch(r'/mnt/us/verso/[a-z0-9-]+-check/[A-Za-z0-9._-]+', source), source
    remote(f'test -d {Q(source)}/dictionary-cache && cp -r {Q(source)}/dictionary-cache {Q(state)}/')


def app_pid(executable, seconds=30):
    """The app's PID. `pidof verso` can also list the background pre-layout
    child (same executable, a verso parent) or the wrapper shell during exec."""
    until = time.monotonic() + seconds
    while time.monotonic() < until:
        found = remote('for p in $(pidof verso); do '
                       'pp=$(cut -d" " -f4 /proc/$p/stat 2>/dev/null); '
                       f'test "$(readlink /proc/$pp/exe)" = {Q(executable)} && continue; '
                       f'test "$(readlink /proc/$p/exe)" = {Q(executable)} && echo $p; done; true').split()
        if len(found) == 1:
            return found[0]
        time.sleep(.1)
    raise RuntimeError('No unique Verso app PID for ' + executable)


def patch_launcher(app_dir, prefix, repo=None):
    """Build an isolated copy of Verso's launcher, failing loudly if markers move."""
    root = Path(repo) if repo is not None else Path(__file__).resolve().parents[2]
    text = (root / 'dist/kindle/verso/verso.sh').read_text()
    app_marker = 'APP_DIR="${ROOT}/verso"'
    tmp_marker = '${TMP}/verso-launcher'
    assert app_marker in text, f'Missing {app_marker} in verso.sh'
    assert tmp_marker in text, f'Missing {tmp_marker} in verso.sh'
    return text.replace(app_marker, f'APP_DIR="{app_dir}"').replace(tmp_marker, prefix)


def wait_for(command, seconds=30):
    until = time.monotonic() + seconds
    while time.monotonic() < until:
        if remote(command + ' && echo yes || true') == 'yes':
            return
        time.sleep(.2)
    raise RuntimeError('Timed out: ' + command)


def ready():
    assert remote('lipc-get-prop com.lab126.powerd state; pidof verso || true; '
                  'pidof reader.lua || true') == 'active', 'Wake Kindle and close readers first'
    # Injected contacts reach Verso's grabbed input, not the paused Amazon UI.
    # Start with a fresh idle deadline without changing the normal sleep policy.
    remote('lipc-set-prop com.lab126.powerd touchScreenSaverTimeout 1')


def memory(pid):
    """Read process accounting; smaps sums also work without smaps_rollup."""
    status = remote(f'cat /proc/{pid}/status')
    values = {key: int(value) for key, value in
              re.findall(r'^(VmRSS|VmHWM|VmSize):\s+(\d+) kB$', status, re.M)}
    totals = remote(f"awk '/^(Rss|Pss|Private_Clean|Private_Dirty|Swap):/ "
                    f"{{sum[$1]+=$2}} END {{for (k in sum) print k, sum[k]}}' /proc/{pid}/smaps")
    values.update({key.rstrip(':'): int(value) for key, value in
                   (line.split() for line in totals.splitlines())})
    return values


def framebuffer_hash():
    return remote('head -c 2157312 /dev/fb0 | sha256sum').split()[0]


def main():
    global APP
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('variant')
    parser.add_argument('binary', type=Path)
    parser.add_argument('--book', required=True, help='EPUB path already on Kindle')
    parser.add_argument('--probe', type=Path, required=True)
    parser.add_argument('--preload', type=Path,
                        help='probe.so; omit for a binary with the probe linked in (static builds)')
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--wake-check', action='store_true',
                        help='After timing, check real sleep/wake restoration and the next turn')
    parser.add_argument('--memory-check', action='store_true',
                        help='Sample smaps/status in library, reader and menu outside timed turns')
    parser.add_argument('--identity-memory-check', action='store_true',
                        help='With --memory-check, compare RSS before/after reading the executable digest')
    parser.add_argument('--turn-delay-us', type=int, default=850000,
                        help='Delay after each 50 ms contact (default: 850000)')
    parser.add_argument('--experiment', choices=['redraw', 'persistence', 'annotations'], default='redraw')
    parser.add_argument('--position-check', action='store_true',
                        help='Verify saved page, close/reopen, sleep and normal-exit completion')
    parser.add_argument('--failure-check', action='store_true',
                        help='Block only the isolated position temp file, check errors and recover')
    parser.add_argument('--home', action='store_true', help='Visit All books from the current Home screen')
    parser.add_argument('--orientation', choices=['portrait', 'landscape'], default='portrait')
    parser.add_argument('--page-flash', type=int, default=6,
                        help='Periodic full-refresh interval; 0 disables periodic flashes')
    parser.add_argument('--font-size-pt', type=float, default=10.,
                        help='Controlled book font size; use a new size to measure layout-cache misses')
    parser.add_argument('--idle-seconds', type=int, default=0,
                        help='Sample settled idle CPU outside the timed turns (0 skips)')
    parser.add_argument('--page-layout', type=json.loads,
                        help='JSON for settings.page_layout (reader bars and margins); default is the old footer')
    parser.add_argument('--reading-speed', type=float,
                        help='Seed reading-speed.json (chars/sec, 10 samples) so time-left bar items render')
    parser.add_argument('--queued-input', action='store_true',
                        help='Allow updates after subsequent touches; require 24 taps, frames and saved-page advances')
    parser.add_argument('--annotation-count', type=int, choices=[0, 100, 1000])
    parser.add_argument('--annotation-fixture', type=Path, help='Local original reader.epub for synthetic records')
    parser.add_argument('--annotation-kind', choices=['highlight', 'bookmark'], default='highlight',
                        help='Whether the synthetic records are highlights or page bookmarks')
    parser.add_argument('--reopen', type=int, default=0, metavar='N',
                        help='With --memory-check, close and reopen the book N times, sampling after each close')
    parser.add_argument('--one-cpu', action='store_true',
                        help='Pin Verso to CPU 0 (VERSO_BENCH_ONE_CPU, set by the probe before main)')
    parser.add_argument('--dictionary-cache', metavar='STATE',
                        help='Copy STATE/dictionary-cache (e.g. a dictionary.py cold state) into the run state')
    args = parser.parse_args()
    APP = '/mnt/us/verso/' + args.experiment + '-check'
    assert 0 < args.turn_delay_us <= 1000000
    assert args.page_flash >= 0 and 0 <= args.idle_seconds <= 30
    assert 6 <= args.font_size_pt <= 36
    assert not args.identity_memory_check or args.memory_check
    assert 0 <= args.reopen <= 10 and (not args.reopen or args.memory_check)
    assert args.orientation == 'portrait' or not (args.position_check or args.failure_check or args.wake_check
                                                  or args.reopen), \
        'Position/failure/wake/reopen lifecycle coordinates currently require portrait'
    assert re.fullmatch(r'[a-z0-9-]+', args.variant)
    ready()
    remote(f'mkdir -p {APP}/bin {APP}/books')
    # Preserve the production executable, settings, and saved positions.
    remote(f'cmp -s {Q(args.book)} {APP}/books/book.epub || cp {Q(args.book)} {APP}/books/book.epub')
    tools = [('probe', args.probe), (args.variant + '/verso', args.binary)]
    for name, source in tools + ([('probe.so', args.preload)] if args.preload else []):
        remote(f'mkdir -p {APP}/bin/{args.variant}')
        upload(APP + '/bin/' + name, source.read_bytes())
    binary_sha256 = remote(f'sha256sum {APP}/bin/{args.variant}/verso').split()[0]
    assert binary_sha256 == hashlib.sha256(args.binary.read_bytes()).hexdigest()
    book_sha256 = remote(f'sha256sum {APP}/books/book.epub').split()[0]
    launcher = patch_launcher(APP, '/tmp/verso-redraw-launcher')
    upload(APP + '/launcher.sh', launcher.encode())
    run = args.variant + '-' + str(time.time_ns())
    state = APP + '/' + run + '.state'
    trace = APP + '/' + run + '.trace'
    remote(f'mkdir -p {state}; chmod 755 {APP}/bin/probe {APP}/bin/{args.variant}/verso')
    if args.dictionary_cache:
        copy_dictionary_cache(args.dictionary_cache, state)
    typography = {'font': 'Literata', 'size_pt': args.font_size_pt, 'spacing': 0, 'alignment': 'book'}
    settings = {'typography': typography, 'library_folders': [APP + '/books'], 'library_sort': 'title',
                'orientation': args.orientation, 'page_flash': args.page_flash, 'setup_done': True}
    if args.page_layout is not None:
        settings['page_layout'] = args.page_layout
    else:
        typography['margins'] = 0
        settings['footer'] = True
    upload(state + '/settings.json', json.dumps(settings).encode())
    if args.reading_speed is not None:
        upload(state + '/reading-speed.json',
               json.dumps({'chars_per_sec': args.reading_speed, 'samples': 10}).encode())
    if args.annotation_count is not None:
        assert args.experiment == 'annotations' and args.annotation_fixture
        from annotation_fixture import fixture_tar
        remote(f'mkdir -p {state}/annotations')
        remote(f'tar -xf - -C {state}/annotations', fixture_tar(args.annotation_fixture, args.annotation_count, kind=args.annotation_kind))
    wrapper = (f'#!/bin/sh\nexport VERSO_DATA={state}\nexport VERSO_BENCH_TRACE={trace}\n'
               f'export RUST_LOG=info,verso=debug,verso::app=trace\n'
               + (f'export LD_PRELOAD={APP}/bin/probe.so\n' if args.preload else '')
               + ('export VERSO_BENCH_ONE_CPU=1\n' if args.one_cpu else '')
               + f'exec {APP}/bin/{args.variant}/verso\n')
    upload(APP + '/verso', wrapper.encode())
    pid = None
    results = None
    try:
        remote(f'chmod 755 {APP}/verso; : > {APP}/verso.log; '
               f'setsid sh {APP}/launcher.sh </dev/null >/dev/null 2>&1 &')
        pid = app_pid(f'{APP}/bin/{args.variant}/verso')
        wait_for(f'grep -q "first frame on screen" {APP}/verso.log')
        # Without it the probe isn't in this process (not preloaded, not linked).
        wait_for(f'grep -q " MARK {pid} process_start$" {trace}', 5)
        if args.one_cpu:
            allowed = remote(f'cat /proc/{pid}/task/*/status | grep "^Cpus_allowed_list:"').splitlines()
            assert allowed and all(line.split()[1] == '0' for line in allowed), allowed
        samples, hashes = {}, {}
        def sample(label):
            if args.memory_check:
                samples[label] = [memory(pid) for _ in range(3)]
                hashes[label] = framebuffer_hash()

        remote('sleep 2')
        sample('library')
        probe = f'VERSO_BENCH_TRACE={trace} {APP}/bin/probe tap {pid}'
        landscape = args.orientation == 'landscape'
        def tap(x, y, label):
            if landscape:
                x, y = y, 1695 - x
            return f'{probe} {x} {y} {label}'
        center = (848, 550) if landscape else (636, 848)
        forward = (1500, 600) if landscape else (1100, 1000)
        # Prime axes: evdev can omit unchanged ABS values between sessions.
        # Verso launches on Home; tap the bottom Library card before opening the first book.
        home = tap(848 if landscape else 636, 1135 if landscape else 1559, 'library') + '; sleep 2; '
        remote(f'{tap(1, 1, "prime")}; sleep 2; {home}{tap(200, 550, "open")}; sleep 5')
        wait_for(f'grep -q "page .* rendered in" {APP}/verso.log', 120)
        remote(f'{tap(*center, "seek_menu")}; sleep 1; '
               f'{tap(440 if landscape else 338, 1046 if landscape else 1458, "seek")}; sleep 2; '
               f'{tap(*center, "seek_close")}; sleep 2')
        def position():
            records = json.loads(remote(f'cat {state}/books/*.json'))
            return {'page': records['page'], 'progress': records['progress'],
                    'position_hash': hashlib.sha256(records['position'].encode()).hexdigest()}

        initial_position = position()
        sample('reader_start')
        remote(f'cp {APP}/verso.log {APP}/{run}.setup.log; : > {APP}/verso.log; '
               f'cat /proc/{pid}/stat > {APP}/{run}.start.stat; '
               f'for t in /proc/{pid}/task/*; do cat "$t/stat"; done > {APP}/{run}.start.tasks; '
               f'VERSO_BENCH_TRACE={trace} {APP}/bin/probe mark turns_start')
        # One remote shell avoids per-tap SSH setup in the measured interval.
        forward_command = tap(*forward, '"turn_$i"')
        settle = 'sleep 1; ' if args.queued_input else ''
        remote(f'i=1; while [ "$i" -le 24 ]; do {forward_command}; '
               f'usleep {args.turn_delay_us}; i=$((i+1)); done; '
               f'{settle}'
               f'VERSO_BENCH_TRACE={trace} {APP}/bin/probe mark turns_end; '
               f'cat /proc/{pid}/stat > {APP}/{run}.end.stat; '
               f'for t in /proc/{pid}/task/*; do cat "$t/stat"; done > {APP}/{run}.end.tasks; '
               f'head -c 2157312 /dev/fb0 | sha256sum > {APP}/{run}.page.hash; '
               f'head -c 1928352 /dev/fb0 | tail -c 1699392 | sha256sum > {APP}/{run}.body.hash')
        # Keep private logs on-device. Export numeric frame/gesture data only.
        frames = remote(f'grep -E "frame: drawn|Down\\(|Tap\\(|presented [0-9]+ update" {APP}/verso.log')
        results = {'variant': args.variant, 'run': run, 'frames': frames,
                   'dictionary_cache_seeded': bool(args.dictionary_cache),
                   'binary_sha256': binary_sha256, 'identity_check_timing': 'before_launch',
                   'book_sha256': book_sha256,
                   'orientation': args.orientation, 'page_flash': args.page_flash,
                   'font_size_pt': args.font_size_pt, 'page_layout': args.page_layout,
                   'queued_input': args.queued_input, 'initial_position': initial_position,
                   'render_log': remote(f'grep "rendered in" {APP}/verso.log || true'),
                   'bars_log': remote(f'grep "bars drawn in" {APP}/verso.log || true'),
                   'startup_log': remote(f'grep -E "startup stage:|book .*parse|book .*layout" {APP}/{run}.setup.log || true'),
                   'annotation_count': args.annotation_count, 'annotation_kind': args.annotation_kind,
                   'annotation_setup': remote(f'grep -E "annotation resolution:|book first page" {APP}/{run}.setup.log || true'),
                   'turn_delay_us': args.turn_delay_us, 'one_cpu': args.one_cpu,
                   'probe': 'preload' if args.preload else 'linked',
                   'persistence': remote(f'grep -E "position write:|after_present:" {APP}/verso.log || true'),
                   'setup_frames': remote(f'grep "frame: drawn" {APP}/{run}.setup.log'),
                   'trace': remote(f'cat {trace}'),
                   'page_hash': remote(f'cat {APP}/{run}.page.hash').split()[0],
                   'body_hash': remote(f'cat {APP}/{run}.body.hash').split()[0]}
        setup_log = remote(f'cat {APP}/{run}.setup.log')
        results['open_stats'] = [dict(parse_ms=int(p), layout_ms=int(l), pages=int(n)) for p, l, n in
                                 re.findall(r': parse (\d+) ms, layout (\d+) ms, (\d+) pages',
                                            '\n'.join(line for line in setup_log.splitlines()
                                                      if 'prelayout]' not in line))]
        results['saved_after_turns'] = position()
        results['timed_position_after_turns'] = results['saved_after_turns']
        for label in ['start', 'end']:
            results[label + '_stat'] = remote(f'cat {APP}/{run}.{label}.stat')
            results[label + '_tasks'] = remote(f'cat {APP}/{run}.{label}.tasks')
        assert results['saved_after_turns']['page'] == initial_position['page'] + 24
        sample('reader_end')
        marks = [float(line.split()[0]) for line in results['trace'].splitlines()
                 if re.search(r' MARK \d+ turn_\d+$', line)]
        sends = [line.split() for line in results['trace'].splitlines() if ' IOCTL ' in line
                 and int(line.split()[3], 16) & 0xffff == 0x462e]
        assert len(marks) == 24
        if args.queued_input:
            start = next(float(s.split()[0]) for s in results['trace'].splitlines() if s.endswith(' turns_start'))
            end = next(float(s.split()[0]) for s in results['trace'].splitlines() if s.endswith(' turns_end'))
            updates = [s for s in sends if start <= float(s[0]) < end]
            assert len(updates) == 24 and len(re.findall(r' Tap\(Point', frames)) == 24
            assert len(re.findall(r'frame: drawn', frames)) == 24
            assert all(s[5] == '0' and s[-2:] == ['1272', '1696'] for s in updates)
        for i, mark in enumerate(marks):
            if args.queued_input:
                assert float(updates[i][0]) >= mark, 'Update preceded its touch'
                continue
            end = marks[i + 1] - .05 if i < 23 else float('inf')
            updates = [s for s in sends if mark <= float(s[0]) < end]
            assert len(updates) == 1 and updates[0][5] == '0', 'Expected one successful update per turn'
            assert updates[0][-2:] == ['1272', '1696'], 'Turn did not refresh the whole page'
        remote(f'cp {APP}/verso.log {APP}/{run}.log')
        # Menu feedback and restoration (outside timed interval).
        remote(f'{tap(*center, "menu_open")}; sleep 1')
        sample('menu')
        remote(f'{tap(*center, "menu_close")}; sleep 1; '
               f'head -c 2157312 /dev/fb0 | sha256sum > {APP}/{run}.restored.hash')
        sample('restored')
        results['restored_hash'] = remote(f'cat {APP}/{run}.restored.hash').split()[0]
        assert results['page_hash'] == results['restored_hash'], 'Menu did not restore page'
        if args.identity_memory_check:
            sample('identity_before')
            assert remote(f'sha256sum {APP}/bin/{args.variant}/verso').split()[0] == binary_sha256
            sample('identity_after')
            assert hashes['identity_before'] == hashes['identity_after'] == results['page_hash']
        if args.idle_seconds:
            # One shell and two /proc reads; no polling during the idle interval.
            results['idle'] = remote(f'cat /proc/{pid}/stat; cat /proc/uptime; '
                                     f'sleep {args.idle_seconds}; '
                                     f'cat /proc/{pid}/stat; cat /proc/uptime').splitlines()
        lifecycle_hash = results['page_hash']
        for i in range(1, args.reopen + 1):
            remote(f'{probe} 636 848 reopen_menu; sleep 1; {probe} 65 70 reopen_close; sleep 3')
            sample(f'closed_{i}')
            remote(f'{probe} 200 550 reopen; sleep 3')
            assert framebuffer_hash() == lifecycle_hash, 'Reopen changed page'
        results['reopens'] = args.reopen
        if args.failure_check:
            saved = position()
            # A directory at the temporary-file path produces EISDIR without
            # changing the valid record. Restrict mutations to isolated state.
            remote(f'for f in {state}/books/*.json; do mkdir "$f.tmp"; done; '
                   f'{probe} 1100 1000 failed_save; sleep 1')
            assert position() == saved
            wait_for(f'grep -q "saving position:" {APP}/verso.log')
            try:
                remote(f'{probe} 636 848 failure_menu; sleep 1; '
                       f'{probe} 65 70 failed_close; sleep 1; kill -TERM {pid}; sleep 1')
                assert remote(f'test -e /proc/{pid} && echo alive') == 'alive'
                wait_for(f'grep -q "ERROR verso::app.*exit:" {APP}/verso.log')
                # Input must still work after the rejected signal exit. Dismiss
                # its toast and close the reader menu before recovery.
                remote(f'{probe} 636 1380 dismiss_error; sleep 4; '
                       f'{probe} 636 848 failure_menu_close; sleep 1')
            finally:
                remote(f'for f in {state}/books/*.json; do rmdir "$f.tmp"; done')
            remote(f'{probe} 636 848 recovery_menu; sleep 1; '
                   f'{probe} 65 70 recovered_close; sleep 1')
            recovered = position()
            assert recovered['page'] == saved['page'] + 1
            results['failure_recovery_checked'] = True
            # Reopen before the remaining sleep/exit checks.
            remote(f'{probe} 200 550 recovery_reopen; sleep 3')
            lifecycle_hash = framebuffer_hash()
            results['recovered_page_hash'] = lifecycle_hash
        if args.position_check:
            saved = position()
            assert saved['page'] == initial_position['page'] + 24 + int(args.failure_check)
            results['saved_after_turns'] = saved
            remote(f'{probe} 636 848 close_menu; sleep 1; {probe} 65 70 book_close; sleep 1')
            assert position() == saved
            remote(f'{probe} 200 550 reopen; sleep 3')
            assert position() == saved
            assert framebuffer_hash() == lifecycle_hash, 'Reopen changed page'
            results['close_reopen_checked'] = True
        if args.wake_check or args.position_check:
            remote('lipc-set-prop com.lab126.powerd powerButton 1')
            try:
                wait_for('test "$(lipc-get-prop com.lab126.powerd state)" = screenSaver')
            finally:
                if remote('lipc-get-prop com.lab126.powerd state') != 'active':
                    remote('lipc-set-prop com.lab126.powerd powerButton 1')
            wait_for('test "$(lipc-get-prop com.lab126.powerd state)" = active')
            # Include the delayed firmware-banner cleanup, outside the timing interval.
            remote('sleep 3')
            results['wake_hash'] = remote('head -c 2157312 /dev/fb0 | sha256sum').split()[0]
            assert results['wake_hash'] == lifecycle_hash, 'Wake did not restore page'
            if args.position_check:
                assert position() == saved
            remote(f'{probe} 1100 1000 post_wake_turn; sleep 1')
            checks = remote(f'cat {trace}').splitlines()
            wake_mark = next(float(s.split()[0]) for s in checks if s.endswith(' post_wake_turn'))
            updates = [s.split() for s in checks if ' IOCTL ' in s
                       and int(s.split()[3], 16) & 0xffff == 0x462e and float(s.split()[0]) >= wake_mark]
            assert len(updates) == 1 and updates[0][5] == '0' and updates[0][-2:] == ['1272', '1696']
            results['post_wake_turn'] = True
        if args.position_check:
            # The last turn is followed immediately by normal termination.
            # The quit path must finish its submitted save before exiting.
            remote(f'{probe} 1100 1000 exit_turn; kill -TERM {pid}')
            wait_for(f'test ! -e /proc/{pid}')
            final = position()
            assert final['page'] == saved['page'] + 2
            results['saved_after_exit'] = final
            results['exit_checked'] = True
        if args.memory_check:
            results.update(memory=samples, screen_hashes=hashes)
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(results, indent=2) + '\n')
        print(args.output, flush=True)
    except BaseException as error:
        if results is not None:
            # Preserve rejected measurements for diagnosis, never as accepted data.
            results['capture_error'] = repr(error)
            args.output.parent.mkdir(parents=True, exist_ok=True)
            args.output.with_suffix('.failed.json').write_text(json.dumps(results, indent=2) + '\n')
        raise
    finally:
        if args.failure_check:
            remote(f'for f in {state}/books/*.json; do '
                   f'test ! -d "$f.tmp" || rmdir "$f.tmp"; done')
        if pid:
            remote(f'test "$(readlink /proc/{pid}/exe)" != {APP}/bin/{args.variant}/verso '
                   f'|| kill -TERM {pid}')
        wait_for('test ! -e /tmp/verso-redraw-launcher.pid', 15)
        states = remote('for n in awesome JunoStatusBarDriver; do p=$(pidof "$n"); '
                        'awk \'/^State:/ {print $2}\' /proc/$p/status; done')
        assert 'T' not in states.split(), 'Kindle UI still paused'


if __name__ == '__main__':
    main()
