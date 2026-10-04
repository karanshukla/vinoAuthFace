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
pub const LIVENESS_MODE: &str = "/usr/local/share/face-auth/liveness-mode.sh";
pub const SAFE_PATH: &str = "/usr/sbin:/usr/bin:/sbin:/bin";

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
    /// `liveness-mode.sh`: the motion liveness preset. One verb per preset,
    /// for the same reason.
    LivenessOff,
    LivenessStandard,
    LivenessStrict,
}

impl Verb {
    pub fn arg(self) -> &'static str {
        match self {
            Verb::Enrol => "enrol",
            Verb::Retrain => "retrain",
            Verb::Uninstall => "uninstall",
            Verb::Upgrade => "upgrade",
            Verb::LoginOff => "login-off",
            Verb::LoginBoth => "login-both",
            Verb::LoginFace => "login-face",
            Verb::LivenessOff => "liveness-off",
            Verb::LivenessStandard => "liveness-standard",
            Verb::LivenessStrict => "liveness-strict",
        }
    }

    const ALL: [Verb; 10] = [
        Verb::Enrol,
        Verb::Retrain,
        Verb::Uninstall,
        Verb::Upgrade,
        Verb::LoginOff,
        Verb::LoginBoth,
        Verb::LoginFace,
        Verb::LivenessOff,
        Verb::LivenessStandard,
        Verb::LivenessStrict,
    ];

    fn parse(arg: &str) -> Option<Verb> {
        Verb::ALL.into_iter().find(|v| v.arg() == arg)
    }
}

/// The verb and target user ID for one helper run. Exactly one argument, a
/// known verb; anything else (a `--user`, a path, a second verb) is refused
/// rather than ignored.
pub fn parse_request(args: &[String], pkexec_uid: Option<&str>) -> Result<(Verb, u32), String> {
    let [arg] = args else {
        let verbs: Vec<_> = Verb::ALL.iter().map(|v| v.arg()).collect();
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
        Verb::LivenessOff => ("/bin/bash", vec![LIVENESS_MODE.into(), "off".into()]),
        Verb::LivenessStandard => ("/bin/bash", vec![LIVENESS_MODE.into(), "standard".into()]),
        Verb::LivenessStrict => ("/bin/bash", vec![LIVENESS_MODE.into(), "strict".into()]),
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
        assert_eq!(parse_request(&args(&["liveness-strict"]), Some("1000")), Ok((Verb::LivenessStrict, 1000)));
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
        assert_eq!(command(Verb::LivenessOff, "alice").1, [LIVENESS_MODE, "off"]);
        assert_eq!(command(Verb::LivenessStandard, "alice").1, [LIVENESS_MODE, "standard"]);
        assert_eq!(command(Verb::LivenessStrict, "alice").1, [LIVENESS_MODE, "strict"]);
    }
}
