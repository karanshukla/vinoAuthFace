//! Per-user backoff after repeated face-match failures.
//!
//! Each `face-auth` run is a fresh process, so state lives in
//! `<store>/<user>/lockout/state.bin`. That directory is the only part of the
//! store the set-group-ID `face-auth` binary can write, because lock screens
//! run it as the user and it still has to record their failures. This only throttles the *face* factor: PAM's
//! `sufficient` line still falls through to the password, so nobody can be
//! locked out of their machine. What it bounds is how fast a scripted loop of
//! spoof attempts (`sudo -k; sudo true` in a loop) can retry.

use crate::storage::{ensure_dir, lockout_dir, user_store_dir, LOCKOUT_DIR_MODE, LOCKOUT_FILE_MODE};
use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const LOCKOUT_VERSION: u32 = 1;
const STATE_FILE: &str = "state.bin";

#[derive(Debug, Clone, Copy, Default)]
struct LockoutState {
    failures: u32,
    last_failure_unix_ms: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct LockoutPolicy {
    /// Consecutive failures allowed before any cooldown applies.
    pub threshold: u32,
    /// Cooldown on the first failure past `threshold`; doubles per failure after.
    pub base_delay_ms: u64,
    /// Ceiling on the computed cooldown.
    pub max_delay_ms: u64,
    /// Ceiling on how long one invocation actually sleeps while locked out, so
    /// a long cooldown never stalls the password prompt behind it.
    pub max_tarpit_ms: u64,
}

impl Default for LockoutPolicy {
    fn default() -> Self {
        Self { threshold: 5, base_delay_ms: 2_000, max_delay_ms: 300_000, max_tarpit_ms: 3_000 }
    }
}

impl LockoutState {
    /// A missing or corrupt file resets the backoff. That fails open on
    /// bookkeeping only; it never lets a non-matching face through.
    fn load(user: &str, embeddings_dir: &Path) -> Self {
        let Ok(dir) = user_store_dir(user, embeddings_dir) else {
            return Self::default();
        };
        let Ok(file) = File::open(lockout_dir(&dir).join(STATE_FILE)) else {
            return Self::default();
        };
        let mut reader = BufReader::new(file);
        let mut parse = || -> std::io::Result<Self> {
            if reader.read_u32::<LittleEndian>()? != LOCKOUT_VERSION {
                return Ok(Self::default());
            }
            let failures = reader.read_u32::<LittleEndian>()?;
            let last_failure_unix_ms = reader.read_u64::<LittleEndian>()?;
            Ok(Self { failures, last_failure_unix_ms })
        };
        parse().unwrap_or_default()
    }

    fn save(&self, user: &str, embeddings_dir: &Path) -> anyhow::Result<()> {
        let user_dir = user_store_dir(user, embeddings_dir)?;
        if !user_dir.is_dir() {
            // Nobody enrolled under this name. The user directory is created
            // only by enrolment, and the group could not create it anyway.
            anyhow::bail!("no store for '{user}'");
        }
        let dir = lockout_dir(&user_dir);
        // Stores enrolled before the lockout directory existed get it here when
        // running as root; the group alone gets a permission error.
        ensure_dir(&dir, LOCKOUT_DIR_MODE)?;

        // Unique and never followed: the directory is group-writable, so a
        // fixed name could be pre-created or pointed elsewhere.
        let tmp_path = dir.join(format!("{STATE_FILE}.{}.tmp", std::process::id()));
        let _ = fs::remove_file(&tmp_path);
        {
            let file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .custom_flags(libc::O_NOFOLLOW)
                .mode(LOCKOUT_FILE_MODE)
                .open(&tmp_path)?;
            // The umask strips the group bits from the create mode.
            file.set_permissions(fs::Permissions::from_mode(LOCKOUT_FILE_MODE))?;
            let mut writer = BufWriter::new(file);
            writer.write_u32::<LittleEndian>(LOCKOUT_VERSION)?;
            writer.write_u32::<LittleEndian>(self.failures)?;
            writer.write_u64::<LittleEndian>(self.last_failure_unix_ms)?;
            writer.flush()?;
            writer.get_ref().sync_all()?;
        }
        fs::rename(&tmp_path, dir.join(STATE_FILE))?;
        // Persist the rename itself.
        if let Ok(d) = File::open(&dir) {
            let _ = d.sync_all();
        }
        Ok(())
    }
}

fn now_unix_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn remaining_cooldown(state: &LockoutState, policy: &LockoutPolicy) -> Option<Duration> {
    if state.failures < policy.threshold {
        return None;
    }
    let extra = (state.failures - policy.threshold).min(20); // bound the shift
    let delay_ms = policy.base_delay_ms.saturating_mul(1u64 << extra).min(policy.max_delay_ms);

    let elapsed_ms = now_unix_ms().saturating_sub(state.last_failure_unix_ms);
    (elapsed_ms < delay_ms).then(|| Duration::from_millis(delay_ms - elapsed_ms))
}

/// If `user` is cooling down, sleep a bounded tarpit and return the remaining
/// cooldown; the caller then skips the camera entirely. `None` means proceed.
pub fn check(user: &str, embeddings_dir: &Path, policy: &LockoutPolicy) -> Option<Duration> {
    let state = LockoutState::load(user, embeddings_dir);
    let remaining = remaining_cooldown(&state, policy)?;
    tracing::info!(
        user,
        failures = state.failures,
        remaining_ms = remaining.as_millis() as u64,
        "locked out after repeated failures; skipping face check"
    );
    std::thread::sleep(remaining.min(Duration::from_millis(policy.max_tarpit_ms)));
    Some(remaining)
}

/// Record a completed scan that saw a face and did not match.
pub fn record_failure(user: &str, embeddings_dir: &Path) -> anyhow::Result<()> {
    let mut state = LockoutState::load(user, embeddings_dir);
    state.failures = state.failures.saturating_add(1);
    state.last_failure_unix_ms = now_unix_ms();
    state.save(user, embeddings_dir)
}

pub fn record_success(user: &str, embeddings_dir: &Path) -> anyhow::Result<()> {
    LockoutState::default().save(user, embeddings_dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn tmpdir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "face-auth-lockout-{}-{}-{:?}",
            tag,
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn no_cooldown_below_threshold() {
        let state = LockoutState { failures: 4, last_failure_unix_ms: now_unix_ms() };
        assert!(remaining_cooldown(&state, &LockoutPolicy::default()).is_none());
    }

    #[test]
    fn cooldown_active_just_past_threshold() {
        let policy = LockoutPolicy::default();
        let state = LockoutState { failures: 5, last_failure_unix_ms: now_unix_ms() };
        let remaining = remaining_cooldown(&state, &policy).expect("in cooldown");
        assert!(remaining.as_millis() > 0 && remaining.as_millis() <= policy.base_delay_ms as u128);
    }

    #[test]
    fn cooldown_expires_after_elapsed_time() {
        let policy = LockoutPolicy::default();
        let state = LockoutState {
            failures: 5,
            last_failure_unix_ms: now_unix_ms().saturating_sub(policy.base_delay_ms + 1_000),
        };
        assert!(remaining_cooldown(&state, &policy).is_none());
    }

    #[test]
    fn cooldown_caps_at_max_delay() {
        let policy = LockoutPolicy::default();
        let state = LockoutState { failures: 1_000, last_failure_unix_ms: now_unix_ms() };
        let remaining = remaining_cooldown(&state, &policy).expect("in cooldown");
        assert!(remaining.as_millis() <= policy.max_delay_ms as u128);
    }

    #[test]
    fn failures_persist_and_success_resets() {
        let dir = tmpdir("persist");
        fs::create_dir_all(dir.join("alice")).unwrap();
        for _ in 0..3 {
            record_failure("alice", &dir).unwrap();
        }
        assert_eq!(LockoutState::load("alice", &dir).failures, 3);

        let mode = |p: &str| fs::metadata(dir.join(p)).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode("alice/lockout/state.bin"), 0o660);
        assert_eq!(mode("alice/lockout"), 0o2770);

        record_success("alice", &dir).unwrap();
        assert_eq!(LockoutState::load("alice", &dir).failures, 0);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rejects_path_traversal_in_username() {
        let dir = tmpdir("traversal");
        assert!(record_failure("../escaped", &dir).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }
}
