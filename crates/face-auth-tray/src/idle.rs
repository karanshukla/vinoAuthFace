//! When the tray icon tucks itself away (#107).
//!
//! After `tray_idle_minutes` with nothing happening the icon reports itself
//! Passive, which Plasma moves into the hidden icons behind the panel's arrow.
//! Anything worth seeing (a scan, an action, a status change, a click) brings
//! it back and restarts the count.
//!
//! The delay is per user, so the Settings menu writes it to the user's own
//! `~/.config/face-auth.toml` (#148) without pkexec.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

const KEY: &str = "tray_idle_minutes";

/// The Settings menu's choices, in minutes; 0 never hides.
pub const CHOICES: [(u64, &str); 4] = [(10, "10 minutes"), (30, "30 minutes"), (60, "1 hour"), (0, "Never")];

/// Time since boot, suspend included. `Instant` stops while the laptop sleeps,
/// which would stretch the idle delay across suspends.
pub fn boottime() -> Duration {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: `ts` is a valid, writable timespec.
    if unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut ts) } != 0 {
        return Duration::ZERO;
    }
    Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32)
}

#[derive(Debug, Clone, Copy)]
pub struct Idle {
    /// `None`: never hide (`tray_idle_minutes = 0`).
    after: Option<Duration>,
    since: Duration,
}

impl Idle {
    pub fn new(minutes: u64, now: Duration) -> Idle {
        let after = (minutes > 0).then(|| Duration::from_secs(minutes.saturating_mul(60)));
        Idle { after, since: now }
    }

    /// Something happened: show the icon and start counting again.
    pub fn touch(&mut self, now: Duration) {
        self.since = now;
    }

    pub fn hidden(&self, now: Duration) -> bool {
        self.after.is_some_and(|after| now.saturating_sub(self.since) >= after)
    }
}

/// `text` with its top-level `tray_idle_minutes` set to `minutes`, the rest
/// untouched. A new key goes first: anything after a `[section]` header would
/// land inside that section.
pub fn with_minutes(text: &str, minutes: u64) -> String {
    let line = format!("{KEY} = {minutes}");
    let mut lines: Vec<&str> = text.lines().collect();
    let top_level = lines.iter().position(|l| l.trim_start().starts_with('[')).unwrap_or(lines.len());
    let existing = lines[..top_level].iter().position(|l| {
        l.trim_start().strip_prefix(KEY).is_some_and(|rest| rest.trim_start().starts_with('='))
    });
    match existing {
        Some(i) => lines[i] = &line,
        None => lines.insert(0, &line),
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

pub fn user_config() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("face-auth.toml"))
}

/// Set the delay in `path`, through a temporary file so a crash never leaves
/// half a config.
pub fn save(path: &Path, minutes: u64) -> std::io::Result<()> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("toml.tray-tmp");
    let mut file = std::fs::File::create(&tmp)?;
    file.write_all(with_minutes(&text, minutes).as_bytes())?;
    file.sync_all()?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: Duration = Duration::from_secs(60);

    #[test]
    fn hides_after_the_delay() {
        let idle = Idle::new(30, MIN);
        assert!(!idle.hidden(MIN * 30));
        assert!(idle.hidden(MIN * 31));
    }

    #[test]
    fn a_touch_restarts_the_count() {
        let mut idle = Idle::new(30, Duration::ZERO);
        assert!(idle.hidden(MIN * 40));
        idle.touch(MIN * 40);
        assert!(!idle.hidden(MIN * 69));
        assert!(idle.hidden(MIN * 70));
    }

    #[test]
    fn zero_never_hides() {
        assert!(!Idle::new(0, Duration::ZERO).hidden(MIN * 100_000));
    }

    #[test]
    fn a_clock_behind_the_touch_reads_as_active() {
        assert!(!Idle::new(1, MIN * 10).hidden(Duration::ZERO));
    }

    #[test]
    fn with_minutes_replaces_the_top_level_key() {
        let text = "threshold = 0.7\ntray_idle_minutes=30 # comment\n\n[liveness]\npreset = \"strict\"\n";
        assert_eq!(
            with_minutes(text, 10),
            "threshold = 0.7\ntray_idle_minutes = 10\n\n[liveness]\npreset = \"strict\"\n"
        );
    }

    #[test]
    fn with_minutes_adds_the_key_above_any_section() {
        assert_eq!(with_minutes("", 0), "tray_idle_minutes = 0\n");
        assert_eq!(
            with_minutes("# mine\n[guards]\ntray_idle_minutes = 5\n", 60),
            "tray_idle_minutes = 60\n# mine\n[guards]\ntray_idle_minutes = 5\n"
        );
    }

    #[test]
    fn with_minutes_leaves_comments_and_lookalike_keys() {
        let text = "# tray_idle_minutes = 30\ntray_idle_minutes_x = 1\n";
        assert_eq!(with_minutes(text, 10), format!("tray_idle_minutes = 10\n{text}"));
    }

    #[test]
    fn save_creates_then_updates_the_file() {
        let dir = std::env::temp_dir().join(format!("vinoauthface-idle-{}", std::process::id()));
        let path = dir.join("face-auth.toml");
        save(&path, 10).unwrap();
        save(&path, 60).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "tray_idle_minutes = 60\n");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn boottime_advances() {
        let a = boottime();
        assert!(a > Duration::ZERO);
        assert!(boottime() >= a);
    }
}
