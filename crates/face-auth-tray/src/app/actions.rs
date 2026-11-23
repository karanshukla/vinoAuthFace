//! What a menu action does: the helper it runs through pkexec, and the notice that follows.

use super::notifier::Notifier;
use super::tray::{Action, After, Tray};
use face_auth_core::installed::{last_line, pkexec_cancelled};
use face_auth_tray::helper::{Verb, FACE_AUTH, HELPER, SAFE_PATH};
use face_auth_tray::progress;
use face_auth_tray::status::LoginMode;
use ksni::blocking::Handle;
use std::io::{BufRead, BufReader, Read};
use std::process::{Command, Stdio};

/// How one `pkexec vinoauthface-helper` run ended.
#[derive(Debug, PartialEq, Eq)]
enum HelperOutcome {
    /// The password prompt was dismissed or refused: nothing ran, nothing to report.
    Cancelled,
    Succeeded,
    /// The last line the helper wrote to stderr.
    Failed(String),
}

impl HelperOutcome {
    fn from_exit(code: Option<i32>, stderr: &str) -> HelperOutcome {
        if pkexec_cancelled(code) {
            HelperOutcome::Cancelled
        } else if code == Some(0) {
            HelperOutcome::Succeeded
        } else {
            HelperOutcome::Failed(last_line(stderr))
        }
    }
}

/// Run `pkexec vinoauthface-helper <verb>`, reading vinoauthface enroll's progress off
/// its stdout.
fn run_helper(verb: Verb, mut on_line: impl FnMut(&str)) -> std::io::Result<HelperOutcome> {
    let mut child = Command::new("pkexec")
        .args([HELPER, verb.arg().as_str()])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stderr = child.stderr.take().map(|mut e| {
        std::thread::spawn(move || {
            let mut text = String::new();
            let _ = e.read_to_string(&mut text);
            text
        })
    });
    if let Some(stdout) = child.stdout.take() {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            on_line(&line);
        }
    }
    let status = child.wait()?;
    let stderr = stderr.and_then(|t| t.join().ok()).unwrap_or_default();
    Ok(HelperOutcome::from_exit(status.code(), &stderr))
}

/// A desktop notification's two lines.
#[derive(Debug, PartialEq, Eq)]
struct Notice {
    summary: String,
    body: String,
}

fn notice(summary: impl Into<String>, body: impl Into<String>) -> Notice {
    Notice {
        summary: summary.into(),
        body: body.into(),
    }
}

/// The helper verb an action runs through pkexec. A test scan needs none: it
/// runs `vinoauthface-auth` as the user.
fn helper_verb(action: Action) -> Option<Verb> {
    match action {
        Action::Test => None,
        Action::Enrol => Some(Verb::Enrol),
        Action::Retrain => Some(Verb::Retrain),
        Action::Upgrade => Some(Verb::Upgrade),
        Action::Login(mode) => Some(mode.verb()),
        Action::Set(setting, choice) => Some(Verb::Set(setting, choice)),
    }
}

/// The notification and tooltip title while a face is enrolled or retrained.
fn enrolment_title(action: Action) -> &'static str {
    if action == Action::Enrol {
        "Enrolling your face"
    } else {
        "Retraining your face"
    }
}

/// What the tooltip says from the click until the action reports something.
fn starting_text(action: Action) -> String {
    match action {
        Action::Test => "Testing: look at the camera".into(),
        Action::Enrol | Action::Retrain => {
            format!("{}: waiting for authentication", enrolment_title(action))
        }
        Action::Upgrade => "Upgrading: waiting for authentication".into(),
        Action::Login(_) => "Login screen: waiting for authentication".into(),
        Action::Set(setting, _) => format!("{}: waiting for authentication", setting.title),
    }
}

/// What a helper run that worked is followed by. `finished` is the last
/// summary line enrolment printed, if it printed one.
fn success_notice(action: Action, finished: Option<String>) -> Notice {
    match action {
        Action::Test => notice("Face recognised", "Face unlock matched your face."),
        Action::Enrol | Action::Retrain => {
            let body = match finished {
                Some(summary) => format!("{summary}. Use Test scan to verify."),
                None => "Enrolment complete. Use Test scan to verify.".into(),
            };
            notice("Face enrolled", body)
        }
        Action::Upgrade => notice(
            "vinoAuthFace upgraded",
            "Restarting tray with the new version.",
        ),
        Action::Login(mode) => {
            let body = match mode {
                LoginMode::Off => "Password only at the login screen.",
                LoginMode::Both => {
                    "Enter your password, then look at the camera. If the camera fails, switch to a text console (Ctrl+Alt+F3) to log in."
                }
                LoginMode::Face => "Select your account and press Enter to scan. Failed scans fall back to your password.",
            };
            notice(format!("Login screen: {}", mode.label()), body)
        }
        Action::Set(setting, choice) => {
            notice(format!("{}: {}", setting.title, choice.label), choice.note)
        }
    }
}

/// The summary line over the helper's own error.
fn failure_summary(action: Action) -> String {
    match action {
        Action::Test => "Test scan failed".into(),
        Action::Enrol | Action::Retrain => format!("{} failed", enrolment_title(action)),
        Action::Upgrade => "Upgrade failed".into(),
        Action::Login(_) => "Could not change the login screen".into(),
        Action::Set(setting, _) => format!("Could not change {}", setting.title.to_lowercase()),
    }
}

/// What to say once a helper run is over. A dismissed prompt says nothing.
fn notice_after(
    result: &std::io::Result<HelperOutcome>,
    action: Action,
    finished: Option<String>,
) -> Option<Notice> {
    match result {
        Ok(HelperOutcome::Succeeded) => Some(success_notice(action, finished)),
        Ok(HelperOutcome::Cancelled) => None,
        Ok(HelperOutcome::Failed(err)) => Some(notice(failure_summary(action), err.as_str())),
        Err(e) => Some(notice("Cannot run pkexec", e.to_string())),
    }
}

/// What a test scan's answer reads as: `(matched, last stderr line)`.
fn test_scan_notice(result: std::io::Result<(bool, String)>) -> Notice {
    match result {
        Ok((true, _)) => success_notice(Action::Test, None),
        Ok((false, err)) if !err.is_empty() => notice(failure_summary(Action::Test), err),
        Ok((false, _)) => notice(
            "Face not recognised",
            "No match within the scan window. Try retraining in different lighting, or check the camera.",
        ),
        Err(e) => notice("Cannot run vinoauthface", e.to_string()),
    }
}

/// Only an upgrade that worked replaces the running tray.
fn after(action: Action, result: &std::io::Result<HelperOutcome>) -> After {
    if action == Action::Upgrade && matches!(result, Ok(HelperOutcome::Succeeded)) {
        After::Restart
    } else {
        After::Stay
    }
}

/// One live verify as the user, the same way the lock screen runs it. Counts
/// toward the lockout like any other failed scan, so it is not a free oracle.
fn test_scan(user: &str) -> std::io::Result<(bool, String)> {
    let out = Command::new(FACE_AUTH)
        .env_clear()
        .env("PATH", SAFE_PATH)
        .env("PAM_USER", user)
        .env("PAM_SERVICE", "vinoauthface-tray")
        .stdin(Stdio::null())
        .output()?;
    Ok((
        out.status.success(),
        last_line(&String::from_utf8_lossy(&out.stderr)),
    ))
}

pub fn perform(action: Action, handle: &Handle<Tray>, notifier: &Notifier, user: &str) -> After {
    let set_busy = |text: Option<String>| {
        handle.update(|t| t.busy = text);
    };
    set_busy(Some(starting_text(action)));
    let Some(verb) = helper_verb(action) else {
        let id = notifier.send(0, "Test scan", "Look at the camera");
        let said = test_scan_notice(test_scan(user));
        notifier.send(id, &said.summary, &said.body);
        set_busy(None);
        return After::Stay;
    };

    let mut id = 0;
    let mut finished = None;
    let result = run_helper(verb, |line| match action {
        Action::Enrol | Action::Retrain => {
            let Some(event) = progress::parse(line) else {
                return;
            };
            let text = progress::describe(&event);
            if let progress::Event::Finished(summary) = &event {
                finished = Some(summary.clone());
            }
            let title = enrolment_title(action);
            set_busy(Some(format!("{title}: {text}")));
            id = notifier.send(id, title, &text);
        }
        Action::Upgrade if id == 0 => {
            set_busy(Some("Upgrading: running installer…".into()));
            id = notifier.send(
                0,
                "Upgrading vinoAuthFace",
                "Downloading and installing the new release.",
            );
        }
        _ => {}
    });
    if let Some(said) = notice_after(&result, action, finished) {
        notifier.send(id, &said.summary, &said.body);
    }
    match after(action, &result) {
        After::Restart => After::Restart,
        After::Stay => {
            set_busy(None);
            After::Stay
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dismissed_prompt_is_cancelled_not_a_failure() {
        assert_eq!(
            HelperOutcome::from_exit(Some(126), "Error: dismissed\n"),
            HelperOutcome::Cancelled
        );
        assert_eq!(
            HelperOutcome::from_exit(Some(127), ""),
            HelperOutcome::Cancelled
        );
    }

    #[test]
    fn a_helper_failure_carries_its_last_stderr_line() {
        assert_eq!(
            HelperOutcome::from_exit(Some(1), "Enrolling\nError: no face found\n\n"),
            HelperOutcome::Failed("Error: no face found".into())
        );
        assert_eq!(
            HelperOutcome::from_exit(None, ""),
            HelperOutcome::Failed(String::new()),
            "killed by a signal"
        );
        assert_eq!(
            HelperOutcome::from_exit(Some(0), "a warning\n"),
            HelperOutcome::Succeeded
        );
    }

    use face_auth_tray::helper::SETTINGS;

    fn every_action() -> Vec<Action> {
        let mut all = vec![
            Action::Test,
            Action::Enrol,
            Action::Retrain,
            Action::Upgrade,
        ];
        all.extend(LoginMode::ALL.map(Action::Login));
        all.extend(
            SETTINGS
                .iter()
                .flat_map(|s| s.choices.iter().map(move |c| Action::Set(s, c))),
        );
        all
    }

    #[test]
    fn every_action_but_the_test_scan_runs_one_helper_verb() {
        assert_eq!(helper_verb(Action::Test), None);
        assert_eq!(helper_verb(Action::Enrol), Some(Verb::Enrol));
        assert_eq!(helper_verb(Action::Retrain), Some(Verb::Retrain));
        assert_eq!(helper_verb(Action::Upgrade), Some(Verb::Upgrade));
        assert_eq!(
            helper_verb(Action::Login(LoginMode::Face)),
            Some(Verb::LoginFace)
        );
        let security = &SETTINGS[0];
        assert_eq!(
            helper_verb(Action::Set(security, &security.choices[0])),
            Some(Verb::Set(security, &security.choices[0]))
        );
        for action in every_action().into_iter().skip(1) {
            assert!(helper_verb(action).is_some(), "{action:?}");
        }
    }

    #[test]
    fn a_dismissed_prompt_says_nothing_and_every_other_ending_says_something() {
        for action in every_action() {
            assert_eq!(
                notice_after(&Ok(HelperOutcome::Cancelled), action, None),
                None,
                "{action:?}"
            );
            let failed =
                notice_after(&Ok(HelperOutcome::Failed("no camera".into())), action, None).unwrap();
            assert_eq!(
                (failed.summary, failed.body.as_str()),
                (failure_summary(action), "no camera"),
                "{action:?}"
            );
            let worked = notice_after(&Ok(HelperOutcome::Succeeded), action, None).unwrap();
            assert_eq!(worked, success_notice(action, None), "{action:?}");
            let no_pkexec =
                notice_after(&Err(std::io::Error::other("not found")), action, None).unwrap();
            assert_eq!(
                no_pkexec,
                notice("Cannot run pkexec", "not found"),
                "{action:?}"
            );
        }
    }

    #[test]
    fn enrolment_reports_what_the_cli_said_it_saved() {
        let said = success_notice(Action::Enrol, Some("Saved 9 embeddings".into()));
        assert_eq!(
            said,
            notice(
                "Face enrolled",
                "Saved 9 embeddings. Use Test scan to verify."
            )
        );
        assert_eq!(
            success_notice(Action::Retrain, None).body,
            "Enrolment complete. Use Test scan to verify."
        );
        assert_eq!(failure_summary(Action::Enrol), "Enrolling your face failed");
        assert_eq!(
            failure_summary(Action::Retrain),
            "Retraining your face failed"
        );
    }

    #[test]
    fn a_setting_change_is_followed_by_its_choices_note() {
        for setting in SETTINGS {
            for choice in setting.choices {
                let said = success_notice(Action::Set(setting, choice), None);
                assert_eq!(
                    said,
                    notice(format!("{}: {}", setting.title, choice.label), choice.note)
                );
            }
            assert_eq!(
                failure_summary(Action::Set(setting, &setting.choices[0])),
                format!("Could not change {}", setting.title.to_lowercase())
            );
        }
    }

    #[test]
    fn a_test_scan_tells_a_match_from_no_match_from_an_error() {
        assert_eq!(
            test_scan_notice(Ok((true, String::new()))).summary,
            "Face recognised"
        );
        assert_eq!(
            test_scan_notice(Ok((false, String::new()))).summary,
            "Face not recognised"
        );
        assert_eq!(
            test_scan_notice(Ok((false, "Error: locked out".into()))),
            notice("Test scan failed", "Error: locked out")
        );
        assert_eq!(
            test_scan_notice(Err(std::io::Error::other("gone"))),
            notice("Cannot run vinoauthface", "gone")
        );
    }

    #[test]
    fn only_a_successful_upgrade_restarts_the_tray() {
        assert!(matches!(
            after(Action::Upgrade, &Ok(HelperOutcome::Succeeded)),
            After::Restart
        ));
        assert!(matches!(
            after(Action::Upgrade, &Ok(HelperOutcome::Failed(String::new()))),
            After::Stay
        ));
        assert!(matches!(
            after(Action::Upgrade, &Ok(HelperOutcome::Cancelled)),
            After::Stay
        ));
        assert!(matches!(
            after(Action::Enrol, &Ok(HelperOutcome::Succeeded)),
            After::Stay
        ));
    }
}
