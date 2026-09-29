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

pub fn available() -> bool {
    Path::new(SYSTEMD_CREDS).exists() && Path::new(TPM_DEVICE).exists()
}

/// `host+tpm2` because scoped mode refuses `tpm2` alone. The key still needs
/// this machine's TPM. No PCR policy, signed or not, so firmware and image
/// updates keep the templates readable.
pub fn seal(user: &str, name: &str, payload: &[u8]) -> Result<Vec<u8>> {
    run(
        &[
            "encrypt",
            &format!("--uid={user}"),
            &format!("--name={name}"),
            "--with-key=host+tpm2",
            "--tpm2-pcrs=",
            "--tpm2-public-key-pcrs=",
            "-",
            "-",
        ],
        payload,
    )
    .context("systemd-creds encrypt")
}

/// Any failure is `SealUnavailable`: the blob is unreadable here, whatever
/// the reason, and the caller must not fall back to anything.
pub fn unseal(user: &str, name: &str, blob: &[u8]) -> Result<Vec<u8>> {
    run(
        &["decrypt", &format!("--uid={user}"), &format!("--name={name}"), "-", "-"],
        blob,
    )
    .map_err(|e| {
        tracing::warn!("unseal failed: {e:#}");
        FaceAuthError::SealUnavailable.into()
    })
}

fn run(args: &[&str], input: &[u8]) -> Result<Vec<u8>> {
    // Cleared environment: this runs from a set-group-ID binary.
    let mut child = Command::new(SYSTEMD_CREDS)
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
