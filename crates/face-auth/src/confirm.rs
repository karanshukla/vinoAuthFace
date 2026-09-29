//! Intent confirmation: after a face match, wait for Enter on the caller's
//! terminal so a background process that runs `sudo` cannot borrow a face that
//! happens to be in front of the camera.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::mem::MaybeUninit;
use std::os::fd::AsRawFd;

/// How long to wait for Enter, in tenths of a second (`VTIME` units).
const TIMEOUT_DECISECONDS: u8 = 200;

pub enum Outcome {
    Confirmed,
    Declined,
    /// No controlling terminal to ask on (pkexec from a GUI, a polkit agent).
    NoTerminal,
}

pub fn ask(user: &str) -> Outcome {
    // /dev/tty rather than stdin: `echo x | sudo tee` has no tty on stdin.
    let Ok(tty) = OpenOptions::new().read(true).write(true).open("/dev/tty") else {
        return Outcome::NoTerminal;
    };
    if confirm_on(tty, user, TIMEOUT_DECISECONDS) {
        Outcome::Confirmed
    } else {
        Outcome::Declined
    }
}

fn confirm_on(mut tty: File, user: &str, timeout: u8) -> bool {
    let fd = tty.as_raw_fd();
    let mut saved = MaybeUninit::<libc::termios>::uninit();
    if unsafe { libc::tcgetattr(fd, saved.as_mut_ptr()) } != 0 {
        return false;
    }
    let saved = unsafe { saved.assume_init() };

    let _ = write!(tty, "Face matched for {user}. Press Enter to continue ");
    let _ = tty.flush();

    let mut raw = saved;
    unsafe { libc::cfmakeraw(&mut raw) };
    raw.c_cc[libc::VMIN] = 0;
    raw.c_cc[libc::VTIME] = timeout;
    // TCSAFLUSH drops typeahead, so a key pressed before the prompt cannot
    // confirm it.
    if unsafe { libc::tcsetattr(fd, libc::TCSAFLUSH, &raw) } != 0 {
        return false;
    }

    let mut key = [0u8; 1];
    let confirmed = matches!(tty.read(&mut key), Ok(1)) && matches!(key[0], b'\n' | b'\r');

    unsafe { libc::tcsetattr(fd, libc::TCSAFLUSH, &saved) };
    let _ = writeln!(tty);
    confirmed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::FromRawFd;
    use std::time::Duration;

    fn pty() -> (File, File) {
        let (mut master, mut slave) = (0, 0);
        let rc = unsafe {
            libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null(), std::ptr::null())
        };
        assert_eq!(rc, 0);
        unsafe { (File::from_raw_fd(master), File::from_raw_fd(slave)) }
    }

    fn answer(keys: &'static [u8]) -> bool {
        let (mut master, slave) = pty();
        let typist = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            master.write_all(keys).unwrap();
            master
        });
        let confirmed = confirm_on(slave, "alice", 20);
        let _master = typist.join().unwrap();
        confirmed
    }

    #[test]
    fn enter_confirms() {
        assert!(answer(b"\r"));
        assert!(answer(b"\n"));
    }

    #[test]
    fn other_keys_and_ctrl_c_decline() {
        assert!(!answer(b"y"));
        assert!(!answer(b"\x03"));
    }

    #[test]
    fn silence_times_out() {
        let (_master, slave) = pty();
        assert!(!confirm_on(slave, "alice", 1));
    }

    #[test]
    fn typeahead_does_not_confirm() {
        let (mut master, slave) = pty();
        master.write_all(b"\r").unwrap();
        assert!(!confirm_on(slave, "alice", 1));
    }
}
