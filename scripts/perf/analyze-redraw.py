#!/usr/bin/env python3
"""Reduce redraw.py results to numeric, reproducible comparison artifacts."""
import argparse
import csv
import json
import math
import re
import statistics
from pathlib import Path


def cpu(stat):
    fields = stat.rsplit(')', 1)[1].split()
    return (int(fields[11]) + int(fields[12])) / 100


def duration(value, unit):
    return float(value) * {'s': 1000, 'ms': 1, 'µs': .001, 'ns': .000001}[unit]


def summarize(path):
    r = json.loads(path.read_text())
    assert not r.get('capture_error'), 'Rejected capture: ' + str(r.get('capture_error'))
    trace = [line.split() for line in r['trace'].splitlines()]
    start = next(float(s[0]) for s in trace if s[1] == 'MARK' and s[3] == 'turns_start')
    end = next(float(s[0]) for s in trace if s[1] == 'MARK' and s[3] == 'turns_end')
    marks = [s for s in trace if s[1] == 'MARK' and re.fullmatch(r'turn_\d+', s[3])]
    sends = [s for s in trace if s[1] == 'IOCTL' and int(s[3], 16) & 0xffff == 0x462e
             and start <= float(s[0]) < end]
    assert len(marks) == len(sends) == 24
    if r.get('queued_input'):
        saved = r.get('timed_position_after_turns', r['saved_after_turns'])
        assert saved['page'] == r['initial_position']['page'] + 24
        assert len(re.findall(r' Tap\(Point', r['frames'])) == 24
        assert len(re.findall(r'frame: drawn', r['frames'])) == 24
    flashes = [s for s in sends if s[8] == '1']
    waits = [s for s in trace if s[1] == 'IOCTL' and int(s[3], 16) & 0xffff == 0x462f
             and start <= float(s[0]) < end]
    for flash in flashes:
        completed = next((w for w in waits if w[6] == flash[6] and float(w[0]) >= float(flash[4])), None)
        assert completed is not None and completed[5] == '0', 'Flash lacks successful completion wait'
        following = next((s for s in sends if float(s[0]) > float(flash[0])), None)
        assert following is None or float(completed[4]) < float(following[0]), 'Update overtook a flash'
    events = []
    for i, (mark, send) in enumerate(zip(marks, sends)):
        assert send[5] == '0' and send[-2:] == ['1272', '1696']
        latency = (float(send[0]) - float(mark[0])) * 1000
        assert latency >= 0
        boundary = float(marks[i + 1][0]) - .05 if i + 1 < len(marks) else end
        assert float(mark[0]) <= float(send[0]), 'Update preceded its touch'
        assert r.get('queued_input') or float(send[0]) < boundary, 'Missing or ambiguous tap/update association'
        # Time when this input waited for an earlier full refresh to finish.
        blocked = sum(max(0, min(float(w[4]), float(send[0])) - max(float(w[0]), float(mark[0])))
                      for w in waits) * 1000
        events.append({'run': path.stem, 'turn': mark[3], 'request_ms': round(latency, 4),
                       'prior_flash_wait_ms': round(blocked, 4),
                       'submit_ioctl_ms': round((float(send[4]) - float(send[0])) * 1000, 4),
                       'waveform': int(send[7]), 'mode': int(send[8])})
    frames = re.findall(r'frame: drawn in ([\d.]+)(ms|µs|ns|s), diffed in ([\d.]+)(ms|µs|ns|s), (\d+) update', r['frames'])
    assert len(frames) >= 24
    timings = [e['request_ms'] for e in events]
    process_cpu = cpu(r['end_stat']) - cpu(r['start_stat'])
    main_pid = r['start_stat'].split()[0]
    main_start = next(s for s in r['start_tasks'].splitlines() if s.split()[0] == main_pid)
    main_end = next(s for s in r['end_tasks'].splitlines() if s.split()[0] == main_pid)
    result = {'run': path.stem, 'variant': r['variant'], 'binary_sha256': r.get('binary_sha256', ''),
              'book_sha256': r.get('book_sha256', ''), 'turns': 24,
              'frames': len(frames), 'unchanged_frames': sum(int(f[4]) == 0 for f in frames),
              'draw_ms': round(sum(duration(f[0], f[1]) for f in frames), 3),
              'diff_ms': round(sum(duration(f[2], f[3]) for f in frames), 3),
              'cpu_seconds': round(process_cpu, 3),
              'main_cpu_seconds': round(cpu(main_end) - cpu(main_start), 3),
              'cpu_ms_per_turn': round(process_cpu * 1000 / 24, 3),
              'request_median_ms': round(statistics.median(timings), 3),
              'request_p95_ms': round(sorted(timings)[math.ceil(.95 * len(timings)) - 1], 3),
              'request_max_ms': round(max(timings), 3),
              'page_hash': r['page_hash'], 'restored_hash': r['restored_hash'],
              'flashes': len(flashes), 'completion_waits': len(waits),
              'wake_hash': r.get('wake_hash', ''), 'post_wake_turn': r.get('post_wake_turn', '')}
    result.update(orientation=r.get('orientation', 'portrait'), page_flash=r.get('page_flash', 6),
                  turn_delay_us=r.get('turn_delay_us', 850000), font_size_pt=r.get('font_size_pt', 10.),
                  identity_check_timing=r.get('identity_check_timing',
                                              'after_turns' if 'binary_sha256' in r else 'not_recorded'))
    def percentile(values):
        return round(sorted(values)[math.ceil(.95 * len(values)) - 1], 4) if values else 0
    present = [duration(*m) for m in re.findall(r'presented \d+ update\(s\) in ([\d.]+)(ms|µs|ns|s)', r['frames'])]
    rendered = [duration(*m) for m in re.findall(r'rendered in ([\d.]+)(ms|µs|ns|s)', r.get('render_log', ''))]
    bars = [int(v) / 1000 for v in re.findall(r'bars drawn in (\d+) us', r.get('bars_log', ''))]
    flash_ms = [(float(w[4]) - float(w[0])) * 1000 for w in waits]
    for name, values in [('present', present), ('render', rendered), ('flash_wait', flash_ms), ('bars', bars)]:
        result[name + '_count'] = len(values)
        result[name + '_median_ms'] = round(statistics.median(values), 4) if values else 0
        result[name + '_p95_ms'] = percentile(values)
    tasks_start = {s.split('(', 1)[1].rsplit(')', 1)[0]: cpu(s) for s in r['start_tasks'].splitlines()}
    tasks_end = {s.split('(', 1)[1].rsplit(')', 1)[0]: cpu(s) for s in r['end_tasks'].splitlines()}
    for name in ['verso-book', 'verso-library', 'verso-position']:
        result[name + '_cpu_seconds'] = round(tasks_end.get(name, 0) - tasks_start.get(name, 0), 4)
    idle = r.get('idle')
    result['idle_seconds'] = round(float(idle[3].split()[0]) - float(idle[1].split()[0]), 3) if idle else ''
    result['idle_cpu_ticks'] = round((cpu(idle[2]) - cpu(idle[0])) * 100) if idle else ''
    if r.get('persistence'):
        result['turn_delay_us'] = r['turn_delay_us']
        writes = re.findall(r'position write: ([\d.]+)(ms|µs|ns|s), ok=(true|false)', r['persistence'])
        hooks = re.findall(r'after_present: ([\d.]+)(ms|µs|ns|s)', r['persistence'])
        assert len(writes) == len(hooks) == 24, 'Expected one save per presented turn'
        assert all(w[2] == 'true' for w in writes), 'Position write failed'
        for label, values in [('save', writes), ('after_present', hooks)]:
            ms = [duration(v[0], v[1]) for v in values]
            result[label + '_median_ms'] = round(statistics.median(ms), 4)
            result[label + '_p95_ms'] = round(sorted(ms)[math.ceil(.95 * len(ms)) - 1], 4)
            result[label + '_max_ms'] = round(max(ms), 4)
            for event, value in zip(events, ms):
                event[label + '_ms'] = round(value, 4)
        # Position worker CPU is included in process CPU, and recorded separately.
        def thread_cpu(records):
            return sum(cpu(s) for s in records.splitlines() if '(verso-position)' in s)
        result['position_cpu_seconds'] = round(thread_cpu(r['end_tasks']) - thread_cpu(r['start_tasks']), 3)
        result['close_reopen_checked'] = r.get('close_reopen_checked', False)
        result['exit_checked'] = r.get('exit_checked', False)
        result['failure_recovery_checked'] = r.get('failure_recovery_checked', False)
    return result, events


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('inputs', type=Path, nargs='+')
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--workloads', action='store_true',
                        help='Compare hashes/waveforms within matching workloads instead of globally')
    args = parser.parse_args()
    runs, events, memory, setup = [], [], [], []
    for path in args.inputs:
        result, turns = summarize(path)
        runs.append(result)
        events.extend(turns)
        raw = json.loads(path.read_text())
        pattern = r'frame: drawn in ([\d.]+)(ms|µs|ns|s), diffed in ([\d.]+)(ms|µs|ns|s), (\d+) update'
        for i, frame in enumerate(re.findall(pattern, raw.get('setup_frames', ''))):
            draw, diff = duration(frame[0], frame[1]), duration(frame[2], frame[3])
            setup.append({'run': path.stem, 'variant': raw['variant'], 'frame': i + 1,
                          'draw_ms': round(draw, 4), 'diff_ms': round(diff, 4),
                          'total_ms': round(draw + diff, 4), 'updates': int(frame[4])})
        for screen, samples in raw.get('memory', {}).items():
            for i, sample in enumerate(samples):
                memory.append({'run': path.stem, 'variant': raw['variant'],
                               'screen': screen, 'sample': i + 1,
                               'identity_read_during_app': screen == 'identity_after' or
                                   result['identity_check_timing'] == 'after_turns' and
                                   screen in ('reader_end', 'menu', 'restored'),
                               'rss_kib': sample['Rss'], 'pss_kib': sample['Pss'],
                               'private_kib': sample['Private_Clean'] + sample['Private_Dirty'],
                               'swap_kib': sample['Swap'], 'status_rss_kib': sample['VmRSS'],
                               'status_hwm_kib': sample['VmHWM'], 'virtual_kib': sample['VmSize'],
                               'screen_hash': raw['screen_hashes'][screen]})
    assert all(r['page_hash'] == r['restored_hash'] for r in runs)
    groups = {}
    for path, run in zip(args.inputs, runs):
        raw = json.loads(path.read_text())
        key = (raw.get('book_sha256'), run['orientation'], run['page_flash'],
               run['turn_delay_us'], run['font_size_pt'], raw.get('annotation_count'),
               json.dumps(raw.get('page_layout'), sort_keys=True)) if args.workloads else 'comparison'
        groups.setdefault(key, []).append(run)
    for key, group in groups.items():
        dynamic_bar = args.workloads and any(k in (key[-1] or '') for k in ('clock_', 'battery'))
        if dynamic_bar:
            body_hashes = {json.loads(p.read_text()).get('body_hash') for p, r in zip(args.inputs, runs) if r in group}
            assert len(body_hashes) == 1 and None not in body_hashes, 'Different final page bodies within dynamic-bar workload'
        else:
            assert len({r['page_hash'] for r in group}) == 1, 'Different final pages within workload'
        # Matched waveforms establish identical full-flash cadence as well as pages.
        patterns = [[(e['waveform'], e['mode']) for e in events if e['run'] == r['run']] for r in group]
        assert all(p == patterns[0] for p in patterns), 'Different waveforms within workload'
    args.output.mkdir(parents=True, exist_ok=True)
    for screen in {s['screen'] for s in memory}:
        if args.workloads:
            # Reader equality was checked per workload, each menu restores its page.
            # Library/status pixels can differ across books, orientations and builds.
            continue
        hashes = {}
        for sample in (s for s in memory if s['screen'] == screen):
            hashes.setdefault(sample['screen_hash'], set()).add(sample['variant'])
        if screen in ('library', 'menu') and len(hashes) > 1:
            # Battery text can change between runs. Every observed status-screen
            # image must still match exactly in at least two distinct builds.
            assert all(len(variants) >= 2 for variants in hashes.values()), f'Unmatched {screen} pixels'
        else:
            assert len(hashes) == 1, f'Different {screen} pixels'
    outputs = [('runs', runs), ('events', events)]
    if memory:
        outputs.append(('memory', memory))
    if setup:
        outputs.append(('setup', setup))
    for name, rows in outputs:
        with (args.output / (name + '.csv')).open('w') as out:
            writer = csv.DictWriter(out, fieldnames=list(rows[0]), lineterminator='\n')
            writer.writeheader()
            writer.writerows(rows)
    print(json.dumps(runs, indent=2))


if __name__ == '__main__':
    main()
