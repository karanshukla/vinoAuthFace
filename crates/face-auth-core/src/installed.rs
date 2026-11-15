//! The installed programs the tray and the settings window run, and how
//! their answers read.

use std::process::{Command, Stdio};

/// The set-group-ID PAM binary, where deploy.sh installs it.
pub const FACE_AUTH: &str = "/usr/local/bin/vinoauthface-auth";
/// Where deploy.sh installs the tray's root helper; polkit pins this path.
pub const HELPER: &str = "/usr/local/libexec/vinoauthface-helper";

/// pkexec's exit codes for a dismissed or refused prompt.
const PKEXEC_CANCELLED: [i32; 2] = [126, 127];

/// Whether the calling user has templates. Asks the set-group-ID
/// `vinoauthface-auth --enrolled`, which can open the store the user cannot.
/// `None` when it is missing or could not answer.
pub fn enrolled() -> Option<bool> {
    let status = Command::new(FACE_AUTH)
        .arg("--enrolled")
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()?;
    enrolled_from_exit_code(status.code())
}

fn enrolled_from_exit_code(code: Option<i32>) -> Option<bool> {
    match code {
        Some(0) => Some(true),
        Some(1) => Some(false),
        _ => None,
    }
}

/// The password prompt was dismissed or refused, so the helper never ran.
pub fn pkexec_cancelled(code: Option<i32>) -> bool {
    code.is_some_and(|c| PKEXEC_CANCELLED.contains(&c))
}

/// The last line of a program's stderr that says something, trimmed.
pub fn last_line(text: &str) -> String {
    text.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_exit_codes_zero_and_one_answer_whether_a_face_is_enrolled() {
        assert_eq!(enrolled_from_exit_code(Some(0)), Some(true));
        assert_eq!(enrolled_from_exit_code(Some(1)), Some(false));
        assert_eq!(enrolled_from_exit_code(Some(2)), None);
        assert_eq!(enrolled_from_exit_code(None), None, "killed by a signal");
    }

    #[test]
    fn a_dismissed_or_refused_prompt_is_cancelled_and_a_helper_failure_is_not() {
        assert!(pkexec_cancelled(Some(126)));
        assert!(pkexec_cancelled(Some(127)));
        assert!(!pkexec_cancelled(Some(125)));
        assert!(!pkexec_cancelled(Some(1)));
        assert!(!pkexec_cancelled(Some(0)));
        assert!(!pkexec_cancelled(None));
    }

    #[test]
    fn last_line_skips_trailing_blank_lines() {
        assert_eq!(last_line("first\n  Error: no camera \n\n  \n"), "Error: no camera");
        assert_eq!(last_line(" \n"), "");
    }
}
