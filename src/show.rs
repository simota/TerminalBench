//! `tbench show` — human-readable recap of one report JSON, including the
//! Metric guide legend; section grouping and unit display deliberately
//! mirror compare's.

use serde_json::Value;

pub const UNIT_DISPLAY: &[(&str, &str)] = &[
    ("ms", "ms"),
    ("us", "μs"),
    ("MBps", "MB/s"),
    ("linesps", "lines/s"),
    ("MB", "MB"),
    ("pct", "%"),
];

pub fn unit_display(unit: &str) -> &str {
    UNIT_DISPLAY
        .iter()
        .find(|(k, _)| *k == unit)
        .map(|(_, v)| *v)
        .unwrap_or(unit)
}

pub const SECTION_ORDER: &[(&str, &str)] = &[
    ("render.throughput", "Render Throughput"),
    ("latency", "Latency"),
    ("resource", "Resource"),
    ("shell", "Shell"),
    ("other", "Other"),
];

pub fn section_for(metric: &str) -> &'static str {
    match metric {
        "render.throughput" => "render.throughput",
        "scroll.proxy" | "input.latency" => "latency",
        "resource.mem" | "resource.cpu" => "resource",
        "shell.startup" | "cmd.overhead" => "shell",
        _ => "other", // unknown future metric: shown, not dropped
    }
}

/// One-line "what does this metric actually measure" summaries — the
/// Metric guide. Full detail lives in docs/benchmark-metrics.md.
pub const METRIC_SUMMARIES: &[(&str, &str)] = &[
    ("shell.startup", "fresh interactive shell process startup (`shell -i -c exit`, rc files included), wall-clock; OS caches are not cleared"),
    ("cmd.overhead", "wall-clock of `shell -c true`: process spawn + shell init + builtin; not a differential against a direct exec"),
    ("render.throughput", "producer-side PTY completion rate for a fixed fixture (plain/ansi/cjk); display presentation is not observed"),
    ("scroll.proxy", "proxy: wall-clock to write the plain fixture to the terminal; scrollback settling is not observed"),
    ("input.latency", "retired: kernel PTY line-discipline echo on a fresh pty, which never involved the terminal emulator; shown for older reports only"),
    ("resource.mem", "RSS of the terminal emulator's own process (idle / under flood load)"),
    ("resource.cpu", "CPU use of the emulator's own process (procfs: CPU time per 1 s window; ps: macOS decayed average, directional only)"),
    ("fire.fps", "DOOM-fire producer-completed frames/second at the geometry recorded by variant"),
];

pub fn metric_summary(metric: &str) -> Option<&'static str> {
    METRIC_SUMMARIES
        .iter()
        .find(|(k, _)| *k == metric)
        .map(|(_, v)| *v)
}

fn fmt(v: Option<f64>) -> String {
    match v {
        None => "-".to_string(),
        Some(v) => {
            // Python's f"{v:,.2f}" — thousands separators + 2 decimals.
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

fn row_label(res: &Value) -> String {
    let mut parts = vec![res["metric"].as_str().unwrap_or("?").to_string()];
    if let Some(v) = res["variant"].as_str() {
        parts.push(format!("[{v}]"));
    }
    if let Some(s) = res["shell"].as_str() {
        parts.push(format!("({s})"));
    }
    parts.join(" ")
}

fn direction_arrow(res: &Value) -> &'static str {
    // measurement.direction is authoritative (Reporting Spec v1); the
    // metric-name fallback only matters for legacy reports.
    match res["measurement"]["direction"].as_str() {
        Some("higher-is-better") => "↑",
        Some("lower-is-better") => "↓",
        _ => match res["metric"].as_str() {
            Some("render.throughput") | Some("fire.fps") => "↑",
            _ => "↓",
        },
    }
}

fn meta_header(meta: &Value) -> String {
    let get = |k: &str| meta[k].as_str().unwrap_or("-").to_string();
    let mut lines = vec![
        format!("Terminal:        {}", get("terminal")),
        format!("Shell:           {}", get("shell")),
        format!("Timestamp:       {}", get("timestamp")),
        format!("OS:              {}", get("os")),
    ];
    if let Some(v) = meta["cpu"].as_str() {
        lines.push(format!("CPU:             {v}"));
    }
    if let Some(v) = meta["machine_model"].as_str() {
        lines.push(format!("Machine model:   {v}"));
    }
    if let Some(v) = meta["power_source"].as_str() {
        let suffix = match meta["low_power_mode"].as_bool() {
            Some(b) => format!(" (low power mode: {})", if b { "on" } else { "off" }),
            None => String::new(),
        };
        lines.push(format!("Power source:    {v}{suffix}"));
    }
    if let Some(v) = meta["display_refresh_hz"].as_f64() {
        lines.push(format!("Display refresh: {v} Hz"));
    }
    if let Some(v) = meta["synchronized_output_supported"].as_str() {
        lines.push(format!("Sync output:     {v}"));
    }
    lines.join("\n")
}

fn results_table(results: &[Value]) -> String {
    if results.is_empty() {
        return "(no results)".to_string();
    }
    let header = ["Metric", "Median", "P95", "Stddev", "Runs", "Unit", "Dir"];
    let rows: Vec<[String; 7]> = results
        .iter()
        .map(|res| {
            [
                row_label(res),
                fmt(res["median"].as_f64()),
                fmt(res["p95"].as_f64()),
                fmt(res["stddev"].as_f64()),
                res["runs"]
                    .as_u64()
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "-".into()),
                unit_display(res["unit"].as_str().unwrap_or("-")).to_string(),
                direction_arrow(res).to_string(),
            ]
        })
        .collect();

    // Column widths in characters (arrows are single-width chars).
    let width = |s: &str| s.chars().count();
    let mut widths: Vec<usize> = header.iter().map(|h| width(h)).collect();
    for row in &rows {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(width(cell));
        }
    }
    let fmt_row = |cells: &[String]| -> String {
        let mut out = Vec::with_capacity(cells.len());
        for (i, c) in cells.iter().enumerate() {
            let pad = widths[i].saturating_sub(width(c));
            if i == 0 {
                out.push(format!("{c}{}", " ".repeat(pad)));
            } else {
                out.push(format!("{}{c}", " ".repeat(pad)));
            }
        }
        out.join("  ")
    };

    let header_cells: Vec<String> = header.iter().map(|s| s.to_string()).collect();
    let header_line = fmt_row(&header_cells);
    let sep_line: String = widths
        .iter()
        .map(|w| "-".repeat(*w))
        .collect::<Vec<_>>()
        .join("  ");

    let mut out = Vec::new();
    let mut first = true;
    for (section, title) in SECTION_ORDER {
        let section_rows: Vec<&[String; 7]> = results
            .iter()
            .zip(&rows)
            .filter(|(res, _)| section_for(res["metric"].as_str().unwrap_or("")) == *section)
            .map(|(_, row)| row)
            .collect();
        if section_rows.is_empty() {
            continue;
        }
        if !first {
            out.push(String::new());
        }
        first = false;
        out.push(title.to_string());
        out.push(header_line.clone());
        out.push(sep_line.clone());
        for row in section_rows {
            out.push(fmt_row(row.as_slice()));
        }
    }
    out.join("\n")
}

fn metric_guide(results: &[Value]) -> Option<String> {
    let mut seen: Vec<&str> = Vec::new();
    for res in results {
        if let Some(m) = res["metric"].as_str() {
            if metric_summary(m).is_some() && !seen.contains(&m) {
                seen.push(m);
            }
        }
    }
    if seen.is_empty() {
        return None;
    }
    let width = seen.iter().map(|m| m.len()).max().unwrap();
    let mut lines = vec!["Metric guide (details: docs/benchmark-metrics.md)".to_string()];
    for m in seen {
        lines.push(format!("  {m:<width$}  {}", metric_summary(m).unwrap()));
    }
    Some(lines.join("\n"))
}

pub fn render(report: &Value) -> String {
    let results: Vec<Value> = report["results"].as_array().cloned().unwrap_or_default();
    let mut out = format!(
        "{}\n\n{}",
        meta_header(&report["meta"]),
        results_table(&results)
    );
    if let Some(guide) = metric_guide(&results) {
        out.push_str("\n\n");
        out.push_str(&guide);
    }
    out
}
