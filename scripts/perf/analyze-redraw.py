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
    trace = [line.split() for line in r['trace'].splitlines()]
    start = next(float(s[0]) for s in trace if s[1] == 'MARK' and s[3] == 'turns_start')
    end = next(float(s[0]) for s in trace if s[1] == 'MARK' and s[3] == 'turns_end')
    marks = [s for s in trace if s[1] == 'MARK' and re.fullmatch(r'turn_\d+', s[3])]
    sends = [s for s in trace if s[1] == 'IOCTL' and int(s[3], 16) & 0xffff == 0x462e
             and start <= float(s[0]) < end]
    assert len(marks) == len(sends) == 24
    flashes = [s for s in sends if s[8] == '1']
    waits = [s for s in trace if s[1] == 'IOCTL' and int(s[3], 16) & 0xffff == 0x462f
             and start <= float(s[0]) < end]
    for flash in flashes:
        completed = next((w for w in waits if w[6] == flash[6] and float(w[0]) >= float(flash[4])), None)
        assert completed is not None and completed[5] == '0', 'Flash lacks successful completion wait'
        following = next((s for s in sends if float(s[0]) > float(flash[0])), None)
        assert following is None or float(completed[4]) < float(following[0]), 'Update overtook a flash'
    events = []
    for mark, send in zip(marks, sends):
        assert send[5] == '0' and send[-2:] == ['1272', '1696']
        latency = (float(send[0]) - float(mark[0])) * 1000
        assert 0 <= latency < 850
        events.append({'run': path.stem, 'turn': mark[3], 'request_ms': round(latency, 4),
                       'waveform': int(send[7]), 'mode': int(send[8])})
    frames = re.findall(r'frame: drawn in ([\d.]+)(ms|µs|ns|s), diffed in ([\d.]+)(ms|µs|ns|s), (\d+) update', r['frames'])
    assert len(frames) >= 24
    timings = [e['request_ms'] for e in events]
    process_cpu = cpu(r['end_stat']) - cpu(r['start_stat'])
    main_pid = r['start_stat'].split()[0]
    main_start = next(s for s in r['start_tasks'].splitlines() if s.split()[0] == main_pid)
    main_end = next(s for s in r['end_tasks'].splitlines() if s.split()[0] == main_pid)
    result = {'run': path.stem, 'variant': r['variant'], 'turns': 24,
              'frames': len(frames), 'unchanged_frames': sum(int(f[4]) == 0 for f in frames),
              'draw_ms': round(sum(duration(f[0], f[1]) for f in frames), 3),
              'diff_ms': round(sum(duration(f[2], f[3]) for f in frames), 3),
              'cpu_seconds': round(process_cpu, 3),
              'main_cpu_seconds': round(cpu(main_end) - cpu(main_start), 3),
              'cpu_ms_per_turn': round(process_cpu * 1000 / 24, 3),
              'request_median_ms': round(statistics.median(timings), 3),
              'request_p95_ms': round(sorted(timings)[math.ceil(.95 * len(timings)) - 1], 3),
              'page_hash': r['page_hash'], 'restored_hash': r['restored_hash'],
              'flashes': len(flashes), 'completion_waits': len(waits),
              'wake_hash': r.get('wake_hash', ''), 'post_wake_turn': r.get('post_wake_turn', '')}
    return result, events


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('inputs', type=Path, nargs='+')
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    runs, events = [], []
    for path in args.inputs:
        result, turns = summarize(path)
        runs.append(result)
        events.extend(turns)
    assert len({r['page_hash'] for r in runs}) == 1, 'Different final pages'
    assert all(r['page_hash'] == r['restored_hash'] for r in runs)
    # Matched waveforms establish identical full-flash cadence as well as pages.
    patterns = [[(e['waveform'], e['mode']) for e in events if e['run'] == r['run']] for r in runs]
    assert all(p == patterns[0] for p in patterns)
    args.output.mkdir(parents=True, exist_ok=True)
    for name, rows in [('runs', runs), ('events', events)]:
        with (args.output / (name + '.csv')).open('w') as out:
            writer = csv.DictWriter(out, fieldnames=list(rows[0]), lineterminator='\n')
            writer.writeheader()
            writer.writerows(rows)
    print(json.dumps(runs, indent=2))


if __name__ == '__main__':
    main()
