//! Unseals one user's TPM-sealed templates, and nothing else.
//!
//! Under SELinux this runs in its own domain, `face_auth_unseal_t`
//! (selinux/face-auth.te), the only one given the TPM and systemd's host
//! credential key. The greeters enter it by exec'ing this file, so they never
//! hold that key themselves. It takes a numeric uid and one of our credential
//! names, then becomes `systemd-creds decrypt`: blob on stdin, payload on
//! stdout. The name check keeps it from decrypting anyone else's credentials.

use std::os::unix::process::CommandExt;
use std::process::{Command, ExitCode};

/// Absolute, so a caller's PATH never picks the binary.
const SYSTEMD_CREDS: &str = "/usr/bin/systemd-creds";
/// Every name `storage::seal_name` builds starts with this.
const NAME_PREFIX: &str = "vinoauthface.";

fn parse(args: &[String]) -> Option<(u32, &str)> {
    let [uid, name] = args else { return None };
    // u32's parser takes a leading `+`; only the canonical form goes through.
    let uid: u32 = uid.parse().ok().filter(|n: &u32| n.to_string() == *uid)?;
    let valid = name.len() > NAME_PREFIX.len()
        && name.len() <= 256
        && name.starts_with(NAME_PREFIX)
        && !name.contains('/')
        && !name.chars().any(char::is_control);
    valid.then_some((uid, name.as_str()))
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some((uid, name)) = parse(&args) else {
        eprintln!("usage: vinoauthface-unseal UID vinoauthface.NAME < sealed > payload");
        return ExitCode::from(2);
    };
    let err = Command::new(SYSTEMD_CREDS)
        .args(["decrypt", &format!("--uid={uid}"), &format!("--name={name}"), "-", "-"])
        .env_clear()
        .exec();
    eprintln!("vinoauthface-unseal: {SYSTEMD_CREDS}: {err}");
    ExitCode::from(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn takes_a_uid_and_one_of_our_names() {
        let a = args(&["1000", "vinoauthface.alice.w600k_mbf.onnx+det_500m.onnx"]);
        assert_eq!(
            parse(&a),
            Some((1000, "vinoauthface.alice.w600k_mbf.onnx+det_500m.onnx"))
        );
    }

    #[test]
    fn refuses_other_credentials_and_odd_uids() {
        for a in [
            &["1000", "ssh.host-key"][..],
            &["1000", "vinoauthface."],
            &["1000", "vinoauthface.alice/../x"],
            &["1000", "vinoauthface.alice\n"],
            &["+1000", "vinoauthface.alice.t"],
            &["01000", "vinoauthface.alice.t"],
            &["alice", "vinoauthface.alice.t"],
            &["-1", "vinoauthface.alice.t"],
            &["1000"],
            &["1000", "vinoauthface.alice.t", "--with-key=null"],
        ] {
            assert_eq!(parse(&args(a)), None, "{a:?}");
        }
    }
}
