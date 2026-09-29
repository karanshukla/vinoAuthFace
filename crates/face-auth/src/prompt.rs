//! A placeholder line on the caller's terminal while the camera scans, so
//! `sudo` on a VT or in a terminal emulator doesn't sit silent for a couple of
//! seconds. It is erased when the scan ends, leaving the password prompt (or
//! the command's output) on a clean line.

use std::fs::{File, OpenOptions};
use std::io::Write;

const MESSAGE: &str = "Looking for your face...";
/// Carriage return, then erase the whole line.
const CLEAR: &str = "\r\x1b[2K";

/// Held for the length of a scan; erases the placeholder on drop.
pub struct ScanPrompt {
    tty: Option<File>,
}

impl ScanPrompt {
    /// Show the placeholder on the controlling terminal. With none (a lock
    /// screen, a polkit agent) this is a no-op. `/dev/tty` rather than stdout:
    /// under `pam_exec` stdout is not the terminal, and `PAM_TTY` is caller
    /// supplied so is never opened.
    pub fn show() -> Self {
        Self::on(OpenOptions::new().write(true).open("/dev/tty").ok())
    }

    fn on(mut tty: Option<File>) -> Self {
        if let Some(t) = tty.as_mut() {
            let _ = write!(t, "{MESSAGE}");
            let _ = t.flush();
        }
        Self { tty }
    }
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
    fn no_terminal_is_a_no_op() {
        drop(ScanPrompt::on(None));
    }
}
