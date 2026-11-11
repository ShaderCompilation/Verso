#!/usr/bin/env python3
"""Summarize probe traces and /proc snapshots; never copies app settings."""
import argparse
import csv
import json
import math
import re
import statistics
from pathlib import Path


def field(path, name):
    match = re.search(rf"^{name}:\s+(\d+)", path.read_text(), re.MULTILINE)
    if not match:
        raise ValueError(f"{name} missing from {path}")
    return int(match[1])


def sample(path):
    # comm may contain spaces/parentheses; accounting starts after its last ')'.
    stat = path.joinpath("stat").read_text().rsplit(")", 1)[1].split()
    return {
        "pid": field(path / "status", "Pid"),
        "rss_mib": field(path / "status", "VmRSS") / 1024,
        "hwm_mib": field(path / "status", "VmHWM") / 1024,
        "pss_mib": field(path / "smaps_rollup", "Pss") / 1024,
        "cpu_seconds": (int(stat[11]) + int(stat[12])) / 100,
        "uptime": float(path.joinpath("uptime").read_text().split()[0]),
    }


def summarize(root, run):
    samples = {p.name: sample(p) for p in sorted((root / f"{run}.samples").iterdir())}
    pid = samples["library"]["pid"]
    lines = [line.split() for line in (root / f"{run}.trace").read_text().splitlines()]
    starts = [float(x[0]) for x in lines if x[1:] == ["MARK", str(pid), "process_start"]]
    if len(starts) != 1:
        raise ValueError(f"expected one process start for {run}, pid {pid}")
    updates = [x for x in lines if x[1] == "IOCTL" and int(x[2]) == pid and x[3].endswith("462e")]
    if not updates or any(int(x[5]) != 0 for x in updates):
        raise ValueError(f"missing or failed display update in {run}")
    events = []
    for i, line in enumerate(lines):
        if line[1] != "MARK" or not re.fullmatch(r"turn_\d+|menu_open|menu_close|open_book", line[3]):
            continue
        update = next((x for x in lines[i + 1:] if x in updates), None)
        if update is None:
            raise ValueError(f"no update after {line[3]} in {run}")
        events.append({
            "run": run,
            "action": line[3],
            "ms_to_request": (float(update[0]) - float(line[0])) * 1000,
            "waveform": int(update[7]),
            "update_mode": int(update[8]),
            "width": int(update[11]),
            "height": int(update[12]),
        })
    turns = [x["ms_to_request"] for x in events if x["action"].startswith("turn_")]
    result = {
        "startup_request_ms": (float(updates[0][0]) - starts[0]) * 1000,
        "samples": samples,
        "events": events,
    }
    if turns:
        if len(turns) != 24:
            raise ValueError(f"expected 24 turns in {run}, got {len(turns)}")
        result["turn_median_ms"] = statistics.median(turns)
        result["turn_p95_ms"] = sorted(turns)[math.ceil(len(turns) * 0.95) - 1]
        before, after = samples["idle-start"], samples["idle-end"]
        result["idle_cpu_percent_one_core"] = 100 * (after["cpu_seconds"] - before["cpu_seconds"]) / (after["uptime"] - before["uptime"])
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", type=Path)
    parser.add_argument("runs", nargs="+")
    parser.add_argument("--csv", type=Path)
    args = parser.parse_args()
    results = {run: summarize(args.root, run) for run in args.runs}
    if args.csv:
        events = [event for result in results.values() for event in result["events"]]
        with args.csv.open("w", newline="") as output:
            writer = csv.DictWriter(output, fieldnames=["run", "action", "ms_to_request", "waveform", "update_mode", "width", "height"])
            writer.writeheader()
            writer.writerows(events)
    print(json.dumps(results, indent=2))


if __name__ == "__main__":
    main()
