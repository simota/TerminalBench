# TerminalBench Reporting Specification v1

Status: Normative
Protocol ID: `urn:tbench:protocol:reporting`
Schema ID: `urn:tbench:schema:report:v1`
Wire version described here: `1.0.0`

The key words **MUST**, **MUST NOT**, **REQUIRED**, **SHOULD**, and **MAY** are normative requirements. This document is sufficient to implement an independent producer or validator. `schemas/tbench-report-v1.schema.json` and `bin/tbench-validate` are checked implementations; this document is the source of truth if they disagree.

## Quickstart: safe comparison of two reports

```sh
set -e

# Run once inside each terminal emulator.
bin/tbench run --output results/report-a.json &&
bin/tbench run --output results/report-b.json &&

# Both reports must conform, then the pair must be eligible.
bin/tbench-validate validate results/report-a.json &&
bin/tbench-validate validate results/report-b.json &&

# Continue only when this prints COMPARABLE and exits 0.
bin/tbench-validate compare results/report-a.json results/report-b.json &&

# The legacy comparator presents values; it does not replace eligibility.
bin/tbench compare results/report-a.json results/report-b.json
```

Stop before the final command on `NOT_COMPARABLE` (exit 7) or `UNDETERMINED` (exit 8).

## 1. Purpose and claim boundary

The format exchanges terminal-performance observations without requiring trust in TerminalBench itself. It is a reporting and comparability protocol, not a leaderboard. A conformant report says that its structure and claims obey this specification. It does not self-certify measurement accuracy, neutrality, reproducibility, or pairwise comparability.

In particular, producer-side PTY completion is not on-screen rendering; a synthetic PTY echo is not GUI input latency or physical-keypress-to-photon latency. A producer MUST describe only the registered observation boundary and MUST carry every registered limitation and excluded claim.

## 2. Versioning and extensions

`wire_version` and `measurement.profile_version` are independent version identifiers. This specification supports exactly wire version `1.0.0`; `1.0.1`, `1.1.0`, and every other value are unsupported and MUST fail closed. A validator MUST also fail closed on an unsupported profile version. Published wire and profile semantics are immutable. Any semantic change to a field, method, unit, scope, boundary, workload, limitation, or exclusion requires a new version or ID.

Core objects are closed: unknown properties are invalid. Vendor data MAY appear only in the top-level `extensions` object. Each key MUST be a lowercase reverse-DNS name such as `com.example.runner`. Unknown extensions MAY be preserved, but MUST NOT establish a core claim, repair invalid core data, or make two reports comparable.

## 3. Report model

A report is a JSON object with these REQUIRED members:

| Member | Requirement |
|---|---|
| `$schema` | exactly `urn:tbench:schema:report:v1` |
| `wire_version` | exactly `1.0.0` |
| `producer` | non-empty `name` and an explicit known/unknown `version` fact |
| `protocol` | exactly `{"id":"urn:tbench:protocol:reporting","version":"1.0.0"}` |
| `run_context.environment` | all facts listed below, each explicitly known or unknown |
| `meta` | legacy-compatible descriptive metadata |
| `results` | non-empty result array |

A string fact is either `{"status":"known","value":"..."}` with a non-empty value, or `{"status":"unknown","reason":"..."}` with a non-empty reason. Boolean and positive-number facts use the same shape with a typed `value`. Unknown never means “same”, “default”, or “not relevant”.

Duplicate member names are forbidden in every JSON object. A validator MUST reject them before semantic validation because last-key-wins parsing can change classifications, controls, or measurements. Every numeric value governed by this specification MUST be finite and have absolute value at most `1e308`; result statistics and samples are additionally non-negative.

`run_context.environment` MUST contain `os`, `architecture`, `terminal`, `shell`, `machine_model`, `power_source`, `low_power_mode`, `display_refresh_hz`, and `synchronized_output_supported`. The first six are string facts; `low_power_mode` is a boolean fact; `display_refresh_hz` is a positive-number fact. A known `synchronized_output_supported` value MUST be exactly `supported`, `unsupported`, or `no_reply`; it MAY instead be an unknown fact.

Known control identifiers use these canonical registries:

- `os`: `darwin`, `linux`, `freebsd`, `openbsd`, `netbsd`, `windows`, or `other:<token>`.
- `architecture`: `aarch64`, `x86_64`, or `other:<token>`. Producers map `arm64`/`aarch64` to `aarch64` and `amd64`/`x86_64` to `x86_64`.
- `power_source`: `ac`, `battery`, or `ups`.
- `<token>`: lowercase ASCII beginning with an alphanumeric and then containing only alphanumerics, `.`, `_`, or `-`.

`machine_model` is a case-sensitive, trimmed opaque identifier: leading/trailing whitespace and line breaks are forbidden. Its source SHOULD be the platform's stable hardware-model identifier, or an explicitly supplied CI value. The reference emitter uses macOS `sysctl -n hw.model`; on Linux it uses the DMI system vendor and product name (the board vendor and name when those are firmware placeholders, or the device-tree model), followed by ` / ` and the CPU model name. A producer MUST NOT case-fold or reinterpret it.

`meta` preserves the legacy consumer surface. It MUST contain non-empty string `timestamp` (RFC 3339 date-time), `os`, `runner`, `terminal`, and `shell`, plus `synchronized_output_supported` (`supported`, `unsupported`, or `no_reply`). It MAY contain `frame_skip_risk_note`, `power_source`, `low_power_mode` (boolean), `machine_model`, `cpu`, `gpu`, `display_refresh_hz` (positive number), and `meta_notes`; no other member is allowed. `meta` is descriptive; pairwise eligibility uses `run_context`, not values guessed from `meta`.

When an environment fact is known, the corresponding legacy `meta` member MUST exist and agree for `os`, `terminal`, `shell`, `machine_model`, `power_source`, `low_power_mode`, `display_refresh_hz`, and `synchronized_output_supported`. Producers MUST use the same terminal identity for a valid `TBENCH_TERMINAL_ID` override and for `TERM_PROGRAM` detection in both locations. A display refresh pair agrees when its absolute difference is at most `0.1` Hz; all other pairs require exact equality.

The reference emitter accepts `TBENCH_TERMINAL_ID`, `TBENCH_MACHINE_MODEL`, `TBENCH_POWER_SOURCE`, `TBENCH_LOW_POWER_MODE`, and `TBENCH_DISPLAY_REFRESH_HZ` for portable/CI collection. Valid overrides populate both the known fact and its legacy metadata counterpart. Invalid overrides MUST NOT become known facts and MUST produce an actionable `meta.meta_notes` entry. The reference emitter also accepts `run --app NAME` as an explicit terminal-identity override; it follows the same trimmed single-line identifier rule (rejected at argument parsing rather than fail-soft) and takes precedence over `TBENCH_TERMINAL_ID`.

## 4. Result invariants

Every result MUST contain legacy fields `metric`, `shell`, `unit`, `median`, `p95`, `stddev`, `runs`, `outliers_removed`, and `backend`, plus `measurement`. `variant`, `notes`, and `samples` are optional.

`measurement` MUST contain exactly `profile_id`, `profile_version`, `classification`, `method_id`, `method_version`, `scope`, `observed_boundary`, `unit`, `direction`, `limitations`, `excluded_claims`, `workload`, and `dimensions`. `classification` is `direct-observation`, `proxy`, or `sampled`; v1 currently registers proxy and sampled profiles. `limitations` and `excluded_claims` are non-empty string arrays. `dimensions` contains exactly nullable-string `shell`, non-empty-string `backend`, and nullable-string `variant`.

- `median`, `p95`, `stddev`, and every sample MUST be finite, non-negative, and at most `1e308`. JSON `NaN` and infinities are forbidden.
- `p95` MUST be greater than or equal to `median`.
- `runs` MUST be an integer of at least one. `outliers_removed` MUST be an integer from zero through `runs`.
- If `samples` is present, it MUST be non-empty and its length MUST equal `runs`.
- The tuple `(metric, shell, unit, variant, profile_id)` MUST be unique within a report.
- `measurement.dimensions.shell`, `.backend`, and `.variant` MUST exactly repeat their legacy result counterparts. This duplication protects tolerant legacy consumers while giving v1 validators a closed identity object.
- `measurement.workload` MUST be a known `{status,id,version}` object matching the registry, or an explicit unknown `{status,reason}` object. The exact registry triple is mandatory only when status is `known`. A genuinely unavailable identity MAY be `unknown` and remains conformant, but every pairwise eligibility decision involving it MUST be `UNDETERMINED` unless a known mismatch independently produces `NOT_COMPARABLE`. A known workload version is `1.0.0`.

## 5. Immutable profile registry

All profiles have `profile_version=1.0.0`, `method_version=1.0.0`. Exact registered fields, limitations, and exclusions are normative.

| metric / unit | profile_id | class | method_id | scope | observed_boundary | direction | variants | backends |
|---|---|---|---|---|---|---|---|---|
| `shell.startup` / `ms` | `org.terminalbench.shell.startup` | sampled | `org.terminalbench.method.process-wall-clock` | `shell-process-startup` | `process-invocation-to-process-exit` | lower | null | `hyperfine`, `zsh-timer`, `rust-timer` |
| `cmd.overhead` / `us` | `org.terminalbench.command.overhead` | sampled | `org.terminalbench.method.process-wall-clock` | `shell-command-process` | `process-invocation-through-command-exit` | lower | null | `hyperfine`, `zsh-timer`, `rust-timer` |
| `render.throughput` / `MBps` | `org.terminalbench.render.throughput.bytes` | proxy | `org.terminalbench.method.pty-write-wall-clock` | `pty-output-pipeline` | `fixture-write-start-to-producer-exit` | higher | plain, ansi, cjk | `zsh-timer`, `rust-timer`, `rust-writer` |
| `render.throughput` / `linesps` | `org.terminalbench.render.throughput.lines` | proxy | `org.terminalbench.method.pty-write-wall-clock` | `pty-output-pipeline` | `fixture-write-start-to-producer-exit` | higher | plain, ansi, cjk | `zsh-timer`, `rust-timer`, `rust-writer` |
| `scroll.proxy` / `ms` | `org.terminalbench.scroll.proxy` | proxy | `org.terminalbench.method.pty-write-wall-clock` | `pty-output-and-scrollback-proxy` | `fixture-write-start-to-producer-exit` | lower | null | `zsh-timer`, `rust-timer`, `rust-writer` |
| `input.latency` / `ms` | `org.terminalbench.input.pty-round-trip` | proxy | `org.terminalbench.method.zpty-echo-round-trip` | `pseudo-terminal-echo` | `byte-write-to-pty-echo-read` | lower | idle, loaded | `zsh-zpty`, `rust-pty` |
| `resource.mem` / `MB` | `org.terminalbench.resource.memory` | sampled | `org.terminalbench.method.process-sample` | `terminal-process-resident-memory` | `identified-terminal-process-rss-sample` | lower | idle, load | `ps`, `procfs` |
| `resource.cpu` / `pct` | `org.terminalbench.resource.cpu` | sampled | `org.terminalbench.method.process-sample` | `terminal-process-cpu` | `identified-terminal-process-cpu-sample` | lower | idle, load | `ps` |
| `resource.cpu` / `pct` | `org.terminalbench.resource.cpu.window` | sampled | `org.terminalbench.method.process-cpu-time-window` | `terminal-process-cpu` | `identified-terminal-process-cpu-time-over-wall-clock-window` | lower | idle, load | `procfs` |
| `fire.fps` / `fps` | `org.terminalbench.fire.fps.geometry` | proxy | `org.terminalbench.method.pty-write-wall-clock` | `pty-output-pipeline` | `frame-write-start-to-producer-exit` | higher | `<width>x<rows>` (`1..1000` × `1..500`) | `fire-v2-overlapped` |
| `fire.fps` / `fps` | `org.terminalbench.fire.fps` | proxy | `org.terminalbench.method.pty-write-wall-clock` | `pty-output-pipeline` | `frame-write-start-to-producer-exit` | higher | null | `fire-v2-overlapped` |

“lower” means `lower-is-better`; “higher” means `higher-is-better`. Shell profiles require a non-empty `shell`; all other profiles require `shell:null`.

Registry amendment (2026-07-20): `rust-timer` (built-in monotonic timer of the Rust single-binary implementation, warmup + IQR filtering equivalent to the prior backends) and `rust-pty` (native openpty echo round trip) are registered as additional backends for the profiles listed above. This is additive: existing reports remain conformant, and — as was already the case between `hyperfine` and `zsh-timer` — a pair of reports whose `dimensions.backend` differ is NOT_COMPARABLE under the pairwise gate. `fire-v2-overlapped` is retained by the Rust implementation because its byte stream, seed, frame order, and overlapped producer design are identical to the C tool it ports.

Registry amendment (2026-07-20, geometry identity): new reports use `org.terminalbench.fire.fps.geometry`, whose required variant is the rendered cell region and whose workload is `urn:tbench:workload:fire-{variant}:v1`. The legacy null-variant `org.terminalbench.fire.fps` remains registered solely so existing reports remain valid. Differing geometry variants produce different result identities and therefore are NOT_COMPARABLE; producers MUST NOT omit a geometry that they can observe.

Registry amendment (2026-11-15, in-process writer): `rust-writer` is registered as an additional backend for the render and scroll profiles. The producer is the runner itself: it generates the fixture in memory, writes it to the controlling terminal, and times that write alone. The bytes are identical to the earlier fixtures, so workload identities are unchanged. Earlier backends ran an external `cat` on a fixture file and timed the whole process, including process creation and the file read; values from the two are therefore not interchangeable, and differing backends are NOT_COMPARABLE as before.

Registry amendment (2026-11-15, Linux resource sampling): `procfs` (resident set size read from Linux `/proc/<pid>/statm` for the process holding the master side of the controlling pty) is registered as an additional backend for `org.terminalbench.resource.memory`. `org.terminalbench.resource.cpu.window` is a new profile for the same metric and unit as `org.terminalbench.resource.cpu`: it reports user plus system CPU time accumulated over a fixed wall-clock window as a percentage of one core, which is a different method from the `ps` statistic and therefore a different profile and workload. Both changes are additive. A report MUST NOT carry both CPU profiles for the same variant, because legacy consumers identify a result by metric, shell, unit, and variant.

The exact limitation / excluded-claim / workload triples are:

| profile | limitations[0] | excluded_claims[0] | workload id |
|---|---|---|---|
| shell startup | `Includes shell startup configuration and process scheduling; it does not isolate terminal rendering.` | `Terminal rendering latency` | `urn:tbench:workload:shell-startup:v1` |
| command overhead | `Includes process spawn and shell initialization in addition to builtin dispatch.` | `In-process command dispatch latency` | `urn:tbench:workload:command-overhead:v1` |
| render bytes (`org.terminalbench.render.throughput.bytes`) | `Measures producer-side PTY write completion; terminal parsing, final-frame presentation, and frame skipping are not observed.` | `On-screen rendering throughput` | `urn:tbench:workload:render-{variant}:v1` |
| render lines (`org.terminalbench.render.throughput.lines`) | `Measures producer-side PTY write completion; terminal parsing, final-frame presentation, and frame skipping are not observed.` | `On-screen rendering throughput` | `urn:tbench:workload:render-{variant}:v1` |
| scroll proxy | `No scroll, redraw, frame-presentation, or display event is observed.` | `On-screen scroll latency` | `urn:tbench:workload:scroll-proxy:v1` |
| input PTY round trip | `Uses a synthetic PTY echo and excludes physical input devices, GUI event routing, rendering, and display presentation.` | `Physical-keypress-to-photon latency` | `urn:tbench:workload:input-{variant}:v1` |
| resource memory | `Samples one identified process and may omit helper-process or shared-memory costs.` | `Whole-system memory cost` | `urn:tbench:workload:resource-{variant}:v1` |
| resource CPU | `Process CPU is a sampled operating-system statistic and is not an instantaneous whole-system measurement.` | `Whole-system CPU cost` | `urn:tbench:workload:resource-{variant}:v1` |
| resource CPU window (`org.terminalbench.resource.cpu.window`) | `Covers one identified process over fixed wall-clock windows at kernel clock-tick resolution; helper processes are not included.` | `Whole-system CPU cost` | `urn:tbench:workload:resource-window-{variant}:v1` |
| fire fps (`org.terminalbench.fire.fps.geometry`) | `Measures producer-side PTY write completion for a truecolor repaint stream at the cell geometry recorded by variant; terminal parsing, compositing, and confirmed on-screen frame presentation are not observed.` | `Confirmed on-screen frame rate` | `urn:tbench:workload:fire-{variant}:v1` |
| fire fps (`org.terminalbench.fire.fps`) | `Measures producer-side PTY write completion for a live window-sized truecolor repaint stream; terminal parsing, compositing, and confirmed on-screen frame presentation are not observed, and fps depends on window cell count.` | `Confirmed on-screen frame rate` | `urn:tbench:workload:fire:v1` |

An independent producer MUST copy these strings exactly for v1. It MUST select a new profile/version rather than relabel a different method.

For proxy profiles, `notes` MUST be absent or exactly the profile-specific canonical safe note below. Arbitrary prose is invalid even when it appears equivalent; closing the field prevents synonym-based claim expansion.

| metric | canonical safe note |
|---|---|
| `render.throughput` | `Proxy only: measures producer-side PTY write completion; it does not observe terminal parsing, final-frame presentation, frame time, FPS, pixels presented, or complete rendering.` |
| `scroll.proxy` | `Proxy only: measures producer-side PTY write completion for the scroll workload; it does not observe scroll, redraw, display events, frame time, FPS, pixels presented, or complete rendering.` |
| `input.latency` | `Proxy only: measures a synthetic PTY echo round trip; it does not observe physical keypress, GUI input latency, screen response, keypress-to-display, or keypress-to-photon latency.` |
| `fire.fps` (`org.terminalbench.fire.fps.geometry`) | `Proxy only: measures producer-completed frames for a truecolor repaint stream at the cell geometry recorded by variant; throughput includes overlapped frame composition and PTY writing and does not observe terminal parsing, compositing, or confirmed on-screen frame presentation, so this fps is not a measured render frame rate.` |
| `fire.fps` (`org.terminalbench.fire.fps`, legacy) | `Proxy only: measures producer-side PTY write completion for a live window-sized truecolor repaint stream; it does not observe terminal parsing, compositing, or confirmed on-screen frame presentation, so this fps is a producer-side drain-rate proxy, not a measured render frame rate, and it scales inversely with window cell count so values are only comparable at matching window geometry.` |

Thus render/scroll profiles cannot claim screen presentation, pixels presented, frame time, FPS, or complete rendering; the input profile cannot claim physical-keypress-to-photon, keypress-to-display, GUI input latency, or screen response; and the fire profile cannot claim a confirmed on-screen frame rate.

## 6. Validation and pairwise eligibility

Validation and comparability are separate operations. `tbench-validate validate REPORT` checks structure, immutable profiles, numerical invariants, and claim safety. `tbench-validate compare A B` decides eligibility only and never computes a winner.

For the same result identity, `COMPARABLE` requires both reports to use wire version `1.0.0`; identical profile/version, method/version, unit, dimensions, and known workload ID/version; and equality of every required known control fact. Required controls for every profile are `os`, `architecture`, `machine_model`, `power_source`, and `low_power_mode`. Shell profiles additionally require `shell`. Render, scroll, and fire profiles additionally require `display_refresh_hz` and `synchronized_output_supported`. Known display refresh controls match when their absolute difference is at most `0.1` Hz; other controls require exact equality.

`terminal` is the subject under test, not a control: both reports MUST have a known terminal identity, but those identities MAY and normally will differ. Treating the subject name as an equality condition would make cross-emulator comparison impossible.

A known mismatch or a different result-identity set yields `NOT_COMPARABLE`. If no known mismatch exists but any decisive fact or workload is unknown, the result is `UNDETERMINED`. A known mismatch takes precedence over unknown data. Unknown is never a wildcard.

Existing `tbench compare` remains a tolerant legacy presentation/statistics consumer. A v1-safe workflow MUST run `validate` on both inputs, then the pairwise eligibility gate, and only then invoke `tbench compare`.

## 7. Diagnostics and exits

The reference validator uses stable rule IDs and deterministic JSON Pointer locations. A diagnostic provides `expected`, a bounded `actual` summary, and a short `remediation`; it does not echo the full report.

| Exit | Meaning |
|---:|---|
| 0 | conformant or `COMPARABLE` |
| 2 | usage error |
| 3 | I/O, UTF-8, strict JSON, or duplicate-key error |
| 4 | conformance/claim-safety failure |
| 5 | unsupported wire version |
| 6 | unsupported profile ID/version |
| 7 | `NOT_COMPARABLE` |
| 8 | `UNDETERMINED` |

## 8. Schema role and conformance

`schemas/tbench-report-v1.schema.json` is a structural subset of this specification. Schema success is necessary but not sufficient for `CONFORMANT`, claim-safe, or `COMPARABLE`: the semantic rules in sections 4–6, including immutable registry equality, cross-field parity, duplicate-key rejection, claim boundaries, and pairwise eligibility, are normative and require a semantic validator.

An implementation is conformant only when it passes the versioned fixtures and mutation corpus in `fixtures/conformance/v1/` and `tests/report-v1.sh`. Passing the corpus does not prove absolute measurement accuracy. Future legacy adapters MUST preserve provenance and report conversion loss; no v1 adapter may invent unknown facts.

Third-party validators can run the portable semantic corpus without implementing reference-only diagnostics, exit-code details, profile listing, or Python internals:

```sh
TBENCH_PORTABLE=1 TBENCH_VALIDATOR=/path/to/validator zsh tests/report-v1.sh
```

The reference emitter on Linux derives `power_source` and `low_power_mode` from `/sys/class/power_supply` and the ACPI platform profile, leaves either unknown when it has no sound basis, and records the basis under the `org.terminalbench.host` extension. When `TERM_PROGRAM` is unset it reports the executable name of the process holding the pty master as the terminal identity. It does not discover the display refresh rate.

For CI, or wherever platform discovery cannot provide a control, supply it explicitly:

```sh
TBENCH_TERMINAL_ID='ci-terminal 1.0' \
TBENCH_MACHINE_MODEL='ci-runner-class-a' \
TBENCH_POWER_SOURCE=ac \
TBENCH_LOW_POWER_MODE=false \
TBENCH_DISPLAY_REFRESH_HZ=60 \
  bin/tbench run --output results/ci-report.json
```
