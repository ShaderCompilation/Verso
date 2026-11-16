#!/usr/bin/env python3
"""Export startup.py captures as numeric CSV; all times are software boundaries."""
import argparse
import csv
import json
from pathlib import Path
import re


def summarize(run, name):
    assert run['clean_exit'], 'Launcher did not restore the UI'
    trace = [line.split() for line in run['trace'].splitlines()]
    sends = [s for s in trace if s[1] == 'IOCTL' and int(s[3], 16) & 0xffff == 0x462e]
    assert sends and all(s[5] == '0' for s in sends), 'Missing or failed display update'
    first = sends[0]
    starts = [s for s in trace if s[1] == 'MARK' and s[2] == first[2] and s[3] == 'process_start']
    assert len(starts) == 1, 'Missing or ambiguous reader start'
    start = float(starts[0][0])
    wait = next(s for s in trace if s[1] == 'IOCTL' and s[2] == first[2]
                and int(s[3], 16) & 0xffff == 0x462f and s[6] == first[6]
                and float(s[0]) >= float(first[4]))
    assert wait[5] == '0'
    logs = '\n'.join(run['timings'])

    def stage(label):
        found = re.findall(r'startup stage: ' + label + r'[^\n]*elapsed_us=(\d+)', logs)
        return round(int(found[-1]) / 1000, 4) if found else ''

    marks = [s for s in trace if s[1] == 'MARK' and s[3] == 'first_collections']
    collections = ''
    if marks:
        mark = float(marks[0][0])
        update = next(s for s in sends if s[2] == first[2] and float(s[0]) >= mark)
        collections = round((float(update[0]) - mark) * 1000, 4)
    fonts = re.search(r'crengine ready: \d+ fonts registered in ([\d.]+)(ns|µs|ms|s)', logs)
    font_ms = float(fonts[1]) * {'ns': 1e-6, 'µs': .001, 'ms': 1, 's': 1000}[fonts[2]] if fonts else ''
    row = dict(run=name, variant=run['variant'], binary_sha256=run.get('binary_sha256', ''),
               fixture=run['fixture'], mode=run['mode'],
               startup_request_ms=round((float(first[0]) - start) * 1000, 4),
               startup_completion_ms=round((float(wait[4]) - start) * 1000, 4),
               background_complete_ms=stage('background complete'),
               first_collections_request_ms=collections, font_init_ms=font_ms,
               collections_build_ms=stage('collections'), state_reads_ms=stage('cached states'),
               view_ms=stage('view'), clean_exit=True)
    for label in ['first_frame', 'background_complete', 'first_collections', 'reader']:
        sample = run['samples'].get(label, {})
        memory = sample.get('memory', {})
        row[label + '_rss_kib'] = memory.get('Rss', sample.get('VmRSS', ''))
        row[label + '_pss_kib'] = memory.get('Pss', '')
        row[label + '_cpu_ticks'] = sample.get('cpu_ticks', '')
    return row


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('inputs', type=Path, nargs='+')
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    rows = [summarize(run, f'{path.stem}-{i + 1}') for path in args.inputs
            for i, run in enumerate(json.loads(path.read_text()))]
    assert rows, 'No startup runs'
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with args.output.open('w') as out:
        writer = csv.DictWriter(out, fieldnames=list(rows[0]), lineterminator='\n')
        writer.writeheader()
        writer.writerows(rows)
    print(f'{len(rows)} validated runs: {args.output}')


if __name__ == '__main__':
    main()
