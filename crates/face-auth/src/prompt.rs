//! A placeholder line on the caller's terminal while the camera scans, so
//! `sudo` on a VT or in a terminal emulator doesn't sit silent for a couple of
//! seconds. It is erased when the scan ends, leaving the password prompt (or
//! the command's output) on a clean line.
//!
//! With no terminal (a lock screen, a polkit agent), the line goes to stdout
//! instead, but only when `pam_exec.so stdout` relays it: the caller then shows
//! it as a PAM info message. It can't be erased there; the greeter replaces it
//! with its own prompt when the scan falls through.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
use std::path::Path;

const MESSAGE: &str = "Looking for your face...";
/// Carriage return, then erase the whole line.
const CLEAR: &str = "\r\x1b[2K";

/// Held for the length of a scan; erases the placeholder on drop.
pub struct ScanPrompt {
    tty: Option<File>,
}

/// Is stdout the pipe `pam_exec.so stdout` relays to the caller? Without that
/// option `pam_exec` points stdout (and stderr) at /dev/null.
pub fn relayed() -> bool {
    std::fs::metadata("/proc/self/fd/1").is_ok_and(|m| m.file_type().is_fifo())
}

impl ScanPrompt {
    /// Show the placeholder on the caller's terminal. The controlling
    /// terminal is tried first; if the child has lost it, `PAM_TTY` is used,
    /// but only when it is a real terminal node owned by the account being
    /// authenticated (see `open_pam_tty`). With no terminal, one line goes to
    /// `relay` (stdout when `relayed()`); with neither, this is a no-op. A
    /// terminal wins over the relay so `sudo` doesn't print the line twice.
    pub fn show(pam_tty: Option<&str>, uid: u32, relay: Option<&mut dyn Write>) -> Self {
        let tty = OpenOptions::new()
            .write(true)
            .open("/dev/tty")
            .ok()
            .or_else(|| pam_tty.and_then(|p| open_pam_tty(p, uid)));
        if tty.is_none() {
            if let Some(out) = relay {
                // One line, flushed: each line is a separate PAM message, and
                // stdout to a pipe is block-buffered until exit otherwise.
                let _ = writeln!(out, "{MESSAGE}");
                let _ = out.flush();
            }
        }
        Self::on(tty)
    }

    fn on(mut tty: Option<File>) -> Self {
        if let Some(t) = tty.as_mut() {
            let _ = write!(t, "{MESSAGE}");
            let _ = t.flush();
        }
        Self { tty }
    }
}

/// `PAM_TTY` is caller supplied, so it is only opened when it names a terminal
/// device, is not a symlink, and belongs to `uid`: the worst a caller can do
/// is print a fixed line on their own terminal.
fn open_pam_tty(path: &str, uid: u32) -> Option<File> {
    let p = Path::new(path);
    let in_dev = p.starts_with("/dev/pts/") || p.starts_with("/dev/tty");
    if !in_dev || p.components().any(|c| c.as_os_str() == "..") {
        return None;
    }
    let file = OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NOCTTY | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(p)
        .ok()?;
    let meta = file.metadata().ok()?;
    (meta.file_type().is_char_device() && meta.uid() == uid).then_some(file)
}

impl Drop for ScanPrompt {
    fn drop(&mut self) {
        if let Some(t) = self.tty.as_mut() {
            let _ = write!(t, "{CLEAR}");
            let _ = t.flush();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::os::fd::FromRawFd;

    fn pty() -> (File, File) {
        let (mut master, mut slave) = (0, 0);
        let rc = unsafe {
            libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null(), std::ptr::null())
        };
        assert_eq!(rc, 0);
        unsafe { (File::from_raw_fd(master), File::from_raw_fd(slave)) }
    }

    #[test]
    fn shows_then_erases() {
        let (mut master, slave) = pty();
        drop(ScanPrompt::on(Some(slave)));
        let mut out = [0u8; 128];
        let n = master.read(&mut out).unwrap();
        let text = String::from_utf8_lossy(&out[..n]);
        assert!(text.starts_with(MESSAGE), "{text:?}");
        assert!(text.ends_with(CLEAR), "{text:?}");
    }

    #[test]
    fn pam_tty_outside_dev_is_refused() {
        let uid = unsafe { libc::getuid() };
        assert!(open_pam_tty("/etc/passwd", uid).is_none());
        assert!(open_pam_tty("/dev/pts/../null", uid).is_none());
        assert!(open_pam_tty("/dev/null", uid).is_none());
    }

    #[test]
    fn no_terminal_is_a_no_op() {
        drop(ScanPrompt::on(None));
    }

    #[test]
    fn relay_gets_one_flushed_line() {
        // Only meaningful without a controlling terminal, as under PAM.
        if OpenOptions::new().write(true).open("/dev/tty").is_ok() {
            return;
        }
        let mut out = Vec::new();
        drop(ScanPrompt::show(None, 0, Some(&mut out)));
        assert_eq!(out, format!("{MESSAGE}\n").as_bytes());
    }
}
