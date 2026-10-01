use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

const LOCK_NAME: &str = "vinoauthface-tray.lock";

/// Held for the life of the process; the kernel drops the lock when it dies.
pub struct Instance(#[allow(dead_code)] File);

/// The lock file's path, or `None` outside a session with a runtime dir.
pub fn lock_path() -> Option<PathBuf> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty())?;
    Some(Path::new(&dir).join(LOCK_NAME))
}

/// `Ok(None)` when another tray holds the lock. The fd is close-on-exec, so
/// the tray's own re-exec after an upgrade can take the lock again.
pub fn acquire(path: &Path) -> io::Result<Option<Instance>> {
    let file = OpenOptions::new().create(true).truncate(false).write(true).open(path)?;
    // SAFETY: flock on an fd we own.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return Ok(Some(Instance(file)));
    }
    let err = io::Error::last_os_error();
    if err.kind() == io::ErrorKind::WouldBlock {
        Ok(None)
    } else {
        Err(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lock_file(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("vinoauthface-{name}-{}.lock", std::process::id()))
    }

    #[test]
    fn the_first_tray_takes_the_lock() {
        let path = lock_file("first");
        assert!(acquire(&path).unwrap().is_some());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_second_tray_is_refused_while_the_first_runs() {
        let path = lock_file("second");
        let first = acquire(&path).unwrap();
        assert!(first.is_some());
        assert!(acquire(&path).unwrap().is_none());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_new_tray_starts_once_the_first_has_exited() {
        let path = lock_file("after");
        drop(acquire(&path).unwrap());
        assert!(acquire(&path).unwrap().is_some());
        let _ = std::fs::remove_file(path);
    }
}
