//! Helpers shared by the unit tests.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

/// A fresh, empty directory per call, so tests running in parallel never share one.
pub fn tmpdir(tag: &str) -> PathBuf {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "face-auth-test-{tag}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
