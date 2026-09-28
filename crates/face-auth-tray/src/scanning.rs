//! Whether a face scan is running, seen passively in `/proc`.
//!
//! `face-auth` never reports to the tray. The old GNOME indicator had root
//! write a status file into `/run/user/<uid>` and follow a planted symlink;
//! looking for the process instead means nothing root-owned writes anywhere
//! the user controls.

use std::path::Path;

/// Is any process under `proc_root` named `face-auth`?
///
/// `comm` is world-readable, so this sees the root scan behind `sudo` or a
/// polkit prompt. With `/proc` mounted `hidepid=1|2` other users' processes
/// are invisible: that reads as "not scanning", never as an error.
pub fn face_auth_running(proc_root: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(proc_root) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let is_pid = entry.file_name().to_str().is_some_and(|n| n.bytes().all(|b| b.is_ascii_digit()));
        is_pid
            && std::fs::read_to_string(entry.path().join("comm"))
                .is_ok_and(|comm| comm.trim_end_matches('\n') == "face-auth")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fake_proc(name: &str, processes: &[(&str, Option<&str>)]) -> PathBuf {
        let root = std::env::temp_dir().join(format!("face-auth-tray-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (entry, comm) in processes {
            let dir = root.join(entry);
            std::fs::create_dir_all(&dir).unwrap();
            if let Some(comm) = comm {
                std::fs::write(dir.join("comm"), comm).unwrap();
            }
        }
        root
    }

    #[test]
    fn finds_a_running_scan() {
        let root = fake_proc("present", &[("1", Some("systemd\n")), ("4242", Some("face-auth\n"))]);
        assert!(face_auth_running(&root));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn nothing_running_and_no_proc_are_both_idle() {
        let root = fake_proc("absent", &[("1", Some("systemd\n")), ("77", Some("sudo\n"))]);
        assert!(!face_auth_running(&root));
        assert!(!face_auth_running(&root.join("missing")));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn skips_unreadable_entries_and_near_misses() {
        let root = fake_proc(
            "near",
            &[
                // Exited between readdir and read.
                ("10", None),
                // comm is truncated to 15 bytes.
                ("11", Some("face-auth-tray\n")),
                ("12", Some("face-auth-helpe\n")),
                ("13", Some("face-enroll\n")),
                // Not a process directory.
                ("self", Some("face-auth\n")),
            ],
        );
        assert!(!face_auth_running(&root));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_comm_without_its_trailing_newline_still_matches() {
        let root = fake_proc("bare", &[("9", Some("face-auth"))]);
        assert!(face_auth_running(&root));
        std::fs::remove_dir_all(&root).unwrap();
    }
}
