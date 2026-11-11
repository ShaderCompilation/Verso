#!/usr/bin/env python3
"""Isolated Kindle redraw comparison. See README.md before running."""
import argparse
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


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('variant')
    parser.add_argument('binary', type=Path)
    parser.add_argument('--book', required=True, help='EPUB path already on Kindle')
    parser.add_argument('--probe', type=Path, required=True)
    parser.add_argument('--preload', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    assert re.fullmatch(r'[a-z0-9-]+', args.variant)
    ready()
    remote(f'mkdir -p {APP}/bin {APP}/books')
    # Preserve the production executable, settings, and saved positions.
    remote(f'cmp -s {Q(args.book)} {APP}/books/book.epub || cp {Q(args.book)} {APP}/books/book.epub')
    for name, source in [('probe', args.probe), ('probe.so', args.preload),
                         (args.variant + '/verso', args.binary)]:
        remote(f'mkdir -p {APP}/bin/{args.variant}')
        upload(APP + '/bin/' + name, source.read_bytes())
    launcher = Path('dist/kindle/verso/verso.sh').read_text()
    launcher = launcher.replace('APP_DIR="/mnt/us/verso"', f'APP_DIR="{APP}"')
    launcher = launcher.replace('/tmp/verso-launcher', '/tmp/verso-redraw-launcher')
    upload(APP + '/launcher.sh', launcher.encode())
    run = args.variant + '-' + str(time.time_ns())
    state = APP + '/' + run + '.state'
    trace = APP + '/' + run + '.trace'
    remote(f'mkdir -p {state}; chmod 755 {APP}/bin/probe {APP}/bin/{args.variant}/verso')
    settings = {'typography': {'font': 'Literata', 'size_pt': 10., 'spacing': 0,
                              'margins': 0, 'justify': True}, 'footer': True,
                'library_folders': [APP + '/books'], 'library_sort': 'title'}
    upload(state + '/settings.json', json.dumps(settings).encode())
    wrapper = (f'#!/bin/sh\nexport VERSO_DATA={state}\nexport VERSO_BENCH_TRACE={trace}\n'
               f'export RUST_LOG=info,verso=debug,verso::app=trace\n'
               f'export LD_PRELOAD={APP}/bin/probe.so\nexec {APP}/bin/{args.variant}/verso\n')
    upload(APP + '/verso', wrapper.encode())
    pid = None
    try:
        remote(f'chmod 755 {APP}/verso; : > {APP}/verso.log; '
               f'setsid sh {APP}/launcher.sh </dev/null >/dev/null 2>&1 &')
        wait_for('pidof verso >/dev/null')
        pid = remote('pidof verso')
        assert remote(f'readlink /proc/{pid}/exe') == f'{APP}/bin/{args.variant}/verso'
        wait_for(f'grep -q "first frame on screen" {APP}/verso.log')
        probe = f'VERSO_BENCH_TRACE={trace} {APP}/bin/probe tap {pid}'
        # Prime axes: evdev can omit unchanged ABS values between sessions.
        remote(f'{probe} 1 1 prime; sleep 2; {probe} 200 550 open; sleep 5')
        assert remote(f'grep -c "page .* rendered in" {APP}/verso.log') != '0', 'Book did not open'
        remote(
               f'{probe} 636 848 seek_menu; sleep 1; {probe} 338 1164 seek; sleep 2; '
               f'{probe} 636 848 seek_close; sleep 2')
        remote(f'cp {APP}/verso.log {APP}/{run}.setup.log; : > {APP}/verso.log; '
               f'cat /proc/{pid}/stat > {APP}/{run}.start.stat; '
               f'for t in /proc/{pid}/task/*; do cat "$t/stat"; done > {APP}/{run}.start.tasks; '
               f'VERSO_BENCH_TRACE={trace} {APP}/bin/probe mark turns_start')
        # One remote shell avoids per-tap SSH setup in the measured interval.
        remote(f'i=1; while [ "$i" -le 24 ]; do {probe} 1100 1000 "turn_$i"; '
               f'usleep 850000; i=$((i+1)); done; '
               f'VERSO_BENCH_TRACE={trace} {APP}/bin/probe mark turns_end; '
               f'cat /proc/{pid}/stat > {APP}/{run}.end.stat; '
               f'for t in /proc/{pid}/task/*; do cat "$t/stat"; done > {APP}/{run}.end.tasks; '
               f'head -c 2157312 /dev/fb0 | sha256sum > {APP}/{run}.page.hash')
        # Keep private logs on-device. Export numeric frame/gesture data only.
        frames = remote(f'grep -E "frame: drawn|Down\\(|Tap\\(|presented [0-9]+ update" {APP}/verso.log')
        results = {'variant': args.variant, 'run': run, 'frames': frames,
                   'trace': remote(f'cat {trace}'),
                   'page_hash': remote(f'cat {APP}/{run}.page.hash').split()[0]}
        marks = [float(line.split()[0]) for line in results['trace'].splitlines()
                 if re.search(r' MARK \d+ turn_\d+$', line)]
        sends = [line.split() for line in results['trace'].splitlines() if ' IOCTL ' in line
                 and int(line.split()[3], 16) & 0xffff == 0x462e]
        assert len(marks) == 24
        for i, mark in enumerate(marks):
            end = marks[i + 1] - .05 if i < 23 else float('inf')
            updates = [s for s in sends if mark <= float(s[0]) < end]
            assert len(updates) == 1 and updates[0][5] == '0', 'Expected one successful update per turn'
            assert updates[0][-2:] == ['1272', '1696'], 'Turn did not refresh the whole page'
        for label in ['start', 'end']:
            results[label + '_stat'] = remote(f'cat {APP}/{run}.{label}.stat')
            results[label + '_tasks'] = remote(f'cat {APP}/{run}.{label}.tasks')
        remote(f'cp {APP}/verso.log {APP}/{run}.log')
        # Menu feedback and restoration (outside timed interval).
        remote(f'{probe} 636 848 menu_open; sleep 1; {probe} 636 848 menu_close; sleep 1; '
               f'head -c 2157312 /dev/fb0 | sha256sum > {APP}/{run}.restored.hash')
        results['restored_hash'] = remote(f'cat {APP}/{run}.restored.hash').split()[0]
        assert results['page_hash'] == results['restored_hash'], 'Menu did not restore page'
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(results, indent=2) + '\n')
        print(args.output, flush=True)
    finally:
        if pid:
            remote(f'test "$(readlink /proc/{pid}/exe)" != {APP}/bin/{args.variant}/verso '
                   f'|| kill -TERM {pid}')
        wait_for('test ! -e /tmp/verso-redraw-launcher.pid', 15)
        states = remote('for n in awesome JunoStatusBarDriver; do p=$(pidof "$n"); '
                        'awk \'/^State:/ {print $2}\' /proc/$p/status; done')
        assert 'T' not in states.split(), 'Kindle UI still paused'


if __name__ == '__main__':
    main()
