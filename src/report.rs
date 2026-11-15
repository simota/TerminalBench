//! Reporting Specification v1 document assembly.
//! The wire format is a fixed contract: schemas/
//! tbench-report-v1.schema.json plus the semantic rules in
//! docs/reporting-spec-v1.md remain authoritative, and bin/tbench-validate
//! must accept every report this module emits.

use serde_json::{json, Map, Value};
use std::collections::BTreeSet;
use std::process::Command;
use std::time::Instant;

/// One result row, in report order. Numeric stats are stored in the
/// result's own unit; rounding to the wire precision (5 decimals for
/// stats, 6 for samples) happens at serialization.
#[derive(Debug, Clone)]
pub struct ResultRow {
    pub metric: String,
    pub shell: Option<String>,
    pub unit: String,
    pub median: f64,
    pub p95: f64,
    pub stddev: f64,
    pub runs: usize,
    pub outliers_removed: usize,
    pub backend: String,
    pub variant: Option<String>,
    pub notes: Option<String>,
    pub samples: Option<Vec<f64>>,
}

fn round_to(v: f64, decimals: i32) -> f64 {
    let f = 10f64.powi(decimals);
    (v * f).round() / f
}

/// The registered profile for a row, from the validator's registry (the
/// single copy in code). A row no profile covers is emitted as
/// "unsupported", which the validator rejects.
fn measurement_json(row: &ResultRow) -> Value {
    use crate::validate::Profile;
    const UNSUPPORTED: Profile = Profile {
        metric: "",
        unit: "",
        profile_id: "org.terminalbench.unsupported",
        classification: "proxy",
        method_id: "org.terminalbench.method.unsupported",
        method_version: "1.0.0",
        scope: "unknown",
        observed_boundary: "unknown",
        direction: "lower-is-better",
        limitation: "No Reporting Specification v1 profile is registered for this metric and unit.",
        excluded: "Any standardized performance claim",
        backends: &[],
        variants: &[],
        shell_required: false,
        workload: "urn:tbench:workload:unknown:v1",
    };
    let p = crate::validate::profile_for_row(
        &row.metric,
        &row.unit,
        row.variant.as_deref(),
        &row.backend,
    )
    .unwrap_or(&UNSUPPORTED);
    let workload_id = p
        .workload
        .replace("{variant}", row.variant.as_deref().unwrap_or("unknown"));
    json!({
        "profile_id": p.profile_id,
        "profile_version": "1.0.0",
        "classification": p.classification,
        "method_id": p.method_id,
        "method_version": p.method_version,
        "scope": p.scope,
        "observed_boundary": p.observed_boundary,
        "unit": row.unit,
        "direction": p.direction,
        "limitations": [p.limitation],
        "excluded_claims": [p.excluded],
        "workload": {"status": "known", "id": workload_id, "version": "1.0.0"},
        "dimensions": {
            "shell": row.shell,
            "backend": row.backend,
            "variant": row.variant,
        },
    })
}

fn result_json(row: &ResultRow) -> Value {
    let mut m = Map::new();
    m.insert("metric".into(), json!(row.metric));
    m.insert("shell".into(), json!(row.shell));
    m.insert("unit".into(), json!(row.unit));
    m.insert("median".into(), json!(round_to(row.median, 5)));
    m.insert("p95".into(), json!(round_to(row.p95, 5)));
    m.insert("stddev".into(), json!(round_to(row.stddev, 5)));
    m.insert("runs".into(), json!(row.runs));
    m.insert("outliers_removed".into(), json!(row.outliers_removed));
    m.insert("backend".into(), json!(row.backend));
    if let Some(v) = &row.variant {
        m.insert("variant".into(), json!(v));
    }
    // Proxy notes are closed profile data in Reporting Spec v1: caller
    // prose is replaced so synonyms cannot broaden a proxy claim.
    let profile_id = crate::validate::profile_for_row(
        &row.metric,
        &row.unit,
        row.variant.as_deref(),
        &row.backend,
    )
    .map(|p| p.profile_id);
    let effective_notes = crate::validate::canonical_proxy_note(&row.metric, profile_id)
        .map(str::to_string)
        .or_else(|| row.notes.clone());
    if let Some(n) = effective_notes {
        m.insert("notes".into(), json!(n));
    }
    if let Some(s) = &row.samples {
        let rounded: Vec<f64> = s.iter().map(|v| round_to(*v, 6)).collect();
        m.insert("samples".into(), json!(rounded));
    }
    m.insert("measurement".into(), measurement_json(row));
    Value::Object(m)
}

// --- environment metadata ------------------------------------------------

fn cmd_stdout(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

pub fn canonical_os(uname_s: &str) -> String {
    let v = uname_s.to_lowercase();
    match v.as_str() {
        "darwin" | "linux" | "freebsd" | "openbsd" | "netbsd" => v,
        _ if v.starts_with("windows")
            || v.starts_with("mingw")
            || v.starts_with("msys")
            || v.starts_with("cygwin") =>
        {
            "windows".to_string()
        }
        _ => format!("other:{}", portable_token(&v)),
    }
}

pub fn canonical_arch(uname_m: &str) -> String {
    match uname_m.to_lowercase().as_str() {
        "arm64" | "aarch64" => "aarch64".to_string(),
        "amd64" | "x86_64" => "x86_64".to_string(),
        other => format!("other:{}", portable_token(other)),
    }
}

fn portable_token(value: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for c in value.to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '_' || c == '-' {
            out.push(c);
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() {
        "unknown".to_string()
    } else {
        out
    }
}

/// Single-line, trimmed, non-empty — the spec's requirement for opaque
/// identifier overrides (TBENCH_TERMINAL_ID etc.).
pub(crate) fn trimmed_opaque_id(value: &str) -> Option<&str> {
    if value.is_empty() || value != value.trim() || value.contains('\n') || value.contains('\r') {
        None
    } else {
        Some(value)
    }
}

fn canonical_power_source(value: &str) -> Option<String> {
    match value.to_lowercase().as_str() {
        "ac" | "battery" | "ups" => Some(value.to_lowercase()),
        _ => None,
    }
}

fn valid_refresh_hz(value: &str) -> Option<f64> {
    // ^(0|[1-9][0-9]*)([.][0-9]+)?$ with length <= 16 — no leading zeros,
    // no sign, no exponent, matching the original zsh validation.
    if value.len() > 16 {
        return None;
    }
    let (int_part, frac_part) = match value.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (value, None),
    };
    let int_ok = int_part == "0"
        || (!int_part.is_empty()
            && !int_part.starts_with('0')
            && int_part.chars().all(|c| c.is_ascii_digit()));
    let frac_ok = match frac_part {
        None => true,
        Some(f) => !f.is_empty() && f.chars().all(|c| c.is_ascii_digit()),
    };
    if !int_ok || !frac_ok {
        return None;
    }
    let v: f64 = value.parse().ok()?;
    if v > 0.0 && v <= 1000.0 {
        Some(v)
    } else {
        None
    }
}

pub fn detect_terminal() -> String {
    match std::env::var("TERM_PROGRAM") {
        Ok(tp) if !tp.is_empty() => match std::env::var("TERM_PROGRAM_VERSION") {
            Ok(ver) if !ver.is_empty() => format!("{tp} {ver}"),
            _ => tp,
        },
        _ => std::env::var("TERM").unwrap_or_else(|_| "unknown".to_string()),
    }
}

/// Metrics whose workload is written to the controlling tty.
pub fn is_tty_bound(metric: &str) -> bool {
    matches!(metric, "render.throughput" | "scroll.proxy" | "fire.fps")
}

#[derive(Default)]
struct HostMeta {
    power_source: Option<String>,
    low_power_mode: Option<bool>,
    machine_model: Option<String>,
    cpu: Option<String>,
    gpu: Option<String>,
    display_refresh_hz: Option<f64>,
    system_profiler_over_budget_ms: Option<u64>,
}

/// Best-effort macOS metadata (proposal §A item 4): every field is
/// optional, failure is omission, and the one slow call
/// (`system_profiler SPDisplaysDataType`) is timed — gpu/refresh are
/// dropped with a meta note if it exceeded its 1s budget.
/// Test hook: report assembly with every discovered host fact absent, on
/// any OS (the variable predates Linux discovery, hence its name).
fn host_discovery_disabled() -> bool {
    std::env::var("TBENCH_TEST_DISABLE_MACOS_META").is_ok_and(|v| v == "1")
}

/// Linux host facts plus the `org.terminalbench.host` extension recording
/// what each derived fact rests on.
fn collect_linux_meta() -> (HostMeta, Option<Value>) {
    if host_discovery_disabled() {
        return (HostMeta::default(), None);
    }
    let host =
        crate::linuxhost::collect(std::path::Path::new("/sys"), std::path::Path::new("/proc"));
    let extension = json!({
        "kernel": cmd_stdout("uname", &["-r"]),
        "session_type": std::env::var("XDG_SESSION_TYPE").ok().filter(|v| !v.is_empty()),
        "cpu_governor": host.cpu_governor,
        "platform_profile": host.platform_profile,
        "power_source_basis": host.power_basis,
        "low_power_mode_basis": host.low_power_basis,
    });
    let meta = HostMeta {
        power_source: host.power_source.map(str::to_string),
        low_power_mode: host.low_power_mode,
        machine_model: host.machine_model,
        cpu: host.cpu,
        ..HostMeta::default()
    };
    (meta, Some(extension))
}

fn collect_macos_meta() -> HostMeta {
    let mut meta = HostMeta::default();

    if host_discovery_disabled() {
        return meta;
    }

    if let Some(batt) = cmd_stdout("pmset", &["-g", "batt"]) {
        if batt.contains("AC Power") {
            meta.power_source = Some("ac".into());
        } else if batt.contains("Battery Power") {
            meta.power_source = Some("battery".into());
        }
    }
    if let Some(pm) = cmd_stdout("pmset", &["-g"]) {
        for line in pm.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix("lowpowermode") {
                match rest.trim() {
                    "1" => meta.low_power_mode = Some(true),
                    "0" => meta.low_power_mode = Some(false),
                    _ => {}
                }
            }
        }
    }
    meta.machine_model = cmd_stdout("sysctl", &["-n", "hw.model"]);
    meta.cpu = cmd_stdout("sysctl", &["-n", "machdep.cpu.brand_string"]);

    let sp_start = Instant::now();
    let sp_json = cmd_stdout("system_profiler", &["SPDisplaysDataType", "-json"]);
    let sp_ms = sp_start.elapsed().as_millis() as u64;
    if sp_ms <= 1000 {
        if let Some(text) = sp_json {
            if let Ok(v) = serde_json::from_str::<Value>(&text) {
                if let Some(displays) = v.get("SPDisplaysDataType").and_then(|d| d.as_array()) {
                    for gpu_entry in displays {
                        if meta.gpu.is_none() {
                            meta.gpu = gpu_entry
                                .get("sppci_model")
                                .and_then(|m| m.as_str())
                                .map(str::to_string);
                        }
                        if meta.display_refresh_hz.is_none() {
                            if let Some(ndrvs) =
                                gpu_entry.get("spdisplays_ndrvs").and_then(|n| n.as_array())
                            {
                                for d in ndrvs {
                                    let res = d
                                        .get("_spdisplays_resolution")
                                        .and_then(|r| r.as_str())
                                        .unwrap_or("");
                                    if let Some(hz) = parse_refresh_hz(res) {
                                        meta.display_refresh_hz = Some(hz);
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    } else {
        meta.system_profiler_over_budget_ms = Some(sp_ms);
    }
    meta
}

/// "... @ 120.00Hz" -> 120.0
fn parse_refresh_hz(resolution: &str) -> Option<f64> {
    let idx = resolution.find('@')?;
    let rest = resolution[idx + 1..].trim();
    let hz = rest.strip_suffix("Hz")?.trim();
    hz.parse().ok()
}

fn fact_known(value: Value) -> Value {
    json!({"status": "known", "value": value})
}

fn fact_unknown(reason: &str) -> Value {
    json!({"status": "unknown", "reason": reason})
}

/// Assemble the full report document. `sync_output` is the DECSET 2026
/// probe verdict ("supported"/"unsupported"/"no_reply"). `app` is the
/// already-validated `run --app` identifier; it wins over both detection
/// and TBENCH_TERMINAL_ID.
pub fn build_report(
    results: &[ResultRow],
    sync_output: &str,
    app: Option<&str>,
    extensions: Map<String, Value>,
) -> Value {
    let os = canonical_os(&cmd_stdout("uname", &["-s"]).unwrap_or_default());
    let arch = canonical_arch(&cmd_stdout("uname", &["-m"]).unwrap_or_default());
    let measured_shells: BTreeSet<&str> = results
        .iter()
        .filter_map(|row| row.shell.as_deref())
        .collect();
    let mut shell_versions = Vec::new();
    let mut shell_version_missing = false;
    for shell in &measured_shells {
        // First line only: bash prints its license text after the version.
        match cmd_stdout(shell, &["--version"])
            .and_then(|out| out.lines().next().map(|line| line.trim().to_string()))
            .filter(|line| !line.is_empty())
        {
            Some(version) => shell_versions.push(format!("{shell}={version}")),
            None => shell_version_missing = true,
        }
    }
    let (shell_meta, shell_fact) = if measured_shells.is_empty() {
        (
            "not-applicable".to_string(),
            fact_unknown("the report contains no shell-scoped measurements"),
        )
    } else if shell_version_missing {
        (
            format!(
                "version-unavailable:{}",
                measured_shells
                    .iter()
                    .copied()
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            fact_unknown("one or more measured shell versions were not available"),
        )
    } else {
        let identity = shell_versions.join("|");
        (identity.clone(), fact_known(json!(identity)))
    };
    let timestamp = utc_timestamp();

    let mut meta_notes: Vec<String> = Vec::new();
    let mut terminal = detect_terminal();
    let mut terminal_override = false;
    let mut terminal_override_invalid = false;
    if let Some(app) = app {
        terminal = app.to_string();
        terminal_override = true;
    } else if let Ok(id) = std::env::var("TBENCH_TERMINAL_ID") {
        match trimmed_opaque_id(&id) {
            Some(v) => {
                terminal = v.to_string();
                terminal_override = true;
            }
            None => {
                terminal_override_invalid = true;
                meta_notes.push(
                    "TBENCH_TERMINAL_ID ignored: expected a non-empty single-line identifier"
                        .into(),
                );
            }
        }
    }

    // TERM_PROGRAM is unset in many Linux emulators. The process holding
    // our pty master is direct evidence of which emulator this is.
    let mut terminal_discovered = false;
    if !terminal_override
        && !terminal_override_invalid
        && !std::env::var("TERM_PROGRAM").is_ok_and(|v| !v.is_empty())
        && !host_discovery_disabled()
    {
        if let Some(name) = crate::resource::terminal_from_pty() {
            terminal = name;
            terminal_discovered = true;
        }
    }

    if let Some(mux) = crate::resource::pty_intermediary() {
        if results.iter().any(|row| is_tty_bound(&row.metric)) {
            meta_notes.push(format!(
                "run inside {mux}: render.throughput, scroll.proxy and fire.fps wrote to the {mux} pty and describe {mux}, not the terminal emulator"
            ));
        }
    }

    let mut extensions = extensions;
    let mut mac = match os.as_str() {
        "darwin" => collect_macos_meta(),
        "linux" => {
            let (meta, host_extension) = collect_linux_meta();
            if let Some(ext) = host_extension {
                extensions.insert("org.terminalbench.host".to_string(), ext);
            }
            meta
        }
        _ => HostMeta::default(),
    };
    if let Some(ms) = mac.system_profiler_over_budget_ms {
        meta_notes.push(format!(
            "gpu/display_refresh_hz omitted: system_profiler took {ms}ms (>1000ms budget)"
        ));
    }

    // Explicit env overrides (CI / non-macOS hosts) — invalid values are
    // ignored with a meta note, never fatal.
    if let Ok(v) = std::env::var("TBENCH_MACHINE_MODEL") {
        match trimmed_opaque_id(&v) {
            Some(m) => mac.machine_model = Some(m.to_string()),
            None => {
                mac.machine_model = None;
                meta_notes.push("TBENCH_MACHINE_MODEL ignored: expected a trimmed non-empty single-line identifier".into());
            }
        }
    } else if let Some(m) = &mac.machine_model {
        if trimmed_opaque_id(m).is_none() {
            mac.machine_model = None;
        }
    }
    if let Ok(v) = std::env::var("TBENCH_POWER_SOURCE") {
        match canonical_power_source(&v) {
            Some(p) => mac.power_source = Some(p),
            None => {
                mac.power_source = None;
                meta_notes.push("TBENCH_POWER_SOURCE ignored: expected ac, battery, or ups".into());
            }
        }
    }
    if let Ok(v) = std::env::var("TBENCH_LOW_POWER_MODE") {
        match v.as_str() {
            "true" => mac.low_power_mode = Some(true),
            "false" => mac.low_power_mode = Some(false),
            _ => {
                mac.low_power_mode = None;
                meta_notes.push("TBENCH_LOW_POWER_MODE ignored: expected true or false".into());
            }
        }
    }
    if let Ok(v) = std::env::var("TBENCH_DISPLAY_REFRESH_HZ") {
        match valid_refresh_hz(&v) {
            Some(hz) => mac.display_refresh_hz = Some(hz),
            None => {
                mac.display_refresh_hz = None;
                meta_notes.push("TBENCH_DISPLAY_REFRESH_HZ ignored: expected a number greater than 0 and at most 1000".into());
            }
        }
    }

    // meta block (insertion-ordered)
    let mut meta = Map::new();
    meta.insert("timestamp".into(), json!(timestamp));
    meta.insert("os".into(), json!(os));
    meta.insert("runner".into(), json!("tbench"));
    meta.insert("terminal".into(), json!(terminal));
    meta.insert("shell".into(), json!(shell_meta));
    meta.insert("synchronized_output_supported".into(), json!(sync_output));
    if sync_output != "supported" {
        meta.insert("frame_skip_risk_note".into(), json!(format!(
            "This terminal did not confirm DECSET 2026 (synchronized output) support ({sync_output}); render.throughput/scroll.proxy measure PTY write completion, not confirmed final-frame rendering, so a batching/frame-skipping terminal could read faster than it actually draws."
        )));
    }
    if let Some(v) = &mac.power_source {
        meta.insert("power_source".into(), json!(v));
    }
    if let Some(v) = mac.low_power_mode {
        meta.insert("low_power_mode".into(), json!(v));
    }
    if let Some(v) = &mac.machine_model {
        meta.insert("machine_model".into(), json!(v));
    }
    if let Some(v) = &mac.cpu {
        meta.insert("cpu".into(), json!(v));
    }
    if let Some(v) = &mac.gpu {
        meta.insert("gpu".into(), json!(v));
    }
    if let Some(v) = mac.display_refresh_hz {
        meta.insert("display_refresh_hz".into(), json!(v));
    }
    if !meta_notes.is_empty() {
        meta.insert("meta_notes".into(), json!(meta_notes.join("; ")));
    }

    // run_context.environment fact objects
    let terminal_fact = if terminal_override
        || terminal_discovered
        || (!terminal_override_invalid
            && std::env::var("TERM_PROGRAM").is_ok_and(|v| !v.is_empty()))
    {
        fact_known(json!(terminal))
    } else {
        fact_unknown("terminal identity was not available without guessing")
    };
    let machine_fact = match &mac.machine_model {
        Some(m) => fact_known(json!(m)),
        None => fact_unknown("machine model was not available"),
    };
    let power_fact = match &mac.power_source {
        Some(p) => fact_known(json!(p)),
        None => fact_unknown("power source was not available"),
    };
    let lpm_fact = match mac.low_power_mode {
        Some(b) => fact_known(json!(b)),
        None => fact_unknown("low-power mode was not available"),
    };
    let refresh_fact = match mac.display_refresh_hz {
        Some(hz) => fact_known(json!(hz)),
        None => fact_unknown("display refresh rate was not available"),
    };

    let mut doc = json!({
        "$schema": "urn:tbench:schema:report:v1",
        "wire_version": "1.0.0",
        "producer": {
            "name": "tbench",
            "version": {"status": "known", "value": env!("CARGO_PKG_VERSION")},
        },
        "protocol": {"id": "urn:tbench:protocol:reporting", "version": "1.0.0"},
        "run_context": {
            "environment": {
                "os": fact_known(json!(os)),
                "architecture": fact_known(json!(arch)),
                "terminal": terminal_fact,
                "shell": shell_fact,
                "machine_model": machine_fact,
                "power_source": power_fact,
                "low_power_mode": lpm_fact,
                "display_refresh_hz": refresh_fact,
                "synchronized_output_supported": fact_known(json!(sync_output)),
            }
        },
        "meta": Value::Object(meta),
        "results": results.iter().map(result_json).collect::<Vec<_>>(),
    });
    if !extensions.is_empty() {
        doc["extensions"] = Value::Object(extensions);
    }
    doc
}

/// UTC "YYYY-MM-DDTHH:MM:SSZ" without a chrono dependency: civil-from-days
/// (Howard Hinnant's algorithm) over the Unix timestamp.
pub fn utc_timestamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let (h, mi, s) = (tod / 3600, (tod % 3600) / 60, tod % 60);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

/// Local "YYYYMMDD-HHMMSS" used in default output filenames. Uses UTC when
/// local offset is unavailable; filename uniqueness is what matters here.
pub fn filename_timestamp() -> String {
    // date +%Y%m%d-%H%M%S is local time in the zsh implementation; shelling
    // out to `date` keeps that behavior without a timezone database.
    cmd_stdout("date", &["+%Y%m%d-%H%M%S"]).unwrap_or_else(|| {
        let t = utc_timestamp();
        t.replace(['-', ':'], "")
            .replace('T', "-")
            .trim_end_matches('Z')
            .to_string()
    })
}

/// Hidden `tbench __smoke-report`: one synthetic result per registered
/// profile, driven through the real report assembly (env overrides and
/// probes included). Exists for tests/report-v1.sh, so the suite tests the
/// actual emitter rather than a copied fixture.
pub fn smoke_rows() -> Vec<ResultRow> {
    let row = |metric: &str,
               unit: &str,
               shell: Option<&str>,
               backend: &str,
               median: f64,
               stddev: f64,
               p95: f64,
               variant: Option<&str>,
               notes: Option<&str>,
               samples: &[f64]| ResultRow {
        metric: metric.into(),
        shell: shell.map(str::to_string),
        unit: unit.into(),
        median,
        p95,
        stddev,
        runs: samples.len(),
        outliers_removed: 0,
        backend: backend.into(),
        variant: variant.map(str::to_string),
        notes: notes.map(str::to_string),
        samples: Some(samples.to_vec()),
    };
    vec![
        row(
            "shell.startup",
            "ms",
            Some("zsh"),
            "rust-timer",
            10.0,
            1.0,
            12.0,
            None,
            None,
            &[9.0, 10.0, 12.0],
        ),
        row(
            "cmd.overhead",
            "us",
            Some("zsh"),
            "rust-timer",
            100.0,
            5.0,
            110.0,
            None,
            None,
            &[95.0, 100.0, 110.0],
        ),
        row(
            "render.throughput",
            "MBps",
            None,
            "rust-timer",
            100.0,
            4.0,
            110.0,
            Some("plain"),
            Some("PTY proxy"),
            &[95.0, 100.0, 110.0],
        ),
        row(
            "render.throughput",
            "linesps",
            None,
            "rust-timer",
            1000.0,
            40.0,
            1100.0,
            Some("plain"),
            Some("PTY proxy"),
            &[950.0, 1000.0, 1100.0],
        ),
        row(
            "scroll.proxy",
            "ms",
            None,
            "rust-timer",
            10.0,
            1.0,
            12.0,
            None,
            Some("scroll proxy"),
            &[9.0, 10.0, 12.0],
        ),
        row(
            "input.latency",
            "ms",
            None,
            "rust-pty",
            2.0,
            0.2,
            3.0,
            Some("idle"),
            Some("PTY echo proxy"),
            &[1.8, 2.0, 3.0],
        ),
        row(
            "resource.mem",
            "MB",
            None,
            "ps",
            200.0,
            5.0,
            210.0,
            Some("idle"),
            Some("process sample"),
            &[195.0, 200.0, 210.0],
        ),
        row(
            "resource.cpu",
            "pct",
            None,
            "ps",
            5.0,
            1.0,
            7.0,
            Some("idle"),
            Some("process sample"),
            &[4.0, 5.0, 7.0],
        ),
        row(
            "resource.cpu",
            "pct",
            None,
            "procfs",
            40.0,
            2.0,
            43.0,
            Some("load"),
            Some("process sample"),
            &[38.0, 40.0, 43.0],
        ),
        row(
            "fire.fps",
            "fps",
            None,
            "fire-v2-overlapped",
            30.0,
            1.0,
            32.0,
            None,
            Some("fire proxy"),
            &[29.0, 30.0, 32.0],
        ),
        row(
            "fire.fps",
            "fps",
            None,
            "fire-v2-overlapped",
            30.0,
            1.0,
            32.0,
            Some("80x24"),
            Some("geometry-aware fire proxy"),
            &[29.0, 30.0, 32.0],
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_shape() {
        let t = utc_timestamp();
        assert_eq!(t.len(), 20, "{t}");
        assert!(t.ends_with('Z'));
    }

    #[test]
    fn refresh_parse() {
        assert_eq!(parse_refresh_hz("2560 x 1440 @ 120.00Hz"), Some(120.0));
        assert_eq!(parse_refresh_hz("2560 x 1440"), None);
    }

    #[test]
    fn tokens() {
        assert_eq!(canonical_os("Darwin"), "darwin");
        assert_eq!(canonical_arch("arm64"), "aarch64");
        assert_eq!(canonical_os("SunOS"), "other:sunos");
    }
}
