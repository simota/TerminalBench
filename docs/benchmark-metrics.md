# Benchmark Metrics in Detail

What each benchmark (metric) measures, how it measures it, and how far to
trust the numbers — units, variants, backends, and known limitations. This
is the long-form companion to the one-line summaries in the README's
Metrics table. The definitions live in `scenarios/*.yaml` and
`src/runner.rs` / `src/measure.rs` / `src/stats.rs` / `src/resource.rs`.

Common behavior:

- Each metric runs N trials (default 20, `--runs`) and reports **median /
  p95 / stddev**. Exceptions: `resource.*` (up to 5 samples
  per variant) and `fire.fps` (duration-based; trial count is
  `min(--runs, 5)`).
- "Direction": low = lower is better, high = higher is better. In the JSON,
  `measurement.direction` is the authoritative source.
- Process-timed trials get a warmup (default 3 discarded
  runs) and 1.5×IQR outlier filtering. The removed count is recorded per
  result as `outliers_removed`. The filter applies to median and stddev
  only; **p95 is taken from every timed trial**, because filtering first
  removes the tail it is meant to describe
  ([p95-outlier-simulation.md](p95-outlier-simulation.md)). With fewer than
  about 50 trials p95 is a coarse figure: at 20 it is the second-largest
  trial. `resource.*` point samples and duration-
  based `fire.fps` trials do not use this filter.
- A `--settle` pause (default 2s) separates scenarios and variants so one
  measurement's leftover load doesn't pollute the next one's first samples.
- A measured child process that does not exit within 30 seconds is killed
  and the trial is rejected; non-zero exits are rejected as well.

## Overview

| Metric | Unit | Direction | Shell-scoped? | Variants | Measures (one line) |
|---|---|---|---|---|---|
| `shell.startup` | ms | low | yes | — | Fresh interactive shell process startup time |
| `cmd.overhead` | μs | low | yes | — | Wall-clock of `{shell} -c true` (spawn + init + builtin) |
| `render.throughput` | MB/s, lines/s | high | no | `plain` / `ansi` / `cjk` | Producer-side PTY output throughput |
| `scroll.proxy` | ms | low | no | — | Time until the large-output producer exits (proxy) |
| `resource.mem` | MB | low | no | `idle` / `load` | RSS of the emulator's own process |
| `resource.cpu` | % | low | no | `idle` / `load` | CPU use of the emulator's own process |
| `fire.fps` | fps | high | no | `<columns>x<rows>` | Producer-completed frames/s for a DOOM-fire repaint stream |

Shell-scoped = yes means the metric runs once per entry in `--shells`
(default `zsh,bash`) and reports that shell in `"shell"`. Shell-scoped = no
means it measures the terminal emulator itself, runs once regardless of
`--shells`, and reports `"shell": null`.

---

## shell.startup — interactive shell startup time

- **What it measures**: wall-clock time of `{shell} -i -c exit`. `-i`
  (interactive) means rc files (`.zshrc` / `.bashrc` etc.) **are** loaded —
  this is the metric for "how much did my dotfiles / plugin-manager change
  slow down startup".
- **How**: a built-in monotonic timer (`std::time::Instant`) around a
  direct process spawn — no shell, no fork per sample. Recorded per result
  as backend `"rust-timer"`. (Reports from the retired `hyperfine`/
  `zsh-timer` backends remain valid; differing backends are
  NOT_COMPARABLE under the pairwise gate.)
- **Unit / direction**: ms, lower is better.
- **Caveats**: supports `--runs auto` (adaptive sampling). Cross-machine
  absolute comparisons still require matching environments — see the
  Reporting Spec's pairwise gate. The run-context shell fact records the
  `--version` output for every measured shell in deterministic name order;
  each result's shell dimension identifies which one produced that row.
  Each trial starts a new process, but filesystem/page caches are not
  cleared, so this is not a machine-cold boot measurement.

## cmd.overhead — command dispatch overhead

- **What it measures**: wall-clock time of `{shell} -c true` = shell
  process spawn + shell initialization + the `true` builtin + exit. This is the fixed
  per-invocation cost a script pays every time it launches a shell. It is
  the raw wall-clock, not the shell-vs-direct-exec differential the original
  proposal described, so process spawn dominates the figure.
- **How**: same as `shell.startup` (`rust-timer`). No rc files are loaded
  (non-interactive), so the difference from `shell.startup` is
  approximately the rc-file contribution.
- **Unit / direction**: μs, lower is better.
- **Caveats**: fork/exec cost has high relative variance; under
  `--runs auto` this metric often hits the 50-trial cap instead of
  converging early (expected, not a failure).

## render.throughput — draw throughput

- **What it measures**: writes a fixed fixture straight to the
  terminal and derives **MB/s** and **lines/s** (two rows) from the
  wall-clock time of that write. It measures the terminal emulator tbench is running
  inside — more precisely its PTY drain rate — not shell performance.
- **Variants**:
  - `plain` — ASCII-only log-like text
  - `ansi` — with ANSI color sequences (adds escape-parsing cost)
  - `cjk` — mixed full-width Japanese text and emoji. UTF-8 encodes those
    characters as 3-4 bytes each, so **`cjk`'s MB/s is not byte-for-byte
    comparable** to `plain`/`ansi`; use lines/s to compare across variants.
  - Each fixture is 100,000 copies of one line, generated in memory by the
    binary (same bytes as the fixture files earlier versions fed to
    `cat`).
- **How**: a child `tbench` process builds the fixture, then times only
  its `write` to the controlling terminal and reports it (backend
  `"rust-writer"`). Process startup and fixture generation are outside the
  timed span; a child is used so a trial can be killed after 30 s if the
  terminal stops reading. Earlier backends timed a whole external `cat`
  process, so their values are not interchangeable with these.
- **Unit / direction**: MB/s and lines/s, higher is better.
- **Caveats**: this relies on `write()` blocking on PTY flow control — it
  measures a **drain rate**, not confirmed on-screen rendering (an emulator
  that skips frames can look faster). Whenever
  `meta.synchronized_output_supported` isn't `"supported"`,
  `meta.frame_skip_risk_note` flags this. Without a controlling terminal
  (`/dev/tty`, e.g. CI), the metric is skipped; it is never relabelled raw
  `/dev/null` I/O.

## scroll.proxy — scroll performance (proxy)

- **What it measures**: total wall-clock time for the `plain` fixture to be
  written to the controlling terminal. It is the same measurement as
  `render.throughput [plain]`, reported as a duration.
- **How**: no native scroll/redraw-event API exists, so "time until output
  completes" is used as a **proxy indicator** (proposal §3 item 3).
- **Unit / direction**: ms, lower is better.
- **Caveats**: a coarse proxy that does not observe scrollback settling or
  any redraw event. Like `render.throughput`, it is skipped without a controlling terminal.

## input.latency — retired

No longer emitted. The measurement wrote one byte to the master side of a
fresh pseudo-terminal and timed the echo. That echo is produced by the
kernel's PTY line discipline, so the result (about 0.01 ms on Linux) was the
same with no `cat` child and no terminal emulator involved; the `loaded`
variant only added CPU contention from the output flood. The
`org.terminalbench.input.pty-round-trip` profile remains registered so
existing reports stay conformant.

## resource.mem / resource.cpu — resource footprint

- **What it measures**: memory (MB) and CPU use (%) of the **terminal
  emulator's own process** — not the shell, not tbench.
- **Which process**:
  - Linux: the process holding the master side of tbench's controlling
    pty, found by scanning `/proc/<pid>/fd` for `ptmx` descriptors whose
    `fdinfo` `tty-index` matches. This identifies the emulator directly,
    without guessing from the process ancestry.
  - macOS: the first ancestor of tbench whose executable lives inside a
    `.app` bundle (`.../Contents/MacOS/<bin>`).
- **Variants**:
  - `idle` — at rest. The scenario runs first in a full `run` and waits
    **at least 5s** regardless of `--settle`.
  - `load` — sampled while a `render.throughput`-style flood is running.
    If that flood cannot start, or exits early, the load rows are dropped
    rather than reported as unloaded samples.
- **How, Linux** (backend `procfs`): up to 5 one-second windows per variant
  (fewer if `--runs` < 5); `load` starts sampling 1 s after the flood.
  `resource.mem` is RSS from `/proc/<pid>/statm` at the end of each window.
  `resource.cpu` is `utime + stime` from `/proc/<pid>/stat` accumulated over
  the window, divided by the wall-clock length of the window, as a
  percentage of one core (so it can exceed 100 for a multi-threaded
  emulator). Resolution is one clock tick per window, normally 1
  percentage point. It carries the profile
  `org.terminalbench.resource.cpu.window`.
- **How, macOS** (backend `ps`): up to 5 point-in-time `ps` samples per
  variant, 200 ms apart; the median is reported.
- **Extra data (Linux)**: `extensions["org.terminalbench.resource"]` holds
  the pid, `comm`, executable path, thread count, the number of pty masters
  the process holds, PSS per variant, and the UTC time each variant's
  sampling started. PSS divides shared pages among
  the processes mapping them and is often far below RSS for GUI programs.
- **Unit / direction**: MB / %, both lower is better.
- **Caveats**:
  - Skipped with a warning when the pty belongs to tmux, screen, `script`,
    sshd, mosh, zellij, dtach or abduco, when no readable process holds it,
    and on systems other than Linux and macOS.
  - A single-instance emulator serves every window from one process, so
    its figures include the other windows. `pty_masters_held` above 1 shows
    that.
  - Helper processes (GPU, renderer, multiplexer daemons) are not included,
    and GPU memory is not part of RSS or PSS.
  - Linux and macOS use different CPU methods; the pairwise gate treats the
    two profiles as NOT_COMPARABLE.
  - On macOS, `ps %cpu` is a decayed running average: treat `resource.cpu`
    there as **directional**, not an instantaneous reading.
  - With at most 5 samples, the reported p95 is the maximum.

## fire.fps — DOOM-fire render stress

- **What it measures**: runs the integrated DOOM-fire workload from
  `src/fire.rs` (a faithful Rust port compatible with the retired vendored
  C implementation, formerly `tools/fire.c` — identical palette, seed,
  frame order, and byte stream), which repaints a truecolor
  half-block region every frame at the live window size (full-window
  condition), and reports **frames/second**.
- **How**: each trial is duration-based — it runs `FIRE_SECS` wall-clock
  seconds (env var, default 5; finite range 0.1–60) rather than a fixed iteration count — and
  the trial count is capped at `min(--runs, 5)` to bound total suite
  runtime. No C compiler is needed: the workload is compiled into the
  tbench binary.
- **Producer**: a writer thread drains frame N into the pty while the main
  thread composes frame N+1 (`fire-v2-overlapped`, recorded in the result's
  `backend` field). Throughput is limited by the slower of composition and
  PTY writing; it does not isolate drain rate —
  identical to the C tool, so their fps values are directly comparable.
  fps from the retired serial (v1) producer is **not** — check `backend`
  before comparing.
- **Unit / direction**: fps, higher is better.
- **Caveats**: like `render.throughput`, this is a **producer-side
  PTY-drain-rate proxy**, not a confirmed on-screen frame rate. fps scales
  roughly with 1/cell-count. The rendered `<columns>x<rows>` region is the
  result variant and workload identity, so differing geometries do not
  compare as the same result. Without a controlling tty the metric is
  skipped.

---

## Where summaries appear in the output

`bin/tbench show` (and `run`'s post-run recap) and `bin/tbench compare`'s
Markdown and HTML outputs append a one-line-per-metric "Metric guide" for
the metrics present in the report. This document is the full-detail
counterpart to those one-liners.
