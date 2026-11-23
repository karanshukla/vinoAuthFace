//! vinoAuthFace's tray icon: status, enrol, retrain, test, the login screen,
//! settings and upgrade.
//!
//! A per-user session app, never in the authentication path. It runs
//! unprivileged and elevates each action that touches the template store
//! through pkexec and `vinoauthface-helper`. It learns about scans only by
//! looking for the process in `/proc` (see `scanning.rs`): `vinoauthface-auth` does not
//! know the tray exists.

mod app {
    pub mod actions;
    pub mod event_loop;
    pub mod menu;
    pub mod notifier;
    pub mod tray;

    pub use face_auth_core::paths::TRAY;
    pub const DOCS: &str = "https://vinoauthface.vinopilot.ca/";
}

use app::tray::Tray;
use face_auth_core::{user, FaceAuthConfig};
use face_auth_tray::single_instance;
use face_auth_tray::status::Status;
use ksni::blocking::TrayMethods;
use std::sync::mpsc;

/// Leaves the launching terminal: a tray started from a shell would otherwise
/// die with it (SIGHUP on close). Autostart and launchers have no tty and skip
/// this. Must run before any thread exists, since it forks.
fn detach_from_terminal() {
    // SAFETY: single-threaded here; the child only calls async-signal-safe libc.
    unsafe {
        if libc::isatty(libc::STDIN_FILENO) != 1 && libc::isatty(libc::STDERR_FILENO) != 1 {
            return;
        }
        match libc::fork() {
            -1 => return,
            0 => {}
            _ => libc::_exit(0),
        }
        libc::setsid();
        let null = libc::open(c"/dev/null".as_ptr(), libc::O_RDWR);
        if null >= 0 {
            for fd in [libc::STDIN_FILENO, libc::STDOUT_FILENO, libc::STDERR_FILENO] {
                libc::dup2(null, fd);
            }
            if null > libc::STDERR_FILENO {
                libc::close(null);
            }
        }
    }
}

fn main() -> anyhow::Result<()> {
    detach_from_terminal();
    let _instance = match single_instance::lock_path().map(|p| single_instance::acquire(&p)) {
        Some(Ok(Some(held))) => Some(held),
        Some(Ok(None)) => return Ok(()),
        // No runtime dir or lock error: a second tray beats no tray.
        _ => None,
    };
    let me = user::current()?;
    let (tx, rx) = mpsc::channel();
    let tray = Tray {
        tx: tx.clone(),
        status: Status::read(),
        scanning: false,
        busy: None,
        update: None,
        hidden: false,
        idle_minutes: FaceAuthConfig::load().map_or(30, |c| c.tray_idle_minutes()),
    };
    // Assumed, not checked: at login this can start before Plasma's tray does.
    let handle = tray.assume_sni_available(true).spawn()?;
    app::event_loop::event_loop(handle.clone(), rx, tx, me.name);
    handle.shutdown().wait();
    Ok(())
}
