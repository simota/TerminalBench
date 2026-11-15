//! DOOM-fire IO stress — integrated port of tools/fire.c (the vendored
//! deterministic DOOM-fire terminal benchmark). The byte stream, seed,
//! frame order, warmup count, and overlapped producer design are identical
//! to the C tool, so the backend id stays "fire-v2-overlapped" (fps from
//! this port and the C tool are directly comparable; the serial v1
//! producer's numbers are not — see README).

use std::fs::File;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::sync::mpsc;
use std::time::Instant;

const DEF_W: usize = 80;
const DEF_ROWS: usize = 24;
const MAX_W: usize = 1000;
const MAX_ROWS: usize = 500;
const NPAL: usize = 37;
const WARMUP_FRAMES: usize = 60;

/// Sanglard's 37-entry DOOM fire palette (black -> red -> yellow -> white).
const PAL: [[u8; 3]; NPAL] = [
    [0x07, 0x07, 0x07],
    [0x1F, 0x07, 0x07],
    [0x2F, 0x0F, 0x07],
    [0x47, 0x0F, 0x07],
    [0x57, 0x17, 0x07],
    [0x67, 0x1F, 0x07],
    [0x77, 0x1F, 0x07],
    [0x8F, 0x27, 0x07],
    [0x9F, 0x2F, 0x07],
    [0xAF, 0x3F, 0x07],
    [0xBF, 0x47, 0x07],
    [0xC7, 0x47, 0x07],
    [0xDF, 0x4F, 0x07],
    [0xDF, 0x57, 0x07],
    [0xDF, 0x57, 0x07],
    [0xD7, 0x5F, 0x07],
    [0xD7, 0x5F, 0x07],
    [0xD7, 0x67, 0x0F],
    [0xCF, 0x6F, 0x0F],
    [0xCF, 0x77, 0x0F],
    [0xCF, 0x7F, 0x0F],
    [0xCF, 0x87, 0x17],
    [0xC7, 0x87, 0x17],
    [0xC7, 0x8F, 0x17],
    [0xC7, 0x97, 0x1F],
    [0xBF, 0x9F, 0x1F],
    [0xBF, 0x9F, 0x1F],
    [0xBF, 0xA7, 0x27],
    [0xBF, 0xA7, 0x27],
    [0xBF, 0xAF, 0x2F],
    [0xB7, 0xAF, 0x2F],
    [0xB7, 0xB7, 0x2F],
    [0xB7, 0xB7, 0x37],
    [0xCF, 0xCF, 0x6F],
    [0xDF, 0xDF, 0x9F],
    [0xEF, 0xEF, 0xC7],
    [0xFF, 0xFF, 0xFF],
];

struct Xorshift32(u32);
impl Xorshift32 {
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        self.0
    }
}

pub struct FireOutcome {
    // frames/elapsed_ns are carried for stderr diagnostics and future use;
    // fps is the reported figure.
    #[allow(dead_code)]
    pub frames: u64,
    #[allow(dead_code)]
    pub elapsed_ns: u64,
    pub fps: f64,
    pub winsize: (u16, u16), // cols x rows as reported by TIOCGWINSZ (0x0 without a tty)
    pub region: (usize, usize), // cells actually rendered
}

pub(crate) fn winsize(fd: i32) -> (u16, u16) {
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    if unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) } == 0 {
        (ws.ws_col, ws.ws_row)
    } else {
        (0, 0)
    }
}

struct Fire {
    w: usize,
    rows: usize,
    ph: usize,
    cells: Vec<u8>,
    rng: Xorshift32,
    sgr_bg: Vec<Vec<u8>>,
    sgr_fg: Vec<Vec<u8>>,
}

impl Fire {
    fn new(w: usize, rows: usize) -> Fire {
        let ph = rows * 2;
        let mut cells = vec![0u8; ph * w];
        for x in 0..w {
            cells[(ph - 1) * w + x] = (NPAL - 1) as u8;
        }
        let sgr_bg = PAL
            .iter()
            .map(|c| format!("\x1b[48;2;{};{};{}m", c[0], c[1], c[2]).into_bytes())
            .collect();
        let sgr_fg = PAL
            .iter()
            .map(|c| format!("\x1b[38;2;{};{};{}m", c[0], c[1], c[2]).into_bytes())
            .collect();
        Fire {
            w,
            rows,
            ph,
            cells,
            rng: Xorshift32(0x9d2c5680),
            sgr_bg,
            sgr_fg,
        }
    }

    /// One simulation step: propagate heat upward with horizontal
    /// drift+decay — identical traversal order and RNG usage to fire.c, so
    /// the frame sequence is byte-identical.
    fn spread(&mut self) {
        let w = self.w as isize;
        for x in 0..self.w as isize {
            for y in 1..self.ph as isize {
                let src = (y * w + x) as usize;
                let p = self.cells[src];
                if p == 0 {
                    self.cells[(src as isize - w) as usize] = 0;
                } else {
                    let r = (self.rng.next() & 3) as isize;
                    let mut dst = src as isize - r + 1;
                    if dst < w {
                        dst = w;
                    }
                    self.cells[(dst - w) as usize] = p - (r & 1) as u8;
                }
            }
        }
    }

    /// Compose one full-region repaint: per cell row an absolute CUP, then
    /// per cell bg=upper pixel, fg=lower pixel, U+2584 lower half block.
    fn render(&self, dst: &mut Vec<u8>) {
        dst.clear();
        for row in 0..self.rows {
            dst.extend_from_slice(format!("\x1b[{};1H", row + 1).as_bytes());
            let up = &self.cells[(row * 2) * self.w..(row * 2 + 1) * self.w];
            let lo = &self.cells[(row * 2 + 1) * self.w..(row * 2 + 2) * self.w];
            for x in 0..self.w {
                dst.extend_from_slice(&self.sgr_bg[up[x] as usize]);
                dst.extend_from_slice(&self.sgr_fg[lo[x] as usize]);
                dst.extend_from_slice("\u{2584}".as_bytes());
            }
        }
        dst.extend_from_slice(b"\x1b[0m"); // reset so attributes don't bleed
    }
}

/// Leaves the alternate screen and shows the cursor again when dropped.
struct ScreenGuard(File);

impl Drop for ScreenGuard {
    fn drop(&mut self) {
        let _ = self.0.write_all(crate::cleanup::RESTORE_SCREEN);
        crate::cleanup::set_alt_screen(false);
    }
}

/// Run the fire workload for `secs` wall-clock seconds against the given
/// output (a real tty for genuine numbers; anything Write otherwise).
/// `full` uses the live window size (capped) instead of the fixed 80x24.
pub fn run(secs: f64, out: &mut File, full: bool) -> std::io::Result<FireOutcome> {
    let fd = out.as_raw_fd();
    let ws = winsize(fd);
    let (mut w, mut rows) = (DEF_W, DEF_ROWS);
    if full && ws.0 >= 2 && ws.1 >= 2 {
        w = (ws.0 as usize).min(MAX_W);
        rows = (ws.1 as usize).min(MAX_ROWS);
    }

    let mut fire = Fire::new(w, rows);

    // alt screen + hidden cursor + clear. The guard puts the screen back
    // on every way out of this function, including a failed write.
    crate::cleanup::set_alt_screen(true);
    let _screen = ScreenGuard(out.try_clone()?);
    out.write_all(b"\x1b[?1049h\x1b[?25l\x1b[2J")?;

    // Warmup stays serial (nothing is timed yet; bytes are identical).
    let mut buf = Vec::new();
    for _ in 0..WARMUP_FRAMES {
        fire.spread();
        fire.render(&mut buf);
        out.write_all(&buf)?;
    }

    // Timed section, overlapped: a writer thread drains frame N while this
    // thread composes frame N+1 (two-buffer ping-pong via rendezvous
    // channels — the same one-slot mailbox discipline as fire.c, so
    // submission order preserves the deterministic stream).
    let t0 = Instant::now();
    let deadline_ns = (secs * 1e9) as u64;
    let (frame_tx, frame_rx) = mpsc::sync_channel::<Vec<u8>>(0);
    let (done_tx, done_rx) = mpsc::channel::<Vec<u8>>();

    let mut writer_out = out.try_clone()?;
    let writer = std::thread::spawn(move || -> (u64, u64, bool) {
        let mut frames = 0u64;
        let mut t_last = 0u64;
        let mut error = false;
        while let Ok(frame) = frame_rx.recv() {
            if writer_out.write_all(&frame).is_err() {
                error = true;
                break;
            }
            frames += 1;
            t_last = t0.elapsed().as_nanos() as u64;
            let stop = t_last >= deadline_ns;
            let _ = done_tx.send(frame); // return the buffer for reuse
            if stop {
                break;
            }
        }
        (frames, t_last, error)
    });

    let mut bufs = vec![Vec::new(), Vec::new()];
    let mut in_flight = 0usize;
    'produce: loop {
        let mut buf = if let Some(b) = bufs.pop() {
            b
        } else {
            match done_rx.recv() {
                Ok(b) => b,
                Err(_) => break 'produce, // writer stopped
            }
        };
        in_flight = in_flight.saturating_sub(1);
        fire.spread();
        fire.render(&mut buf);
        if frame_tx.send(buf).is_err() {
            break 'produce; // writer hit deadline/error
        }
        in_flight += 1;
    }
    drop(frame_tx);
    let (frames, t_last_ns, error) = writer.join().unwrap_or((0, 0, true));

    if error {
        return Err(std::io::Error::other("fire: write failed"));
    }
    if frames == 0 {
        return Err(std::io::Error::other("fire: no frames completed"));
    }
    let elapsed_ns = t_last_ns.max(1);
    let fps = frames as f64 / (elapsed_ns as f64 / 1e9);
    Ok(FireOutcome {
        frames,
        elapsed_ns,
        fps,
        winsize: ws,
        region: (w, rows),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXIT_SEQ_LEN: usize = 18; // "\x1b[0m\x1b[?25h\x1b[?1049l"

    fn capture(tag: &str, secs: f64) -> Vec<u8> {
        let p = std::env::temp_dir().join(format!("tbench-fire-cap-{}-{tag}", std::process::id()));
        let mut f = File::create(&p).unwrap();
        let _ = run(secs, &mut f, false).unwrap();
        let bytes = std::fs::read(&p).unwrap();
        let _ = std::fs::remove_file(&p);
        bytes
    }

    /// The frame stream is deterministic run-to-run: identical prefix up
    /// to the shorter run's tail restore sequence (whose offset depends
    /// only on how many frames fit the wall-clock budget).
    #[test]
    fn stream_is_deterministic() {
        let a = capture("det-a", 0.05);
        let b = capture("det-b", 0.05);
        let n = a.len().min(b.len()) - EXIT_SEQ_LEN;
        assert!(n > 1000);
        assert_eq!(&a[..n], &b[..n], "first divergence inside the frame stream");
    }

    /// The guard is what restores the screen on an early return, so it
    /// has to write the full restore sequence when dropped.
    #[test]
    fn screen_guard_writes_the_restore_sequence_on_drop() {
        let p = std::env::temp_dir().join(format!("tbench-fire-guard-{}", std::process::id()));
        drop(ScreenGuard(File::create(&p).unwrap()));
        assert_eq!(std::fs::read(&p).unwrap(), crate::cleanup::RESTORE_SCREEN);
        let _ = std::fs::remove_file(p);
    }

    /// A failing terminal write ends the run with an error, not a hang.
    #[test]
    #[cfg(target_os = "linux")]
    fn failed_write_is_an_error() {
        let mut full = File::options().write(true).open("/dev/full").unwrap();
        assert!(run(0.05, &mut full, false).is_err());
    }

    /// Byte parity against the vendored C tool (`tools/fire.c`, retired):
    /// set TBENCH_FIRE_REF to a file holding the C tool's stdout (default
    /// 80x24 mode) to assert the streams share their common prefix.
    #[test]
    fn stream_matches_c_reference() {
        let Ok(ref_path) = std::env::var("TBENCH_FIRE_REF") else {
            return; // reference capture not provided; parity was verified at port time
        };
        let c = std::fs::read(&ref_path).unwrap();
        let r = capture("cref", 0.05);
        let n = c.len().min(r.len()) - EXIT_SEQ_LEN;
        assert!(n > 1000);
        if let Some(i) = (0..n).find(|&i| c[i] != r[i]) {
            panic!("diverges from C reference at byte {i} of {n}");
        }
    }
}
