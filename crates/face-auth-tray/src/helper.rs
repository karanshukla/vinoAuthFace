//! What `vinoauthface-helper` may be asked to do, and for whom.
//!
//! polkit's `exec.path` pins the binary pkexec runs, not its arguments, so
//! `pkexec vinoauthface enroll --embeddings-dir …` would be root writing wherever the
//! caller says. The helper is what polkit authorises instead: it takes one
//! fixed verb and no flags, and acts only for the user pkexec says invoked it
//! (`PKEXEC_UID`, which pkexec sets and a caller cannot).

pub const HELPER: &str = "/usr/local/libexec/vinoauthface-helper";
pub const FACE_ENROLL: &str = "/usr/local/bin/vinoauthface";
pub const FACE_AUTH: &str = "/usr/local/bin/vinoauthface-auth";
pub const UNINSTALLER: &str = "/usr/local/share/face-auth/uninstall.sh";
pub const UPGRADER: &str = "/usr/local/bin/vinoauthface-upgrade";
pub const LOGIN_MODE: &str = "/usr/local/share/face-auth/login-mode.sh";
pub const SETTING_MODE: &str = "/usr/local/share/face-auth/setting-mode.sh";
pub const SAFE_PATH: &str = "/usr/sbin:/usr/bin:/sbin:/bin";

/// One choice in a setting's menu. `id` is `setting-mode.sh`'s word for it.
#[derive(Debug, PartialEq, Eq)]
pub struct Choice {
    pub id: &'static str,
    pub label: &'static str,
    /// Shown in the notification after it is chosen; for a choice that
    /// weakens something.
    pub note: &'static str,
}

/// A setting in `/etc/face-auth.toml` the tray's Settings menu can change.
/// Each choice is one polkit action. Only what is listed here (and in
/// `setting-mode.sh`) can be set: the camera, models, template directory,
/// lockout, camera pin, backend and the rest of the guards are system policy
/// and stay out of the menu on purpose.
#[derive(Debug, PartialEq, Eq)]
pub struct Setting {
    pub key: &'static str,
    pub title: &'static str,
    pub choices: &'static [Choice],
}

const fn choice(id: &'static str, label: &'static str, note: &'static str) -> Choice {
    Choice { id, label, note }
}

pub const SETTINGS: &[Setting] = &[
    Setting {
        key: "security",
        title: "Security",
        choices: &[
            choice(
                "convenient",
                "Convenient",
                "Matches more easily, and so does a lookalike. The lock screen scans at once, so someone at the camera when you lock it is let back in. Liveness stays on.",
            ),
            choice("balanced", "Balanced", "The default."),
            choice(
                "strict",
                "Strict",
                "Rejects a photo moved by hand, matches more strictly, wants your face fairly close and waits 5 seconds before scanning a lock screen. It may fail if you sit very still or far back.",
            ),
        ],
    },
    Setting {
        key: "scan",
        title: "Scan time",
        choices: &[
            choice("3s", "3 seconds", ""),
            choice("5s", "5 seconds", "The default."),
            choice("8s", "8 seconds", ""),
            choice("12s", "12 seconds", ""),
        ],
    },
    Setting {
        key: "confirm",
        title: "Confirm sudo with Enter",
        choices: &[
            choice("off", "Off", ""),
            choice("on", "On", "After a face match for sudo, su or polkit in a terminal, press Enter to let it through."),
        ],
    },
    Setting {
        key: "updates",
        title: "Check for updates",
        choices: &[
            choice("on", "On", "One anonymous request a day to GitHub's releases API."),
            choice("off", "Off", "No update checks. Restart the tray for it to stop asking."),
        ],
    },
];

pub fn setting(key: &str) -> Option<&'static Setting> {
    SETTINGS.iter().find(|s| s.key == key)
}

/// One per polkit action in `data/io.github.karanshukla.vinoauthface.policy`,
/// matched there by `exec.argv1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    /// First-time enrolment. Replaces any existing templates.
    Enrol,
    /// `vinoauthface improve`: append frames, keep what is there.
    Retrain,
    Uninstall,
    /// `vinoauthface-upgrade` to the newest release.
    Upgrade,
    /// `login-mode.sh`: face unlock at the Plasma login screen. One verb per
    /// mode, since polkit pins the argument per action.
    LoginOff,
    LoginBoth,
    LoginFace,
    /// `setting-mode.sh set KEY CHOICE`, one verb per choice of each entry in
    /// `SETTINGS`, for the same reason: polkit pins the argument per action.
    Set(&'static Setting, &'static Choice),
}

impl Verb {
    pub fn arg(self) -> String {
        match self {
            Verb::Enrol => "enrol".into(),
            Verb::Retrain => "retrain".into(),
            Verb::Uninstall => "uninstall".into(),
            Verb::Upgrade => "upgrade".into(),
            Verb::LoginOff => "login-off".into(),
            Verb::LoginBoth => "login-both".into(),
            Verb::LoginFace => "login-face".into(),
            Verb::Set(setting, choice) => format!("set-{}-{}", setting.key, choice.id),
        }
    }

    const FIXED: [Verb; 7] =
        [Verb::Enrol, Verb::Retrain, Verb::Uninstall, Verb::Upgrade, Verb::LoginOff, Verb::LoginBoth, Verb::LoginFace];

    /// Every verb there is: the fixed ones, then one per setting choice.
    pub fn all() -> Vec<Verb> {
        let sets = SETTINGS.iter().flat_map(|s| s.choices.iter().map(move |c| Verb::Set(s, c)));
        Verb::FIXED.into_iter().chain(sets).collect()
    }

    fn parse(arg: &str) -> Option<Verb> {
        Verb::all().into_iter().find(|v| v.arg() == arg)
    }
}

/// The verb and target user ID for one helper run. Exactly one argument, a
/// known verb; anything else (a `--user`, a path, a second verb) is refused
/// rather than ignored.
pub fn parse_request(args: &[String], pkexec_uid: Option<&str>) -> Result<(Verb, u32), String> {
    let [arg] = args else {
        let verbs: Vec<_> = Verb::all().iter().map(|v| v.arg()).collect();
        return Err(format!("expected exactly one of {}; got {} arguments", verbs.join(", "), args.len()));
    };
    let verb = Verb::parse(arg).ok_or_else(|| format!("unknown action {arg:?}"))?;
    let uid = pkexec_uid
        .ok_or("PKEXEC_UID is not set: run this through pkexec, not directly")?
        .parse::<u32>()
        .map_err(|_| "PKEXEC_UID is not a user ID")?;
    Ok((verb, uid))
}

/// The program and arguments a verb runs for `user`, a name already resolved
/// from the pkexec caller's user ID.
pub fn command(verb: Verb, user: &str) -> (&'static str, Vec<String>) {
    match verb {
        Verb::Enrol => (FACE_ENROLL, vec!["enroll".into(), "--user".into(), user.into()]),
        Verb::Retrain => (FACE_ENROLL, vec!["improve".into(), "--user".into(), user.into()]),
        // Run as root with no SUDO_USER, so its per-user cleanup looks in
        // root's home, not in one the caller controls.
        Verb::Uninstall => ("/bin/bash", vec![UNINSTALLER.into()]),
        // The same: no SUDO_USER, so the release is downloaded and unpacked
        // under root's home, never somewhere the caller could swap it between
        // the checksum and deploy.sh running it.
        Verb::Upgrade => ("/bin/bash", vec![UPGRADER.into()]),
        Verb::LoginOff => ("/bin/bash", vec![LOGIN_MODE.into(), "off".into()]),
        Verb::LoginBoth => ("/bin/bash", vec![LOGIN_MODE.into(), "both".into()]),
        Verb::LoginFace => ("/bin/bash", vec![LOGIN_MODE.into(), "face".into()]),
        Verb::Set(setting, choice) => {
            ("/bin/bash", vec![SETTING_MODE.into(), "set".into(), setting.key.into(), choice.id.into()])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn accepts_each_verb_for_the_pkexec_caller() {
        assert_eq!(parse_request(&args(&["enrol"]), Some("1000")), Ok((Verb::Enrol, 1000)));
        assert_eq!(parse_request(&args(&["retrain"]), Some("1000")), Ok((Verb::Retrain, 1000)));
        assert_eq!(parse_request(&args(&["uninstall"]), Some("0")), Ok((Verb::Uninstall, 0)));
        assert_eq!(parse_request(&args(&["upgrade"]), Some("1000")), Ok((Verb::Upgrade, 1000)));
        assert_eq!(parse_request(&args(&["login-both"]), Some("1000")), Ok((Verb::LoginBoth, 1000)));
    }

    #[test]
    fn rejects_unknown_verbs_and_flags() {
        for list in [&["enroll"][..], &["ENROL"], &["--improve"], &[""], &["enrol ", ], &["login"], &["login-status"]] {
            assert!(parse_request(&args(list), Some("1000")).is_err(), "{list:?}");
        }
    }

    #[test]
    fn a_target_user_cannot_be_named() {
        // The only identity is PKEXEC_UID; a second argument naming someone
        // else is refused, not ignored.
        for list in [&["enrol", "bob"][..], &["enrol", "--user", "bob"], &["retrain", "0"], &["upgrade", "v1"], &["login-face", "off"]] {
            assert!(parse_request(&args(list), Some("1000")).is_err(), "{list:?}");
        }
        assert!(parse_request(&args(&[]), Some("1000")).is_err());
    }

    #[test]
    fn requires_a_numeric_pkexec_uid() {
        assert!(parse_request(&args(&["enrol"]), None).is_err());
        for bad in ["", "alice", "-1", "1000 ", "4294967296"] {
            assert!(parse_request(&args(&["enrol"]), Some(bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn commands_never_carry_a_path_override() {
        for verb in [Verb::Enrol, Verb::Retrain] {
            let (program, argv) = command(verb, "alice");
            assert_eq!(program, FACE_ENROLL);
            assert_eq!(&argv[1..3], ["--user", "alice"]);
            assert!(argv.iter().all(|a| !a.contains("dir") && !a.contains("model") && !a.contains("device")));
        }
        assert_eq!(command(Verb::Enrol, "alice").1[0], "enroll");
        assert_eq!(command(Verb::Retrain, "alice").1[0], "improve");
        assert_eq!(command(Verb::Uninstall, "alice").1, [UNINSTALLER]);
        assert_eq!(command(Verb::Upgrade, "alice").1, [UPGRADER]);
        assert_eq!(command(Verb::LoginOff, "alice").1, [LOGIN_MODE, "off"]);
        assert_eq!(command(Verb::LoginBoth, "alice").1, [LOGIN_MODE, "both"]);
        assert_eq!(command(Verb::LoginFace, "alice").1, [LOGIN_MODE, "face"]);
        let security = setting("security").unwrap();
        assert_eq!(command(Verb::Set(security, &security.choices[2]), "alice").1, [SETTING_MODE, "set", "security", "strict"]);
    }

    #[test]
    fn parses_a_setting_verb_and_nothing_near_it() {
        let (verb, uid) = parse_request(&args(&["set-security-strict"]), Some("1000")).unwrap();
        assert_eq!(uid, 1000);
        assert!(matches!(verb, Verb::Set(s, c) if s.key == "security" && c.id == "strict"));
        // The preset's parts are not settable one by one.
        for bad in ["set-security", "set-security-", "set-security-loose", "set-lockout-off", "set-security-strict ", "set", "set-liveness-off", "set-match-relaxed", "set-delay-off"] {
            assert!(parse_request(&args(&[bad]), Some("1000")).is_err(), "{bad:?}");
        }
        assert!(parse_request(&args(&["set-scan-5s", "x"]), Some("1000")).is_err());
    }

    /// Every verb has its polkit action, pinned to that verb's argument, and
    /// the policy has no action that no verb uses.
    #[test]
    fn the_policy_has_one_action_per_verb() {
        let policy = include_str!("../data/io.github.karanshukla.vinoauthface.policy");
        let mut ids: Vec<_> = policy
            .lines()
            .filter_map(|l| l.trim().strip_prefix("<annotate key=\"org.freedesktop.policykit.exec.argv1\">"))
            .map(|l| l.trim_end_matches("</annotate>").to_string())
            .collect();
        let mut verbs: Vec<_> = Verb::all().iter().map(|v| v.arg()).collect();
        ids.sort();
        verbs.sort();
        assert_eq!(ids, verbs);
        for verb in Verb::all() {
            let action = format!("<action id=\"io.github.karanshukla.vinoauthface.{}\">", verb.arg());
            assert!(policy.contains(&action), "{action}");
        }
    }

    /// `setting-mode.sh` accepts exactly the menu's choices and reads each
    /// back as the choice written.
    #[test]
    fn the_script_agrees_with_the_menu() {
        let dir = std::env::temp_dir().join(format!("vinoauthface-settings-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let conf = dir.join("face-auth.toml");
        let script = concat!(env!("CARGO_MANIFEST_DIR"), "/../../setting-mode.sh");
        let run = |args: &[&str]| {
            std::process::Command::new("bash")
                .arg(script)
                .args(args)
                .env("FACE_AUTH_SETTINGS_CONF", &conf)
                .output()
                .unwrap()
        };
        for setting in SETTINGS {
            for choice in setting.choices {
                assert!(run(&["set", setting.key, choice.id]).status.success(), "{} {}", setting.key, choice.id);
                let status = String::from_utf8(run(&["status"]).stdout).unwrap();
                assert!(status.lines().any(|l| l == format!("{} {}", setting.key, choice.id)), "{status}");
            }
            assert!(!run(&["set", setting.key, "bogus"]).status.success());
        }
        assert!(!run(&["set", "lockout", "off"]).status.success());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
