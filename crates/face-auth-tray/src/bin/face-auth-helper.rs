//! Root side of the tray's enrol, retrain and uninstall entries, run through
//! pkexec. See `face_auth_tray::helper` for why this exists instead of
//! pkexec running `face-enroll` directly.

use face_auth_core::user;
use face_auth_tray::helper::{self, SAFE_PATH};
use std::os::unix::process::CommandExt;
use std::process::Command;

fn die(msg: &str) -> ! {
    eprintln!("face-auth-helper: {msg}");
    std::process::exit(2)
}

fn main() {
    if unsafe { libc::geteuid() } != 0 {
        die("run this through pkexec");
    }
    // getent, below, runs through PATH.
    std::env::set_var("PATH", SAFE_PATH);

    let args: Vec<String> = std::env::args().skip(1).collect();
    let pkexec_uid = std::env::var("PKEXEC_UID").ok();
    let (verb, uid) = helper::parse_request(&args, pkexec_uid.as_deref()).unwrap_or_else(|e| die(&e));

    // By user ID, then back by name: the name becomes a path component under
    // the template store, so it goes through the same validation face-enroll
    // applies, and must resolve to the same account.
    let info = user::by_uid(uid)
        .and_then(|info| user::lookup(&info.name))
        .unwrap_or_else(|e| die(&format!("user ID {uid}: {e}")));
    if info.uid != uid {
        die(&format!("user ID {uid} resolves to {}, which is user ID {}", info.name, info.uid));
    }

    let (program, argv) = helper::command(verb, &info.name);
    let err = Command::new(program)
        .args(&argv)
        .env_clear()
        .env("PATH", SAFE_PATH)
        .current_dir("/")
        .exec();
    die(&format!("cannot run {program}: {err}"));
}
