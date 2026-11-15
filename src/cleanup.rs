//! Puts things back when a run is interrupted. A SIGINT, SIGTERM or SIGHUP
//! would otherwise leave the output flood writing to the terminal forever,
//! the terminal on the alternate screen with a hidden cursor or in raw
//! mode, and a lock file behind.
//!
//! The handler runs in signal context, so the state it needs lives in
//! atomics and it only calls async-signal-safe functions.

use std::cell::UnsafeCell;
use std::ffi::CString;
use std::mem::MaybeUninit;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicPtr, Ordering};

static CHILD_PID: AtomicI32 = AtomicI32::new(0);
static ALT_SCREEN: AtomicBool = AtomicBool::new(false);
static TERMIOS_SAVED: AtomicBool = AtomicBool::new(false);
static LOCK_PATH: AtomicPtr<libc::c_char> = AtomicPtr::new(std::ptr::null_mut());

struct SavedTermios(UnsafeCell<MaybeUninit<libc::termios>>);
// Written only while TERMIOS_SAVED is false, read only while it is true.
unsafe impl Sync for SavedTermios {}
static SAVED_TERMIOS: SavedTermios = SavedTermios(UnsafeCell::new(MaybeUninit::uninit()));

/// Leave the alternate screen, show the cursor, reset attributes.
pub const RESTORE_SCREEN: &[u8] = b"\x1b[0m\x1b[?25h\x1b[?1049l";
const TTY: &[u8] = b"/dev/tty\0";

extern "C" fn on_signal(sig: libc::c_int) {
    unsafe {
        let pid = CHILD_PID.load(Ordering::SeqCst);
        if pid > 0 {
            libc::kill(pid, libc::SIGKILL);
        }
        let raw_mode = TERMIOS_SAVED.load(Ordering::SeqCst);
        let alt_screen = ALT_SCREEN.load(Ordering::SeqCst);
        if raw_mode || alt_screen {
            let fd = libc::open(TTY.as_ptr().cast(), libc::O_RDWR | libc::O_NOCTTY);
            if fd >= 0 {
                if raw_mode {
                    libc::tcsetattr(fd, libc::TCSANOW, (*SAVED_TERMIOS.0.get()).as_ptr());
                }
                if alt_screen {
                    libc::write(fd, RESTORE_SCREEN.as_ptr().cast(), RESTORE_SCREEN.len());
                }
                libc::close(fd);
            }
        }
        let lock = LOCK_PATH.load(Ordering::SeqCst);
        if !lock.is_null() {
            libc::unlink(lock);
        }
        libc::_exit(128 + sig);
    }
}

/// Install the handler for SIGINT, SIGTERM and SIGHUP. Call once, before
/// anything is registered.
pub fn install() {
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = on_signal as extern "C" fn(libc::c_int) as usize;
        libc::sigemptyset(&mut action.sa_mask);
        for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            libc::sigaction(sig, &action, std::ptr::null_mut());
        }
    }
}

/// A child that must not outlive an interrupted run (the output flood).
pub fn set_child(pid: u32) {
    CHILD_PID.store(pid as i32, Ordering::SeqCst);
}

pub fn clear_child() {
    CHILD_PID.store(0, Ordering::SeqCst);
}

pub fn set_alt_screen(active: bool) {
    ALT_SCREEN.store(active, Ordering::SeqCst);
}

/// Remember the terminal settings to put back while the tty is in raw mode.
pub fn save_termios(saved: &libc::termios) {
    TERMIOS_SAVED.store(false, Ordering::SeqCst);
    unsafe { (*SAVED_TERMIOS.0.get()).write(*saved) };
    TERMIOS_SAVED.store(true, Ordering::SeqCst);
}

pub fn clear_termios() {
    TERMIOS_SAVED.store(false, Ordering::SeqCst);
}

/// The lock file this process owns. The path is leaked on purpose: the
/// handler may read it at any time.
pub fn set_lock(path: &Path) {
    if let Ok(c) = CString::new(path.as_os_str().as_bytes()) {
        LOCK_PATH.store(c.into_raw(), Ordering::SeqCst);
    }
}

pub fn clear_lock() {
    LOCK_PATH.store(std::ptr::null_mut(), Ordering::SeqCst);
}
