# Kindle performance tools

General measurement tools. Every one of them drives the Kindle over `ssh kindle`,
so read the readiness and safety rules in `AGENTS.md` first: the Kindle must be
awake, no other reader running, and the launcher must have restored the Kindle UI
when a run ends. Runs use isolated state under `/mnt/us/verso/*-check`.

- `probe.c`: observes the display ioctls (`probe.so`, preloaded into Verso) and
  injects touches (`probe`). Build both with zig:

  ```sh
  zig cc -target arm-linux-gnueabihf.2.20 -O2 -DPRELOAD -shared -fPIC scripts/perf/probe.c -o /tmp/probe.so -ldl
  zig cc -target arm-linux-gnueabihf.2.20 -O2 scripts/perf/probe.c -o /tmp/probe
  ```

- `redraw.py`: page-turn runs (24 turns, isolated state), also the helper module
  (`remote`, `upload`, `ready`, …) the other scripts import.
- `startup.py`: Home startup runs.
- `analyze.py`, `analyze-redraw.py`, `analyze-startup.py`: summarize captures
  (`test_analyze_redraw.py` tests the analyzer).
- `annotation_fixture.py`: builds synthetic highlight records for `redraw.py`.
- `koreader.lua`: runs KOReader the same way, for comparisons.

```sh
python3 scripts/perf/redraw.py NAME target/armv7-unknown-linux-gnueabihf/release/verso \
  --book /mnt/us/verso/testbooks/BOOK.epub --probe /tmp/probe --preload /tmp/probe.so \
  --output /tmp/perf/NAME.json
python3 scripts/perf/analyze-redraw.py /tmp/perf/*.json --output /tmp/perf/results
```

Run `--help` on a script for its options. Install the latest build first
(`scripts/deploy.sh --no-run`), and repeat independent launches rather than
counting consecutive samples as independent runs.
