//! Timing backends. "rust-timer" spawns a command and times it to exit
//! with `std::time::Instant`; "rust-writer" times the write of a built-in
//! fixture to the controlling terminal. Both get warmup trials and IQR
//! filtering. Backend ids are registered in Reporting Spec v1's profile
//! registry (docs/reporting-spec-v1.md).

use std::fs::File;
use std::io::{Error, ErrorKind, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use wait_timeout::ChildExt;

use crate::stats;

pub const BACKEND_TIMER: &str = "rust-timer";

/// Adaptive-sampling tunables (--runs auto).
pub const ADAPTIVE_BATCH: usize = 10;
pub const ADAPTIVE_CAP: usize = 50;
pub const ADAPTIVE_TARGET_PCT: f64 = 5.0;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Runs {
    Fixed(usize),
    Auto,
}

impl Runs {
    pub fn parse(s: &str) -> Option<Runs> {
        if s == "auto" {
            return Some(Runs::Auto);
        }
        let n: usize = s.parse().ok()?;
        if n >= 1 {
            Some(Runs::Fixed(n))
        } else {
            None
        }
    }
}

/// Measured commands are argv vectors — no shell in between, so the
/// timed boundary is exactly the process being measured. (The zsh
/// implementation used `eval` on a command string; scenario commands are
/// simple enough that argv splitting on whitespace preserves behavior.)
pub fn split_command(cmd: &str) -> Vec<String> {
    // Minimal quote-aware splitter for the fixed scenario command shapes
    // ("{shell} -i -c exit", "cat '/path/with spaces'").
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for c in cmd.chars() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                } else {
                    cur.push(c);
                }
            }
            None => match c {
                '\'' | '"' => quote = Some(c),
                c if c.is_whitespace() => {
                    if !cur.is_empty() {
                        out.push(std::mem::take(&mut cur));
                    }
                }
                _ => cur.push(c),
            },
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn run_once(argv: &[String]) -> std::io::Result<f64> {
    run_once_with_timeout(argv, COMMAND_TIMEOUT)
}

fn run_once_with_timeout(argv: &[String], timeout: Duration) -> std::io::Result<f64> {
    if argv.is_empty() {
        return Err(Error::new(ErrorKind::InvalidInput, "empty command"));
    }
    let start = Instant::now();
    let mut child = Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let status = match child.wait_timeout(timeout)? {
        Some(status) => status,
        None => {
            child.kill()?;
            let _ = child.wait()?;
            return Err(Error::new(
                ErrorKind::TimedOut,
                format!(
                    "measured command exceeded {:.0}s timeout",
                    timeout.as_secs_f64()
                ),
            ));
        }
    };
    let ms = start.elapsed().as_secs_f64() * 1000.0;
    if !status.success() {
        return Err(Error::other(format!(
            "measured command exited unsuccessfully: {status}"
        )));
    }
    Ok(ms)
}

pub struct Collected {
    pub backend: &'static str,
    pub outliers_removed: usize,
    /// Trials kept by the outlier filter, in trial order.
    pub samples_ms: Vec<f64>,
    /// Every timed trial, in trial order. p95 is computed from these.
    pub raw_ms: Vec<f64>,
}

/// Collect timed trials from `trial` (one call = one trial, in ms):
/// `warmup` discarded trials, then a fixed count or the adaptive scheme,
/// then 1.5×IQR filtering.
///
/// `Runs::Auto` collects in batches and stops once the bootstrap 95% CI
/// half-width on the retained median is within ADAPTIVE_TARGET_PCT, or
/// after ADAPTIVE_CAP raw trials.
pub fn collect_with(
    mut trial: impl FnMut() -> std::io::Result<f64>,
    runs: Runs,
    warmup: usize,
    backend: &'static str,
) -> std::io::Result<Collected> {
    for _ in 0..warmup {
        let _ = trial()?;
    }
    let raw = match runs {
        Runs::Fixed(n) => {
            let mut raw = Vec::with_capacity(n);
            for _ in 0..n {
                raw.push(trial()?);
            }
            raw
        }
        Runs::Auto => {
            let mut raw = Vec::with_capacity(ADAPTIVE_CAP);
            while raw.len() < ADAPTIVE_CAP {
                let batch = ADAPTIVE_BATCH.min(ADAPTIVE_CAP - raw.len());
                for _ in 0..batch {
                    raw.push(trial()?);
                }
                let (_, kept) = stats::iqr_filter(&raw);
                if kept.len() >= ADAPTIVE_BATCH
                    && stats::bootstrap_ci_halfwidth_pct(&kept) <= ADAPTIVE_TARGET_PCT
                {
                    break;
                }
            }
            raw
        }
    };
    let (outliers_removed, samples_ms) = stats::iqr_filter(&raw);
    Ok(Collected {
        backend,
        outliers_removed,
        samples_ms,
        raw_ms: raw,
    })
}

/// Process-timed collection: spawn `argv`, time it to exit.
pub fn collect(argv: &[String], runs: Runs, warmup: usize) -> std::io::Result<Collected> {
    collect_with(|| run_once(argv), runs, warmup, BACKEND_TIMER)
}

// --- terminal output workloads -------------------------------------------

pub const FIXTURE_LINES: usize = 100_000;
const LINE_TEXT: &str = "the quick brown fox jumps over the lazy dog 0123456789 abcdefghijklmnopqrstuvwxyz tbench-render-throughput-fixture-line";
const LINE_CJK: &str = "こんにちは、世界🌏 The quick brown 狐 jumps over 怠け者の犬 ASCII0123456789 絵文字✨🚀 全角テストtbench-cjk-fixture-line";

/// The fixed output workloads, generated in memory: FIXTURE_LINES copies
/// of one line. Byte-identical to the files the earlier implementations
/// wrote under fixtures/ and fed to `cat`.
pub fn fixture_bytes(variant: &str) -> Option<Vec<u8>> {
    let (line, ansi) = match variant {
        "plain" => (LINE_TEXT, false),
        "ansi" => (LINE_TEXT, true),
        "cjk" => (LINE_CJK, false),
        _ => return None,
    };
    let mut one = Vec::new();
    if ansi {
        one.extend_from_slice(b"\x1b[32m");
    }
    one.extend_from_slice(line.as_bytes());
    if ansi {
        one.extend_from_slice(b"\x1b[0m");
    }
    one.push(b'\n');
    Some(one.repeat(FIXTURE_LINES))
}

pub const BACKEND_WRITER: &str = "rust-writer";

fn tty_available() -> std::io::Result<()> {
    File::options()
        .write(true)
        .open("/dev/tty")
        .map(|_| ())
        .map_err(|e| Error::new(e.kind(), format!("controlling tty unavailable: {e}")))
}

/// One trial of a terminal output workload: a child `tbench __emit`
/// builds the fixture, then times only its write to the controlling tty
/// and reports the milliseconds. A child keeps the trial killable when a
/// terminal stops reading, and keeps process startup and fixture
/// generation outside the timed span.
fn emit_once(variant: &str) -> std::io::Result<f64> {
    let exe = std::env::current_exe()?;
    let mut child = Command::new(exe)
        .arg("__emit")
        .arg(variant)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let status = match child.wait_timeout(COMMAND_TIMEOUT)? {
        Some(status) => status,
        None => {
            child.kill()?;
            let _ = child.wait()?;
            return Err(Error::new(
                ErrorKind::TimedOut,
                format!(
                    "terminal did not accept the output within {:.0}s",
                    COMMAND_TIMEOUT.as_secs_f64()
                ),
            ));
        }
    };
    if !status.success() {
        return Err(Error::other(format!("output writer failed: {status}")));
    }
    let mut text = String::new();
    if let Some(mut out) = child.stdout.take() {
        std::io::Read::read_to_string(&mut out, &mut text)?;
    }
    text.trim()
        .parse::<f64>()
        .ok()
        .filter(|ms| ms.is_finite() && *ms > 0.0)
        .ok_or_else(|| Error::other("output writer reported no timing"))
}

/// Terminal-output collection for one fixture variant (backend
/// "rust-writer"). Errors without a controlling tty: the workload is the
/// write to the terminal, so there is nothing to substitute.
pub fn collect_emit(variant: &str, runs: Runs, warmup: usize) -> std::io::Result<Collected> {
    tty_available()?;
    collect_with(|| emit_once(variant), runs, warmup, BACKEND_WRITER)
}

/// `tbench __emit VARIANT` — internal: write one fixture to the tty and
/// print how long the write took, in milliseconds.
pub fn emit_main(variant: &str) -> ! {
    let Some(data) = fixture_bytes(variant) else {
        std::process::exit(2);
    };
    let Ok(mut target) = File::options().write(true).open("/dev/tty") else {
        std::process::exit(1);
    };
    let start = Instant::now();
    if target.write_all(&data).is_err() || target.flush().is_err() {
        std::process::exit(1);
    }
    println!("{:.6}", start.elapsed().as_secs_f64() * 1000.0);
    std::process::exit(0);
}

/// Background output flood for "load" variants: a child of this same
/// binary in hidden `__flood` mode writes the plain fixture to the tty in
/// a loop until it is killed.
pub struct Flood {
    child: std::process::Child,
}

pub fn flood_start() -> std::io::Result<Flood> {
    tty_available()?;
    let exe = std::env::current_exe()?;
    let mut child = Command::new(exe)
        .arg("__flood")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    // Let the flood ramp up before the caller starts sampling.
    std::thread::sleep(std::time::Duration::from_millis(200));
    if let Some(status) = child.try_wait()? {
        return Err(Error::other(format!(
            "output flood exited before sampling: {status}"
        )));
    }
    crate::cleanup::set_child(child.id());
    Ok(Flood { child })
}

impl Flood {
    /// Stop a flood that was still alive at the end of sampling. If it
    /// exited on its own, the caller must discard the loaded variant because
    /// the offered load was not present for the full measurement window.
    pub fn stop(mut self) -> std::io::Result<()> {
        crate::cleanup::clear_child();
        if let Some(status) = self.child.try_wait()? {
            return Err(Error::other(format!(
                "output flood exited during sampling: {status}"
            )));
        }
        self.child.kill()?;
        let _ = self.child.wait()?;
        Ok(())
    }
}

/// `tbench __flood` — internal: loop-write the plain fixture to the tty
/// until killed. Exits unsuccessfully if setup or output fails.
pub fn flood_main() -> ! {
    // Die with the parent even if it is killed outright and never gets to
    // stop us: the flood must not keep writing to the user's terminal.
    #[cfg(target_os = "linux")]
    unsafe {
        libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
        if libc::getppid() == 1 {
            std::process::exit(1);
        }
    }
    let Some(data) = fixture_bytes("plain") else {
        std::process::exit(1);
    };
    let mut target = match File::options().write(true).open("/dev/tty") {
        Ok(f) => f,
        Err(_) => std::process::exit(1),
    };
    loop {
        if target.write_all(&data).is_err() {
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_quoted_commands() {
        assert_eq!(
            split_command("cat '/tmp/a b.txt'"),
            vec!["cat".to_string(), "/tmp/a b.txt".to_string()]
        );
        assert_eq!(
            split_command("/bin/zsh -i -c exit"),
            vec!["/bin/zsh", "-i", "-c", "exit"]
        );
    }

    #[test]
    fn fixtures_have_the_registered_shape() {
        let plain = fixture_bytes("plain").unwrap();
        let ansi = fixture_bytes("ansi").unwrap();
        let cjk = fixture_bytes("cjk").unwrap();
        for data in [&plain, &ansi, &cjk] {
            assert_eq!(data.iter().filter(|b| **b == b'\n').count(), FIXTURE_LINES);
            assert_eq!(*data.last().unwrap(), b'\n');
        }
        assert_eq!(plain.len(), (LINE_TEXT.len() + 1) * FIXTURE_LINES);
        assert_eq!(ansi.len(), plain.len() + 9 * FIXTURE_LINES);
        assert!(ansi.starts_with(b"\x1b[32mthe quick"));
        assert!(std::str::from_utf8(&cjk).is_ok());
        assert_eq!(fixture_bytes("other"), None);
    }

    #[test]
    fn collect_with_counts_trials_and_keeps_raw_values() {
        let mut calls = 0;
        let c = collect_with(
            || {
                calls += 1;
                Ok(calls as f64)
            },
            Runs::Fixed(5),
            2,
            "test",
        )
        .unwrap();
        assert_eq!(calls, 7);
        assert_eq!(c.raw_ms, vec![3.0, 4.0, 5.0, 6.0, 7.0]);
        assert_eq!(c.backend, "test");
    }

    #[test]
    fn runs_parse() {
        assert_eq!(Runs::parse("auto"), Some(Runs::Auto));
        assert_eq!(Runs::parse("20"), Some(Runs::Fixed(20)));
        assert_eq!(Runs::parse("0"), None);
        assert_eq!(Runs::parse("x"), None);
    }

    #[test]
    fn unsuccessful_command_is_not_a_sample() {
        let argv = vec!["/usr/bin/false".to_string()];
        assert!(collect(&argv, Runs::Fixed(1), 0).is_err());
    }

    #[test]
    fn hanging_command_is_stopped_and_not_sampled() {
        let argv = vec!["/bin/sleep".to_string(), "1".to_string()];
        let start = Instant::now();
        assert!(run_once_with_timeout(&argv, Duration::from_millis(20)).is_err());
        assert!(start.elapsed() < Duration::from_millis(500));
    }
}
