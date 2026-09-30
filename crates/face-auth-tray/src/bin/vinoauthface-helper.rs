//! Root side of the tray's enrol, retrain, uninstall, upgrade and login-screen entries, run through
//! pkexec. See `face_auth_tray::helper` for why this exists instead of
//! pkexec running `vinoauthface` directly.

use face_auth_core::user;
use face_auth_tray::helper::{self, SAFE_PATH};
use std::os::unix::process::CommandExt;
use std::process::Command;

fn die(msg: &str) -> ! {
    eprintln!("vinoauthface-helper: {msg}");
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
    // the template store, so it goes through the same validation vinoauthface enroll
    // applies, and must resolve to the same account.
    let info = user::by_uid(uid)
        .and_then(|info| user::lookup(&info.name))
        .unwrap_or_else(|_| die("the calling account is not a valid local user"));
    if info.uid != uid {
        die("the calling account resolves to a different user ID");
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
