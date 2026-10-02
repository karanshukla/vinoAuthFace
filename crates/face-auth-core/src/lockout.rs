//! Per-user backoff after repeated face-match failures.
//!
//! State lives in `<store>/<user>/lockout/state.bin` because each `face-auth`
//! run is a fresh process. That directory is the only group-writable part of
//! the store: lock screens run the set-group-ID binary as the user, and it
//! still has to record their failures. Only the face factor is throttled; PAM
//! falls through to the password, so nobody is locked out of the machine.
//!
//! Every update is a load-modify-save under an exclusive `flock` on
//! `lockout/state.lock`, so scans running in parallel each count.

use crate::storage::{ensure_dir, lockout_dir, user_store_dir, LOCKOUT_DIR_MODE, LOCKOUT_FILE_MODE};
use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const LOCKOUT_VERSION: u32 = 1;
const STATE_FILE: &str = "state.bin";
const LOCK_FILE: &str = "state.lock";
/// How long an update waits for another one. Each holder only reads and
/// writes a few bytes, so only a stopped or hung process takes this long.
const LOCK_WAIT: Duration = Duration::from_secs(2);
const MAX_STATE_FILE_LEN: u64 = 64;
/// Caps the doubling exponent so the shift cannot overflow.
const MAX_DOUBLINGS: u32 = 20;

/// Another process held the lockout state lock for all of `LOCK_WAIT`.
/// Distinct from a failed write: a scan that can't be counted must not run.
#[derive(Debug, thiserror::Error)]
#[error("lockout state is held by another process")]
pub struct LockBusy;

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
        // Never follow a link and never block on a FIFO: the directory is
        // group-writable, and PAM must not hang on what is found in it.
        let Ok(file) = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(lockout_dir(&dir).join(STATE_FILE))
        else {
            return Self::default();
        };
        if !file.metadata().is_ok_and(|m| m.is_file() && m.len() <= MAX_STATE_FILE_LEN) {
            return Self::default();
        }
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

    /// Write into `dir`, the user's `lockout/`. Callers hold the lock.
    fn save(&self, dir: &Path) -> anyhow::Result<()> {
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
        if let Ok(d) = File::open(dir) {
            let _ = d.sync_all();
        }
        Ok(())
    }
}

/// Take the exclusive lock on `dir/state.lock`, released when the returned
/// file is dropped. Never follows a link, for the same reason as `load`.
///
/// `None` if `flock` itself is refused (an SELinux policy from before this
/// lock existed denies it to the greeters): the update then goes ahead
/// unlocked, as it always used to.
fn lock(dir: &Path) -> anyhow::Result<Option<File>> {
    let path = dir.join(LOCK_FILE);
    let open = |create: bool| {
        OpenOptions::new()
            .read(true)
            .write(create)
            .create_new(create)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .mode(LOCKOUT_FILE_MODE)
            .open(&path)
    };
    let file = match open(true) {
        Ok(file) => {
            // The umask strips the group bits from the create mode.
            file.set_permissions(fs::Permissions::from_mode(LOCKOUT_FILE_MODE))?;
            file
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => open(false)?,
        Err(e) => return Err(e.into()),
    };
    if !file.metadata()?.is_file() {
        anyhow::bail!("{} is not a regular file", path.display());
    }

    let deadline = Instant::now() + LOCK_WAIT;
    loop {
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(Some(file));
        }
        let err = std::io::Error::last_os_error();
        match err.raw_os_error() {
            Some(libc::EINTR) => {}
            Some(libc::EWOULDBLOCK) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Some(libc::EWOULDBLOCK) => return Err(LockBusy.into()),
            _ => {
                tracing::warn!("cannot lock {}, updating unlocked: {err}", path.display());
                return Ok(None);
            }
        }
    }
}

/// Load, change and save `user`'s state under the lock.
fn update(user: &str, embeddings_dir: &Path, change: impl FnOnce(&mut LockoutState)) -> anyhow::Result<()> {
    let user_dir = user_store_dir(user, embeddings_dir)?;
    if !user_dir.is_dir() {
        // Only enrolment creates the user directory; the group cannot.
        anyhow::bail!("no store for '{user}'");
    }
    let dir = lockout_dir(&user_dir);
    // Stores enrolled before the lockout directory existed get it here when
    // running as root; the group alone gets a permission error.
    ensure_dir(&dir, LOCKOUT_DIR_MODE)?;
    let _lock = lock(&dir)?;
    let mut state = LockoutState::load(user, embeddings_dir);
    change(&mut state);
    state.save(&dir)
}

fn now_unix_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn remaining_cooldown(state: &LockoutState, policy: &LockoutPolicy) -> Option<Duration> {
    if state.failures < policy.threshold {
        return None;
    }
    let extra = (state.failures - policy.threshold).min(MAX_DOUBLINGS);
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

/// Failure count and remaining cooldown, read without sleeping or writing.
/// For `vinoauthface doctor`.
pub fn peek(user: &str, embeddings_dir: &Path, policy: &LockoutPolicy) -> (u32, Option<Duration>) {
    let state = LockoutState::load(user, embeddings_dir);
    (state.failures, remaining_cooldown(&state, policy))
}

/// Count one failed attempt. Called when a scan first sees a face, before
/// anything can match, and undone by [`record_success`] on a match. An error
/// that is [`LockBusy`] means nothing was counted.
pub fn record_failure(user: &str, embeddings_dir: &Path) -> anyhow::Result<()> {
    update(user, embeddings_dir, |state| {
        state.failures = state.failures.saturating_add(1);
        state.last_failure_unix_ms = now_unix_ms();
    })
}

pub fn record_success(user: &str, embeddings_dir: &Path) -> anyhow::Result<()> {
    update(user, embeddings_dir, |state| *state = LockoutState::default())
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
    fn peek_reports_without_changing_state() {
        let dir = tmpdir("peek");
        fs::create_dir_all(dir.join("alice")).unwrap();
        let policy = LockoutPolicy::default();
        assert_eq!(peek("alice", &dir, &policy), (0, None));
        for _ in 0..policy.threshold {
            record_failure("alice", &dir).unwrap();
        }
        let (failures, remaining) = peek("alice", &dir, &policy);
        assert_eq!(failures, policy.threshold);
        assert!(remaining.is_some());
        assert_eq!(LockoutState::load("alice", &dir).failures, policy.threshold);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn parallel_failures_all_count() {
        let dir = tmpdir("parallel");
        fs::create_dir_all(dir.join("alice")).unwrap();
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let dir = dir.clone();
                std::thread::spawn(move || {
                    for _ in 0..5 {
                        record_failure("alice", &dir).unwrap();
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(LockoutState::load("alice", &dir).failures, 40, "no increment may be lost");
        let mode = fs::metadata(dir.join("alice/lockout/state.lock")).unwrap().permissions().mode();
        assert_eq!(mode & 0o7777, 0o660);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_held_lock_is_busy_not_skipped() {
        let dir = tmpdir("busy");
        fs::create_dir_all(dir.join("alice/lockout")).unwrap();
        let held = lock(&dir.join("alice/lockout")).unwrap().expect("flock works here");
        let err = record_failure("alice", &dir).unwrap_err();
        assert!(err.is::<LockBusy>(), "{err}");
        assert_eq!(LockoutState::load("alice", &dir).failures, 0);
        drop(held);
        record_failure("alice", &dir).unwrap();
        assert_eq!(LockoutState::load("alice", &dir).failures, 1);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn lock_file_is_never_followed() {
        let dir = tmpdir("locklink");
        let lockout = dir.join("alice/lockout");
        fs::create_dir_all(&lockout).unwrap();
        let target = dir.join("elsewhere");
        fs::write(&target, b"").unwrap();
        std::os::unix::fs::symlink(&target, lockout.join(LOCK_FILE)).unwrap();
        assert!(record_failure("alice", &dir).is_err());
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
