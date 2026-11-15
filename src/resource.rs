//! resource.mem / resource.cpu. Targets the terminal emulator's own
//! process. On Linux that is the process holding the master side of our
//! controlling pty, sampled from /proc (backend "procfs"). Elsewhere it is
//! found by walking the ancestry to the first executable inside a macOS
//! .app bundle and sampled with `ps` (backend "ps").

use std::path::Path;
use std::process::Command;

use serde_json::json;

use crate::measure;
use crate::procfs;
use crate::report::ResultRow;
use crate::runner::RunConfig;
use crate::scenario::Scenario;
use crate::stats;

fn ps_field(pid: u32, field: &str) -> Option<String> {
    let out = Command::new("ps")
        .args(["-o", &format!("{field}="), "-p", &pid.to_string()])
        .output()
        .ok()?;
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

/// Walk ppid links from this process up to the first .app-bundle
/// executable; None (tmux/SSH/non-macOS) means the scenario must be
/// skipped rather than misattributing resource usage.
pub fn find_terminal_pid() -> Option<(u32, String)> {
    let mut pid = std::process::id();
    while pid > 1 {
        let comm = ps_field(pid, "comm")?;
        if comm.contains(".app/Contents/MacOS/") {
            return Some((pid, comm));
        }
        let ppid: u32 = ps_field(pid, "ppid")?.parse().ok()?;
        if ppid == 0 {
            break;
        }
        pid = ppid;
    }
    None
}

fn sample_rss_mb(pid: u32) -> Option<f64> {
    let kb: f64 = ps_field(pid, "rss")?.parse().ok()?;
    Some(kb / 1024.0)
}

fn sample_cpu_pct(pid: u32) -> Option<f64> {
    ps_field(pid, "pcpu")?.parse().ok()
}

pub const BACKEND_PROCFS: &str = "procfs";
const PROC_ROOT: &str = "/proc";
const WINDOW_SECS: f64 = 1.0;
const LOAD_RAMP_SECS: f64 = 1.0;
const NOTE_PROCFS: &str = "Samples the process holding the master side of the controlling pty, read from /proc. One sample per 1 s window: resource.mem is RSS at the end of the window, resource.cpu is user+system CPU time accumulated during it, as a percentage of one core. Helper processes are not included. Process identity and PSS are recorded under extensions.";

/// What sits on the other end of our controlling tty when it is not a
/// terminal emulator: tmux or screen (from their environment variables),
/// or on Linux a known intermediary holding the pty master.
pub fn pty_intermediary() -> Option<String> {
    let set = |name: &str| std::env::var_os(name).is_some_and(|v| !v.is_empty());
    if set("TMUX") {
        return Some("tmux".to_string());
    }
    if set("STY") {
        return Some("screen".to_string());
    }
    if !cfg!(target_os = "linux") {
        return None;
    }
    let root = Path::new(PROC_ROOT);
    let index = procfs::controlling_pts(root)?;
    procfs::pty_master_owners(root, index)
        .iter()
        .find_map(|o| procfs::intermediary_name(&o.comm))
        .map(str::to_string)
}

/// Name of the terminal emulator holding our pty master (Linux only):
/// the basename of its executable, or its `comm`. None when there is no
/// controlling pty, the owner is not readable, or it is an intermediary.
pub fn terminal_from_pty() -> Option<String> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let root = Path::new(PROC_ROOT);
    let index = procfs::controlling_pts(root)?;
    let owners = procfs::pty_master_owners(root, index);
    if owners
        .iter()
        .any(|o| procfs::intermediary_name(&o.comm).is_some())
    {
        return None;
    }
    let owner = owners.first()?;
    let from_exe = owner.exe.as_deref().and_then(|exe| {
        let exe = exe.strip_suffix(" (deleted)").unwrap_or(exe);
        Path::new(exe)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
    });
    from_exe
        .or_else(|| Some(owner.comm.clone()))
        .filter(|name| !name.trim().is_empty())
}

fn sysconf(name: libc::c_int) -> Option<u64> {
    let v = unsafe { libc::sysconf(name) };
    (v > 0).then_some(v as u64)
}

struct Window {
    rss_mb: f64,
    cpu_pct: f64,
}

/// One sample per WINDOW_SECS: CPU time accumulated over the window and
/// RSS at its end. None once the process can no longer be read.
fn sample_windows(pid: u32, count: usize, ticks_per_sec: u64, page_kb: u64) -> Vec<Window> {
    let root = Path::new(PROC_ROOT);
    let mut out = Vec::new();
    for _ in 0..count {
        let Some((before, _)) = procfs::cpu_ticks_and_threads(root, pid) else {
            break;
        };
        let start = std::time::Instant::now();
        std::thread::sleep(std::time::Duration::from_secs_f64(WINDOW_SECS));
        let wall = start.elapsed().as_secs_f64();
        let (Some((after, _)), Some(rss)) = (
            procfs::cpu_ticks_and_threads(root, pid),
            procfs::rss_kb(root, pid, page_kb),
        ) else {
            break;
        };
        out.push(Window {
            rss_mb: rss as f64 / 1024.0,
            cpu_pct: procfs::cpu_pct(before, after, ticks_per_sec, wall),
        });
    }
    out
}

fn run_linux(scenario: &Scenario, cfg: &RunConfig, results: &mut Vec<ResultRow>) -> bool {
    let mem_metric = scenario.field("mem_metric").unwrap_or("resource.mem");
    let cpu_metric = scenario.field("cpu_metric").unwrap_or("resource.cpu");
    let unit_mem = scenario.field("unit_mem").unwrap_or("MB");
    let unit_cpu = scenario.field("unit_cpu").unwrap_or("pct");
    let root = Path::new(PROC_ROOT);

    let Some(index) = procfs::controlling_pts(root) else {
        eprintln!("tbench: no controlling pty, skipping resource.mem/resource.cpu");
        return false;
    };
    let owners = procfs::pty_master_owners(root, index);
    let Some(owner) = owners.first() else {
        eprintln!("tbench: no readable process holds the master of /dev/pts/{index} (terminal running as another user, or over SSH?), skipping resource.mem/resource.cpu");
        return false;
    };
    if let Some(name) = owners
        .iter()
        .find_map(|o| procfs::intermediary_name(&o.comm))
    {
        eprintln!("tbench: /dev/pts/{index} belongs to {name}, not a terminal emulator; skipping resource.mem/resource.cpu");
        return false;
    }
    let (Some(ticks_per_sec), Some(page_bytes)) =
        (sysconf(libc::_SC_CLK_TCK), sysconf(libc::_SC_PAGESIZE))
    else {
        eprintln!(
            "tbench: could not read clock tick or page size, skipping resource.mem/resource.cpu"
        );
        return false;
    };
    let page_kb = page_bytes / 1024;
    eprintln!(
        "tbench: resource.mem/resource.cpu targeting pid {} ({}), master of /dev/pts/{index}",
        owner.pid, owner.comm
    );
    if owners.len() > 1 {
        eprintln!(
            "tbench: {} processes hold that pty master; using the lowest pid",
            owners.len()
        );
    }

    let windows = match cfg.runs {
        measure::Runs::Fixed(n) => n,
        measure::Runs::Auto => {
            eprintln!(
                "tbench: --runs auto is not supported for resource.mem/resource.cpu yet, using 5"
            );
            5
        }
    }
    .clamp(1, 5);

    let idle_settle = cfg.settle_secs.max(5.0);
    eprintln!("tbench: settling {idle_settle}s before resource.*/idle sampling");
    std::thread::sleep(std::time::Duration::from_secs_f64(idle_settle));

    let mut pss = serde_json::Map::new();
    let mut sampling_started = serde_json::Map::new();
    let mut produced = false;
    for variant in ["idle", "load"] {
        let mut flood = None;
        if variant == "load" {
            match measure::flood_start() {
                Ok(f) => flood = Some(f),
                Err(e) => {
                    eprintln!(
                        "tbench: resource.*/load could not start output flood: {e}; skipping"
                    );
                    continue;
                }
            }
            std::thread::sleep(std::time::Duration::from_secs_f64(LOAD_RAMP_SECS));
        }

        let started = crate::report::utc_timestamp();
        let samples = sample_windows(owner.pid, windows, ticks_per_sec, page_kb);
        let pss_mb = procfs::pss_kb(root, owner.pid).map(|kb| kb as f64 / 1024.0);

        if let Some(f) = flood {
            if let Err(e) = f.stop() {
                eprintln!(
                    "tbench: resource.*/{variant} output flood failed during sampling: {e}; discarding the variant"
                );
                continue;
            }
        }
        if samples.len() < windows {
            eprintln!("tbench: resource.*/{variant} lost the target process during sampling; discarding the variant");
            continue;
        }
        if let Some(v) = pss_mb {
            pss.insert(variant.to_string(), json!((v * 100.0).round() / 100.0));
        }
        sampling_started.insert(variant.to_string(), json!(started));

        let mem: Vec<f64> = samples.iter().map(|w| w.rss_mb).collect();
        let cpu: Vec<f64> = samples.iter().map(|w| w.cpu_pct).collect();
        for (metric, unit, values) in [(mem_metric, unit_mem, &mem), (cpu_metric, unit_cpu, &cpu)] {
            results.push(ResultRow {
                metric: metric.to_string(),
                shell: None,
                unit: unit.to_string(),
                median: stats::median(values),
                p95: stats::p95(values),
                stddev: stats::stddev(values),
                runs: values.len(),
                outliers_removed: 0,
                backend: BACKEND_PROCFS.to_string(),
                variant: Some(variant.to_string()),
                notes: Some(NOTE_PROCFS.to_string()),
                samples: cfg.keep_samples.then(|| values.clone()),
            });
        }
        produced = true;
    }

    if produced {
        let threads = procfs::cpu_ticks_and_threads(root, owner.pid).map(|(_, t)| t);
        cfg.extensions.borrow_mut().insert(
            "org.terminalbench.resource".to_string(),
            json!({
                "discovery": "pty-master-owner",
                "pts": index,
                "pid": owner.pid,
                "comm": owner.comm,
                "exe": owner.exe,
                "threads": threads,
                "pty_masters_held": owner.pty_masters,
                "pty_master_holders": owners.len(),
                "window_secs": WINDOW_SECS,
                "load_ramp_secs": LOAD_RAMP_SECS,
                "clock_ticks_per_sec": ticks_per_sec,
                "pss_mb": pss,
                "sampling_started": sampling_started,
            }),
        );
    }
    produced
}

pub fn run(scenario: &Scenario, cfg: &RunConfig, results: &mut Vec<ResultRow>) -> bool {
    if cfg!(target_os = "linux") {
        return run_linux(scenario, cfg, results);
    }
    let mem_metric = scenario
        .field("mem_metric")
        .unwrap_or("resource.mem")
        .to_string();
    let cpu_metric = scenario
        .field("cpu_metric")
        .unwrap_or("resource.cpu")
        .to_string();
    let unit_mem = scenario.field("unit_mem").unwrap_or("MB").to_string();
    let unit_cpu = scenario.field("unit_cpu").unwrap_or("pct").to_string();
    let notes = scenario.field("notes").map(str::to_string);

    let Some((term_pid, term_comm)) = find_terminal_pid() else {
        eprintln!("tbench: could not identify the terminal emulator process (tmux/SSH/unsupported OS?), skipping resource.mem/resource.cpu");
        return false;
    };
    eprintln!("tbench: resource.mem/resource.cpu targeting pid {term_pid} ({term_comm})");

    let runs = match cfg.runs {
        measure::Runs::Fixed(n) => n,
        measure::Runs::Auto => {
            eprintln!(
                "tbench: --runs auto is not supported for resource.mem/resource.cpu yet, using 5"
            );
            5
        }
    };
    let samples_per_variant = runs.clamp(1, 5);

    // Settle before *idle* sampling specifically: macOS's `ps %cpu` is a
    // decayed running average and inherits whatever ran just before this
    // scenario (the observed 243% idle p95). Floor of 5s regardless of
    // --settle.
    let idle_settle = cfg.settle_secs.max(5.0);
    eprintln!("tbench: settling {idle_settle}s before resource.*/idle sampling");
    std::thread::sleep(std::time::Duration::from_secs_f64(idle_settle));

    for variant in ["idle", "load"] {
        let mut flood = None;
        if variant == "load" {
            match measure::flood_start() {
                Ok(f) => flood = Some(f),
                Err(e) => {
                    eprintln!(
                        "tbench: resource.*/load could not start output flood: {e}; skipping"
                    );
                    continue;
                }
            }
        }

        let mut mem_samples = Vec::new();
        let mut cpu_samples = Vec::new();
        for i in 0..samples_per_variant {
            if let Some(m) = sample_rss_mb(term_pid) {
                mem_samples.push(m);
            }
            if let Some(c) = sample_cpu_pct(term_pid) {
                cpu_samples.push(c);
            }
            if i + 1 < samples_per_variant {
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
        }

        if let Some(f) = flood {
            if let Err(e) = f.stop() {
                eprintln!(
                    "tbench: resource.*/{variant} output flood failed during sampling: {e}; discarding the variant"
                );
                continue;
            }
        }

        if mem_samples.is_empty() && cpu_samples.is_empty() {
            eprintln!("tbench: resource.*/{variant} produced no usable samples (process exited?)");
            continue;
        }

        for (metric, unit, samples) in [
            (&mem_metric, &unit_mem, &mem_samples),
            (&cpu_metric, &unit_cpu, &cpu_samples),
        ] {
            if samples.is_empty() {
                continue;
            }
            results.push(ResultRow {
                metric: metric.clone(),
                shell: None,
                unit: unit.clone(),
                median: stats::median(samples),
                p95: stats::p95(samples),
                stddev: stats::stddev(samples),
                runs: samples.len(),
                outliers_removed: 0,
                backend: "ps".to_string(),
                variant: Some(variant.to_string()),
                notes: notes.clone(),
                samples: cfg.keep_samples.then(|| samples.clone()),
            });
        }
    }
    true
}

/// Used by orchestrate-launched runs to keep relative paths stable.
pub fn _abs(p: &Path) -> std::path::PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}
