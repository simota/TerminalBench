//! results/.tbench.lock — concurrent-run detection (proposal §A item 2).
//! One live `tbench run` at a time: a second run waits (up to 120s) rather
//! than cross-polluting the first's measurements; a stale lock (dead pid)
//! is reclaimed automatically.

use std::path::{Path, PathBuf};

pub struct Lock {
    path: PathBuf,
    owned: bool,
}

fn read_lock_pid(path: &Path) -> Option<i32> {
    let text = std::fs::read_to_string(path).ok()?;
    text.lines().next()?.trim().parse().ok()
}

fn pid_alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

/// Wait for a live lock holder to finish. Ok once the lock is free or its
/// holder is gone, Err after 120 s.
fn wait_for(path: &Path, pid: i32) -> Result<(), String> {
    eprintln!("tbench: another tbench run (pid {pid}) appears active; waiting for it to finish (up to 120s; use --force to skip this check)...");
    let mut waited = 0;
    while waited < 120 {
        std::thread::sleep(std::time::Duration::from_secs(2));
        waited += 2;
        match read_lock_pid(path) {
            Some(holder) if pid_alive(holder) => {}
            _ => return Ok(()),
        }
    }
    match read_lock_pid(path) {
        Some(holder) if pid_alive(holder) => Err(format!(
            "tbench: timed out waiting for pid {holder} to finish; aborting (use --force to skip this check)"
        )),
        _ => Ok(()),
    }
}

pub fn acquire(path: &Path, force: bool) -> Result<Lock, String> {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let content = format!(
        "{}\n{}\n",
        std::process::id(),
        crate::report::utc_timestamp()
    );
    let write_error = |e: std::io::Error| format!("tbench: failed to write lock: {e}");

    // create_new is atomic: of two runs starting together, exactly one
    // gets the file. The loser sees AlreadyExists and goes through the
    // same live/stale check as any later run.
    for _ in 0..5 {
        match std::fs::File::options()
            .write(true)
            .create_new(true)
            .open(path)
        {
            Ok(mut file) => {
                use std::io::Write;
                file.write_all(content.as_bytes()).map_err(write_error)?;
                crate::cleanup::set_lock(path);
                return Ok(Lock {
                    path: path.to_path_buf(),
                    owned: true,
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                match read_lock_pid(path) {
                    Some(pid) if pid_alive(pid) => {
                        if force {
                            eprintln!("tbench: another tbench run (pid {pid}) appears active; --force given, continuing anyway (results may be polluted by cross-run interference)");
                            std::fs::write(path, &content).map_err(write_error)?;
                            crate::cleanup::set_lock(path);
                            return Ok(Lock {
                                path: path.to_path_buf(),
                                owned: true,
                            });
                        }
                        wait_for(path, pid)?;
                    }
                    Some(pid) => {
                        eprintln!("tbench: found a stale lock (pid {pid} is no longer running); reclaiming");
                        let _ = std::fs::remove_file(path);
                    }
                    // Unreadable or still being written by its creator.
                    None => {
                        std::thread::sleep(std::time::Duration::from_millis(100));
                        if read_lock_pid(path).is_none() {
                            let _ = std::fs::remove_file(path);
                        }
                    }
                }
            }
            Err(e) => return Err(write_error(e)),
        }
    }
    Err("tbench: could not take results/.tbench.lock; another run keeps claiming it".to_string())
}

impl Drop for Lock {
    fn drop(&mut self) {
        crate::cleanup::clear_lock();
        // Only remove a lock this process itself owns.
        if self.owned {
            if let Some(pid) = read_lock_pid(&self.path) {
                if pid == std::process::id() as i32 {
                    let _ = std::fs::remove_file(&self.path);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_lock(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "tbench-lock-{}-{tag}/.tbench.lock",
            std::process::id()
        ))
    }

    #[test]
    fn lock_records_our_pid_and_is_removed_on_drop() {
        let path = temp_lock("own");
        let lock = acquire(&path, false).unwrap();
        assert_eq!(read_lock_pid(&path), Some(std::process::id() as i32));
        drop(lock);
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_lock_left_by_a_dead_process_is_reclaimed() {
        let path = temp_lock("stale");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // i32::MAX is above any pid the kernel hands out.
        std::fs::write(&path, format!("{}\n2026-01-01T00:00:00Z\n", i32::MAX)).unwrap();
        let lock = acquire(&path, false).unwrap();
        assert_eq!(read_lock_pid(&path), Some(std::process::id() as i32));
        drop(lock);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn only_one_of_many_simultaneous_claims_gets_a_free_lock() {
        // create_new is the arbiter; without --force a live holder (this
        // process) makes every other claim wait, so count raw creations.
        let path = temp_lock("race");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let winners: usize = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..16)
                .map(|_| {
                    scope.spawn(|| {
                        std::fs::File::options()
                            .write(true)
                            .create_new(true)
                            .open(&path)
                            .is_ok() as usize
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).sum()
        });
        assert_eq!(winners, 1);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn force_takes_over_a_live_lock() {
        let path = temp_lock("force");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "1\n").unwrap(); // pid 1 is always alive
        let lock = acquire(&path, true).unwrap();
        assert_eq!(read_lock_pid(&path), Some(std::process::id() as i32));
        drop(lock);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn dropping_does_not_remove_a_lock_another_process_took_over() {
        let path = temp_lock("foreign");
        let lock = acquire(&path, false).unwrap();
        std::fs::write(&path, "1\n").unwrap(); // pid 1 now "owns" it
        drop(lock);
        assert!(path.exists());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
