# TerminalBench (tbench)

Single-binary (Rust) benchmark runner for shell startup/dispatch latency
and terminal-emulator draw/input/resource characteristics. Implementation of
proposal recommendation C (`docs/proposal-terminal-bench.md`), MVP scope
plus the extension-1 emulator metrics (§3 items 1-3), the extension-2
`compare` subcommand (§6.4/§6.6), further emulator-focused metrics
(CJK/emoji rendering, resource footprint),
the measurement-quality hardening from `docs/proposal-detailed-benchmark.md`
§A (settle, concurrent-run detection, warmup/outlier filtering, environment
metadata, adaptive `--runs auto`), the §E `orchestrate` subcommand for
sequential multi-emulator runs, the §C synchronized-output probe, and the
§F statistics (`--keep-samples`, bootstrap CI, Mann-Whitney significance).

Upgrading from an earlier build? Several metrics and the regression gate
changed in ways that make old baselines incomparable:
[`docs/upgrade-notes.md`](docs/upgrade-notes.md).

## Usage

```sh
# All scenarios, default shells (zsh,bash), 20 runs each
bin/tbench run

# One scenario only
bin/tbench run --scenario shell-startup

# Choose shells and trial count (shell-scoped scenarios only, see below)
bin/tbench run --scenario shell-startup --shells zsh,bash --runs 5

# Write JSON to a specific path (default: results/tbench-<timestamp>.json)
bin/tbench run --output results/my-report.json

# Record an explicit app/terminal identity instead of TERM_PROGRAM detection
# (wins over the TBENCH_TERMINAL_ID environment override)
bin/tbench run --app 'Ghostty nightly' --output results/ghostty-nightly.json

# List available scenarios (name, metric, unit, scope, description)
bin/tbench list

# Human-readable recap of one report (default: newest results/tbench-*.json)
bin/tbench show
bin/tbench show results/my-report.json
```

Output is a single JSON document (see schema below) printed to stdout and
also written under `results/` (gitignored); `run` also prints a `show`-style
recap to stderr afterward, so stdout stays script-pipeable while a human
still gets a readable summary. `list` and `show` are read-only (no
results/.tbench.lock) and safe to run alongside an in-progress `run`.

## Reporting Specification v1

For claim-safe, vendor-neutral report exchange, see the normative
[`docs/reporting-spec-v1.md`](docs/reporting-spec-v1.md) and its versioned
Schema/validator. Validate both reports and run the eligibility gate before
using the existing comparator:

```sh
set -e
bin/tbench-validate validate results/a.json &&
bin/tbench-validate validate results/b.json &&
bin/tbench-validate compare results/a.json results/b.json &&  # must be COMPARABLE
bin/tbench compare results/a.json results/b.json
```

The JSON Schema is a structural subset: passing it is necessary but is not
sufficient for `CONFORMANT`, claim-safe, or `COMPARABLE`. The semantic rules
and corpus in the specification remain authoritative. Controls that
discovery cannot provide can be supplied explicitly. On Linux that is
normally only `TBENCH_DISPLAY_REFRESH_HZ`, which the gate needs for
`render.throughput`, `scroll.proxy` and `fire.fps`; on CI it is all of them:

```sh
TBENCH_TERMINAL_ID='ci-terminal 1.0' \
TBENCH_MACHINE_MODEL='ci-runner-class-a' \
TBENCH_POWER_SOURCE=ac \
TBENCH_LOW_POWER_MODE=false \
TBENCH_DISPLAY_REFRESH_HZ=60 \
  bin/tbench run --output results/ci-report.json
```

## Measurement quality (settle, concurrency, warmup/outliers, adaptive runs)

Real usage surfaced four ways the numbers themselves could be polluted;
`run` now guards against each (`docs/proposal-detailed-benchmark.md` §A):

- **Settle between scenarios and variants** (`--settle N`, default 2s):
  `run` pauses `N` seconds between scenario files, and `render.throughput`
  pauses between its `plain`/`ansi`/`cjk` variants, so one variant's leftover load (a flood still
  draining, a decaying CPU average) doesn't bleed into the next
  measurement's first samples. `resource.mem`/`resource.cpu`'s `idle`
  sampling specifically waits **at least 5s** regardless of `--settle`
  (macOS's `ps %cpu` is a decayed running average — this is the fix for an
  observed `resource.cpu[idle]` p95 of 243%, inherited from whatever ran
  immediately before it).
- **Concurrent-run detection**: `run` records its PID in
  `results/.tbench.lock` and checks for one at startup. A live conflicting
  run makes `tbench` warn and wait (polling, up to 120s) rather than start
  measuring in parallel with it — two simultaneous full-suite runs were
  observed to cross-pollute each other's `render.throughput` (70→13 MB/s).
  `--force` skips the wait (with a visible warning: the risk doesn't go
  away, it's just accepted). A lock whose PID is no longer running (a
  stale lock, e.g. after a crash) is reclaimed automatically. This only
  detects other `tbench` processes, not unrelated system load (a
  concurrent Xcode build, Time Machine, etc.).
- **Warmup + IQR outlier filtering**: `--warmup N` (default 3) discarded
  trials run before timing starts, and samples more than 1.5×IQR outside
  the interquartile range are dropped before computing median and stddev.
  **p95 is computed from every timed trial**, filtered or not: filtering
  first biased it about 4% low and hid slow trials that belong to the
  workload ([`docs/p95-outlier-simulation.md`](docs/p95-outlier-simulation.md)).
  At the default 20 trials p95 is the second-largest value and lands below
  the true 95th percentile in about three of four runs; treat it as a tail
  figure only from roughly 50 trials (`--runs 50`).
  The removed count is reported per result as `"outliers_removed"` (always
  present, `0` when not applicable — `resource.*` and `fire.fps`).
- **Adaptive sampling** (`--runs auto`, for `shell.startup`/`cmd.overhead`/
  `render.throughput`/`scroll.proxy` only): instead of a fixed trial count,
  collects in batches of 10 and stops once a bootstrap 95% CI on the median
  is within 5% of the median, or 50 total trials are reached — noisier
  metrics (e.g. `cmd.overhead`, whose fork/exec cost has high relative
  variance) will often hit the 50-trial cap rather than converge early,
  which is an expected, not a failed, outcome. `resource.*` and
  `fire.fps` don't support `auto` yet and fall back to a fixed default
  with a stderr note.
- **Interrupted runs clean up**: on Ctrl-C, SIGTERM or a closed terminal,
  `run` kills its output flood, leaves the alternate screen, restores the
  cursor and terminal mode, removes its lock file, and exits `128 + signal`
  without writing a report. On Linux the flood also dies if `tbench` itself
  is killed outright.
- **Failed-trial rejection**: non-zero child exits and commands exceeding
  the 30-second per-trial safety limit are errors, never timing samples.
- **Environment metadata on Linux** (best-effort — a fact with no sound
  basis stays unknown):
  - `machine_model`: DMI system vendor and product name, or the board
    vendor and name when those are firmware placeholders, or the
    device-tree model; followed by the CPU model, because a board alone
    does not fix the CPU.
  - `cpu`: the model name from `/proc/cpuinfo`.
  - `power_source`: `ac` when a mains or USB supply is online, `battery`
    when the system battery is discharging, `ac` when there is no system
    battery at all. Peripheral batteries are ignored.
  - `low_power_mode`: true when the ACPI platform profile is `low-power`,
    false for any other profile, and false when the machine has neither a
    profile switch nor a battery. Unknown for a battery machine without a
    profile switch.
  - `terminal`: when `TERM_PROGRAM` is unset, the executable name of the
    process holding the pty master (for example `kitty`), unless that is
    tmux, screen or another intermediary.
  - Display refresh rate is not discovered on Linux; set
    `TBENCH_DISPLAY_REFRESH_HZ`.
  - `extensions["org.terminalbench.host"]` records the kernel release,
    session type, CPU governor, platform profile, and the basis of the
    power and low-power facts.
- **Run parameters**: `extensions["org.terminalbench.run"]` records the
  requested `--runs`, `--warmup`, `--settle`, `--shells`, the scenarios in
  the order they ran, those that produced no results, `FIRE_SECS`, and the
  terminal size in cells. Extensions never affect conformance or the
  comparability gate; they are there so a reader can see whether two
  reports followed the same protocol.
- **Environment metadata on macOS** (best-effort — omitted, not fatal,
  on failure): `meta.power_source` (`pmset -g batt`), `meta.low_power_mode`,
  `meta.machine_model`/`meta.cpu` (`sysctl`), and `meta.gpu`/
  `meta.display_refresh_hz` (`system_profiler SPDisplaysDataType`, timed —
  if it takes over 1s, those two fields are dropped and `meta.meta_notes`
  explains why, so one slow machine doesn't tax every run).

## Rendering fidelity probe (synchronized output)

Every `run` sends a DECRQM query (`CSI ? 2026 $ p`) to the real terminal —
asking whether it recognizes DECSET mode 2026, "synchronized output"
(batch a frame instead of drawing it incrementally, so a terminal that
supports it *can* avoid skipping/tearing frames) — and records the reply as
`meta.synchronized_output_supported`: `"supported"`, `"unsupported"`
(recognized the query but reported the mode as unset — rare), or
`"no_reply"` (no controlling tty, or the terminal didn't answer within 1s —
most terminals older than ~2023 fall here). This is **not** a frame-skip
*detector* — it doesn't know whether any given `render.throughput`/
`scroll.proxy` run actually skipped a frame, only whether the terminal
*could* have avoided it. proposal §C considered a marker-line +
screen-capture approach that would actually confirm the final rendered
frame, but that needs the macOS screen-recording permission and adds OCR
error risk for what proposal §C judged not to be worth it yet — this probe
is the cheap, no-permission first step: whenever
`synchronized_output_supported` isn't `"supported"`, `meta.frame_skip_risk_note`
is added explaining that `render.throughput`/`scroll.proxy` numbers reflect
PTY-write completion, not confirmed on-screen rendering.

The probe writes to and reads from `/dev/tty` directly (raw mode, 1s
timeout) — the terminal emulator itself answers automatically the instant
it parses the query, so this needs a real controlling terminal but no user
keystroke. Verified directly against Ghostty (`"supported"`) and against a
real tty with nothing listening on the other end (`"no_reply"` after ~1s).

## Comparing reports across terminal emulators

Run `bin/tbench run` inside each terminal emulator you want to compare
(each invocation writes its own JSON, tagged with that emulator's
`meta.terminal` — pass `--app NAME` to set that tag explicitly, e.g. to
distinguish two builds of the same emulator), then feed the resulting
files to `compare` — or use
`bin/tbench orchestrate` (below) to automate the "open each emulator, run
tbench inside it, then compare" sequence instead of doing it by hand:

```sh
# Side-by-side Markdown table, 2 or more files
bin/tbench compare results/ghostty.json results/iterm2.json results/terminal-app.json

# Write the table to a file as well as stdout
bin/tbench compare results/a.json results/b.json --output results/comparison.md

# Self-contained HTML chart report (written to results/compare-<timestamp>.html
# unless --output is given; never printed to stdout)
bin/tbench compare results/a.json results/b.json --format html

# Regression gate: 2 files (baseline, latest), exit 3 on regression
bin/tbench compare results/baseline.json results/latest.json --threshold 10%

# Regression gate over repeated runs (at least 4 per side for a significance test)
bin/tbench compare --threshold 5% results/base-*.json --latest results/new-*.json

# Keep raw per-trial samples (opt-in — see "Raw samples" below) so
# compare can add a bootstrap CI
bin/tbench run --keep-samples --output results/baseline.json
```

Rows are `metric [variant] (shell) (unit)`; columns are each file's
`meta.terminal` (disambiguated with the timestamp if two files share the
same terminal name). Cells show the median, with the best value in a row
**bolded** (every cell sharing the best median, and none when all files
tie) — direction-aware: `render.throughput` and `fire.fps` are
higher-is-better, every other metric is lower-is-better (each result's
`measurement.direction` is authoritative). A metric
missing from a given file shows as `-` rather than dropping the row. A
"Detail" table (p95/stddev, plus a bootstrap CI when `--keep-samples` data
is present — see below) follows the main comparison.

`--threshold` mode is the regression gate. It exits `0` if nothing got
worse past the threshold, or `3` with a table of the flagged rows. The
threshold must be a non-negative percentage (`10%` or `10`); anything else
is a usage error (exit `2`). The report it prints states every rule it
applied.

- **Two files** (baseline, then latest): each row is judged on the
  threshold alone. There is no significance test, and the report and a
  stderr line say so. Identical configurations differ by a few percent from
  run to run, so use a threshold well above that or pass repeated runs.
- **Groups of runs** (`BASELINE... --latest LATEST...`): each row is
  compared on the median of its per-run medians. With **at least 4 runs on
  each side**, a flagged row fails only if a Mann-Whitney U test across the
  run medians gives p<0.05. With fewer runs the test cannot reach that
  level, so the threshold alone decides, as in the two-file form. This form
  prints the Markdown regression report only.
- **Absolute rule**: a percentage-unit row whose baseline median is 0 (an
  idle `resource.cpu`) has no relative change. It fails when the latest
  median is more than `--threshold-points` (default 5) percentage points
  above the baseline. Percentage-unit rows with a non-zero baseline must
  also move by more than 1 point, the resolution of one sampling window.
- A baseline row with no counterpart in any latest run exits `3`: a metric
  that disappeared cannot pass the gate. A zero-baseline row in another
  unit is listed as not evaluated.

Trials inside a run are never used for significance. They share the run's
conditions, and testing them made two identical runs look different about
a quarter of the time; see [`docs/aa-baseline.md`](docs/aa-baseline.md).

### A/A self-check (`tbench aa`)

Before trusting a threshold, measure what your machine does with no change
at all:

```sh
# 8 runs of one scenario, then the analysis
bin/tbench aa --scenario shell-startup

# Analyse reports you already have of one configuration
bin/tbench aa --from results/aa-*/aa-*.json
```

`aa` runs the same configuration `--repeats` times (default 8), then for
each row shows the spread of the per-run medians and how many of the
possible half-vs-half splits the regression gate would fail at each of
`--thresholds` (default `0,2,5,10`). Every one of those failures is noise,
so the output ends with the smallest tested threshold that produced none.
With fewer than 8 runs the halves are too small for the gate's significance
test and the figures are for the threshold alone. The runs share one
session, so leave headroom for drift between sessions.
[`docs/aa-baseline.md`](docs/aa-baseline.md) has reference numbers.

### Raw samples and the bootstrap CI (`--keep-samples`)

By default `run` only keeps median/p95/stddev — the raw per-trial values
are discarded once those are computed, keeping report size/shape exactly
as before. `run --keep-samples` attaches each result's raw samples (in the
result's own unit, in trial order — the retained trials median and stddev were computed from; p95 also covers the dropped ones)
as a `"samples"` array, opt-in specifically because it changes JSON size
(20 trials ≈ 20 extra numbers per result).

When `compare` finds `"samples"` on a file, it computes a **bootstrap 95%
CI on the median** (1000 resamples, fixed-seed so the CI is reproducible
from the same samples) and shows it in the Markdown detail table, the HTML
report's table view and tooltip, and as a band in each per-metric chart.
Without samples, the chart uses a median→p95 whisker instead. The interval
describes the trials of that one run; it does not cover run-to-run
variation. The regression gate does not use samples.

### Automated multi-emulator runs (`orchestrate`, macOS only)

Manually opening each emulator and remembering to run them one at a time
(never simultaneously — see the settle/lock-file section above) is exactly
the kind of thing that gets skipped under time pressure, which is how the
70→13 MB/s `render.throughput` cross-pollution that motivated the
concurrent-run lock happened in the first place. `orchestrate` automates
the whole sequence — launch, wait, next, then compare — so it can't happen:

```sh
bin/tbench orchestrate --apps Ghostty,iTerm,Terminal \
  --scenario shell-startup --runs 20 \
  --output-dir results/orchestrate-$(date +%Y%m%d) \
  --format html
```

For each app in `--apps` (comma-separated), in order, `orchestrate`:

1. Launches a **new window/instance** of that app running `tbench run`
   (with `--scenario`/`--runs` passed through), via an app-specific adapter:
   `open -na <App> --args -e <script>` for Ghostty; AppleScript
   `do script`/`create window with default profile command` for
   Terminal/iTerm. **The first `do script` triggers a one-time macOS
   Automation permission prompt** for that app — accept it, since there's
   no unattended way around a permission macOS itself gates.
2. Waits (polling every 2s, `--timeout` seconds max, default 600) for that
   app's JSON to appear — it never launches the next app until the current
   one finishes, so two runs are never in flight at once. A timeout leaves
   that app's window open (for manual inspection) and moves on to the next
   app rather than aborting the whole batch.
3. Once every app has been tried, runs `bin/tbench compare` over however
   many JSON files were actually collected (`--format` is passed through)
   and prints where the report went.

An app that's already running when its turn comes up is **not quit** —
`orchestrate` only warns that its existing windows may add load to the
measurement, since silently closing a user's terminal session to get a
cleaner benchmark would be a worse trade. An app name with no adapter
(anything other than `ghostty`/`iterm`/`terminal`/`noa`, case-insensitively) is
skipped with a warning rather than falling back to generic UI-scripting
automation, which is fragile and would need its own (separate)
Accessibility permission grant for a result that isn't guaranteed to work;
add a new adapter arm in `src/orchestrate.rs` (`dispatch`) to support
another emulator.
`orchestrate` itself is macOS-only — `open -na`/AppleScript have no
portable Linux equivalent for "launch this GUI app and run a command in a
new window."

### HTML chart report (`--format html`)

A single self-contained file — inline SVG + vanilla JS only, no external
CDN/fonts/images — with one dot-and-interval chart per `metric`+variant+unit
combination (never mixing units on one axis), grouped into sections in this
order: Render Throughput, Latency, Resource, Shell. The report includes a
**Results at a glance** table showing every
entity's median and direction-aware competition rank for each metric (exact
median ties share a rank: `1, 1, 3`), an **Overview vs baseline**, and the
per-metric charts. Each chart:

- plots every entity's (file's) median as a dot in that entity's fixed color
  (same color for the same entity across every chart on the page — a legend
  at the top names the mapping), labeled alongside its rank (`#N`)
- marks every rank-1 dot in the row with a direction-aware **★ best** badge;
  exact ties therefore mark every tied winner
- shows a bootstrap 95% CI band on the median when `--keep-samples` data is
  available; otherwise it shows a median→p95 whisker
- shows a hover/keyboard-focus tooltip with rank/median/p95/stddev/runs

The report is light-only by design and ends with the same median/p95/stddev
table as the Markdown detail section, augmented with each entity's
direction-aware competition rank, as an accessible non-chart fallback.
Missing metric cells receive no rank.

## Trend over time

`compare` is snapshot-vs-snapshot; `trend` is `compare`'s longitudinal
counterpart — it plots one metric's history across every `results/tbench-*.json`
report you've accumulated, in chronological order (by `meta.timestamp`), so
a terminal/shell's benchmark trajectory is visible without picking two files
by hand:

```sh
# Default: every results/tbench-*.json report, sorted by meta.timestamp
# (errors clearly if fewer than 2 are found)
bin/tbench trend

# Explicit files, Markdown to stdout (default format)
bin/tbench trend results/a.json results/b.json results/c.json

# Self-contained HTML line-chart report (written to results/trend-<timestamp>.html
# unless --output is given; never printed to stdout)
bin/tbench trend --format html --output results/trend.html
```

Each `results/tbench-*.json`'s `meta.terminal` is that report's series
identity — if the accumulated reports span more than one terminal (e.g. you
re-ran `tbench run` in Ghostty and again in iTerm over time), each terminal
becomes its own colored line/table column rather than being averaged
together. Markdown output is one table per `metric [variant] (shell) (unit)`
row: timestamp, terminal, median, and the change from that terminal's
previous point. The change is annotated `(better)`/`(worse)`
(direction-aware) only beyond a noise band, `--noise PCT` (default 5%);
inside it the number is shown as `(within noise)`, because identical runs
already differ by a few percent ([`docs/aa-baseline.md`](docs/aa-baseline.md)).
Percentage-unit rows are compared in points with a band of 1 point. The HTML report is one small-multiple line chart per row
(x = report index, y = median), zero-baselined by default — except a
narrow-range series (all values within ~20% of each other) gets a
non-zero, explicitly annotated baseline so the line isn't flattened to
nothing — with a median→p95 whisker and a hover/focus tooltip per point. A
report missing a metric breaks that terminal's line rather than
interpolating across the gap.

## Metrics

Per-metric detail — exactly what each benchmark measures, how, and how far
to trust it — lives in [`docs/benchmark-metrics.md`](docs/benchmark-metrics.md);
this table and the sections below are the summary. `show` (and
`run`'s recap) and `compare`'s Markdown/HTML outputs also append a one-line
"Metric guide" for the metrics present in the report.

| Metric | Unit | Shell-scoped? | Variants | What it measures |
|---|---|---|---|---|
| `shell.startup` | ms | yes | — | `{shell} -i -c exit` wall-clock time — interactive startup including rc files |
| `cmd.overhead` | μs | yes | — | `{shell} -c true` wall-clock time — dominated by process spawn and shell initialization; not a differential against a direct exec |
| `render.throughput` | MB/s, lines/s | no | `plain`, `ansi`, `cjk` | Producer-side completion rate for a fixed fixture written to the terminal |
| `scroll.proxy` | ms | no | — | Proxy: wall-clock time to write the `plain` fixture to the terminal |
| `resource.mem` | MB | no | `idle`, `load` | RSS of the terminal emulator's own process |
| `resource.cpu` | % | no | `idle`, `load` | CPU use of the terminal emulator's own process (Linux: CPU time per 1 s window; macOS: `ps %cpu`) |
| `fire.fps` | fps | no | `<columns>x<rows>` | Producer-completed frames/second of the deterministic DOOM-fire stream at the recorded region |

Each metric runs N trials (default 20, `--runs`) and reports median, stddev,
and p95. `resource.mem`/`resource.cpu` instead take up to 5 samples per
variant (fewer if `--runs` < 5): one-second windows on Linux, point-in-time
`ps` samples on macOS. `fire.fps` is
duration-based rather than iteration-based: each trial runs `FIRE_SECS`
wall-clock seconds (env var, default 5; finite range 0.1–60), and the trial count used is
`min(--runs, 5)` for the same reason `resource.*` caps its sample count —
running it a full 20 times by default would make the suite's total
wall-clock unreasonably long.

**Shell-scoped vs. not**: `shell.startup`/`cmd.overhead` run once per entry
in `--shells` and report `"shell"` accordingly. `render.throughput`,
`scroll.proxy`, and `fire.fps` are written to the terminal
emulator, not any particular shell, so they ignore `--shells`, run
once, and report `"shell": null`.

### Comparing terminal emulators

`render.throughput`/`scroll.proxy`/`fire.fps` results are only
meaningful in the context of the emulator they ran in. Each report's
`meta.terminal` field (from `$TERM_PROGRAM`/`$TERM_PROGRAM_VERSION`, falling
back to `$TERM`) identifies that emulator — to compare emulators, run
`bin/tbench run` inside each one and diff the resulting JSON files by
`meta.terminal`.

### `render.throughput` variants: plain / ansi / cjk

`cjk` mixes full-width Japanese text and emoji into the same line count as
`plain`/`ansi`. Because UTF-8 encodes those characters as 3-4 bytes each
(vs. 1 for ASCII), **`cjk`'s MB/s is not a fair byte-for-byte comparison**
against `plain`/`ansi` — the same visual width costs more bytes. Use
`linesps` (or a cells/s metric you compute yourself: multiply CJK/emoji
runs by roughly 2 for their double-width terminal columns) for a more
apples-to-apples pace comparison across variants.

### `input.latency` is retired

`run` no longer emits `input.latency`. It opened a fresh pty and timed the
echo of one byte, and that echo comes from the kernel's line discipline: the
same ~0.01 ms round trip appears with no `cat` and no terminal emulator
attached. The number said nothing about the terminal it was reported under,
and its `loaded` variant only reflected CPU contention from the flood. The
profile stays registered in Reporting Spec v1 so older reports still
validate and render.

### `fire.fps`: DOOM-fire IO stress

Runs the integrated DOOM-fire workload (`src/fire.rs` — a faithful Rust
port of the retired vendored C tool, with identical palette, seed, frame
order, and byte stream), which repaints a truecolor half-block region every frame at the
live window size (`full` mode — upstream DOOM-fire-zig's full-window
condition). fps scales ~1/cell-count. The region actually rendered is
recorded as the result's `<columns>x<rows>` variant and workload identity,
so reports at differing geometries do not align as the same result. Without
a controlling tty the metric is skipped. Like
`render.throughput`, its `write()` blocks on pty flow control, so
frames/second approximates the terminal's drain rate, not a confirmed
on-screen frame rate — see the proxy caveats below. No C compiler is
needed anymore.

The workload uses an overlapped producer (`fire-v2-overlapped`, recorded
in the report row's `backend` field): a writer thread drains frame N into
the pty while the main thread composes frame N+1, hiding compose time
behind the drain; throughput remains limited by whichever of composition
or PTY writing is slower. The byte stream and
producer design are identical to the C tool, so fps from the two is
directly comparable; the retired serial (v1) producer's numbers are not —
check `backend` before comparing `fire.fps` values across reports.

### `resource.mem` / `resource.cpu`: which process, and how it is sampled

These target the **terminal emulator's own process**, not the shell or
`tbench` itself. The scenario runs first in a full `run`, so `idle` is
sampled before any other scenario has pushed output through the terminal.

**Linux** (backend `procfs`): the target is the process holding the master
side of `tbench`'s controlling pty, found through `/proc/<pid>/fdinfo`.
Each variant takes up to 5 one-second windows (fewer if `--runs` < 5); the
`load` variant starts sampling 1 s after the flood begins.

- `resource.mem` is RSS at the end of each window (`/proc/<pid>/statm`,
  the figure `ps -o rss` shows).
- `resource.cpu` is user plus system CPU time accumulated during the
  window as a percentage of one core, at kernel clock-tick resolution
  (1 percentage point per window at 100 ticks/s). This is reported under
  its own profile, `org.terminalbench.resource.cpu.window`, because it is
  a different method from the `ps` figure.
- The report's `extensions["org.terminalbench.resource"]` records the pid,
  executable, thread count, how many pty masters the process holds (more
  than one means it also serves other windows, tabs or panes), PSS per
  variant, and when each variant's sampling started. Extensions never affect comparability.

If the pty belongs to tmux, screen, `script`, sshd, mosh, zellij, dtach or
abduco, or no readable process holds it, the scenario is **skipped with a
stderr warning**. Helper processes of multi-process emulators are not
included.

**macOS** (backend `ps`): the target is found by walking the process
ancestry from `tbench` upward to the first executable living inside a
`.app` bundle (`.../Contents/MacOS/<bin>`); inside `tmux` or over SSH none
is found and the scenario is skipped. Up to 5 `ps` samples are taken 200 ms
apart. macOS's `ps -o %cpu` is a **decayed running average**, not an
instantaneous sample; taking the median of several samples is a
mitigation, not a fix, so treat `resource.cpu` there as directional.

Other systems are skipped.

### Proxy/approximate metrics — read before trusting the numbers

- `scroll.proxy` has **no native scroll or redraw-event API** behind it. It
  is literally the wall-clock time to write the `plain` fixture, the same
  measurement as `render.throughput [plain]` — it does not observe terminal settling and is not a true redraw-event
  measurement (proposal §3 item 3).
- `render.throughput`/`scroll.proxy`/`fire.fps` describe whatever reads the
  controlling tty. Inside `tmux` or GNU `screen` that is the multiplexer,
  not the emulator: `run` warns on stderr and records it in
  `meta.meta_notes`. On Linux the same happens when the pty master is held
  by `script`, sshd, mosh, zellij, dtach or abduco.
- `render.throughput`/`scroll.proxy`/`fire.fps` need a controlling terminal
  (`/dev/tty`) to measure the registered PTY workload. Without one
  (non-interactive/CI contexts), those metrics are skipped; no `/dev/null`
  substitute is reported under the same profile.
- `render.throughput` and `scroll.proxy` time only the write of an
  in-memory fixture to the terminal (backend `rust-writer`). Reports from
  earlier versions timed a whole external `cat` process including its
  startup and file read (backend `rust-timer`); the gate treats the two as
  NOT_COMPARABLE.
- `fire.fps` reports frames/second, but — like `render.throughput` — it is
  a producer-side PTY-write-completion proxy, not a confirmed on-screen
  frame rate; see the caveat above.
- `resource.mem`/`resource.cpu` need a directly-launched terminal emulator
  on Linux or macOS (see above).

Every result carries these caveats inline via an optional `"notes"` field
in the JSON (see schema below) so they travel with the data.

## Backend and precision

Process-timed metrics use a built-in monotonic timer (`std::time::Instant` —
no fork per sample), with warmup (default 3 discarded trials) and 1.5×IQR
outlier rejection, reported as backend `"rust-timer"`. This replaces both
the previous zsh `EPOCHREALTIME` timer and the optional hyperfine
dependency — warmup + outlier handling, hyperfine's main advantages over
the old fallback, are now built in.

Each result object includes a `"backend"` field (`"rust-timer"`,
`"rust-writer"` for `render.throughput`/`scroll.proxy`,
`"ps"` or `"procfs"` for `resource.*`,
`"fire-v2-overlapped"` for `fire.fps`) so reports are traceable to the
backend that produced them. Reports from the earlier backends
(`hyperfine`, `zsh-timer`, `zsh-zpty`) remain conformant under Reporting
Spec v1; as always, the pairwise gate treats differing backends as
NOT_COMPARABLE, so compare like with like.

## Development

```sh
make help         # list targets
make build        # release build (bin/tbench also builds on demand)
make test         # cargo unit tests + tests/report-v1.sh (Reporting Spec v1 suite)
make lint         # rustfmt --check + clippy -D warnings
make check        # lint + test — what CI runs
```

CI (`.github/workflows/ci.yml`) runs `make lint`, `make unit`, and the
conformance suite on Linux and macOS runners with the explicit `TBENCH_*`
controls from the section above. Runners have no controlling terminal, so
`tests/pty_run.rs` runs the whole suite inside a pty it owns and prints
the rows it got: a tty-bound scenario that stops producing results fails
CI instead of being skipped. On Linux that covers every scenario
(`--strict`); on macOS `resource.*` cannot run from a test binary, and the
test asserts that it is the only scenario without results.

## Requirements

- A Rust toolchain to build (`cargo build --release`); the result is one
  binary that carries its scenarios and fixtures, so `cargo install --path .`
  gives a `tbench` that works from any directory (reports go to
  `./results/`)
- the shells being benchmarked (`zsh`/`bash`/... ) on `PATH`
- a controlling terminal, for `render.throughput`/`scroll.proxy`/
  `fire.fps` — unavailable metrics
  are skipped with an actionable warning rather than replaced by a
  different workload; `run` lists the scenarios that produced no results,
  and `--strict` turns that into exit `1`

hyperfine, python3, a C compiler and `cat` are not needed at run time
(`compare`/`show`/`trend`/`validate` are built in, the DOOM-fire workload
is an integrated port of the retired C tool with an identical byte stream,
and the output fixtures are generated in memory). The binary still calls a
few standard tools: `uname` and `date` everywhere, each measured shell with
`--version`, `ps` for `resource.*` on macOS, and `pmset`, `sysctl`,
`system_profiler` and `osascript` on macOS for metadata and `orchestrate`.

The scenario files are compiled in. A `scenarios/` directory next to the
binary or in the working directory takes precedence, which is what a
checkout uses; the files keep the fixed flat `key: value` structure.

## Directory layout

```
bin/tbench           # thin launcher: builds (if needed) and execs the Rust binary
bin/tbench-validate  # thin launcher for `tbench validate` (Reporting Spec v1 CLI)
src/main.rs          # CLI dispatch (run/list/show/compare/trend/orchestrate/validate)
src/stats.rs         # median/p95/stddev, IQR, bootstrap CI, Mann-Whitney
src/measure.rs       # process timer, terminal writer, adaptive sampling, output flood
src/scenario.rs      # scenarios/*.yaml loading (flat key: value reader)
src/runner.rs        # scenario kind dispatch
src/lock.rs          # results/.tbench.lock concurrent-run detection
src/resource.rs      # terminal-PID discovery, resource.mem/resource.cpu
src/procfs.rs        # Linux /proc readers (pty-master owner, RSS, PSS, CPU time)
src/fidelity.rs      # DECSET 2026 synchronized-output probe
src/fire.rs          # integrated DOOM-fire workload (port of the retired C tool)
src/report.rs        # Reporting Spec v1 JSON assembly + env metadata
src/linuxhost.rs     # Linux host facts (machine model, CPU, power, low-power mode)
src/show.rs / compare.rs / trend.rs   # report views (recap, tables/HTML, history)
src/aa.rs            # A/A self-check of the regression gate
src/orchestrate.rs   # per-app launch adapters (macOS)
src/validate.rs      # Reporting Spec v1 reference validator
scenarios/           # *.yaml scenario definitions
schemas/             # tbench-report-v1.schema.json (Reporting Spec v1)
tests/report-v1.sh   # Reporting Spec v1 conformance suite (make conformance)
tests/pty_run.rs     # full run inside a pty, so CI covers tty-bound scenarios
docs/                # proposals, metric guide, reporting spec
fixtures/            # generated *.txt (gitignored) + conformance/ corpus
results/             # JSON/HTML output + .tbench.lock (gitignored)
```

## JSON report format

The normative format is defined by the
[`Reporting Specification v1`](docs/reporting-spec-v1.md). Use the
[`JSON Schema`](schemas/tbench-report-v1.schema.json) for structural checks
and the [`valid minimal fixture`](fixtures/conformance/v1/valid-minimal.json)
as a non-normative illustration. A Schema-only pass does not establish
semantic conformance, claim safety, or pairwise comparability; use
`bin/tbench-validate` and the versioned corpus for those decisions.

## Known limitations

- `bin/tbench run` still only emits JSON (no Markdown report from `run`
  itself — `compare` produces Markdown/HTML from existing JSON files); no `prompt.render` metric — deferred to later phases
  (proposal §6.6 extension 2/3).
- `--runs auto` only converges for `shell.startup`/`cmd.overhead`/
  `render.throughput`/`scroll.proxy`; `resource.*`/
  `fire.fps` fall back to a fixed trial count with a stderr note.
- The concurrent-run lock only detects other `tbench run` processes, not
  unrelated system load (a build, backups, etc.) — see caveat above.
- `orchestrate` is macOS-only, supports `ghostty`/`iterm`/`terminal`/`noa` only
  (see above for adding an adapter), and can't guarantee the new window
  actually has keyboard/tty focus — that's an emulator implementation
  detail, and an unfocused window could skew `render.throughput` etc.
- The DECSET 2026 probe only reports whether the terminal *recognizes*
  synchronized output — it does not confirm any given run actually
  avoided frame skipping (that would need screen-capture + a permission
  grant, judged not worth it yet per proposal §C).
- The bootstrap CI needs `--keep-samples` data and describes one run only.
  The regression gate's significance test needs at least 4 runs per side.
- `scroll.proxy` is an approximation, not a true redraw-event
  measurement — see caveats above.
- `render.throughput`'s `cjk` variant's MB/s isn't directly comparable to
  `plain`/`ansi`'s (different bytes-per-character) — see caveat above.
- `resource.mem`/`resource.cpu` only work when `tbench` runs inside a
  directly-launched terminal emulator on Linux or macOS (not tmux/SSH).
  The two platforms use different CPU methods and profiles, and on macOS
  `resource.cpu` inherits the decayed-average `%cpu` semantics.
- `fire.fps`'s `--runs` is duration-based (`FIRE_SECS`, finite range
  0.1–60 seconds per trial) and capped at 5 trials regardless of `--runs`, unlike the
  iteration-based scenarios.
