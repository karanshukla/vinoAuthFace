use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const LOCKOUT_VERSION: u32 = 1;

/// Exponential backoff applied after repeated face-match failures for a user, tracked
/// across separate PAM invocations (each `face-auth` run is a fresh process) via a small
/// state file next to that user's embeddings. This only throttles the *face* factor —
/// PAM's `sufficient` fallthrough to password is untouched, so it can't lock anyone out
/// of their machine. It exists to bound how fast an automated loop of spoof attempts
/// (or a scripted `sudo -k; sudo true` loop) can retry, since without it each attempt is
/// only as slow as the camera scan itself.
#[derive(Debug, Clone, Copy, Default)]
struct LockoutState {
    failures: u32,
    last_failure_unix_ms: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct LockoutPolicy {
    /// Consecutive failures allowed before any cooldown is applied.
    pub threshold: u32,
    /// Cooldown applied on the first failure past `threshold`; doubles per failure after.
    pub base_delay_ms: u64,
    /// Ceiling on the computed cooldown window.
    pub max_delay_ms: u64,
    /// Ceiling on how long a single face-auth invocation will actually sleep while locked
    /// out, so a long cooldown window still can't stall PAM's password fallback.
    pub max_tarpit_ms: u64,
}

impl Default for LockoutPolicy {
    fn default() -> Self {
        Self {
            threshold: 5,
            base_delay_ms: 2_000,
            max_delay_ms: 300_000,
            max_tarpit_ms: 3_000,
        }
    }
}

impl LockoutState {
    fn path(user: &str, embeddings_dir: &Path) -> std::path::PathBuf {
        embeddings_dir.join(user).join("lockout.bin")
    }

    fn load(user: &str, embeddings_dir: &Path) -> Self {
        let path = Self::path(user, embeddings_dir);
        let Ok(file) = File::open(&path) else {
            return Self::default();
        };
        let mut reader = BufReader::new(file);
        let mut parse = || -> std::io::Result<Self> {
            let version = reader.read_u32::<LittleEndian>()?;
            if version != LOCKOUT_VERSION {
                return Ok(Self::default());
            }
            let failures = reader.read_u32::<LittleEndian>()?;
            let last_failure_unix_ms = reader.read_u64::<LittleEndian>()?;
            Ok(Self { failures, last_failure_unix_ms })
        };
        // A missing or corrupt lockout file must never block authentication — worst case
        // it just resets the backoff, which is safe (fails open on bookkeeping, not on auth).
        parse().unwrap_or_default()
    }

    fn save(&self, user: &str, embeddings_dir: &Path) -> anyhow::Result<()> {
        let user_dir = embeddings_dir.join(user);
        fs::create_dir_all(&user_dir)?;
        fs::set_permissions(&user_dir, fs::Permissions::from_mode(0o700))?;

        let tmp_path = user_dir.join("lockout.bin.tmp");
        let path = user_dir.join("lockout.bin");

        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp_path)?;
        let mut writer = BufWriter::new(file);

        writer.write_u32::<LittleEndian>(LOCKOUT_VERSION)?;
        writer.write_u32::<LittleEndian>(self.failures)?;
        writer.write_u64::<LittleEndian>(self.last_failure_unix_ms)?;
        writer.flush()?;
        drop(writer);

        fs::rename(&tmp_path, &path)?;
        Ok(())
    }
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Cooldown remaining for `user`, if the failure count is currently past `policy.threshold`
/// and less than the computed backoff has elapsed since the last recorded failure.
fn remaining_cooldown(state: &LockoutState, policy: &LockoutPolicy) -> Option<Duration> {
    if state.failures < policy.threshold {
        return None;
    }
    let extra = (state.failures - policy.threshold).min(20); // cap shift to avoid overflow
    let delay_ms = policy
        .base_delay_ms
        .saturating_mul(1u64 << extra)
        .min(policy.max_delay_ms);

    let elapsed_ms = now_unix_ms().saturating_sub(state.last_failure_unix_ms);
    if elapsed_ms >= delay_ms {
        return None;
    }
    Some(Duration::from_millis(delay_ms - elapsed_ms))
}

/// Checks whether `user` is currently in a lockout cooldown. If so, sleeps a bounded
/// "tarpit" delay (never the full cooldown, so PAM's password fallback is never stalled
/// for long) and returns the remaining cooldown. Returns `None` if the caller should
/// proceed with a normal capture/verify attempt.
pub fn check(user: &str, embeddings_dir: &Path, policy: &LockoutPolicy) -> Option<Duration> {
    let state = LockoutState::load(user, embeddings_dir);
    let remaining = remaining_cooldown(&state, policy)?;
    let tarpit = remaining.min(Duration::from_millis(policy.max_tarpit_ms));
    tracing::warn!(
        user,
        failures = state.failures,
        remaining_ms = remaining.as_millis() as u64,
        "face-auth: locked out after repeated failures, tarpitting before falling back to password"
    );
    std::thread::sleep(tarpit);
    Some(remaining)
}

/// Records a failed face-match attempt (a completed scan that didn't match — not a
/// capture/init error), advancing the backoff.
pub fn record_failure(user: &str, embeddings_dir: &Path) -> anyhow::Result<()> {
    let mut state = LockoutState::load(user, embeddings_dir);
    state.failures = state.failures.saturating_add(1);
    state.last_failure_unix_ms = now_unix_ms();
    state.save(user, embeddings_dir)
}

/// Resets the backoff after a successful match.
pub fn record_success(user: &str, embeddings_dir: &Path) -> anyhow::Result<()> {
    LockoutState::default().save(user, embeddings_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_cooldown_below_threshold() {
        let state = LockoutState { failures: 4, last_failure_unix_ms: now_unix_ms() };
        let policy = LockoutPolicy::default();
        assert!(remaining_cooldown(&state, &policy).is_none());
    }

    #[test]
    fn cooldown_active_just_past_threshold() {
        let state = LockoutState { failures: 5, last_failure_unix_ms: now_unix_ms() };
        let policy = LockoutPolicy::default();
        let remaining = remaining_cooldown(&state, &policy).expect("should be in cooldown");
        assert!(remaining.as_millis() > 0);
        assert!(remaining.as_millis() <= policy.base_delay_ms as u128);
    }

    #[test]
    fn cooldown_expires_after_elapsed_time() {
        let state = LockoutState {
            failures: 5,
            last_failure_unix_ms: now_unix_ms().saturating_sub(policy_default_base_plus_margin()),
        };
        let policy = LockoutPolicy::default();
        assert!(remaining_cooldown(&state, &policy).is_none());
    }

    fn policy_default_base_plus_margin() -> u64 {
        LockoutPolicy::default().base_delay_ms + 1_000
    }

    #[test]
    fn cooldown_caps_at_max_delay() {
        let state = LockoutState { failures: 1_000, last_failure_unix_ms: now_unix_ms() };
        let policy = LockoutPolicy::default();
        let remaining = remaining_cooldown(&state, &policy).expect("should be in cooldown");
        assert!(remaining.as_millis() <= policy.max_delay_ms as u128);
    }
}
