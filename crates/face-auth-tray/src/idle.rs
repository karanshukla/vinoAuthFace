//! When the tray icon tucks itself away (#107).
//!
//! After `tray_idle_minutes` with nothing happening the icon reports itself
//! Passive, which Plasma moves into the hidden icons behind the panel's arrow.
//! Anything worth seeing (a scan, an action, a status change, a click) brings
//! it back and restarts the count.

use std::time::Duration;

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
    fn boottime_advances() {
        let a = boottime();
        assert!(a > Duration::ZERO);
        assert!(boottime() >= a);
    }
}
