//! Linux /proc readers for resource.mem / resource.cpu: find the process
//! that holds the master side of our controlling pty, then read its memory
//! and accumulated CPU time. Every function takes the proc root so the
//! parsing can be tested against a fixture tree.

use std::path::Path;

/// The process holding the pty master for one pts index.
#[derive(Debug, Clone, PartialEq)]
pub struct Owner {
    pub pid: u32,
    pub comm: String,
    pub exe: Option<String>,
    /// How many pty masters this process holds in total. More than one
    /// means it serves several windows, tabs or panes, and its memory and
    /// CPU cover all of them.
    pub pty_masters: usize,
}

/// `comm` and the whitespace-separated fields after it from a
/// /proc/<pid>/stat line. `comm` may itself contain spaces and parentheses,
/// so split on the last ')'. Index 0 of the returned fields is `state`.
fn split_stat(text: &str) -> Option<(&str, Vec<&str>)> {
    let open = text.find('(')?;
    let close = text.rfind(')')?;
    if close < open {
        return None;
    }
    let rest: Vec<&str> = text[close + 1..].split_whitespace().collect();
    Some((&text[open + 1..close], rest))
}

fn read_stat(proc_root: &Path, pid: &str) -> Option<String> {
    std::fs::read_to_string(proc_root.join(pid).join("stat")).ok()
}

/// Unix98 pts index from a stat `tty_nr` value, None for anything that is
/// not a pts (no controlling tty, a virtual console, a serial line).
pub fn pts_index(tty_nr: u64) -> Option<u32> {
    let major = (tty_nr >> 8) & 0xfff;
    let minor = (tty_nr & 0xff) | ((tty_nr >> 12) & 0xfff00);
    if (136..=143).contains(&major) {
        Some(((major - 136) * 256 + minor) as u32)
    } else {
        None
    }
}

/// pts index of the calling process's controlling terminal.
pub fn controlling_pts(proc_root: &Path) -> Option<u32> {
    let stat = read_stat(proc_root, "self")?;
    let (_, rest) = split_stat(&stat)?;
    pts_index(rest.get(4)?.parse().ok()?)
}

fn tty_index(fdinfo: &str) -> Option<u32> {
    fdinfo
        .lines()
        .find_map(|line| line.strip_prefix("tty-index:"))
        .and_then(|v| v.trim().parse().ok())
}

/// Every readable process holding a pty master whose `tty-index` is
/// `index`, lowest pid first. Processes owned by other users are not
/// readable and are silently absent.
pub fn pty_master_owners(proc_root: &Path, index: u32) -> Vec<Owner> {
    let mut owners = Vec::new();
    let Ok(entries) = std::fs::read_dir(proc_root) else {
        return owners;
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let name = entry.file_name();
        let Some(pid_str) = name.to_str() else {
            continue;
        };
        let Ok(pid) = pid_str.parse::<u32>() else {
            continue;
        };
        let dir = entry.path();
        let Ok(fds) = std::fs::read_dir(dir.join("fd")) else {
            continue;
        };
        let mut masters = 0usize;
        let mut holds_ours = false;
        for fd in fds.filter_map(|e| e.ok()) {
            let is_ptmx = std::fs::read_link(fd.path())
                .map(|target| target.to_string_lossy().ends_with("/ptmx"))
                .unwrap_or(false);
            if !is_ptmx {
                continue;
            }
            masters += 1;
            let info = std::fs::read_to_string(dir.join("fdinfo").join(fd.file_name()));
            if info.ok().as_deref().and_then(tty_index) == Some(index) {
                holds_ours = true;
            }
        }
        if !holds_ours {
            continue;
        }
        let comm = std::fs::read_to_string(dir.join("comm"))
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        let exe = std::fs::read_link(dir.join("exe"))
            .ok()
            .map(|p| p.to_string_lossy().into_owned());
        owners.push(Owner {
            pid,
            comm,
            exe,
            pty_masters: masters,
        });
    }
    owners.sort_by_key(|o| o.pid);
    owners
}

/// Programs that sit between a terminal emulator and the shell. Their pty
/// is not the emulator's, so tty-bound numbers describe them instead.
pub fn intermediary_name(comm: &str) -> Option<&'static str> {
    const KNOWN: &[(&str, &str)] = &[
        ("tmux", "tmux"),
        ("screen", "screen"),
        ("script", "script"),
        ("sshd", "sshd"),
        ("mosh-server", "mosh"),
        ("zellij", "zellij"),
        ("dtach", "dtach"),
        ("abduco", "abduco"),
    ];
    KNOWN
        .iter()
        .find(|(prefix, _)| comm.starts_with(prefix))
        .map(|(_, name)| *name)
}

/// Resident set size in kB, from statm (the figure `ps -o rss` reports).
pub fn rss_kb(proc_root: &Path, pid: u32, page_kb: u64) -> Option<u64> {
    let statm = std::fs::read_to_string(proc_root.join(pid.to_string()).join("statm")).ok()?;
    let pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
    Some(pages * page_kb)
}

/// Proportional set size in kB, None where smaps_rollup is unavailable.
pub fn pss_kb(proc_root: &Path, pid: u32) -> Option<u64> {
    let text =
        std::fs::read_to_string(proc_root.join(pid.to_string()).join("smaps_rollup")).ok()?;
    text.lines()
        .find_map(|line| line.strip_prefix("Pss:"))
        .and_then(|v| v.split_whitespace().next())
        .and_then(|v| v.parse().ok())
}

/// (user + system CPU time in clock ticks, thread count). The times cover
/// every thread of the process, including ones that have already exited.
pub fn cpu_ticks_and_threads(proc_root: &Path, pid: u32) -> Option<(u64, u64)> {
    let stat = read_stat(proc_root, &pid.to_string())?;
    let (_, rest) = split_stat(&stat)?;
    let utime: u64 = rest.get(11)?.parse().ok()?;
    let stime: u64 = rest.get(12)?.parse().ok()?;
    let threads: u64 = rest.get(17)?.parse().ok()?;
    Some((utime + stime, threads))
}

/// CPU use over one window as a percentage of one core.
pub fn cpu_pct(ticks_before: u64, ticks_after: u64, ticks_per_sec: u64, wall_secs: f64) -> f64 {
    if ticks_per_sec == 0 || wall_secs <= 0.0 {
        return 0.0;
    }
    let cpu_secs = ticks_after.saturating_sub(ticks_before) as f64 / ticks_per_sec as f64;
    cpu_secs / wall_secs * 100.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("tbench-procfs-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    /// tty_nr for /dev/pts/N as the kernel encodes it (major 136).
    fn pts_tty_nr(index: u64) -> u64 {
        (136 << 8) | (index & 0xff) | ((index & 0xfff00) << 12)
    }

    fn add_process(root: &Path, pid: u32, comm: &str, tty_nr: u64, masters: &[u32]) {
        let dir = root.join(pid.to_string());
        std::fs::create_dir_all(dir.join("fd")).unwrap();
        std::fs::create_dir_all(dir.join("fdinfo")).unwrap();
        std::fs::write(dir.join("comm"), format!("{comm}\n")).unwrap();
        std::fs::write(
            dir.join("stat"),
            format!(
                "{pid} ({comm}) S 1 {pid} {pid} {tty_nr} {pid} 4194304 100 0 0 0 250 50 0 0 20 0 7 0 1000 0 0\n"
            ),
        )
        .unwrap();
        std::fs::write(dir.join("statm"), "5000 1200 300 10 0 900 0\n").unwrap();
        std::fs::write(
            dir.join("smaps_rollup"),
            "55d0-7ffd ---p 00000000 00:00 0 [rollup]\nRss:  4800 kB\nPss:  2100 kB\n",
        )
        .unwrap();
        std::os::unix::fs::symlink("/dev/null", dir.join("fd/0")).unwrap();
        for (i, index) in masters.iter().enumerate() {
            let fd = (10 + i).to_string();
            std::os::unix::fs::symlink("/dev/ptmx", dir.join("fd").join(&fd)).unwrap();
            std::fs::write(
                dir.join("fdinfo").join(&fd),
                format!("pos:\t0\nflags:\t02004002\nmnt_id:\t28\ntty-index:\t{index}\n"),
            )
            .unwrap();
        }
    }

    #[test]
    fn pts_index_decodes_small_and_large_minors() {
        assert_eq!(pts_index(pts_tty_nr(0)), Some(0));
        assert_eq!(pts_index(pts_tty_nr(7)), Some(7));
        assert_eq!(pts_index(pts_tty_nr(300)), Some(300));
        assert_eq!(pts_index(0), None); // no controlling tty
        assert_eq!(pts_index(4 << 8 | 1), None); // /dev/tty1
    }

    #[test]
    fn stat_split_survives_parentheses_and_spaces_in_comm() {
        let (comm, rest) = split_stat("42 (tmux: server (x)) S 1 42 42 34816 42").unwrap();
        assert_eq!(comm, "tmux: server (x)");
        assert_eq!(rest[0], "S");
        assert_eq!(rest[4], "34816");
    }

    #[test]
    fn finds_the_process_holding_our_pty_master() {
        let root = fixture("owner");
        add_process(&root, 100, "kitty", 0, &[3, 5]);
        add_process(&root, 200, "foot", 0, &[4]);
        add_process(&root, 300, "zsh", pts_tty_nr(5), &[]);
        std::fs::write(root.join("not-a-pid"), "x").unwrap();

        let owners = pty_master_owners(&root, 5);
        assert_eq!(owners.len(), 1);
        assert_eq!(owners[0].pid, 100);
        assert_eq!(owners[0].comm, "kitty");
        assert_eq!(owners[0].pty_masters, 2);
        assert!(pty_master_owners(&root, 9).is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn controlling_pts_reads_self_stat() {
        let root = fixture("self");
        add_process(&root, 300, "tbench", pts_tty_nr(12), &[]);
        std::fs::rename(root.join("300"), root.join("self")).unwrap();
        assert_eq!(controlling_pts(&root), Some(12));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn reads_memory_cpu_and_threads() {
        let root = fixture("sample");
        add_process(&root, 100, "kitty", 0, &[1]);
        assert_eq!(rss_kb(&root, 100, 4), Some(4800));
        assert_eq!(pss_kb(&root, 100), Some(2100));
        assert_eq!(cpu_ticks_and_threads(&root, 100), Some((300, 7)));
        assert_eq!(rss_kb(&root, 999, 4), None);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn cpu_percentage_is_cpu_time_over_wall_time() {
        assert_eq!(cpu_pct(100, 150, 100, 1.0), 50.0);
        assert_eq!(cpu_pct(100, 400, 100, 2.0), 150.0); // more than one core
        assert_eq!(cpu_pct(100, 90, 100, 1.0), 0.0); // counter never goes back
        assert_eq!(cpu_pct(0, 10, 0, 1.0), 0.0);
    }

    #[test]
    fn recognises_intermediaries_by_comm_prefix() {
        assert_eq!(intermediary_name("tmux: server"), Some("tmux"));
        assert_eq!(intermediary_name("sshd-session"), Some("sshd"));
        assert_eq!(intermediary_name("script"), Some("script"));
        assert_eq!(intermediary_name("kitty"), None);
    }
}
