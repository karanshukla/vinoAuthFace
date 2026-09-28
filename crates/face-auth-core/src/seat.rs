//! Only authenticate the user sitting at the camera.
//!
//! The camera is on seat0. With fast user switching or a background session,
//! a `sudo` for account A can run while account B is in front of it. Decided
//! from logind's state files under `/run/systemd` rather than D-Bus: plain
//! files, readable from `pam_exec` as any user.

use std::path::Path;

/// May `target_uid` authenticate by face right now?
///
/// | Active seat0 session | Allowed |
/// |---|---|
/// | Greeter (login screen) | Any target |
/// | Belongs to user U | Target must be U |
/// | None, or unreadable | Nobody |
///
/// Without `seats/` there's no logind to ask, so the check is skipped rather
/// than locking non-systemd setups out of face-auth entirely.
pub fn check(systemd_run: &Path, target_uid: u32) -> Result<(), String> {
    let seats = systemd_run.join("seats");
    if !seats.is_dir() {
        tracing::debug!("{} missing; skipping the seat check", seats.display());
        return Ok(());
    }
    let seat = read_state(&seats.join("seat0"))?;
    let session_id = field(&seat, "ACTIVE").ok_or("no active session on seat0")?;
    // A path component from a file, so keep it to what logind generates.
    if !session_id.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(format!("unexpected session id {session_id:?}"));
    }
    let session = read_state(&systemd_run.join("sessions").join(session_id))?;
    if field(&session, "CLASS") == Some("greeter") {
        return Ok(());
    }
    let active_uid: u32 = field(&seat, "ACTIVE_UID")
        .and_then(|uid| uid.parse().ok())
        .ok_or("seat0 has no ACTIVE_UID")?;
    if active_uid == target_uid {
        Ok(())
    } else {
        Err(format!("seat0 belongs to user ID {active_uid}, not {target_uid}"))
    }
}

fn read_state(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))
}

/// Value of `KEY=value` in a logind state file.
fn field<'a>(contents: &'a str, key: &str) -> Option<&'a str> {
    contents.lines().find_map(|line| {
        line.strip_prefix(key)
            .and_then(|rest| rest.strip_prefix('='))
            .filter(|value| !value.is_empty())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fake_run(name: &str, seat0: &str, sessions: &[(&str, &str)]) -> PathBuf {
        let root = std::env::temp_dir().join(format!("face-auth-seat-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("seats")).unwrap();
        std::fs::create_dir_all(root.join("sessions")).unwrap();
        std::fs::write(root.join("seats/seat0"), seat0).unwrap();
        for (id, contents) in sessions {
            std::fs::write(root.join("sessions").join(id), contents).unwrap();
        }
        root
    }

    #[test]
    fn only_the_seat_owner_passes() {
        let root = fake_run(
            "owner",
            "IS_SEAT0=1\nACTIVE=2\nACTIVE_UID=1000\n",
            &[("2", "UID=1000\nCLASS=user\n")],
        );
        assert!(check(&root, 1000).is_ok());
        assert!(check(&root, 1001).is_err(), "another account must not match the seat owner's face");
        assert!(check(&root, 0).is_err());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn greeter_allows_any_target() {
        let root = fake_run(
            "greeter",
            "ACTIVE=c1\nACTIVE_UID=42\n",
            &[("c1", "UID=42\nCLASS=greeter\n")],
        );
        assert!(check(&root, 1000).is_ok());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn no_active_or_unreadable_session_denies() {
        let root = fake_run("none", "IS_SEAT0=1\n", &[]);
        assert!(check(&root, 1000).is_err());
        std::fs::write(root.join("seats/seat0"), "ACTIVE=7\nACTIVE_UID=1000\n").unwrap();
        assert!(check(&root, 1000).is_err(), "missing session file must fail closed");
        std::fs::write(root.join("seats/seat0"), "ACTIVE=../x\nACTIVE_UID=1000\n").unwrap();
        assert!(check(&root, 1000).is_err());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn no_logind_skips_the_check() {
        let root = std::env::temp_dir().join(format!("face-auth-seat-nologind-{}", std::process::id()));
        assert!(check(&root, 1000).is_ok());
    }

    #[test]
    fn field_matches_whole_keys_only() {
        let contents = "ACTIVE_UID=1000\nACTIVE=3\nEMPTY=\n";
        assert_eq!(field(contents, "ACTIVE"), Some("3"));
        assert_eq!(field(contents, "ACTIVE_UID"), Some("1000"));
        assert_eq!(field(contents, "EMPTY"), None);
        assert_eq!(field(contents, "CLASS"), None);
    }
}
