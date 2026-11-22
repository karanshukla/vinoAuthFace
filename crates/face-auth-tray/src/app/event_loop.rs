//! The loop behind the icon: menu messages, the scan poll, the daily update check, idling.

use super::actions::perform;
use super::notifier::Notifier;
use super::tray::{After, Msg, Tray};
use super::TRAY;
use face_auth_core::installed::enrolled;
use face_auth_core::{update, FaceAuthConfig};
use face_auth_tray::idle::{self, Idle};
use face_auth_tray::scanning;
use face_auth_tray::status::Status;
use ksni::blocking::Handle;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

/// How often `/proc` is checked for a running scan.
const SCAN_POLL: Duration = Duration::from_millis(200);
/// How often enrolment is re-read, to catch a `sudo vinoauthface enroll` run by hand.
const STATUS_POLL: Duration = Duration::from_secs(30);
/// First update check, after login's own network and tray startup settle.
const UPDATE_FIRST_CHECK: Duration = Duration::from_secs(60);
const UPDATE_CHECK_EVERY: Duration = Duration::from_secs(24 * 60 * 60);

pub fn event_loop(handle: Handle<Tray>, rx: Receiver<Msg>, tx: Sender<Msg>, user: String) {
    let proc_root = Path::new("/proc");
    let mut last_status = Instant::now();
    let mut scanning = false;
    let mut next_update_check = Instant::now() + UPDATE_FIRST_CHECK;
    let mut notified_update: Option<String> = None;
    let minutes = handle.update(|t| t.idle_minutes).unwrap_or(30);
    let mut idle = Idle::new(minutes, idle::boottime());
    let mut hidden = false;
    let mut state = handle.update(|t| t.state());
    loop {
        match rx.recv_timeout(SCAN_POLL) {
            Ok(Msg::Quit) => return,
            Ok(Msg::Restart) => {
                let err = Command::new(TRAY).exec();
                eprintln!("vinoauthface-tray: cannot restart {TRAY}: {err}");
                return;
            }
            Ok(Msg::Touch) => idle.touch(idle::boottime()),
            Ok(Msg::IdleMinutes(minutes)) => {
                let saved = idle::user_config()
                    .ok_or_else(|| std::io::Error::other("no home directory"))
                    .and_then(|path| idle::save(&path, minutes));
                match saved {
                    Ok(()) => {
                        idle = Idle::new(minutes, idle::boottime());
                        handle.update(|t| t.idle_minutes = minutes);
                    }
                    Err(e) => {
                        Notifier::new().send(
                            0,
                            "Could not save idle timeout",
                            &format!("~/.config/face-auth.toml: {e}"),
                        );
                    }
                }
            }
            Ok(Msg::Update(latest)) => {
                if latest.is_some() && latest != notified_update {
                    idle.touch(idle::boottime());
                    if let Some(tag) = &latest {
                        Notifier::new().send(
                            0,
                            "vinoAuthFace update available",
                            &format!("Version {tag} is available (installed: {}). Open the tray menu to update.", update::CURRENT),
                        );
                    }
                    notified_update = latest.clone();
                }
                handle.update(|t| t.update = latest);
            }
            Ok(Msg::Run(action)) => {
                idle.touch(idle::boottime());
                let (handle, tx, user) = (handle.clone(), tx.clone(), user.clone());
                handle.clone().update(|t| t.busy = Some("Starting…".into()));
                std::thread::spawn(move || {
                    let _ = tx.send(match perform(action, &handle, &Notifier::new(), &user) {
                        After::Stay => Msg::Done,
                        After::Restart => Msg::Restart,
                    });
                });
            }
            Ok(Msg::Done) => {
                let status = Status::read();
                handle.update(|t| t.status = status);
                last_status = Instant::now();
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }

        // Off the event loop: curl can take its full timeout, and the loop
        // also paces the scan icon.
        if Instant::now() >= next_update_check {
            next_update_check = Instant::now() + UPDATE_CHECK_EVERY;
            if update::release_number(update::CURRENT).is_some()
                && FaceAuthConfig::load().map_or(true, |c| c.update_check())
            {
                let tx = tx.clone();
                std::thread::spawn(move || {
                    let _ = tx.send(Msg::Update(update::newer_release(update::CURRENT)));
                });
            }
        }

        let now = scanning::face_auth_running(proc_root);
        if now != scanning {
            scanning = now;
            handle.update(|t| t.scanning = now);
        }
        // `vinoauthface-auth --enrolled` is itself a scan process, so it only
        // runs from here, between two /proc checks, never alongside one.
        if !scanning && last_status.elapsed() >= STATUS_POLL {
            let enrolled = enrolled();
            handle.update(|t| t.status.enrolled = enrolled);
            last_status = Instant::now();
        }

        // A scan, an action or a status change shows the icon again; so does
        // a busy action that outlasts the delay, until it finishes.
        let (now_state, busy) = handle.update(|t| (t.state(), t.busy.is_some())).unzip();
        if now_state != state || busy == Some(true) {
            state = now_state;
            idle.touch(idle::boottime());
        }
        let hide = idle.hidden(idle::boottime());
        if hide != hidden {
            hidden = hide;
            handle.update(|t| t.hidden = hide);
        }
    }
}
