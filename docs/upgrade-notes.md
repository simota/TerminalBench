# Upgrade notes

What changes for existing users, baselines and scripts. Newest first.

## 2.0.0-dev, November 2026: measurement audit

The changes below came out of an audit of what each metric measures and
how results are compared. Several of them make new reports incompatible
with old ones on purpose: the old numbers meant something other than their
names said.

### Reports are not comparable across this change

- **`render.throughput` and `scroll.proxy` have a new backend,
  `rust-writer`.** The binary now writes the fixture itself and times only
  that write. Before, it timed a whole external `cat` process, including
  its startup and file read, under backend `rust-timer`. The comparability
  gate answers `NOT_COMPARABLE` for old against new. **Re-record every
  baseline that contains these two metrics.**
- **`resource.cpu` on Linux is a new profile,**
  `org.terminalbench.resource.cpu.window` (CPU time per 1 s window). The
  macOS `ps` profile is unchanged. The two are `NOT_COMPARABLE`.
- **p95 is computed from every timed trial**, no longer from the
  outlier-filtered set, so it is higher than before for the same data.
  Median and stddev are unchanged. A p95 from an old report and one from a
  new report are different statistics even though the gate does not tell
  them apart.
- **`resource.*` runs first** in a full `run`, so `idle` is sampled before
  any output flood. Old `idle` memory figures included scrollback from
  earlier scenarios.

### Removed

- **`input.latency` is no longer emitted.** It timed the kernel's pty echo
  on a fresh pty and never involved the terminal emulator. Old reports
  that contain it still validate. A baseline with `input.latency` rows
  fails the regression gate against a new report ("metric disappeared");
  re-record it.
- **The "wins per entity" tally** is gone from the HTML compare report.
- **Fixture files** under `fixtures/` are no longer written or read.

### Regression gate (`compare --threshold`)

- **Significance is tested across runs, not across trials.** Pass repeated
  runs: `compare --threshold 5% BASE... --latest NEW...`. With at least 4
  runs per side a flagged row fails only at p<0.05 across the run medians.
- **Two files (one run each) are judged on the threshold alone.** The old
  behaviour, a Mann-Whitney test on the trials of the two runs, is gone: it
  called identical configurations different about a quarter of the time
  (`docs/aa-baseline.md`). Expect a two-file gate with a low threshold to
  fail more often than before; raise the threshold or pass more runs. Use
  `tbench aa` to see what your machine supports.
- **A row missing from the latest report fails the gate** (exit 3). It was
  silently skipped.
- **An invalid `--threshold` is a usage error** (exit 2). It used to be
  read as 0%.
- **Percentage-unit rows** with a zero baseline use an absolute rule
  (`--threshold-points`, default 5); with a non-zero baseline they must
  also move by more than 1 point.

### Exit codes and output

- `run --strict` exits 1, after writing the report, when a scenario
  produced no results. Without it `run` still exits 0 and lists them.
- An interrupted `run` exits `128 + signal` and writes no report.
- `compare`'s Markdown table no longer bolds a row in which every file
  ties; it bolds every cell that shares the best median.
- `trend` calls a change better or worse only beyond a noise band
  (`--noise`, default 5%; 1 point for percentage rows). Smaller changes
  read `(within noise)`. The column is headed `Δ` instead of `Δ%`.
- `compare` ranks `fire.fps` from pre-v1 reports (no direction field) as
  higher-is-better. It used to mark the slowest as best.
- `--keep-samples` arrays are in trial order, not sorted.
- `meta.shell` holds the first line of each shell's `--version` output.

### New, without breaking anything

- `tbench aa`: A/A self-check of the regression gate.
- Linux: `resource.mem` / `resource.cpu`, host facts (machine model, power
  source, low-power mode) and terminal identity are discovered, so the
  comparability gate no longer needs every `TBENCH_*` override. The display
  refresh rate still has to be supplied with `TBENCH_DISPLAY_REFRESH_HZ`
  for `render.throughput`, `scroll.proxy` and `fire.fps`.
- Reports carry run parameters and host details under `extensions`.
- An installed binary works from any directory: scenarios and fixtures are
  built in.
