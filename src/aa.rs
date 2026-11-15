//! `tbench aa` — A/A self-check. Runs the same configuration several times
//! (or reads existing reports of it), shows how far identical runs drift
//! apart, and counts how often the regression gate would fail one half of
//! those runs against the other. Anything it flags is noise by construction,
//! so the output tells a user which threshold their machine can support.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use serde_json::Value;

use crate::compare::{
    evaluate_gate, fmtv, parse_threshold, row_label, rows_by_key, GateConfig, Key,
    MIN_RUNS_FOR_SIGNIFICANCE,
};
use crate::stats;

const DEFAULT_REPEATS: usize = 8;
const DEFAULT_THRESHOLDS: &str = "0,2,5,10";
/// Above this many balanced splits, a fixed-seed random sample is used.
const MAX_SPLITS: usize = 1000;

fn die(msg: &str, code: u8) -> ExitCode {
    eprintln!("{msg}");
    ExitCode::from(code)
}

fn binomial(n: usize, k: usize) -> usize {
    let k = k.min(n - k);
    let mut acc: usize = 1;
    for i in 0..k {
        acc = acc.saturating_mul(n - i) / (i + 1);
    }
    acc
}

/// Every way to pick `k` of `n` indices, in lexicographic order.
fn combinations(n: usize, k: usize) -> Vec<Vec<usize>> {
    let mut out = Vec::new();
    let mut cur: Vec<usize> = (0..k).collect();
    if k == 0 || k > n {
        return out;
    }
    loop {
        out.push(cur.clone());
        let mut i = k;
        while i > 0 && cur[i - 1] == n - k + i - 1 {
            i -= 1;
        }
        if i == 0 {
            break;
        }
        cur[i - 1] += 1;
        for j in i..k {
            cur[j] = cur[j - 1] + 1;
        }
    }
    out
}

/// Baseline index sets for the splits to evaluate, and whether they are the
/// full enumeration.
fn splits(n: usize, k: usize) -> (Vec<Vec<usize>>, bool) {
    if binomial(n, k) <= MAX_SPLITS {
        return (combinations(n, k), true);
    }
    let mut rng = stats::Rng::new(1);
    let mut out = Vec::with_capacity(MAX_SPLITS);
    for _ in 0..MAX_SPLITS {
        let mut idx: Vec<usize> = (0..n).collect();
        for i in 0..k {
            let j = i + rng.below(n - i);
            idx.swap(i, j);
        }
        let mut pick = idx[..k].to_vec();
        pick.sort_unstable();
        out.push(pick);
    }
    (out, false)
}

struct RowSpread {
    key: Key,
    runs: usize,
    center: f64,
    min: f64,
    max: f64,
}

impl RowSpread {
    /// Range of the per-run medians: relative to their median, or in the
    /// row's own unit when that median is 0.
    fn spread(&self) -> String {
        if self.center == 0.0 {
            format!("{} (absolute)", fmtv(self.max - self.min))
        } else {
            format!("{:.1}%", (self.max - self.min) / self.center * 100.0)
        }
    }
}

fn row_spreads(reports: &[Value]) -> Vec<RowSpread> {
    let (order, rows) = rows_by_key(reports);
    order
        .into_iter()
        .map(|key| {
            let medians: Vec<f64> = rows[&key]
                .iter()
                .map(|r| r["median"].as_f64().unwrap_or(0.0))
                .collect();
            RowSpread {
                runs: medians.len(),
                center: stats::median(&medians),
                min: medians.iter().cloned().fold(f64::INFINITY, f64::min),
                max: medians.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
                key,
            }
        })
        .collect()
}

/// The A/A report. `thresholds` are fractions.
fn analyze(reports: &[Value], thresholds: &[f64], min_points: f64) -> String {
    let n = reports.len();
    let k = n / 2;
    let (baseline_sets, exhaustive) = splits(n, k);
    let spreads = row_spreads(reports);

    // per threshold: (splits that failed, per-row failing counts)
    let mut failed_splits = vec![0usize; thresholds.len()];
    let mut row_fails: Vec<Vec<usize>> = vec![vec![0; thresholds.len()]; spreads.len()];
    for set in &baseline_sets {
        let baseline: Vec<Value> = set.iter().map(|&i| reports[i].clone()).collect();
        let latest: Vec<Value> = (0..n)
            .filter(|i| !set.contains(i))
            .map(|i| reports[i].clone())
            .collect();
        for (ti, &threshold) in thresholds.iter().enumerate() {
            let cfg = GateConfig {
                min_points,
                ..GateConfig::new(threshold)
            };
            let outcome = evaluate_gate(&baseline, &latest, cfg);
            if outcome.failed() {
                failed_splits[ti] += 1;
            }
            for row in outcome.flagged.iter().filter(|row| row.failing) {
                if let Some(ri) = spreads.iter().position(|s| s.key == row.key) {
                    row_fails[ri][ti] += 1;
                }
            }
        }
    }

    let total = baseline_sets.len();
    let tested = k >= MIN_RUNS_FOR_SIGNIFICANCE && n - k >= MIN_RUNS_FOR_SIGNIFICANCE;
    let pct_label = |t: f64| {
        let pct = t * 100.0;
        if (pct - pct.round()).abs() < 1e-9 {
            format!("{pct:.0}%")
        } else {
            format!("{pct:.1}%")
        }
    };
    let mut lines = vec![
        "# tbench A/A self-check".to_string(),
        String::new(),
        format!(
            "{n} runs of one configuration. Every difference below is noise, so every gate failure is a false failure."
        ),
        String::new(),
        format!(
            "Splits: {total} ({k} baseline vs {} latest; {}).",
            n - k,
            if exhaustive {
                "every combination"
            } else {
                "fixed-seed random sample"
            }
        ),
        if tested {
            "Significance: tested across run medians, as `compare --threshold ... --latest ...` does with these run counts.".to_string()
        } else {
            format!(
                "Significance: not tested. It needs at least {MIN_RUNS_FOR_SIGNIFICANCE} runs per side, so at least {} runs here; the figures below are for the threshold alone.",
                2 * MIN_RUNS_FOR_SIGNIFICANCE
            )
        },
        String::new(),
    ];

    let mut header = "| Metric | Runs | Median of run medians | Min | Max | Spread |".to_string();
    let mut sep = "|---|---|---|---|---|---|".to_string();
    for &t in thresholds {
        header.push_str(&format!(" False fails at {} |", pct_label(t)));
        sep.push_str("---|");
    }
    lines.push(header);
    lines.push(sep);
    for (ri, s) in spreads.iter().enumerate() {
        let mut line = format!(
            "| {} | {} | {} | {} | {} | {} |",
            row_label(&s.key),
            s.runs,
            fmtv(s.center),
            fmtv(s.min),
            fmtv(s.max),
            s.spread()
        );
        for count in &row_fails[ri] {
            line.push_str(&format!(" {count} of {total} |"));
        }
        lines.push(line);
    }
    lines.push(String::new());
    lines.push("Whole-report false failures (any row):".to_string());
    lines.push(String::new());
    for (ti, &t) in thresholds.iter().enumerate() {
        lines.push(format!(
            "- threshold {}: {} of {total} splits ({:.1}%)",
            pct_label(t),
            failed_splits[ti],
            failed_splits[ti] as f64 / total.max(1) as f64 * 100.0
        ));
    }
    lines.push(String::new());
    let clean = thresholds
        .iter()
        .zip(&failed_splits)
        .filter(|(_, fails)| **fails == 0)
        .map(|(t, _)| *t)
        .fold(f64::INFINITY, f64::min);
    lines.push(if clean.is_finite() {
        format!(
            "Smallest tested threshold with no false failure: {}. These runs share one session; drift between sessions is not in this data, so leave headroom above it.",
            pct_label(clean)
        )
    } else {
        "Every tested threshold produced false failures. Use more runs per side or a larger threshold.".to_string()
    });
    lines.join("\n")
}

pub fn cmd_aa(args: &[String], root: &Path) -> ExitCode {
    let mut from: Vec<String> = Vec::new();
    let mut reading_files = false;
    let mut repeats = DEFAULT_REPEATS;
    let mut thresholds_raw = DEFAULT_THRESHOLDS.to_string();
    let mut min_points = 1.0f64;
    let mut output: Option<PathBuf> = None;
    let mut output_dir: Option<PathBuf> = None;
    let mut passthrough: Vec<String> = Vec::new();

    let mut it = args.iter();
    while let Some(arg) = it.next() {
        macro_rules! next_val {
            ($name:literal) => {
                match it.next() {
                    Some(v) => v.clone(),
                    None => return die(concat!("tbench: ", $name, " requires a value"), 2),
                }
            };
        }
        match arg.as_str() {
            "--from" => reading_files = true,
            "--repeats" => match next_val!("--repeats").parse::<usize>() {
                Ok(n) if n >= 2 => repeats = n,
                _ => return die("tbench: --repeats must be an integer of at least 2", 2),
            },
            "--thresholds" => thresholds_raw = next_val!("--thresholds"),
            "--min-points" => match next_val!("--min-points").parse::<f64>() {
                Ok(v) if v.is_finite() && v >= 0.0 => min_points = v,
                _ => return die("tbench: --min-points must be a non-negative number", 2),
            },
            "--output" => output = Some(PathBuf::from(next_val!("--output"))),
            "--output-dir" => output_dir = Some(PathBuf::from(next_val!("--output-dir"))),
            "--scenario" | "--shells" | "--runs" | "--settle" | "--warmup" => {
                let value = next_val!("a run option");
                passthrough.push(arg.clone());
                passthrough.push(value);
            }
            "-h" | "--help" => {
                eprintln!("see: tbench --help");
                return ExitCode::SUCCESS;
            }
            other if other.starts_with("--") => {
                return die(&format!("tbench: unknown option '{other}'"), 2)
            }
            _ if reading_files => from.push(arg.clone()),
            other => {
                return die(
                    &format!(
                        "tbench: unexpected argument '{other}' (report files go after --from)"
                    ),
                    2,
                )
            }
        }
    }

    let mut thresholds = Vec::new();
    for part in thresholds_raw.split(',') {
        match parse_threshold(part) {
            Some(t) => thresholds.push(t),
            None => {
                return die(
                    &format!("tbench: --thresholds must be comma-separated non-negative percentages, got '{part}'"),
                    2,
                )
            }
        }
    }

    let files: Vec<PathBuf> = if reading_files {
        if !passthrough.is_empty() {
            return die(
                "tbench: --from analyses existing reports; run options do not apply",
                2,
            );
        }
        from.iter().map(PathBuf::from).collect()
    } else {
        let dir = output_dir.unwrap_or_else(|| {
            root.join(format!(
                "results/aa-{}",
                crate::report::filename_timestamp()
            ))
        });
        if std::fs::create_dir_all(&dir).is_err() {
            return die(&format!("tbench: failed to create {}", dir.display()), 1);
        }
        let Ok(exe) = std::env::current_exe() else {
            return die("tbench: cannot locate the tbench binary", 1);
        };
        let mut files = Vec::new();
        for i in 1..=repeats {
            let path = dir.join(format!("aa-{i:02}.json"));
            eprintln!("tbench aa: run {i} of {repeats}");
            let status = Command::new(&exe)
                .arg("run")
                .args(&passthrough)
                .arg("--output")
                .arg(&path)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            if !status.map(|s| s.success()).unwrap_or(false) || !path.is_file() {
                return die(
                    &format!("tbench aa: run {i} failed; try the same options with `tbench run` to see why"),
                    1,
                );
            }
            files.push(path);
        }
        eprintln!("tbench aa: reports in {}", dir.display());
        files
    };

    if files.len() < 2 {
        return die("tbench: aa needs at least 2 reports", 2);
    }
    let mut reports = Vec::new();
    for f in &files {
        match std::fs::read_to_string(f)
            .map_err(|e| e.to_string())
            .and_then(|t| serde_json::from_str::<Value>(&t).map_err(|e| e.to_string()))
        {
            Ok(v) => reports.push(v),
            Err(e) => return die(&format!("tbench: failed to read {}: {e}", f.display()), 2),
        }
    }
    if reports.len() < 2 * MIN_RUNS_FOR_SIGNIFICANCE {
        eprintln!(
            "tbench: {} runs split into halves of fewer than {MIN_RUNS_FOR_SIGNIFICANCE}; the gate's significance test needs {} or more runs",
            reports.len(),
            2 * MIN_RUNS_FOR_SIGNIFICANCE
        );
    }

    let text = analyze(&reports, &thresholds, min_points);
    if let Some(path) = &output {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = std::fs::write(path, format!("{text}\n")) {
            return die(
                &format!("tbench: failed to write {}: {e}", path.display()),
                1,
            );
        }
        eprintln!("tbench: wrote {}", path.display());
    }
    println!("{text}");
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(metric: &str, unit: &str, median: f64) -> Value {
        serde_json::json!({"results": [{
            "metric": metric, "shell": null, "unit": unit, "median": median,
            "measurement": {"direction": "lower-is-better"}
        }]})
    }

    #[test]
    fn combinations_are_complete_and_ordered() {
        assert_eq!(binomial(8, 4), 70);
        assert_eq!(binomial(12, 6), 924);
        let c = combinations(4, 2);
        assert_eq!(c.len(), 6);
        assert_eq!(c[0], vec![0, 1]);
        assert_eq!(c[5], vec![2, 3]);
        assert_eq!(combinations(8, 4).len(), 70);
    }

    #[test]
    fn large_run_counts_are_sampled_deterministically() {
        let (a, exhaustive) = splits(16, 8);
        assert!(!exhaustive);
        assert_eq!(a.len(), MAX_SPLITS);
        assert!(a
            .iter()
            .all(|s| s.len() == 8 && s.windows(2).all(|w| w[0] < w[1])));
        assert_eq!(a, splits(16, 8).0);
    }

    #[test]
    fn steady_runs_report_no_false_failures() {
        let reports: Vec<Value> = [10.0, 10.1, 9.9, 10.0, 10.05, 9.95, 10.0, 10.1]
            .iter()
            .map(|m| run("shell.startup", "ms", *m))
            .collect();
        let text = analyze(&reports, &[0.02, 0.05], 1.0);
        assert!(
            text.contains("Splits: 70 (4 baseline vs 4 latest; every combination)"),
            "{text}"
        );
        assert!(
            text.contains("- threshold 2%: 0 of 70 splits (0.0%)"),
            "{text}"
        );
        assert!(
            text.contains("Smallest tested threshold with no false failure: 2%"),
            "{text}"
        );
    }

    #[test]
    fn a_drifting_series_shows_up_as_false_failures() {
        // First half slow, second half fast: one split separates them fully.
        let reports: Vec<Value> = [12.0, 12.1, 11.9, 12.0, 10.0, 10.1, 9.9, 10.0]
            .iter()
            .map(|m| run("shell.startup", "ms", *m))
            .collect();
        let text = analyze(&reports, &[0.05], 1.0);
        assert!(text.contains("| shell.startup (ms) | 8 |"), "{text}");
        assert!(text.contains("- threshold 5%: 1 of 70 splits"), "{text}");
        assert!(
            text.contains("Every tested threshold produced false failures"),
            "{text}"
        );
    }

    #[test]
    fn point_floor_is_what_stops_one_tick_flips() {
        // Idle CPU hopping between 1 and 2 points, two runs per side.
        let reports: Vec<Value> = [1.0, 2.0, 1.0, 2.0]
            .iter()
            .map(|m| run("resource.cpu", "pct", *m))
            .collect();
        let with_floor = analyze(&reports, &[0.05], 1.0);
        assert!(
            with_floor.contains("- threshold 5%: 0 of 6 splits"),
            "{with_floor}"
        );
        let without = analyze(&reports, &[0.05], 0.0);
        assert!(
            without.contains("- threshold 5%: 1 of 6 splits"),
            "{without}"
        );
    }
}
