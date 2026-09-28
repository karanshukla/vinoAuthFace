//! Situations where a face scan can't succeed, checked before loading models
//! so PAM falls through to the password prompt straight away.

use std::path::Path;
use std::time::Duration;

/// How far up the process tree to look for an SSH server.
const MAX_ANCESTRY_DEPTH: usize = 16;

#[derive(Debug, PartialEq, Eq)]
pub enum SkipReason {
    SshSession,
    LidClosed,
}

impl std::fmt::Display for SkipReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            SkipReason::SshSession => "running under an SSH session",
            SkipReason::LidClosed => "laptop lid is closed",
        })
    }
}

/// What is asking for authentication, judged from `PAM_SERVICE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    /// Deliberate escalation by someone already at the machine.
    Elevation,
    /// Greeters and login managers.
    Login,
    /// A screen locker, or anything unrecognised.
    ScreenLock,
}

/// Unknown services count as a screen lock: an unrecognised locker gets the
/// start delay rather than silently skipping it.
pub fn classify_pam_service(service: Option<&str>) -> Surface {
    match service {
        Some("sudo" | "sudo-i" | "su" | "su-l" | "doas" | "run0" | "polkit-1" | "pkexec") => {
            Surface::Elevation
        }
        Some(
            "login" | "sddm" | "lightdm" | "greetd" | "gdm-password" | "gdm-launch-environment",
        ) => Surface::Login,
        _ => Surface::ScreenLock,
    }
}

/// How long ago a process started, from its `/proc/<pid>/stat` and
/// `/proc/uptime`. `starttime` is field 22, in clock ticks since boot. A retry
/// on the same locker reads the same start time, so a delay measured this way
/// is paid once per lock rather than once per attempt.
pub fn process_age(stat: &str, uptime: &str, clock_ticks_per_sec: u64) -> Option<Duration> {
    if clock_ticks_per_sec == 0 {
        return None;
    }
    let (_, rest) = stat.rsplit_once(')')?;
    let start_ticks: u64 = rest.split_whitespace().nth(19)?.parse().ok()?;
    let uptime_secs: f64 = uptime.split_whitespace().next()?.parse().ok()?;
    let start_secs = start_ticks as f64 / clock_ticks_per_sec as f64;
    (uptime_secs.is_finite() && uptime_secs >= start_secs)
        .then(|| Duration::from_secs_f64(uptime_secs - start_secs))
}

/// Is any ancestor of `pid` an SSH server process?
///
/// Checked by ancestry, not `SSH_CONNECTION`: `pam_exec` builds our
/// environment from the PAM handle, and sudo scrubs it anyway. `comm` and
/// `stat` are world-readable, so this also works from the lock screen, where
/// face-auth runs as the user. A process that can't be read (it exited
/// mid-walk) counts as SSH: the scan is skipped, and the password still works.
pub fn under_ssh(proc_root: &Path, pid: u32) -> bool {
    let mut pid = pid;
    for _ in 0..MAX_ANCESTRY_DEPTH {
        if pid <= 1 {
            return false;
        }
        let dir = proc_root.join(pid.to_string());
        let Ok(comm) = std::fs::read_to_string(dir.join("comm")) else {
            return true;
        };
        if matches!(comm.trim_end(), "sshd" | "sshd-session") {
            return true;
        }
        let Some(ppid) = std::fs::read_to_string(dir.join("stat"))
            .ok()
            .and_then(|stat| parent_pid(&stat))
        else {
            return true;
        };
        pid = ppid;
    }
    false
}

/// Field 4 of `/proc/<pid>/stat`. Parsed after the *last* `)`, since the
/// command name in field 2 may itself contain spaces and parentheses.
fn parent_pid(stat: &str) -> Option<u32> {
    let (_, rest) = stat.rsplit_once(')')?;
    rest.split_whitespace().nth(1)?.parse().ok()
}

/// Is any ACPI lid switch under `lid_root` (`/proc/acpi/button/lid`) closed?
/// No lid, or an unreadable one, means open: desktops have none.
pub fn lid_closed(lid_root: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(lid_root) else {
        return false;
    };
    entries.flatten().any(|entry| {
        std::fs::read_to_string(entry.path().join("state"))
            .is_ok_and(|state| lid_state_is_closed(&state))
    })
}

/// Parse `state:      closed`.
fn lid_state_is_closed(state: &str) -> bool {
    state
        .strip_prefix("state:")
        .is_some_and(|value| value.trim() == "closed")
}

/// Does the firmware report the camera's USB port as user-pluggable?
///
/// Walks up from the camera's sysfs bus path to the USB device and reads its
/// `removable` attribute: `fixed` for a built-in laptop camera, `removable`
/// for an external one. Only a definite `removable` counts; `unknown` is
/// common on built-in ports whose firmware doesn't say.
pub fn camera_is_external(bus_path: &Path) -> bool {
    bus_path
        .ancestors()
        .find_map(|dir| std::fs::read_to_string(dir.join("removable")).ok())
        .is_some_and(|value| value.trim() == "removable")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("face-auth-env-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn fake_process(root: &Path, pid: u32, comm: &str, ppid: u32) {
        let dir = root.join(pid.to_string());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("comm"), format!("{comm}\n")).unwrap();
        std::fs::write(dir.join("stat"), format!("{pid} ({comm}) S {ppid} 1 1 0")).unwrap();
    }

    #[test]
    fn pam_services_are_classified() {
        assert_eq!(classify_pam_service(Some("sudo")), Surface::Elevation);
        assert_eq!(classify_pam_service(Some("polkit-1")), Surface::Elevation);
        assert_eq!(classify_pam_service(Some("gdm-password")), Surface::Login);
        assert_eq!(classify_pam_service(Some("swaylock")), Surface::ScreenLock);
        assert_eq!(classify_pam_service(Some("kde-fingerprint")), Surface::ScreenLock);
        assert_eq!(classify_pam_service(None), Surface::ScreenLock);
    }

    /// 21 fields precede starttime after the command name; `rest` field 3 is 'S'.
    fn stat_with_start(comm: &str, start_ticks: u64) -> String {
        format!("42 ({comm}) S 1 42 42 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 {start_ticks} 0 0")
    }

    #[test]
    fn process_age_is_uptime_minus_start() {
        let age = process_age(&stat_with_start("swaylock", 10_000), "105.50 400.0", 100).unwrap();
        assert_eq!(age, Duration::from_millis(5_500));
    }

    #[test]
    fn process_age_survives_parentheses_in_comm() {
        let age = process_age(&stat_with_start("a) (b", 500), "10.00 1.0", 100).unwrap();
        assert_eq!(age, Duration::from_secs(5));
    }

    #[test]
    fn process_age_rejects_garbage() {
        assert!(process_age("garbage", "10.0", 100).is_none());
        assert!(process_age(&stat_with_start("x", 100), "nope", 100).is_none());
        assert!(process_age(&stat_with_start("x", 100), "10.0", 0).is_none());
        // Started "after" now: clock mismatch, don't guess.
        assert!(process_age(&stat_with_start("x", 5_000), "10.0", 100).is_none());
    }

    #[test]
    fn parent_pid_survives_parentheses_in_comm() {
        assert_eq!(parent_pid("42 (a) b) (c) S 7 42 42 0"), Some(7));
        assert_eq!(parent_pid("42 (sudo) S 1000 42"), Some(1000));
        assert_eq!(parent_pid("garbage"), None);
    }

    #[test]
    fn ssh_found_anywhere_up_the_tree() {
        let root = temp_dir("ssh");
        fake_process(&root, 900, "sshd-session", 1);
        fake_process(&root, 901, "bash", 900);
        fake_process(&root, 902, "sudo", 901);
        fake_process(&root, 903, "face-auth", 902);
        assert!(under_ssh(&root, 903));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn local_session_is_not_ssh() {
        let root = temp_dir("local");
        fake_process(&root, 800, "konsole", 1);
        fake_process(&root, 801, "bash", 800);
        fake_process(&root, 802, "sudo", 801);
        assert!(!under_ssh(&root, 802));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn unreadable_ancestor_counts_as_ssh() {
        let root = temp_dir("gone");
        fake_process(&root, 701, "sudo", 700);
        assert!(under_ssh(&root, 701));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn lid_state_parsing() {
        assert!(lid_state_is_closed("state:      closed\n"));
        assert!(!lid_state_is_closed("state:      open\n"));
        assert!(!lid_state_is_closed(""));
        assert!(!lid_state_is_closed("closed"));
    }

    #[test]
    fn any_closed_lid_counts_and_no_lid_is_open() {
        let root = temp_dir("lid");
        assert!(!lid_closed(&root.join("missing")));
        std::fs::create_dir_all(root.join("LID0")).unwrap();
        std::fs::write(root.join("LID0/state"), "state:      open\n").unwrap();
        assert!(!lid_closed(&root));
        std::fs::create_dir_all(root.join("LID1")).unwrap();
        std::fs::write(root.join("LID1/state"), "state:      closed\n").unwrap();
        assert!(lid_closed(&root));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn camera_port_removability() {
        let root = temp_dir("usb");
        let port = root.join("usb3/3-6");
        let interface = port.join("3-6:1.0");
        std::fs::create_dir_all(&interface).unwrap();
        assert!(!camera_is_external(&interface), "no attribute: treat as built in");
        std::fs::write(port.join("removable"), "fixed\n").unwrap();
        assert!(!camera_is_external(&interface));
        std::fs::write(port.join("removable"), "unknown\n").unwrap();
        assert!(!camera_is_external(&interface));
        std::fs::write(port.join("removable"), "removable\n").unwrap();
        assert!(camera_is_external(&interface));
        std::fs::remove_dir_all(&root).unwrap();
    }
}
