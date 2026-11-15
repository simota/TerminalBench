# Five terminals on Linux: a worked comparison

One machine, one afternoon, five terminal emulators: alacritty, ghostty,
kitty, rio and WezTerm. This is an example of a full comparison run and of
how to read one. The numbers describe this host and these builds, not the
terminals in general.

## Summary

- ghostty and rio drain output fastest, then kitty, then alacritty. WezTerm
  is far behind on every output metric.
- rio uses the least memory by a wide margin, ghostty the most.
- rio is second on plain and coloured text but fourth on CJK, where it is
  about a third slower than kitty.
- `shell.startup` and `cmd.overhead` do not separate the terminals. They
  measure the shell.
- ghostty's throughput varies up to 19% between runs, so its lead over rio
  on plain text and on `scroll.proxy` is not established by this data.

## Setup

| | |
|---|---|
| Date | 2026-11-15 |
| Machine | Gigabyte B550 AORUS ELITE V2, AMD Ryzen 7 3700X |
| OS | Linux 7.2.14-zen1 x86_64, governor `performance`, on mains |
| Session | Wayland, niri 26.04, 2560x1440 output at 99.946 Hz, scale 1 |
| Shells | zsh 5.9.2, bash 5.3.20 |
| tbench | 2.0.0-dev with the Linux `resource.*` backend and the `rust-writer` backend |

| Terminal | Version | Grid (cols x rows) |
|---|---|---|
| alacritty | 0.17.0 | 282x67 |
| ghostty | 1.4.0 | 282x66 |
| kitty | 0.50.0 | 281x68 |
| rio | 0.5.31 | 281x64 |
| WezTerm | 20260716-195552-76b606ec | 280x68 |

Each run started a fresh terminal process, moved its window to the same
output and set it to the full output width (2544x1380 px). kitty and rio
were started with JetBrains Mono and opaque backgrounds, rio with a font
size chosen to bring its grid close to kitty's. alacritty, ghostty and
WezTerm ran with the local configuration. The grids are close but not
equal, which matters for `fire.fps` (see below).

## Method

Four runs per terminal, interleaved (one run of each terminal, then the
next round), about half an hour in total. One run at a time, guarded by a
lock.

```sh
TBENCH_DISPLAY_REFRESH_HZ=99.946 \
  tbench run --keep-samples --strict --output results/<terminal>-run<N>.json
```

Defaults otherwise: 20 trials, 3 warmup trials, 2 s settle, 1.5xIQR
filtering for median and stddev, p95 from every timed trial.

Checks on every report before it was used:

- `tbench run --strict` exited 0 and no scenario was without results.
- The report validates as conformant.
- The process holding the pty master was the terminal under test
  (`extensions["org.terminalbench.resource"].comm`), not a multiplexer.
- No warnings on stderr.

All 20 runs passed.

## How the tables are computed

- **Median**: the median of the four per-run medians.
- **Run medians**: the smallest and the largest of the four.
- **p95**, **stddev**: the median of the four per-run values.
- **Significant** means a two-sided Mann-Whitney U test across the four
  run medians of each side gives p<0.05, the same test the regression gate
  uses. With four runs per side the smallest attainable p is 0.030, which
  is reached only when the two groups do not overlap at all. Four runs can
  therefore say "separated" or "not shown", nothing finer.

## Results

### Output throughput

**`render.throughput` plain (MB/s, higher is better)**

| Terminal | Median | Run medians (min..max) | p95 | Stddev within a run |
|---|---|---|---|---|
| ghostty | 156.0 | 138.9..164.6 | 172.6 | 8.55 |
| rio | 136.7 | 130.5..157.0 | 152.3 | 8.85 |
| kitty | 111.6 | 109.8..112.5 | 114.6 | 2.00 |
| alacritty | 68.43 | 68.11..69.36 | 69.98 | 1.02 |
| wezterm | 17.91 | 17.75..18.20 | 18.72 | 0.528 |

**`render.throughput` ansi (MB/s, higher is better)**

| Terminal | Median | Run medians (min..max) | p95 | Stddev within a run |
|---|---|---|---|---|
| ghostty | 161.2 | 135.7..166.6 | 174.1 | 8.87 |
| rio | 132.6 | 132.0..133.9 | 143.2 | 5.76 |
| kitty | 103.7 | 103.2..105.3 | 107.1 | 1.86 |
| alacritty | 68.32 | 68.11..68.53 | 69.20 | 0.699 |
| wezterm | 18.74 | 18.51..18.92 | 19.50 | 0.474 |

**`render.throughput` cjk (MB/s, higher is better)**

| Terminal | Median | Run medians (min..max) | p95 | Stddev within a run |
|---|---|---|---|---|
| ghostty | 167.2 | 146.3..178.5 | 176.9 | 10.89 |
| rio | 68.33 | 68.13..68.99 | 70.22 | 0.986 |
| kitty | 99.89 | 99.48..101.0 | 103.3 | 1.84 |
| alacritty | 75.74 | 75.04..75.83 | 77.41 | 0.921 |
| wezterm | 22.08 | 21.95..22.17 | 22.65 | 0.436 |

**`scroll.proxy` (ms, lower is better)**

| Terminal | Median | Run medians (min..max) | p95 | Stddev within a run |
|---|---|---|---|---|
| ghostty | 80.64 | 72.58..93.12 | 95.20 | 6.09 |
| rio | 87.21 | 86.44..92.14 | 96.51 | 4.86 |
| kitty | 107.9 | 107.3..108.4 | 113.4 | 2.68 |
| alacritty | 175.3 | 173.2..176.3 | 179.9 | 2.37 |
| wezterm | 667.7 | 663.1..676.8 | 691.0 | 16.04 |

Every pair is significant except ghostty against rio on plain text
(p=0.112) and on `scroll.proxy` (p=0.885). ghostty's run medians spread by
16 to 26% on these metrics, against less than 3% for kitty, alacritty and
WezTerm, so its position relative to rio needs more runs.

These rows time how long the terminal takes to read a fixed amount of
output from the pty. They say nothing about when the pixels reach the
screen.

### Frame rate under full-screen redraw

**`fire.fps` (frames per second, higher is better, grids differ)**

| Terminal | Median | Run medians (min..max) | p95 | Stddev within a run |
|---|---|---|---|---|
| ghostty | 238.4 | 234.2..240.1 | 240.1 | 3.68 |
| rio | 188.5 | 187.5..189.5 | 189.3 | 0.775 |
| kitty | 184.2 | 183.7..184.6 | 185.0 | 1.18 |
| alacritty | 186.7 | 182.1..187.7 | 187.5 | 0.627 |
| wezterm | 20.48 | 20.15..20.77 | 25.84 | 2.21 |

`fire.fps` redraws the whole grid, so the result depends on the number of
cells, and the grids differ by up to 6% (rio has the fewest cells, kitty
and WezTerm the most). The comparability gate reports these rows as
`NOT_COMPARABLE` across terminals for that reason. What the data supports:
ghostty is clearly ahead, WezTerm is clearly behind, and alacritty, kitty
and rio are too close to rank given the grid differences.

### Memory and CPU

**`resource.mem` idle (MB RSS, lower is better)**

| Terminal | Median | Run medians (min..max) | p95 | Stddev within a run |
|---|---|---|---|---|
| ghostty | 190.0 | 189.3..190.5 | 190.0 | 0.000 |
| rio | 56.91 | 56.81..57.05 | 56.91 | 0.000 |
| kitty | 134.7 | 134.4..134.8 | 134.7 | 0.000 |
| alacritty | 107.2 | 107.1..107.3 | 107.2 | 0.000 |
| wezterm | 113.7 | 113.5..114.4 | 113.7 | 0.128 |

**`resource.mem` load (MB RSS, lower is better)**

| Terminal | Median | Run medians (min..max) | p95 | Stddev within a run |
|---|---|---|---|---|
| ghostty | 239.2 | 238.7..240.0 | 239.2 | 0.026 |
| rio | 79.48 | 79.43..79.74 | 79.48 | 0.000 |
| kitty | 225.6 | 224.4..226.2 | 225.6 | 0.112 |
| alacritty | 172.8 | 172.6..172.8 | 172.8 | 0.000 |
| wezterm | 127.6 | 126.9..128.1 | 128.0 | 0.472 |

**`resource.cpu` load (% of one core, lower is better)**

| Terminal | Median | Run medians (min..max) | p95 | Stddev within a run |
|---|---|---|---|---|
| ghostty | 217.0 | 214.0..217.0 | 218.5 | 2.19 |
| rio | 93.99 | 93.99..99.99 | 95.49 | 1.18 |
| kitty | 149.5 | 149.0..158.0 | 151.5 | 1.89 |
| alacritty | 108.0 | 108.0..108.0 | 109.0 | 0.561 |
| wezterm | 132.0 | 131.0..133.0 | 134.5 | 1.36 |

All pairs are significant. Idle CPU was 0% for every terminal in every
run. RSS counts shared pages in full; the proportional figure (PSS, from
`extensions`) is lower for all five and keeps both orders:

| Terminal | PSS idle (MB) | PSS load (MB) |
|---|---|---|
| rio | 29.1 | 51.8 |
| alacritty | 40.0 | 105.5 |
| wezterm | 50.7 | 64.1 |
| kitty | 54.1 | 145.1 |
| ghostty | 100.9 | 150.0 |

"Load" is sampled while the terminal drains a flood of output, and the
terminals drain it at very different speeds. A terminal that reads faster
has more scrollback filled and does more work per second, so load memory
and load CPU are not a cost per unit of work. GPU memory and compositor
work are not included.

### Shell metrics

**`shell.startup` zsh (ms, lower is better)**

| Terminal | Median | Run medians (min..max) | p95 | Stddev within a run |
|---|---|---|---|---|
| ghostty | 7.66 | 7.64..7.70 | 8.01 | 0.163 |
| rio | 7.67 | 7.61..7.69 | 7.89 | 0.125 |
| kitty | 7.57 | 7.46..7.69 | 7.86 | 0.153 |
| alacritty | 7.69 | 7.56..7.72 | 8.07 | 0.179 |
| wezterm | 7.71 | 7.57..7.75 | 7.98 | 0.189 |

**`shell.startup` bash (ms, lower is better)**

| Terminal | Median | Run medians (min..max) | p95 | Stddev within a run |
|---|---|---|---|---|
| ghostty | 27.95 | 27.89..28.00 | 28.78 | 0.364 |
| rio | 27.85 | 27.33..27.94 | 28.37 | 0.238 |
| kitty | 27.76 | 27.27..27.99 | 28.20 | 0.238 |
| alacritty | 27.78 | 27.25..28.02 | 28.37 | 0.322 |
| wezterm | 27.76 | 27.59..27.84 | 28.31 | 0.301 |

**`cmd.overhead` zsh (µs, lower is better)**

| Terminal | Median | Run medians (min..max) | p95 | Stddev within a run |
|---|---|---|---|---|
| ghostty | 3,833 | 3,764..3,914 | 4,042 | 127.7 |
| rio | 3,826 | 3,704..3,878 | 3,980 | 76.53 |
| kitty | 3,816 | 3,786..3,823 | 3,963 | 56.95 |
| alacritty | 3,838 | 3,780..3,955 | 4,024 | 104.7 |
| wezterm | 3,827 | 3,797..3,841 | 3,959 | 104.9 |

**`cmd.overhead` bash (µs, lower is better)**

| Terminal | Median | Run medians (min..max) | p95 | Stddev within a run |
|---|---|---|---|---|
| ghostty | 1,029 | 1,004..1,069 | 1,132 | 35.36 |
| rio | 1,025 | 1,009..1,050 | 1,118 | 38.84 |
| kitty | 1,032 | 1,004..1,059 | 1,126 | 47.08 |
| alacritty | 1,050 | 1,021..1,071 | 1,159 | 49.17 |
| wezterm | 1,020 | 1,012..1,041 | 1,160 | 38.03 |

No pair is significant except WezTerm against ghostty on `shell.startup`
for bash (0.7%, p=0.030). With 40 pairwise tests in this group, one hit at
the smallest attainable p is what chance alone produces. These metrics do
not depend on the terminal in this data.

## Repeatability

kitty was measured a second time 45 minutes later with the same settings.
No row differed significantly between the two series. The largest
differences were 2.4% on `shell.startup` for zsh (p=0.061) and 1.5 points
on `resource.cpu` under load; throughput and `scroll.proxy` agreed within
0.2%.

A third kitty series used a translucent, blurred background and a 5%
taller cell. Throughput, `scroll.proxy` and memory matched the opaque
series within 1%. `fire.fps` was 3% higher, in line with the 4.4% smaller
grid. The cost of blur falls on the compositor, which is not measured.

## Limits

- One machine, one GPU driver, one compositor, one day.
- Four runs per terminal. See the note on p=0.030 above.
- Font and configuration were aligned for kitty and rio only.
- ghostty's run-to-run spread is large and unexplained.
- Wayland only. No X11 run.
- Nothing here measures input latency or time to photon.

## Commands

```sh
# per terminal and run, inside a fresh terminal window
TBENCH_DISPLAY_REFRESH_HZ=99.946 \
  tbench run --keep-samples --strict --output results/<terminal>-run<N>.json

# validation and the full matrix
tbench validate results/<terminal>-run<N>.json
tbench compare results/*-run*.json --format markdown --output results/compare-matrix.md

# pairwise against kitty, run level
tbench compare results/kitty-run*.json --latest results/rio-run*.json --threshold 0%

# drift within one terminal
tbench trend results/kitty-run*.json
```
