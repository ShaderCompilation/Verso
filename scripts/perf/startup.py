#!/usr/bin/env python3
"""Isolated Home/library startup measurements; launches only through Verso's launcher.

Copies the real index/state/covers, never writes the user's real data. Output
contains numeric timings/accounting and anonymous trace records, not book names.
See docs/research/home-startup.md for measurement boundaries and cold-cache limits.
"""
import argparse
import hashlib
import json
from pathlib import Path
import re
import shlex
import subprocess
import time
from redraw import memory

APP = '/mnt/us/verso/startup-check'
Q = shlex.quote


def large_index(path):
    """Upgrade the original cached-only fixture's placeholder cover keys.

    Matches covers::key: otherwise every missing EPUB is retried and persisted,
    which measures cache regeneration instead of unchanged-library startup.
    """
    index = json.loads(path.read_text())
    for book in index['books']:
        if book.get('cover') == {'Unavailable': 'fixture'}:
            stamp = {k: book['stamp'][k] for k in ('size', 'modified_secs', 'modified_nanos')}
            payload = json.dumps([1, book['id'], stamp], separators=(',', ':'), ensure_ascii=False).encode()
            book['cover'] = {'Unavailable': hashlib.sha256(payload).hexdigest()}
    return json.dumps(index, ensure_ascii=False).encode()


def remote(command, data=None):
    return subprocess.run(['ssh', '-o', 'ConnectTimeout=5', 'kindle', command],
                          input=data, capture_output=True, check=True, timeout=55).stdout.decode().strip()


def upload(path, data):
    remote('cat > ' + Q(path), data)


def wait(command, seconds=300):
    until = time.monotonic() + seconds
    while time.monotonic() < until:
        if remote(command + ' && echo yes || true') == 'yes':
            return
        time.sleep(.1)
    raise RuntimeError('Timed out: ' + command)


def fb_hash(rows=1696):
    return remote(f'head -c {1272 * rows} /dev/fb0 | sha256sum').split()[0]


def sleep_wake():
    """Real sleep and wake through powerd's power button; always returns awake."""
    remote('lipc-set-prop com.lab126.powerd powerButton 1')
    try:
        wait('test "$(lipc-get-prop com.lab126.powerd state)" = screenSaver', 30)
    finally:
        if remote('lipc-get-prop com.lab126.powerd state') != 'active':
            remote('lipc-set-prop com.lab126.powerd powerButton 1')
    wait('test "$(lipc-get-prop com.lab126.powerd state)" = active', 30)
    remote('sleep 3')  # Includes the delayed firmware-banner cleanup.


def lifecycle(probe, state, pid):
    """Home and reader sleep/wake, exact reopen, close back to Home and exit."""
    checks = {}
    wait(f'grep -q "startup stage: background complete" {APP}/verso.log')
    remote('sleep 2')
    home, header = fb_hash(), fb_hash(300)
    sleep_wake()
    checks['home_wake_restored'] = fb_hash() == home
    summary = json.loads(remote(f'cat {state}/home.json'))
    book = next(e['book']['id'] for e in summary['books'] if e['status']['has_position']
                and not e['status']['finished'] and not e['status']['hidden_from_continue'] and e['book']['available'])
    record = f'{state}/books/{book}.json'
    before = json.loads(remote(f'cat {record}'))
    remote(f'{probe} 1 1 prime; {probe} 400 400 open')
    wait(f'grep -q "book first page requested" {APP}/verso.log')
    remote('sleep 2')
    opened = json.loads(remote(f'cat {record}'))
    checks['exact_resume'] = (opened['position'], opened['page']) == (before['position'], before['page'])
    page = fb_hash()
    sleep_wake()
    checks['reader_wake_restored'] = fb_hash() == page
    remote(f'{probe} 1100 1000 turn; sleep 2')
    turned = json.loads(remote(f'cat {record}'))
    checks['turn_saved'] = turned['page'] == before['page'] + 1
    remote(f'{probe} 636 848 menu; sleep 1; {probe} 65 70 close; sleep 2')
    checks['closed_to_home'] = fb_hash(300) == header
    resume = json.loads(remote(f'cat {state}/home.json'))['books'][0]
    checks['home_updated'] = resume['book']['id'] == book and resume['status']['progress'] == turned['progress']
    remote(f'kill -TERM {pid}')
    wait(f'test ! -e /proc/{pid}')
    checks['position_after_exit'] = json.loads(remote(f'cat {record}'))['position'] == turned['position']
    assert all(checks.values()), checks
    return checks


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('variant')
    parser.add_argument('binary', type=Path)
    parser.add_argument('--large', type=Path, help='Required only for the large-library fixture')
    parser.add_argument('--probe', type=Path, help='Touch/clock helper; supply together with --preload')
    parser.add_argument('--preload', type=Path, help='Display ioctl observer')
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--runs', type=int, default=2)
    parser.add_argument('--fixture', choices=['real', 'large'])
    parser.add_argument('--mode', choices=['startup', 'immediate-open', 'lifecycle'])
    parser.add_argument('--installed', action='store_true', help='Reuse the variant executable already installed by an earlier run')
    parser.add_argument('--collections-tap', default='950,200',
                        help='Collections tab position (baseline 950,200; three-tab Home 636,230)')
    parser.add_argument('--summary', choices=['copied', 'missing', 'seeded'], default='copied',
                        help='Home summary: as copied, removed (migration), or the saved real-library seed')
    args = parser.parse_args()
    assert re.fullmatch(r'[a-z0-9-]+', args.variant)
    assert re.fullmatch(r'\d+,\d+', args.collections_tap)
    if bool(args.probe) != bool(args.preload):
        parser.error('--probe and --preload must be supplied together')
    if args.fixture != 'real' and args.large is None:
        parser.error('--large is required unless --fixture real is selected')
    assert remote('lipc-get-prop com.lab126.powerd state; pidof verso || true; pidof reader.lua || true') == 'active'
    repo = Path(__file__).resolve().parents[2]
    remote(f'mkdir -p {APP}/bin/{Q(args.variant)}')
    if not args.installed:
        upload(f'{APP}/bin/{args.variant}/verso', args.binary.read_bytes())
    if args.probe:
        upload(APP + '/bin/probe', args.probe.read_bytes())
        upload(APP + '/bin/probe.so', args.preload.read_bytes())
    else:
        # Compatibility with earlier runs; new runs should provide both tools.
        remote(f'cp /mnt/us/verso/persistence-check/bin/probe {APP}/bin/probe; '
               f'cp /mnt/us/verso/persistence-check/bin/probe.so {APP}/bin/probe.so')
    remote(f'chmod 755 {APP}/bin/{Q(args.variant)}/verso {APP}/bin/probe')
    binary_sha256 = remote(f'sha256sum {APP}/bin/{args.variant}/verso').split()[0]
    assert binary_sha256 == hashlib.sha256(args.binary.read_bytes()).hexdigest(), 'Installed variant differs from binary'
    launcher = (repo / 'dist/kindle/verso/verso.sh').read_text().replace('APP_DIR="/mnt/us/verso"', f'APP_DIR="{APP}"')
    launcher = launcher.replace('/tmp/verso-launcher', '/tmp/verso-startup-launcher')
    upload(APP + '/launcher.sh', launcher.encode())
    results = json.loads(args.output.read_text()) if args.output.exists() else []
    combos = [('real', 'startup'), ('large', 'startup'), ('real', 'immediate-open'), ('real', 'lifecycle')]
    for fixture, mode in combos:
        # Lifecycle checks sleep the device, so they run only when requested.
        if (args.fixture and args.fixture != fixture) or (args.mode and args.mode != mode) \
                or (mode == 'lifecycle' and args.mode != mode):
            continue
        for number in range(args.runs):
            assert remote('lipc-get-prop com.lab126.powerd state; pidof verso || true; pidof reader.lua || true') == 'active'
            remote('lipc-set-prop com.lab126.powerd touchScreenSaverTimeout 1')
            run = f'{args.variant}-{fixture}-{mode}-{time.time_ns()}'
            state = APP + '/' + run + '.state'
            trace = APP + '/' + run + '.trace'
            remote(f'mkdir -p {state}; cp -r /mnt/us/verso/data/. {state}/')
            if fixture == 'large':
                upload(state + '/library.json', large_index(args.large / 'library.json'))
                upload(state + '/settings.json', (args.large / 'settings.json').read_bytes())
            # A predictable source view; real reading positions/flags stay copied.
            upload(state + '/library-navigation.json', b'{"tab":"all_books","all_page":0}')
            if args.summary == 'missing':
                remote(f'rm -f {state}/home.json')
            elif args.summary == 'seeded':
                # A complete summary produced by an earlier real-library migration run.
                remote(f'cp {APP}/seed-home.json {state}/home.json')
            wrapper = (f'#!/bin/sh\nexport VERSO_DATA={state}\nexport VERSO_BENCH_TRACE={trace}\n'
                       'export RUST_LOG=info,verso=debug\n'
                       f'export LD_PRELOAD={APP}/bin/probe.so\nexec {APP}/bin/{args.variant}/verso\n')
            upload(APP + '/verso', wrapper.encode())
            pid = None
            result = dict(variant=args.variant, binary_sha256=binary_sha256,
                          fixture=fixture, mode=mode, number=number, summary=args.summary,
                          cache='warm OS cache; fresh copy of persisted app data', samples={})
            try:
                remote(f'chmod 755 {APP}/verso; : > {APP}/verso.log; '
                       f'setsid sh {APP}/launcher.sh </dev/null >/dev/null 2>&1 &')
                wait('pidof verso >/dev/null')
                pid = remote('pidof verso')
                assert remote(f'readlink /proc/{pid}/exe') == f'{APP}/bin/{args.variant}/verso'
                wait(f'grep -q "first frame on screen" {APP}/verso.log')

                def sample(label):
                    records = remote(f'cat /proc/{pid}/stat /proc/{pid}/status /proc/{pid}/io')
                    result['samples'][label] = dict(stat=records.splitlines()[0], status=records, io=records,
                                                   memory=memory(pid))

                sample('first_frame')
                probe = f'VERSO_BENCH_TRACE={trace} {APP}/bin/probe tap {pid}'
                if mode == 'immediate-open':
                    # Prime both axes outside the UI, then tap the resume card.
                    remote(f'{probe} 1 1 prime; {probe} 400 400 open')
                    wait(f'grep -q "book first page requested" {APP}/verso.log')
                    sample('reader')
                elif mode == 'lifecycle':
                    result['lifecycle'] = lifecycle(probe, state, pid)
                else:
                    wait(f'grep -q "startup stage: background complete" {APP}/verso.log')
                    sample('background_complete')
                    remote(f'{probe} 1 1 prime; {probe} {args.collections_tap.replace(",", " ")} first_collections')
                    remote('sleep 1')
                    sample('first_collections')
                # Keep content-bearing logs only in the isolated device directory.
                lines = remote(f'cat {APP}/verso.log').splitlines()
                result['timings'] = [line for line in lines if any(key in line for key in [
                    'startup stage:', 'first frame on screen', 'library covers:', 'library scan:',
                    'library identities complete', 'crengine ready:'])]
                result['trace'] = remote(f'cat {trace}')
                if fixture == 'real' and mode == 'startup' and args.summary == 'missing':
                    remote(f'grep -q {Q('"complete":true')} {state}/home.json && cp {state}/home.json {APP}/seed-home.json')
            finally:
                if pid and remote(f'test -e /proc/{pid} && echo yes || true') == 'yes':
                    remote(f'kill -TERM {pid}')
                    wait(f'test ! -e /proc/{pid}')
                wait('test ! -e /tmp/verso-startup-launcher.pid')
                states = remote('for n in awesome JunoStatusBarDriver; do p=$(pidof "$n"); '
                                'test -n "$p" && awk \'/^State:/ {print $2}\' /proc/$p/status; done')
                assert len(states.split()) == 2 and 'T' not in states.split(), 'Kindle UI not restored'
                result['clean_exit'] = True
            # No process names, argv, kernel addresses or document paths in export.
            for sample_set in result['samples'].values():
                fields = sample_set.pop('stat').split(') ', 1)[1].split()
                sample_set['cpu_ticks'] = int(fields[11]) + int(fields[12])
                status = sample_set.pop('status')
                sample_set.update({k: int(v) for k, v in re.findall(r'^(VmRSS|VmHWM):\s+(\d+) kB$', status, re.M)})
                sample_set['io'] = dict((k, int(v)) for k, v in re.findall(r'^(\w+):\s+(\d+)$', sample_set['io'], re.M))
            results.append(result)
            args.output.parent.mkdir(parents=True, exist_ok=True)
            args.output.write_text(json.dumps(results, indent=2) + '\n')
            print(f'{fixture} {mode} run {number + 1}: complete; Kindle UI restored', flush=True)


if __name__ == '__main__':
    main()
