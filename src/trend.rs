//! `tbench trend` — longitudinal per-metric history across accumulated
//! reports: markdown tables + HTML line charts.
//!
//! Reports are sorted by meta.timestamp (ISO8601 lexicographic order);
//! each report's meta.terminal is its series identity — multiple terminals
//! become multiple series/columns and are never averaged together.

use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::report::filename_timestamp;
use crate::show::{section_for, unit_display};

// (light, dark) hex pairs — dataviz reference palette, CVD-safe order.
// Color is assigned by terminal (first-appearance order across the
// chronologically-sorted reports), fixed across every chart.
const PALETTE: &[(&str, &str)] = &[
    ("#2a78d6", "#3987e5"), // 1 blue
    ("#008300", "#008300"), // 2 green
    ("#e87ba4", "#d55181"), // 3 magenta
    ("#eda100", "#c98500"), // 4 yellow
    ("#1baf7a", "#199e70"), // 5 aqua
    ("#eb6834", "#d95926"), // 6 orange
    ("#4a3aa7", "#9085e9"), // 7 violet
    ("#e34948", "#e66767"), // 8 red
];

const SECTION_TITLES: &[(&str, &str)] = &[
    ("render.throughput", "Render Throughput"),
    ("latency", "Latency"),
    ("resource", "Resource"),
    ("shell", "Shell"),
    ("other", "Other"),
];
const SECTION_ORDER: &[&str] = &["render.throughput", "latency", "resource", "shell", "other"];

// measurement.direction is authoritative; this fallback only matters for
// legacy reports emitted before that field existed.
const HIGHER_IS_BETTER_FALLBACK: &[&str] = &["render.throughput", "fire.fps"];

fn section_title(name: &str) -> &'static str {
    SECTION_TITLES
        .iter()
        .find(|(k, _)| *k == name)
        .map(|(_, v)| *v)
        .unwrap_or("Other")
}

// --- data model -----------------------------------------------------------

struct ResultRow {
    metric: String,
    variant: String,
    shell: String,
    unit: String,
    median: Option<f64>,
    p95: Option<f64>,
    runs: String,
    direction: Option<String>,
}

struct Report {
    path: String,
    timestamp: Option<String>,
    terminal: String,
    results: Vec<ResultRow>,
}

type Key = (String, String, String, String);

/// A row's data point: `idx` is the report's position in the
/// chronologically-sorted list (the shared x axis).
struct Point<'a> {
    idx: usize,
    res: &'a ResultRow,
}

fn value_to_runs(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        _ => String::new(),
    }
}

fn parse_report(path: &str, v: &Value) -> Report {
    let meta = v.get("meta");
    let timestamp = meta
        .and_then(|m| m.get("timestamp"))
        .and_then(|t| t.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let terminal = meta
        .and_then(|m| m.get("terminal"))
        .and_then(|t| t.as_str())
        .unwrap_or("unknown")
        .to_string();

    let mut results = Vec::new();
    if let Some(arr) = v.get("results").and_then(|r| r.as_array()) {
        for res in arr {
            let metric = res
                .get("metric")
                .and_then(|m| m.as_str())
                .unwrap_or("")
                .to_string();
            let unit = res
                .get("unit")
                .and_then(|u| u.as_str())
                .unwrap_or("")
                .to_string();
            let variant = res
                .get("variant")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            let shell = res
                .get("shell")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            let median = res.get("median").and_then(|m| m.as_f64());
            let p95 = res.get("p95").and_then(|m| m.as_f64());
            let runs = value_to_runs(res.get("runs"));
            let direction = res
                .get("measurement")
                .and_then(|m| m.get("direction"))
                .and_then(|d| d.as_str())
                .map(|s| s.to_string());
            results.push(ResultRow {
                metric,
                variant,
                shell,
                unit,
                median,
                p95,
                runs,
                direction,
            });
        }
    }
    Report {
        path: path.to_string(),
        timestamp,
        terminal,
        results,
    }
}

fn row_key(r: &ResultRow) -> Key {
    (
        r.metric.clone(),
        r.variant.clone(),
        r.shell.clone(),
        r.unit.clone(),
    )
}

fn row_label(key: &Key) -> String {
    let (metric, variant, shell, unit) = key;
    let mut parts = vec![metric.clone()];
    if !variant.is_empty() {
        parts.push(format!("[{variant}]"));
    }
    if !shell.is_empty() {
        parts.push(format!("({shell})"));
    }
    parts.push(format!("({})", unit_display(unit)));
    parts.join(" ")
}

/// Python's f"{v:,.2f}" — thousands separators + 2 decimals; None -> "-".
fn fmt(v: Option<f64>) -> String {
    match v {
        None => "-".to_string(),
        Some(v) => {
            let s = format!("{v:.2}");
            let (int_part, frac) = s.split_once('.').unwrap();
            let (sign, digits) = if let Some(d) = int_part.strip_prefix('-') {
                ("-", d)
            } else {
                ("", int_part)
            };
            let mut grouped = String::new();
            for (i, c) in digits.chars().enumerate() {
                if i > 0 && (digits.len() - i) % 3 == 0 {
                    grouped.push(',');
                }
                grouped.push(c);
            }
            format!("{sign}{grouped}.{frac}")
        }
    }
}

fn is_higher_better(metric: &str, direction: &Option<String>) -> bool {
    match direction.as_deref() {
        Some("higher-is-better") => true,
        Some("lower-is-better") => false,
        _ => HIGHER_IS_BETTER_FALLBACK.contains(&metric),
    }
}

/// Direction-aware annotation for a percentage delta. `None` for pct == 0.
fn delta_annot(pct: f64, higher_better: bool) -> Option<&'static str> {
    if pct > 0.0 {
        Some(if higher_better { "better" } else { "worse" })
    } else if pct < 0.0 {
        Some(if higher_better { "worse" } else { "better" })
    } else {
        None
    }
}

/// Default band inside which a change between two reports is not called
/// better or worse. Identical runs on one machine differed by 1 to 6% in
/// docs/aa-baseline.md; `tbench aa` measures it for a given setup.
pub const DEFAULT_NOISE: f64 = 0.05;
/// One sampling tick: percentage-unit rows cannot resolve less.
const NOISE_POINTS: f64 = 1.0;

/// Change from the previous point of the same terminal. A verdict
/// (better/worse) is given only outside the noise band; inside it the
/// number is shown and marked as noise. Percentage-unit rows are compared
/// in points, since a relative change from 1% to 2% means nothing.
fn format_delta(
    prev: Option<f64>,
    median: Option<f64>,
    higher_better: bool,
    unit: &str,
    noise: f64,
) -> String {
    let (Some(prev), Some(median)) = (prev, median) else {
        return "-".to_string();
    };
    if unit == "pct" {
        let points = median - prev;
        return if points.abs() <= NOISE_POINTS {
            format!("{points:+.1} pt (within noise)")
        } else {
            match delta_annot(points, higher_better) {
                Some(a) => format!("{points:+.1} pt ({a})"),
                None => format!("{points:+.1} pt"),
            }
        };
    }
    if prev == 0.0 {
        return "-".to_string();
    }
    let pct = (median - prev) / prev * 100.0;
    let base = format!("{pct:+.1}%");
    if pct == 0.0 {
        base
    } else if pct.abs() <= noise * 100.0 {
        format!("{base} (within noise)")
    } else {
        match delta_annot(pct, higher_better) {
            Some(a) => format!("{base} ({a})"),
            None => base,
        }
    }
}

/// First-appearance terminal order across the sorted reports.
fn terminal_order(reports: &[Report]) -> Vec<String> {
    let mut order = Vec::new();
    for r in reports {
        if !order.contains(&r.terminal) {
            order.push(r.terminal.clone());
        }
    }
    order
}

/// Row keys in first-seen order, and each key's points (idx + result ref).
fn collect_rows(reports: &[Report]) -> (Vec<Key>, HashMap<Key, Vec<Point<'_>>>) {
    let mut order: Vec<Key> = Vec::new();
    let mut rows: HashMap<Key, Vec<Point>> = HashMap::new();
    for (idx, r) in reports.iter().enumerate() {
        for res in &r.results {
            let key = row_key(res);
            if !rows.contains_key(&key) {
                order.push(key.clone());
            }
            rows.entry(key).or_default().push(Point { idx, res });
        }
    }
    (order, rows)
}

/// Sort by (missing-timestamp, timestamp) — present ones ascending, missing
/// ones last, stable within each group. ISO8601 sorts correctly lexically.
fn sort_reports(reports: &mut [Report]) {
    reports.sort_by(|a, b| {
        let ka = (
            a.timestamp.is_none(),
            a.timestamp.clone().unwrap_or_default(),
        );
        let kb = (
            b.timestamp.is_none(),
            b.timestamp.clone().unwrap_or_default(),
        );
        ka.cmp(&kb)
    });
}

fn esc_md(s: &str) -> String {
    s.replace('|', "\\|")
}

// --- markdown report ------------------------------------------------------

fn build_markdown_report(
    reports: &[Report],
    order: &[Key],
    rows: &HashMap<Key, Vec<Point>>,
    noise: f64,
) -> String {
    let mut sections: HashMap<&str, Vec<&Key>> = HashMap::new();
    for name in SECTION_ORDER {
        sections.insert(name, Vec::new());
    }
    for key in order {
        sections.get_mut(section_for(&key.0)).unwrap().push(key);
    }

    let mut lines: Vec<String> = vec![
        "# tbench trend".to_string(),
        String::new(),
        format!("Reports: {} (chronological)", reports.len()),
        String::new(),
        format!(
            "Δ is the change from that terminal's previous report. It is called better or worse only beyond ±{:.0}% (±1 point for percentage rows); smaller changes are within what identical runs show (`tbench aa`, `--noise PCT`).",
            noise * 100.0
        ),
        String::new(),
    ];

    for name in SECTION_ORDER {
        let keys = &sections[name];
        if keys.is_empty() {
            continue;
        }
        lines.push(format!("## {}", section_title(name)));
        lines.push(String::new());
        for key in keys {
            let metric = &key.0;
            let mut points: Vec<&Point> = rows[*key].iter().collect();
            points.sort_by_key(|p| p.idx);
            lines.push(format!("### {}", row_label(key)));
            lines.push(String::new());
            lines.push("| Timestamp | Terminal | Median | Δ |".to_string());
            lines.push("|---|---|---|---|".to_string());
            let mut last_by_terminal: HashMap<String, f64> = HashMap::new();
            for p in points {
                let res = p.res;
                let ts = reports[p.idx].timestamp.as_deref().unwrap_or("-");
                let terminal = &reports[p.idx].terminal;
                let median = res.median;

                let prev = last_by_terminal.get(terminal).copied();
                let delta_str = format_delta(
                    prev,
                    median,
                    is_higher_better(metric, &res.direction),
                    &key.3,
                    noise,
                );
                if let Some(m) = median {
                    last_by_terminal.insert(terminal.clone(), m);
                }

                lines.push(format!(
                    "| {} | {} | {} | {} |",
                    esc_md(ts),
                    esc_md(terminal),
                    fmt(median),
                    delta_str
                ));
            }
            lines.push(String::new());
        }
    }
    lines.join("\n")
}

// --- HTML report ----------------------------------------------------------

const CHART_W: f64 = 820.0;
const CHART_H: f64 = 240.0;
const MARGIN_LEFT: f64 = 56.0;
const MARGIN_RIGHT: f64 = 20.0;
const MARGIN_TOP: f64 = 16.0;
const MARGIN_BOTTOM: f64 = 40.0;

fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            _ => out.push(c),
        }
    }
    out
}

fn short_ts(ts: &str) -> String {
    if ts.is_empty() {
        return "?".to_string();
    }
    match ts.split_once('T') {
        Some((date_part, time_part)) => {
            let segs: Vec<&str> = date_part.split('-').collect();
            if segs.len() >= 3 {
                let mmdd = format!("{}-{}", segs[1], segs[2]);
                let hhmm: String = time_part.chars().take(5).collect();
                format!("{mmdd} {hhmm}")
            } else {
                ts.chars().take(10).collect()
            }
        }
        None => ts.chars().take(10).collect(),
    }
}

fn axis_ticks(y_min: f64, y_max: f64, count: usize) -> Vec<f64> {
    if count <= 1 {
        return vec![y_min];
    }
    let step = (y_max - y_min) / (count as f64 - 1.0);
    (0..count).map(|i| y_min + step * i as f64).collect()
}

/// Decide the y-axis baseline. Returns Some(baseline) for the narrow-range
/// non-zero-baselined case, None for the default zero baseline.
fn narrow_range_baseline(min_v: f64, max_v: f64) -> Option<f64> {
    if min_v > 0.0 && max_v > 0.0 && (max_v - min_v) / max_v < 0.2 {
        Some(min_v * 0.95)
    } else {
        None
    }
}

fn build_line_chart_svg(
    key: &Key,
    reports: &[Report],
    points: &[Point],
    terminals: &HashMap<String, usize>,
) -> String {
    let unit = &key.3;
    let n = reports.len();
    let unit_disp = unit_display(unit);

    let present_by_idx: HashMap<usize, &Point> = points.iter().map(|p| (p.idx, p)).collect();

    let mut terminals_here: Vec<String> = Vec::new();
    for p in points {
        let t = &reports[p.idx].terminal;
        if !terminals_here.contains(t) {
            terminals_here.push(t.clone());
        }
    }

    let mut values: Vec<f64> = Vec::new();
    for p in points {
        if let Some(m) = p.res.median {
            values.push(m);
        }
        if let Some(v) = p.res.p95 {
            values.push(v);
        }
    }
    if values.is_empty() {
        values.push(0.0);
    }
    let min_v = values.iter().cloned().fold(f64::INFINITY, f64::min);
    let max_v = values.iter().cloned().fold(f64::NEG_INFINITY, f64::max);

    let baseline_note = narrow_range_baseline(min_v, max_v);
    let y_min = baseline_note.unwrap_or(0.0);
    let mut y_max = if max_v > 0.0 { max_v * 1.05 } else { 1.0 };
    if y_max <= y_min {
        y_max = y_min + 1.0;
    }

    let x0 = MARGIN_LEFT;
    let x1 = CHART_W - MARGIN_RIGHT;
    let y0 = MARGIN_TOP;
    let y1 = CHART_H - MARGIN_BOTTOM;

    let xf = |idx: usize| -> f64 {
        if n > 1 {
            x0 + (x1 - x0) * (idx as f64 / (n as f64 - 1.0))
        } else {
            (x0 + x1) / 2.0
        }
    };
    let yf = |v: f64| -> f64 { y0 + (y_max - v) / (y_max - y_min) * (y1 - y0) };

    let mut parts: Vec<String> = Vec::new();
    parts.push(format!(
        "<svg viewBox=\"0 0 {} {}\" class=\"tb-chart\" role=\"img\" aria-label=\"{}\">",
        CHART_W as i64,
        CHART_H as i64,
        esc(&row_label(key))
    ));

    for tick in axis_ticks(y_min, y_max, 4) {
        let ty = yf(tick);
        parts.push(format!(
            "<line x1=\"{}\" y1=\"{:.2}\" x2=\"{}\" y2=\"{:.2}\" class=\"tb-gridline\"></line>",
            x0 as i64, ty, x1 as i64, ty
        ));
        parts.push(format!(
            "<text x=\"{}\" y=\"{:.2}\" class=\"tb-axis-label\" text-anchor=\"end\">{}</text>",
            (x0 - 8.0) as i64,
            ty + 4.0,
            esc(&fmt(Some(tick)))
        ));
    }

    match baseline_note {
        Some(note) => parts.push(format!(
            "<text x=\"{}\" y=\"{}\" class=\"tb-axis-note\">axis starts at {} {} (compressed range, not zero-baselined)</text>",
            x0 as i64,
            (CHART_H - 6.0) as i64,
            esc(&fmt(Some(note))),
            esc(unit_disp)
        )),
        None => parts.push(format!(
            "<text x=\"{}\" y=\"{}\" class=\"tb-axis-note\">y-axis from 0 {}</text>",
            x0 as i64,
            (CHART_H - 6.0) as i64,
            esc(unit_disp)
        )),
    }

    let tick_count = n.min(6);
    let tick_idxs: Vec<usize> = if tick_count > 1 {
        let mut v: Vec<usize> = (0..tick_count)
            .map(|i| (i as f64 * (n as f64 - 1.0) / (tick_count as f64 - 1.0)).round() as usize)
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    } else {
        vec![0]
    };
    for idx in tick_idxs {
        let tx = xf(idx);
        parts.push(format!(
            "<line x1=\"{:.2}\" y1=\"{}\" x2=\"{:.2}\" y2=\"{}\" class=\"tb-gridline-v\"></line>",
            tx, y0 as i64, tx, y1 as i64
        ));
        let ts = reports[idx].timestamp.as_deref().unwrap_or("");
        parts.push(format!(
            "<text x=\"{:.2}\" y=\"{:.2}\" class=\"tb-axis-label\" text-anchor=\"middle\">{}</text>",
            tx,
            y1 + 16.0,
            esc(&short_ts(ts))
        ));
    }

    parts.push(format!(
        "<line x1=\"{}\" y1=\"{:.2}\" x2=\"{}\" y2=\"{:.2}\" class=\"tb-baseline\"></line>",
        x0 as i64, y1, x1 as i64, y1
    ));

    for terminal in &terminals_here {
        let order_index = terminals[terminal];
        let color_slot = order_index % PALETTE.len();
        let t_report_idxs: Vec<usize> = (0..n)
            .filter(|&i| &reports[i].terminal == terminal)
            .collect();

        // Segment on gaps: a report belonging to this terminal but missing
        // this row's metric breaks the line (no interpolation across).
        let mut segments: Vec<Vec<usize>> = Vec::new();
        let mut current: Vec<usize> = Vec::new();
        for &idx in &t_report_idxs {
            if present_by_idx.contains_key(&idx) {
                current.push(idx);
            } else if !current.is_empty() {
                segments.push(std::mem::take(&mut current));
            }
        }
        if !current.is_empty() {
            segments.push(current);
        }

        for seg in &segments {
            if seg.len() > 1 {
                let pts: Vec<String> = seg
                    .iter()
                    .filter_map(|i| {
                        present_by_idx[i]
                            .res
                            .median
                            .map(|m| format!("{:.2},{:.2}", xf(*i), yf(m)))
                    })
                    .collect();
                if pts.len() > 1 {
                    parts.push(format!(
                        "<polyline points=\"{}\" class=\"tb-line\" style=\"stroke:var(--series-{})\"></polyline>",
                        pts.join(" "),
                        color_slot + 1
                    ));
                }
            }
        }

        for &idx in &t_report_idxs {
            let p = match present_by_idx.get(&idx) {
                Some(p) => p,
                None => continue,
            };
            let res = p.res;
            let median = match res.median {
                Some(m) => m,
                None => continue,
            };
            let px = xf(idx);
            let py = yf(median);

            if let Some(p95) = res.p95 {
                if p95 != median {
                    let p95y = yf(p95);
                    parts.push(format!(
                        "<line x1=\"{px:.2}\" y1=\"{py:.2}\" x2=\"{px:.2}\" y2=\"{p95y:.2}\" class=\"tb-whisker\" style=\"stroke:var(--series-{})\"></line>",
                        color_slot + 1
                    ));
                }
            }

            let ts = reports[idx].timestamp.as_deref().unwrap_or("-");
            parts.push(format!(
                "<circle cx=\"{px:.2}\" cy=\"{py:.2}\" r=\"5\" class=\"tb-point\" tabindex=\"0\" role=\"img\" style=\"fill:var(--series-{slot})\" data-timestamp=\"{ts_e}\" data-entity=\"{term_e}\" data-median=\"{med_e} {unit_e}\" data-p95=\"{p95_e} {unit_e}\" data-runs=\"{runs_e}\" aria-label=\"{term_e} {sts_e}: median {med_e} {unit_e}\"></circle>",
                slot = color_slot + 1,
                ts_e = esc(ts),
                term_e = esc(terminal),
                med_e = esc(&fmt(Some(median))),
                unit_e = esc(unit_disp),
                p95_e = esc(&fmt(res.p95)),
                runs_e = esc(&res.runs),
                sts_e = esc(&short_ts(ts)),
            ));
        }
    }

    parts.push("</svg>".to_string());
    parts.join("")
}

fn build_html_report(reports: &[Report], source_files: &[String]) -> String {
    let (order, rows) = collect_rows(reports);
    let t_order = terminal_order(reports);
    let terminals: HashMap<String, usize> = t_order
        .iter()
        .enumerate()
        .map(|(i, t)| (t.clone(), i))
        .collect();

    let mut sections: HashMap<&str, Vec<&Key>> = HashMap::new();
    for name in SECTION_ORDER {
        sections.insert(name, Vec::new());
    }
    for key in &order {
        sections.get_mut(section_for(&key.0)).unwrap().push(key);
    }

    let legend_html = if t_order.len() > 1 {
        let items: String = t_order
            .iter()
            .enumerate()
            .map(|(i, t)| {
                format!(
                    "<div class=\"tb-legend-item\"><span class=\"tb-swatch\" style=\"background:var(--series-{})\"></span><span>{}</span></div>",
                    i % PALETTE.len() + 1,
                    esc(t)
                )
            })
            .collect();
        format!("<div class=\"tb-legend\">{items}</div>")
    } else {
        String::new()
    };

    let mut section_html = String::new();
    for name in SECTION_ORDER {
        let keys = &sections[name];
        if keys.is_empty() {
            continue;
        }
        let charts: String = keys
            .iter()
            .map(|key| {
                format!(
                    "<figure class=\"tb-chart-card\"><figcaption>{}</figcaption>{}</figure>",
                    esc(&row_label(key)),
                    build_line_chart_svg(key, reports, &rows[*key], &terminals)
                )
            })
            .collect();
        section_html.push_str(&format!(
            "<section class=\"tb-section\"><h2>{}</h2><div class=\"tb-chart-grid\">{}</div></section>",
            esc(section_title(name)),
            charts
        ));
    }

    let n_slots = t_order.len().max(1);
    let palette_vars_light: String = (0..n_slots)
        .map(|i| {
            format!(
                "      --series-{}: {};",
                i + 1,
                PALETTE[i % PALETTE.len()].0
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let palette_vars_dark: String = (0..n_slots)
        .map(|i| {
            format!(
                "      --series-{}: {};",
                i + 1,
                PALETTE[i % PALETTE.len()].1
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    let sources: String = source_files
        .iter()
        .map(|f| esc(f))
        .collect::<Vec<_>>()
        .join(", ");
    let first_ts = reports
        .first()
        .and_then(|r| r.timestamp.as_deref())
        .unwrap_or("?");
    let last_ts = reports
        .last()
        .and_then(|r| r.timestamp.as_deref())
        .unwrap_or("?");

    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>tbench trend</title>
<style>
  :root, .viz-root {{
    color-scheme: light;
    --surface-1: #fcfcfb;
    --text-primary: #0b0b0b;
    --text-secondary: #52514e;
    --line: #e5e4e1;
{palette_vars_light}
  }}
  @media (prefers-color-scheme: dark) {{
    :root, .viz-root {{
      color-scheme: dark;
      --surface-1: #1a1a19;
      --text-primary: #ffffff;
      --text-secondary: #c3c2b7;
      --line: #2c2c2a;
{palette_vars_dark}
    }}
  }}
  * {{ box-sizing: border-box; }}
  body {{
    margin: 0;
    padding: 24px;
    background: var(--surface-1);
    color: var(--text-primary);
    font: 14px/1.5 -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
  }}
  h1 {{ font-size: 20px; margin: 0 0 4px; }}
  h2 {{ font-size: 15px; margin: 28px 0 8px; color: var(--text-primary); }}
  .tb-meta {{ color: var(--text-secondary); font-size: 12px; margin-bottom: 16px; }}
  .tb-legend {{ display: flex; flex-wrap: wrap; gap: 14px; padding: 10px 0; border-bottom: 1px solid var(--line); }}
  .tb-legend-item {{ display: flex; align-items: center; gap: 6px; font-size: 13px; color: var(--text-primary); }}
  .tb-swatch {{ width: 12px; height: 12px; border-radius: 2px; display: inline-block; }}
  .tb-section h2 {{ border-top: 1px solid var(--line); padding-top: 16px; }}
  .tb-chart-grid {{ display: grid; grid-template-columns: repeat(auto-fit, minmax(420px, 1fr)); gap: 16px; }}
  .tb-chart-card {{ margin: 0; padding: 10px 12px; }}
  .tb-chart-card figcaption {{ font-size: 12px; color: var(--text-secondary); margin-bottom: 4px; font-variant-numeric: tabular-nums; }}
  .tb-chart {{ width: 100%; height: auto; overflow: visible; }}
  .tb-axis-label {{ font-size: 10px; fill: var(--text-secondary); font-variant-numeric: tabular-nums; }}
  .tb-axis-note {{ font-size: 10px; fill: var(--text-secondary); }}
  .tb-baseline {{ stroke: var(--line); stroke-width: 1; }}
  .tb-gridline {{ stroke: var(--line); stroke-width: 1; }}
  .tb-gridline-v {{ stroke: var(--line); stroke-width: 1; }}
  .tb-line {{ fill: none; stroke-width: 2; stroke-linejoin: round; stroke-linecap: round; }}
  .tb-whisker {{ stroke-width: 1.5; opacity: 0.45; }}
  .tb-point {{ cursor: pointer; stroke: var(--surface-1); stroke-width: 2; }}
  .tb-point:hover, .tb-point:focus {{ opacity: 0.82; outline: none; }}
  table.tb-table {{ border-collapse: collapse; width: 100%; margin-top: 8px; font-size: 12px; font-variant-numeric: tabular-nums; }}
  table.tb-table th, table.tb-table td {{ text-align: left; padding: 6px 10px; border-bottom: 1px solid var(--line); }}
  table.tb-table th {{ color: var(--text-secondary); font-weight: 600; }}
  #tb-tooltip {{
    position: absolute; display: none; pointer-events: none; z-index: 10;
    background: var(--text-primary); color: var(--surface-1);
    border-radius: 6px; padding: 8px 10px; font-size: 12px; min-width: 160px;
  }}
  #tb-tooltip.visible {{ display: block; }}
  .tb-tooltip-row {{ display: flex; justify-content: space-between; gap: 12px; }}
  .tb-tooltip-k {{ opacity: 0.75; }}
  .tb-tooltip-v {{ font-weight: 600; font-variant-numeric: tabular-nums; }}
</style>
</head>
<body class="viz-root">
<h1>tbench trend</h1>
<div class="tb-meta">Sources: {sources} — {n_reports} reports, {first_ts} → {last_ts}</div>
{legend_html}
{section_html}
<div id="tb-tooltip" role="tooltip"></div>
<script>
(function () {{
  var tooltip = document.getElementById('tb-tooltip');
  function fillTooltip(el) {{
    var d = el.dataset;
    tooltip.textContent = '';
    var rows = [
      ['Timestamp', d.timestamp],
      ['Terminal', d.entity],
      ['Median', d.median],
      ['P95', d.p95],
      ['Runs', d.runs]
    ];
    rows.forEach(function (pair) {{
      var row = document.createElement('div');
      row.className = 'tb-tooltip-row';
      var k = document.createElement('span');
      k.className = 'tb-tooltip-k';
      k.textContent = pair[0];
      var v = document.createElement('span');
      v.className = 'tb-tooltip-v';
      v.textContent = pair[1];
      row.appendChild(k);
      row.appendChild(v);
      tooltip.appendChild(row);
    }});
  }}
  function position(el, evt) {{
    var rect = el.getBoundingClientRect();
    var x = evt && typeof evt.clientX === 'number' ? evt.clientX : rect.left + rect.width / 2;
    var y = rect.top;
    tooltip.style.left = (x + window.scrollX + 14) + 'px';
    tooltip.style.top = (y + window.scrollY - 8) + 'px';
  }}
  function show(el, evt) {{
    fillTooltip(el);
    tooltip.classList.add('visible');
    position(el, evt);
  }}
  function hide() {{
    tooltip.classList.remove('visible');
  }}
  document.querySelectorAll('.tb-point').forEach(function (el) {{
    el.addEventListener('pointerenter', function (evt) {{ show(el, evt); }});
    el.addEventListener('pointermove', function (evt) {{ position(el, evt); }});
    el.addEventListener('pointerleave', hide);
    el.addEventListener('focus', function (evt) {{ show(el, evt); }});
    el.addEventListener('blur', hide);
  }});
}})();
</script>
</body>
</html>
"#,
        n_reports = reports.len(),
    )
}

// --- CLI ------------------------------------------------------------------

fn die(msg: &str, code: u8) -> ExitCode {
    eprintln!("{msg}");
    ExitCode::from(code)
}

/// Default report set: all results/tbench-*.json plain files, sorted by
/// name (lexicographic == chronological for the tbench-<ts>.json naming;
/// load_reports re-sorts by meta.timestamp regardless).
fn default_reports(root: &Path) -> Vec<PathBuf> {
    let dir = root.join("results");
    let mut files: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_file())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("tbench-") && n.ends_with(".json"))
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    files.sort();
    files
}

pub fn cmd_trend(args: &[String], root: &Path) -> ExitCode {
    let mut format = "markdown".to_string();
    let mut output: Option<PathBuf> = None;
    let mut files: Vec<PathBuf> = Vec::new();
    let mut noise = DEFAULT_NOISE;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--format" => {
                if i + 1 >= args.len() {
                    return die("tbench: --format requires a value", 2);
                }
                format = args[i + 1].clone();
                i += 2;
            }
            "--output" => {
                if i + 1 >= args.len() {
                    return die("tbench: --output requires a value", 2);
                }
                output = Some(PathBuf::from(&args[i + 1]));
                i += 2;
            }
            "--noise" => {
                match args
                    .get(i + 1)
                    .and_then(|v| crate::compare::parse_threshold(v))
                {
                    Some(v) => noise = v,
                    None => {
                        return die(
                            "tbench: --noise requires a non-negative percentage such as 5%",
                            2,
                        )
                    }
                }
                i += 2;
            }
            other if other.starts_with("--") => {
                return die(&format!("tbench: unknown option '{other}'"), 2);
            }
            path => {
                files.push(PathBuf::from(path));
                i += 1;
            }
        }
    }

    if files.is_empty() {
        files = default_reports(root);
    }

    if files.len() < 2 {
        return die(
            &format!(
                "tbench: trend requires at least 2 JSON files (found {}; pass FILE args or add more results/tbench-*.json reports)",
                files.len()
            ),
            2,
        );
    }

    for f in &files {
        if !f.is_file() {
            return die(&format!("tbench: file not found: {}", f.display()), 2);
        }
    }

    if format != "markdown" && format != "html" {
        return die(
            &format!("tbench: unsupported --format '{format}' (supported: markdown, html)"),
            2,
        );
    }

    // html always lands in a file (default results/trend-<ts>.html), same
    // convention as compare's html output.
    if output.is_none() && format == "html" {
        output = Some(root.join(format!("results/trend-{}.html", filename_timestamp())));
    }

    let source_files: Vec<String> = files.iter().map(|f| f.display().to_string()).collect();

    let mut reports: Vec<Report> = Vec::with_capacity(files.len());
    for f in &files {
        let text = match std::fs::read_to_string(f) {
            Ok(t) => t,
            Err(e) => return die(&format!("tbench: failed to read {}: {e}", f.display()), 2),
        };
        let v: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(e) => return die(&format!("tbench: failed to read {}: {e}", f.display()), 2),
        };
        reports.push(parse_report(&f.display().to_string(), &v));
    }

    for r in &reports {
        if r.timestamp.is_none() {
            eprintln!(
                "tbench: warning: {} has no meta.timestamp; sorted last",
                r.path
            );
        }
    }

    sort_reports(&mut reports);

    let out_text = if format == "html" {
        build_html_report(&reports, &source_files)
    } else {
        let (order, rows) = collect_rows(&reports);
        build_markdown_report(&reports, &order, &rows, noise)
    };

    if let Some(path) = &output {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::write(path, format!("{out_text}\n")) {
            return die(
                &format!("tbench: failed to write {}: {e}", path.display()),
                1,
            );
        }
        eprintln!("tbench: wrote {}", path.display());
    }

    // markdown always prints to stdout; html lands in the file only.
    if format == "markdown" {
        println!("{out_text}");
    }

    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(ts: Option<&str>, terminal: &str) -> Report {
        Report {
            path: "p".into(),
            timestamp: ts.map(|s| s.into()),
            terminal: terminal.into(),
            results: Vec::new(),
        }
    }

    #[test]
    fn delta_direction_aware() {
        // higher-is-better: increase is better, decrease is worse.
        assert_eq!(delta_annot(5.0, true), Some("better"));
        assert_eq!(delta_annot(-5.0, true), Some("worse"));
        // lower-is-better: increase is worse, decrease is better.
        assert_eq!(delta_annot(5.0, false), Some("worse"));
        assert_eq!(delta_annot(-5.0, false), Some("better"));
        // no change: no annotation.
        assert_eq!(delta_annot(0.0, true), None);
    }

    #[test]
    fn deltas_get_a_verdict_only_outside_the_noise_band() {
        let d = |prev, cur, higher| format_delta(prev, cur, higher, "ms", 0.05);
        assert_eq!(d(Some(100.0), Some(110.0), true), "+10.0% (better)");
        assert_eq!(d(Some(100.0), Some(90.0), false), "-10.0% (better)");
        assert_eq!(d(Some(100.0), Some(110.0), false), "+10.0% (worse)");
        // inside the band: the number stays, the verdict goes
        assert_eq!(d(Some(100.0), Some(103.0), false), "+3.0% (within noise)");
        assert_eq!(d(Some(100.0), Some(95.0), true), "-5.0% (within noise)");
        assert_eq!(d(Some(100.0), Some(100.0), true), "+0.0%");
        // no previous point, a zero previous, or a missing median
        assert_eq!(d(None, Some(100.0), true), "-");
        assert_eq!(d(Some(0.0), Some(100.0), true), "-");
        assert_eq!(d(Some(100.0), None, true), "-");
        // --noise 0 restores a verdict on every change
        assert_eq!(
            format_delta(Some(100.0), Some(100.4), false, "ms", 0.0),
            "+0.4% (worse)"
        );
    }

    #[test]
    fn percentage_rows_are_compared_in_points() {
        let d = |prev, cur| format_delta(Some(prev), Some(cur), false, "pct", 0.05);
        assert_eq!(d(1.0, 2.0), "+1.0 pt (within noise)");
        assert_eq!(d(0.0, 0.0), "+0.0 pt (within noise)");
        assert_eq!(d(0.0, 20.0), "+20.0 pt (worse)");
        assert_eq!(d(40.0, 30.0), "-10.0 pt (better)");
    }

    #[test]
    fn narrow_range_gets_nonzero_baseline() {
        // Within 20% of the max -> compressed non-zero baseline at 0.95*min.
        assert_eq!(narrow_range_baseline(95.0, 100.0), Some(95.0 * 0.95));
        // Wide range -> zero baseline (None).
        assert_eq!(narrow_range_baseline(10.0, 100.0), None);
        // Boundary: exactly 20% is not narrow (strict <).
        assert_eq!(narrow_range_baseline(80.0, 100.0), None);
        // Non-positive minimum -> zero baseline.
        assert_eq!(narrow_range_baseline(0.0, 100.0), None);
    }

    #[test]
    fn sort_by_timestamp_missing_last_stable() {
        let mut reports = vec![
            r(Some("2026-07-19T11:00:00Z"), "b"),
            r(None, "no-ts-1"),
            r(Some("2026-07-18T10:00:00Z"), "a"),
            r(None, "no-ts-2"),
        ];
        sort_reports(&mut reports);
        let terms: Vec<&str> = reports.iter().map(|r| r.terminal.as_str()).collect();
        // ascending timestamp first, then missing ones in original order.
        assert_eq!(terms, vec!["a", "b", "no-ts-1", "no-ts-2"]);
    }

    #[test]
    fn thousands_formatting() {
        assert_eq!(fmt(Some(1234.5)), "1,234.50");
        assert_eq!(fmt(Some(-1234567.0)), "-1,234,567.00");
        assert_eq!(fmt(None), "-");
    }
}
