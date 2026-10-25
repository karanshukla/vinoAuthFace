//! TPM sealing through `systemd-creds`, so there is no TPM code of our own.
//!
//! Credentials are user-scoped (`--uid`, systemd 256+). A system-scoped one
//! can only be decrypted by a non-root caller through polkit, which a lock
//! screen running as the user cannot answer. A user-scoped one is decrypted
//! for that user without polkit, and root (sudo, the polkit helper) can
//! decrypt any user's.

use crate::error::FaceAuthError;
use anyhow::{Context, Result};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// Absolute, so a caller's PATH never picks the binary.
const SYSTEMD_CREDS: &str = "/usr/bin/systemd-creds";
const TPM_DEVICE: &str = "/dev/tpmrm0";
/// Where deploy.sh installs the unseal helper (crates/face-auth/src/unseal.rs).
/// SELinux gives it the only domain allowed the TPM and the host key, so the
/// greeters unseal through it. Older installs have no helper and run
/// `systemd-creds` directly, which works everywhere but a confined greeter.
const UNSEAL_HELPER: &str = "/usr/local/libexec/vinoauthface-unseal";

pub fn available() -> bool {
    Path::new(SYSTEMD_CREDS).exists() && Path::new(TPM_DEVICE).exists()
}

/// `host+tpm2` because scoped mode refuses `tpm2` alone. The key still needs
/// this machine's TPM. No PCR policy, signed or not, so firmware and image
/// updates keep the templates readable.
pub fn seal(user: &str, name: &str, payload: &[u8]) -> Result<Vec<u8>> {
    seal_with(SYSTEMD_CREDS, user, name, payload)
}

fn seal_with(creds: &str, user: &str, name: &str, payload: &[u8]) -> Result<Vec<u8>> {
    let args = encrypt_args(user, name);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    run(creds, &args, payload).context("systemd-creds encrypt")
}

fn encrypt_args(user: &str, name: &str) -> Vec<String> {
    vec![
        "encrypt".into(),
        format!("--uid={user}"),
        format!("--name={name}"),
        "--with-key=host+tpm2".into(),
        "--tpm2-pcrs=".into(),
        "--tpm2-public-key-pcrs=".into(),
        "-".into(),
        "-".into(),
    ]
}

/// Any failure is `SealUnavailable`: the blob is unreadable here, whatever
/// the reason, and the caller must not fall back to anything.
pub fn unseal(user: &str, name: &str, blob: &[u8]) -> Result<Vec<u8>> {
    unseal_with(SYSTEMD_CREDS, Path::new(UNSEAL_HELPER), user, name, blob)
}

fn unseal_with(creds: &str, helper: &Path, user: &str, name: &str, blob: &[u8]) -> Result<Vec<u8>> {
    decrypt(creds, helper, user, name, blob).map_err(|e| {
        tracing::warn!("unseal failed: {e:#}");
        FaceAuthError::SealUnavailable.into()
    })
}

fn decrypt(creds: &str, helper: &Path, user: &str, name: &str, blob: &[u8]) -> Result<Vec<u8>> {
    if !helper.is_file() {
        return run(
            creds,
            &["decrypt", &format!("--uid={user}"), &format!("--name={name}"), "-", "-"],
            blob,
        );
    }
    // Resolved here, in the caller's domain: the helper's has no NSS.
    let uid = crate::user::lookup(user)?.uid;
    run(&helper.to_string_lossy(), &[&uid.to_string(), name], blob)
}

fn run(program: &str, args: &[&str], input: &[u8]) -> Result<Vec<u8>> {
    // Cleared environment: this runs from a set-group-ID binary.
    let mut child = Command::new(program)
        .args(args)
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    // Written from a thread so a full stdout pipe can't deadlock us.
    let mut stdin = child.stdin.take().expect("stdin is piped");
    let input = input.to_vec();
    let writer = std::thread::spawn(move || stdin.write_all(&input));

    let out = child.wait_with_output()?;
    let written = writer.join().expect("stdin writer panicked");
    // Status first: a child that exits early breaks the pipe, and its stderr
    // says why.
    anyhow::ensure!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
    written?;
    Ok(out.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    /// A stand-in for `systemd-creds` or the helper: records its argv (one per
    /// line) and its environment beside itself, then runs `body`.
    struct Fake {
        dir: PathBuf,
    }

    impl Fake {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("face-auth-seal-{tag}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self { dir }
        }

        fn tool(&self, name: &str, body: &str) -> String {
            let path = self.dir.join(name);
            let log = self.dir.join(format!("{name}.argv"));
            let env = self.dir.join(format!("{name}.env"));
            fs::write(
                &path,
                format!(
                    "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\n/usr/bin/env > '{}'\n{body}\n",
                    log.display(),
                    env.display()
                ),
            )
            .unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            path.to_string_lossy().into_owned()
        }

        fn argv(&self, name: &str) -> Vec<String> {
            fs::read_to_string(self.dir.join(format!("{name}.argv")))
                .unwrap()
                .lines()
                .map(str::to_owned)
                .collect()
        }

        fn ran(&self, name: &str) -> bool {
            self.dir.join(format!("{name}.argv")).exists()
        }
    }

    impl Drop for Fake {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    /// Exec of a script another test thread is still writing fails with
    /// ETXTBSY when a concurrent fork inherited the write fd; retry that.
    fn retry<T>(mut f: impl FnMut() -> Result<T>) -> Result<T> {
        for _ in 0..50 {
            match f() {
                Err(e) if format!("{e:#}").contains("Text file busy") => {
                    std::thread::sleep(std::time::Duration::from_millis(20))
                }
                other => return other,
            }
        }
        f()
    }

    fn me() -> String {
        crate::user::current().unwrap().name
    }

    #[test]
    fn seal_asks_for_a_user_scoped_tpm_credential_with_no_pcr_policy() {
        let fake = Fake::new("encrypt");
        let creds = fake.tool("systemd-creds", "cat");
        let out = retry(|| seal_with(&creds, "alice", "vinoauthface.alice.m1", b"payload")).unwrap();
        assert_eq!(out, b"payload");
        assert_eq!(
            fake.argv("systemd-creds"),
            [
                "encrypt",
                "--uid=alice",
                "--name=vinoauthface.alice.m1",
                "--with-key=host+tpm2",
                "--tpm2-pcrs=",
                "--tpm2-public-key-pcrs=",
                "-",
                "-"
            ]
        );
    }

    #[test]
    fn tools_run_with_an_empty_environment() {
        // This runs from a set-group-ID binary: nothing of the caller's may reach it.
        let fake = Fake::new("env");
        let creds = fake.tool("systemd-creds", "cat");
        retry(|| seal_with(&creds, "alice", "n", b"x")).unwrap();
        let env = fs::read_to_string(fake.dir.join("systemd-creds.env")).unwrap();
        for line in env.lines() {
            // sh itself sets PWD, SHLVL and `_`; nothing else may appear.
            let key = line.split('=').next().unwrap();
            assert!(matches!(key, "PWD" | "SHLVL" | "_" | "OLDPWD"), "leaked {line}");
        }
    }

    #[test]
    fn a_failed_encrypt_is_an_error_with_its_stderr() {
        let fake = Fake::new("encfail");
        let creds = fake.tool("systemd-creds", "echo 'no TPM2 device' >&2; exit 1");
        let err = retry(|| seal_with(&creds, "alice", "n", b"payload")).unwrap_err();
        assert!(format!("{err:#}").contains("no TPM2 device"), "{err:#}");
    }

    #[test]
    fn unseal_without_the_helper_decrypts_directly() {
        let fake = Fake::new("direct");
        let creds = fake.tool("systemd-creds", "cat");
        let helper = fake.dir.join("no-helper-here");
        let out = retry(|| unseal_with(&creds, &helper, "alice", "vinoauthface.alice.m1", b"blob")).unwrap();
        assert_eq!(out, b"blob");
        assert_eq!(
            fake.argv("systemd-creds"),
            ["decrypt", "--uid=alice", "--name=vinoauthface.alice.m1", "-", "-"]
        );
    }

    #[test]
    fn unseal_prefers_the_helper_and_passes_a_numeric_uid() {
        let fake = Fake::new("helper");
        let creds = fake.tool("systemd-creds", "exit 1");
        let helper = PathBuf::from(fake.tool("vinoauthface-unseal", "cat"));
        let user = me();
        let out = retry(|| unseal_with(&creds, &helper, &user, "vinoauthface.x.m1", b"blob")).unwrap();
        assert_eq!(out, b"blob");
        let uid = unsafe { libc::getuid() }.to_string();
        assert_eq!(fake.argv("vinoauthface-unseal"), [uid.as_str(), "vinoauthface.x.m1"]);
        assert!(!fake.ran("systemd-creds"), "fell through to systemd-creds");
    }

    #[test]
    fn every_unseal_failure_is_seal_unavailable_with_no_fallback() {
        let fake = Fake::new("unsealfail");
        // stdout before failing: none of it may be returned.
        let creds = fake.tool("systemd-creds", "echo plaintext; exit 1");
        let helper = PathBuf::from(fake.tool("vinoauthface-unseal", "echo plaintext; exit 1"));
        let user = me();
        for helper in [helper, fake.dir.join("no-helper-here")] {
            let err = retry(|| unseal_with(&creds, &helper, &user, "n", b"blob")).unwrap_err();
            assert!(
                matches!(err.downcast_ref::<FaceAuthError>(), Some(FaceAuthError::SealUnavailable)),
                "{err:#}"
            );
        }
        // An unknown user fails the NSS lookup before the helper runs: same answer.
        let helper = fake.dir.join("vinoauthface-unseal");
        let _ = fs::remove_file(fake.dir.join("vinoauthface-unseal.argv"));
        let err = retry(|| unseal_with(&creds, &helper, "no-such-user-face-auth", "n", b"blob")).unwrap_err();
        assert!(matches!(err.downcast_ref::<FaceAuthError>(), Some(FaceAuthError::SealUnavailable)));
        assert!(!fake.ran("vinoauthface-unseal"));
    }

    #[test]
    fn a_missing_tool_is_seal_unavailable() {
        let fake = Fake::new("missing");
        let creds = fake.dir.join("absent").to_string_lossy().into_owned();
        let err = unseal_with(&creds, &fake.dir.join("absent-helper"), "alice", "n", b"blob").unwrap_err();
        assert!(matches!(err.downcast_ref::<FaceAuthError>(), Some(FaceAuthError::SealUnavailable)));
    }

    #[test]
    fn a_large_payload_does_not_deadlock_on_the_pipes() {
        let fake = Fake::new("large");
        let creds = fake.tool("systemd-creds", "cat");
        let payload: Vec<u8> = (0..4 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
        let out = retry(|| seal_with(&creds, "alice", "n", &payload)).unwrap();
        assert_eq!(out, payload);
    }

    #[test]
    fn a_tool_that_exits_without_reading_reports_its_status_not_a_broken_pipe() {
        let fake = Fake::new("early");
        let creds = fake.tool("systemd-creds", "echo 'refused' >&2; exit 3");
        let payload = vec![0u8; 1024 * 1024];
        let err = retry(|| seal_with(&creds, "alice", "n", &payload)).unwrap_err();
        assert!(format!("{err:#}").contains("refused"), "{err:#}");
    }
}
