//! DECSET 2026 (synchronized output) probe.
//! Writes DECRQM (`CSI ? 2026 $ p`) to /dev/tty in raw mode and reads the
//! terminal's automatic reply (`CSI ? 2026 ; Ps $ y`). Not a frame-skip
//! detector: it only reports whether the terminal recognizes mode 2026.

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;

/// "supported" | "unsupported" | "no_reply"
pub fn probe_sync_output() -> &'static str {
    // Test hook: force a deterministic verdict so conformance tests can
    // exercise report assembly without a live terminal.
    if let Ok(v) = std::env::var("TBENCH_TEST_SYNC_OUTPUT") {
        return match v.as_str() {
            "supported" => "supported",
            "unsupported" => "unsupported",
            _ => "no_reply",
        };
    }
    match probe_impl() {
        Some(v) => v,
        None => "no_reply",
    }
}

fn probe_impl() -> Option<&'static str> {
    let mut tty = File::options()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .ok()?;
    let fd = tty.as_raw_fd();

    // Raw mode with a 1.0s per-byte read timeout (VMIN=0, VTIME=10),
    // restored on every exit path — the exact stty settings the zsh
    // implementation used (`stty raw -echo min 0 time 10`).
    let mut saved: libc::termios = unsafe { std::mem::zeroed() };
    if unsafe { libc::tcgetattr(fd, &mut saved) } != 0 {
        return None;
    }
    let mut raw = saved;
    unsafe { libc::cfmakeraw(&mut raw) };
    raw.c_cc[libc::VMIN] = 0;
    raw.c_cc[libc::VTIME] = 10;
    crate::cleanup::save_termios(&saved);
    if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) } != 0 {
        crate::cleanup::clear_termios();
        return None;
    }

    let verdict = (|| {
        tty.write_all(b"\x1b[?2026$p").ok()?;
        tty.flush().ok()?;
        // Each read blocks up to 1s and returns 0 bytes if nothing arrived
        // — that 0-read is the "terminal never answered" signal, so this
        // drains exactly the reply without knowing its length up front.
        let mut reply = Vec::new();
        let mut byte = [0u8; 1];
        while reply.len() < 32 {
            match tty.read(&mut byte) {
                Ok(1) => {
                    reply.push(byte[0]);
                    if byte[0] == b'y' {
                        break; // DECRPM terminator
                    }
                }
                _ => break,
            }
        }
        let reply = String::from_utf8_lossy(&reply);
        if reply.is_empty() || !reply.contains("$y") {
            return None;
        }
        if reply.contains(";0$y") {
            Some("unsupported")
        } else {
            Some("supported")
        }
    })();

    unsafe { libc::tcsetattr(fd, libc::TCSANOW, &saved) };
    crate::cleanup::clear_termios();
    verdict
}
