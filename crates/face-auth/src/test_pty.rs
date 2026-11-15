//! A pseudo-terminal pair for the tests that need a tty.

use std::fs::File;
use std::os::fd::FromRawFd;

/// The master and slave ends, in that order.
pub fn pty() -> (File, File) {
    let (mut master, mut slave) = (0, 0);
    // SAFETY: openpty fills both descriptors or fails; each is owned by exactly one File.
    let rc = unsafe {
        libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null(), std::ptr::null())
    };
    assert_eq!(rc, 0);
    unsafe { (File::from_raw_fd(master), File::from_raw_fd(slave)) }
}
