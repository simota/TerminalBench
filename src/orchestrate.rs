//! `tbench orchestrate`. Sequentially
//! launches a fresh window/instance of each requested terminal emulator,
//! runs `tbench run` inside it, waits for completion, then compares the
//! collected reports. Sequential by construction — the structural fix for
//! the concurrent-run interference the lock file only detects. macOS only.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

pub fn cmd_orchestrate(args: &[String], root: &Path) -> ExitCode {
    let mut apps = String::new();
    let mut scenario = String::new();
    let mut runs = "20".to_string();
    let mut format = "markdown".to_string();
    let mut output_dir: Option<PathBuf> = None;
    let mut timeout_s: u64 = 600;

    let mut it = args.iter();
    while let Some(arg) = it.next() {
        macro_rules! next_val {
            ($name:literal) => {
                match it.next() {
                    Some(v) => v.clone(),
                    None => {
                        eprintln!(concat!("tbench: ", $name, " requires a value"));
                        return ExitCode::from(2);
                    }
                }
            };
        }
        match arg.as_str() {
            "--apps" => apps = next_val!("--apps"),
            "--scenario" => scenario = next_val!("--scenario"),
            "--runs" => runs = next_val!("--runs"),
            "--format" => format = next_val!("--format"),
            "--output-dir" => output_dir = Some(PathBuf::from(next_val!("--output-dir"))),
            "--timeout" => match next_val!("--timeout").parse() {
                Ok(t) if t >= 1 => timeout_s = t,
                _ => {
                    eprintln!("tbench: --timeout must be a positive integer");
                    return ExitCode::from(2);
                }
            },
            "-h" | "--help" => {
                eprintln!("see: tbench --help");
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("tbench: unknown option '{other}'");
                return ExitCode::from(2);
            }
        }
    }

    if std::env::consts::OS != "macos" {
        eprintln!("tbench: orchestrate is macOS-only (uses 'open -na'/AppleScript to launch terminal emulators — see docs/proposal-detailed-benchmark.md §E)");
        return ExitCode::from(1);
    }
    if apps.is_empty() {
        eprintln!("tbench: --apps is required, e.g. --apps Ghostty,iTerm,Terminal");
        return ExitCode::from(2);
    }
    if runs != "auto" && runs.parse::<u64>().map(|n| n >= 1) != Ok(true) {
        eprintln!("tbench: --runs must be a positive integer or \"auto\"");
        return ExitCode::from(2);
    }
    if format != "markdown" && format != "html" {
        eprintln!("tbench: unsupported --format '{format}' (supported: markdown, html)");
        return ExitCode::from(2);
    }

    let output_dir = output_dir.unwrap_or_else(|| {
        root.join(format!(
            "results/orchestrate-{}",
            crate::report::filename_timestamp()
        ))
    });
    if std::fs::create_dir_all(&output_dir).is_err() {
        eprintln!("tbench: failed to create {}", output_dir.display());
        return ExitCode::from(1);
    }
    // Launched emulator windows start in their own cwd; anchor the dir so
    // embedded script/JSON paths survive that.
    let output_dir = output_dir.canonicalize().unwrap_or(output_dir);
    let lockfile = root.join("results/.tbench.lock");

    let mut collected: Vec<PathBuf> = Vec::new();
    let mut failed: Vec<String> = Vec::new();
    for app in apps.split(',').filter(|a| !a.is_empty()) {
        match run_one(app, &scenario, &runs, &output_dir, timeout_s, &lockfile) {
            Some(json) => collected.push(json),
            None => failed.push(app.to_string()),
        }
    }

    eprintln!();
    eprintln!(
        "tbench: orchestrate summary — {} succeeded, {} failed/skipped",
        collected.len(),
        failed.len()
    );
    if !collected.is_empty() {
        let list: Vec<String> = collected.iter().map(|p| p.display().to_string()).collect();
        eprintln!("  succeeded: {}", list.join(" "));
    }
    if !failed.is_empty() {
        eprintln!("  failed/skipped: {}", failed.join(" "));
    }

    if collected.len() < 2 {
        eprintln!("tbench: fewer than 2 successful runs, skipping compare");
        return ExitCode::from(1);
    }

    eprintln!(
        "tbench: running compare over {} reports...",
        collected.len()
    );
    let mut compare_args = vec!["--format".to_string(), format];
    compare_args.extend(collected.iter().map(|p| p.display().to_string()));
    crate::compare::cmd_compare(&compare_args, root)
}

fn slug(app: &str) -> String {
    app.to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() {
                c
            } else {
                '-'
            }
        })
        .collect()
}

fn display_name(app: &str) -> &str {
    match app.to_lowercase().as_str() {
        "ghostty" => "Ghostty",
        "iterm" | "iterm2" => "iTerm",
        "terminal" | "terminal.app" => "Terminal",
        "noa" => "Noa",
        _ => app,
    }
}

/// "application X is running" is a Standard Additions process check, not
/// an Apple Event to the target app — it does not trigger the Automation
/// permission prompt the `do script` adapters do.
fn is_running(name: &str) -> bool {
    Command::new("osascript")
        .args(["-e", &format!("application \"{name}\" is running")])
        .output()
        .ok()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .trim()
                .eq_ignore_ascii_case("true")
        })
        .unwrap_or(false)
}

fn osascript(script: &str) -> bool {
    Command::new("osascript")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            if let Some(stdin) = child.stdin.as_mut() {
                stdin.write_all(script.as_bytes())?;
            }
            child.wait()
        })
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Launch a new window/instance of `app` running `script_path`, via the
/// app-specific adapter. Deliberately narrow: an unrecognized app is
/// unsupported (no generic UI-scripting fallback). Add a new emulator by
/// adding an arm here.
fn dispatch(app: &str, script_path: &Path) -> Option<bool> {
    let script = script_path.display().to_string();
    match app.to_lowercase().as_str() {
        "ghostty" => Some(
            Command::new("open")
                .args(["-na", "Ghostty", "--args", "-e", &script])
                .status()
                .map(|s| s.success())
                .unwrap_or(false),
        ),
        "iterm" | "iterm2" => Some(osascript(&format!(
            "tell application \"iTerm\"\n  create window with default profile command \"{script}\"\nend tell"
        ))),
        "terminal" | "terminal.app" => Some(osascript(&format!(
            "tell application \"Terminal\"\n  do script \"{script}\"\nend tell"
        ))),
        "noa" => Some(osascript(&format!(
            "tell application \"Noa\"\n  new window command \"{script}\"\nend tell"
        ))),
        _ => None,
    }
}

/// Launch one app, poll for completion (target JSON exists AND the lock is
/// gone — a race-free completion signal), up to timeout_s. Returns the
/// JSON path on success.
fn run_one(
    app: &str,
    scenario: &str,
    runs: &str,
    output_dir: &Path,
    timeout_s: u64,
    lockfile: &Path,
) -> Option<PathBuf> {
    let name = display_name(app);
    let slug = slug(app);
    let out_json = output_dir.join(format!("{slug}.json"));

    if is_running(name) {
        eprintln!("tbench: {name} is already running — its existing windows may add load to this measurement");
    }

    let tmp_dir = output_dir.join(".scripts");
    let _ = std::fs::create_dir_all(&tmp_dir);
    let script_path = tmp_dir.join(format!("{slug}.sh"));
    let exe = std::env::current_exe().ok()?;
    let scenario_arg = if scenario.is_empty() {
        String::new()
    } else {
        format!("--scenario \"{scenario}\" ")
    };
    let script = format!(
        "#!/bin/sh\n\"{}\" run {}--runs \"{}\" --output \"{}\"\n",
        exe.display(),
        scenario_arg,
        runs,
        out_json.display()
    );
    std::fs::write(&script_path, script).ok()?;
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755));
    }
    let _ = std::fs::remove_file(&out_json);

    match dispatch(app, &script_path) {
        None => {
            eprintln!("tbench: no orchestrate adapter for \"{app}\" — skipping (supported: ghostty, iterm, terminal, noa)");
            return None;
        }
        Some(false) => {
            eprintln!("tbench: failed to launch {name} — skipping");
            return None;
        }
        Some(true) => {}
    }

    eprintln!(
        "tbench: launched {name}, waiting for {} (up to {timeout_s}s)...",
        out_json.display()
    );
    let mut waited = 0;
    while waited < timeout_s {
        if out_json.is_file() && !lockfile.exists() {
            eprintln!("tbench: {name} finished -> {}", out_json.display());
            return Some(out_json);
        }
        std::thread::sleep(std::time::Duration::from_secs(2));
        waited += 2;
    }
    eprintln!("tbench: timed out waiting for {name} after {timeout_s}s (its window was left open — check it manually)");
    None
}
