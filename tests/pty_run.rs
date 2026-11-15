//! End-to-end run inside a pseudo-terminal the test owns. CI runners have
//! no controlling tty, so without this every tty-bound scenario would be
//! skipped there and only ever exercised on a developer's machine. The
//! test process holds the pty master, which on Linux also makes it the
//! "terminal" that resource.* discovers and samples.
//!
//! Each test prints what it observed; run with `-- --nocapture` (CI does)
//! to see the rows a run produced.
//!
//! On macOS resource.* cannot run here: it looks for a .app bundle in the
//! process ancestry, and a test binary has none. That is asserted, not
//! skipped. Other systems have no test in this file.
#![cfg(any(target_os = "linux", target_os = "macos"))]

use std::fs::File;
use std::io::Read;
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// One `tbench run` at a time: they share results/.tbench.lock.
static SERIAL: Mutex<()> = Mutex::new(());

const COLS: u16 = 80;
const ROWS: u16 = 24;

fn open_pty() -> (OwnedFd, OwnedFd) {
    let (mut master, mut slave) = (-1, -1);
    let rc = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(rc, 0, "openpty failed");
    // Set the size with an ioctl: openpty's winsize parameter is *const on
    // Linux and *mut on macOS, so one call site cannot satisfy both.
    let mut size: libc::winsize = unsafe { std::mem::zeroed() };
    size.ws_col = COLS;
    size.ws_row = ROWS;
    let rc = unsafe { libc::ioctl(master, libc::TIOCSWINSZ as _, &size) };
    assert_eq!(rc, 0, "TIOCSWINSZ failed");
    unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) }
}

/// The last bytes the "terminal" received.
type Tail = Arc<Mutex<Vec<u8>>>;
const TAIL_BYTES: usize = 1 << 16;

/// Spawn tbench with the pty slave as its controlling terminal and keep
/// reading the master so writes to the terminal never block.
fn spawn_in_pty(args: &[&str], envs: &[(&str, &str)]) -> (Child, Tail) {
    let (master, slave) = open_pty();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_tbench"));
    cmd.args(args)
        .envs(envs.iter().copied())
        .env_remove("TMUX")
        .env_remove("STY")
        .env_remove("TERM_PROGRAM")
        .env_remove("TBENCH_TERMINAL_ID")
        .stdin(Stdio::from(slave))
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = cmd.spawn().expect("spawn tbench");
    let tail: Tail = Arc::default();
    let sink = Arc::clone(&tail);
    std::thread::spawn(move || {
        let mut master = File::from(master);
        let mut buf = vec![0u8; 1 << 16];
        while let Ok(n) = master.read(&mut buf) {
            if n == 0 {
                break;
            }
            let mut tail = sink.lock().unwrap();
            tail.extend_from_slice(&buf[..n]);
            let excess = tail.len().saturating_sub(TAIL_BYTES);
            tail.drain(..excess);
        }
    });
    (child, tail)
}

fn wait_until(limit: Duration, mut done: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < limit {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    done()
}

/// Pid of a live `tbench __flood` child of `parent`, if there is one.
#[cfg(target_os = "linux")]
fn flood_child_of(parent: u32) -> Option<u32> {
    for entry in std::fs::read_dir("/proc").ok()?.filter_map(|e| e.ok()) {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        let Some(rest) = stat.rsplit_once(')').map(|(_, rest)| rest) else {
            continue;
        };
        let fields: Vec<&str> = rest.split_whitespace().collect();
        if fields.first() == Some(&"Z") || fields.get(1) != Some(&parent.to_string().as_str()) {
            continue;
        }
        let cmdline = std::fs::read(entry.path().join("cmdline")).unwrap_or_default();
        if cmdline.split(|b| *b == 0).any(|arg| arg == b"__flood") {
            return Some(pid);
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn alive(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|stat| {
            stat.rsplit_once(')')
                .map(|(_, rest)| !rest.trim_start().starts_with('Z'))
        })
        .unwrap_or(false)
}

fn lock_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("results/.tbench.lock")
}

fn temp_report(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("tbench-pty-{}-{tag}.json", std::process::id()))
}

#[test]
fn every_scenario_produces_results_inside_a_pty() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let out = temp_report("full");
    let _ = std::fs::remove_file(&out);
    // Linux can run everything, so a scenario without results is an error
    // there. macOS cannot run resource.* from a test (see the module doc).
    let mut args = vec![
        "run",
        "--shells",
        "bash",
        "--runs",
        "2",
        "--warmup",
        "0",
        "--settle",
        "0",
        "--output",
        out.to_str().unwrap(),
    ];
    if cfg!(target_os = "linux") {
        args.push("--strict");
    }
    let (mut child, _tail) = spawn_in_pty(&args, &[("FIRE_SECS", "0.2")]);
    let status = child.wait().expect("wait for tbench");
    assert!(status.success(), "tbench run failed: {status}");

    let report: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&out).expect("report written")).unwrap();
    let _ = std::fs::remove_file(&out);
    let rows = report["results"].as_array().unwrap();
    println!("pty run on {}: {} rows", std::env::consts::OS, rows.len());
    for r in rows {
        println!(
            "pty run:   {} [{}] {} = {} {} (runs {})",
            r["metric"].as_str().unwrap_or("?"),
            r["variant"].as_str().unwrap_or("-"),
            r["backend"].as_str().unwrap_or("?"),
            r["median"],
            r["unit"].as_str().unwrap_or("?"),
            r["runs"],
        );
    }
    let run = &report["extensions"]["org.terminalbench.run"];
    println!(
        "pty run: scenarios {} | without results {}",
        run["scenarios"], run["scenarios_without_results"]
    );

    let has = |metric: &str, variant: Option<&str>| {
        rows.iter().any(|r| {
            r["metric"] == metric
                && match variant {
                    Some(v) => r["variant"] == v,
                    None => true,
                }
        })
    };
    for (metric, variant) in [
        ("shell.startup", None),
        ("cmd.overhead", None),
        ("render.throughput", Some("plain")),
        ("render.throughput", Some("ansi")),
        ("render.throughput", Some("cjk")),
        ("scroll.proxy", None),
        ("fire.fps", Some("80x24")),
    ] {
        assert!(has(metric, variant), "missing {metric} {variant:?}");
    }
    assert_eq!(run["tty"]["cols"], COLS);
    assert_eq!(run["tty"]["rows"], ROWS);
    assert_eq!(report["meta"]["meta_notes"], serde_json::Value::Null);

    if cfg!(target_os = "linux") {
        for (metric, variant) in [
            ("resource.mem", Some("idle")),
            ("resource.mem", Some("load")),
            ("resource.cpu", Some("idle")),
            ("resource.cpu", Some("load")),
        ] {
            assert!(has(metric, variant), "missing {metric} {variant:?}");
        }
        // resource.* found this test process as the pty owner and ran first.
        assert_eq!(rows[0]["metric"], "resource.mem");
        assert_eq!(rows[0]["backend"], "procfs");
        let resource = &report["extensions"]["org.terminalbench.resource"];
        assert_eq!(resource["pid"], std::process::id());
        assert_eq!(resource["discovery"], "pty-master-owner");
        assert_eq!(
            run["scenarios_without_results"].as_array().unwrap().len(),
            0
        );
        assert_eq!(
            report["run_context"]["environment"]["terminal"]["status"],
            "known"
        );
    } else {
        // Exactly one scenario has no results, and it is the expected one.
        assert_eq!(
            run["scenarios_without_results"],
            serde_json::json!(["resource-footprint"]),
            "only resource.* may be missing on macOS"
        );
        assert!(!has("resource.mem", None) && !has("resource.cpu", None));
    }
}

// The flood only runs in the resource.* load variant, which needs Linux.
#[test]
#[cfg(target_os = "linux")]
fn sigterm_stops_the_flood_and_removes_the_lock() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let out = temp_report("term");
    let (mut child, _tail) = spawn_in_pty(
        &[
            "run",
            "--scenario",
            "resource-footprint",
            "--runs",
            "5",
            "--settle",
            "0",
            "--output",
            out.to_str().unwrap(),
        ],
        &[],
    );
    let tbench = child.id();

    // idle settle and sampling come first; the flood starts after ~10 s
    let mut flood = None;
    assert!(
        wait_until(Duration::from_secs(40), || {
            flood = flood_child_of(tbench);
            flood.is_some()
        }),
        "the load variant never started its flood"
    );
    let flood = flood.unwrap();
    assert!(lock_path().is_file(), "no lock while running");

    unsafe { libc::kill(tbench as i32, libc::SIGTERM) };
    let status = child.wait().expect("wait for tbench");
    assert_eq!(status.code(), Some(128 + libc::SIGTERM), "{status}");
    assert!(
        wait_until(Duration::from_secs(5), || !alive(flood)),
        "flood pid {flood} outlived the interrupted run"
    );
    assert!(!lock_path().exists(), "lock left behind");
    assert!(!out.exists(), "an interrupted run wrote a report");
    println!("pty sigterm: tbench exited {status}, flood pid {flood} gone, lock removed");
}

#[test]
fn sigint_during_fire_leaves_the_alternate_screen() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let out = temp_report("fire");
    let (mut child, tail) = spawn_in_pty(
        &[
            "run",
            "--scenario",
            "fire",
            "--runs",
            "1",
            "--settle",
            "0",
            "--output",
            out.to_str().unwrap(),
        ],
        &[("FIRE_SECS", "20")],
    );
    let enter: &[u8] = b"\x1b[?1049h";
    let leave: &[u8] = b"\x1b[?1049l";
    let seen = |needle: &[u8]| {
        let tail = tail.lock().unwrap();
        tail.windows(needle.len()).rposition(|w| w == needle)
    };
    // The enter sequence scrolls out of the tail quickly; fps-speed frames
    // arriving is the sign the alternate screen is active.
    assert!(
        wait_until(Duration::from_secs(10), || tail.lock().unwrap().len()
            >= TAIL_BYTES),
        "fire never started drawing"
    );
    std::thread::sleep(Duration::from_millis(300));

    unsafe { libc::kill(child.id() as i32, libc::SIGINT) };
    let status = child.wait().expect("wait for tbench");
    assert_eq!(status.code(), Some(128 + libc::SIGINT), "{status}");
    assert!(
        wait_until(Duration::from_secs(2), || seen(leave).is_some()),
        "terminal was left on the alternate screen"
    );
    let left_at = seen(leave).unwrap();
    assert!(seen(enter).is_none_or(|entered_at| entered_at < left_at));
    assert!(!lock_path().exists(), "lock left behind");
    println!("pty sigint: tbench exited {status}, alternate screen left, lock removed");
}
