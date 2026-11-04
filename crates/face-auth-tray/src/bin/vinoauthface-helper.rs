//! Root side of the tray's enrol, retrain, upgrade, login-screen and settings entries, and of the
//! settings window's Apply, run through pkexec. See `face_auth_tray::helper` for why this exists
//! instead of pkexec running `vinoauthface` directly.

use face_auth_core::config::SYSTEM_CONFIG_PATH;
use face_auth_core::{config_write, user};
use face_auth_tray::helper::{self, Verb, SAFE_PATH};
use std::io::Read;
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
    // Nothing here should search PATH, but pin it anyway.
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

    let Some((program, argv)) = helper::command(verb, &info.name) else {
        debug_assert_eq!(verb, Verb::WriteConfig);
        write_config();
    };
    let err = Command::new(program)
        .args(&argv)
        .env_clear()
        .env("PATH", SAFE_PATH)
        .current_dir("/")
        .exec();
    die(&format!("cannot run {program}: {err}"));
}

/// `write-config`: the new `/etc/face-auth.toml` on stdin, checked by
/// `config_write::check` against the file as it is now, then written in
/// one rename.
fn write_config() -> ! {
    let mut payload = String::new();
    let limit = config_write::MAX_CONFIG_BYTES as u64 + 1024;
    if std::io::stdin().take(limit).read_to_string(&mut payload).is_err() {
        die("the new settings are not text");
    }
    let path = std::path::Path::new(SYSTEM_CONFIG_PATH);
    let current = match std::fs::read_to_string(path) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => die(&format!("cannot read {SYSTEM_CONFIG_PATH}: {e}")),
    };
    let text = config_write::check(current.as_deref(), &payload, config_write::trusted_path)
        .unwrap_or_else(|e| die(&format!("{e:#}")));
    if let Err(e) = config_write::write_atomic(path, &text) {
        die(&format!("cannot write {SYSTEM_CONFIG_PATH}: {e:#}"));
    }
    println!("Saved {SYSTEM_CONFIG_PATH}");
    std::process::exit(0)
}
