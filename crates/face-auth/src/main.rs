//! PAM authentication helper, invoked via `pam_exec.so`.
//!
//! Exit 0 authenticates the user; any other status falls through to the next
//! module in the stack (normally a password prompt). Everything this process
//! reads from its environment is attacker-influenced except `PAM_USER`, which
//! `pam_exec` sets from the PAM handle itself.

mod confirm;
mod prompt;
#[cfg(test)]
mod test_pty;

use face_auth_core::environment::{self, SkipReason};
use face_auth_core::storage::EmbeddingStore;
use face_auth_core::{capture, seat, user, FaceAuth, FaceAuthConfig};
use std::env;
use std::path::Path;
use std::time::Instant;
use tracing_subscriber::{fmt, EnvFilter};

/// Refuse to authenticate a session that is not physically at this machine.
///
/// The camera is attached to the console. Without this, a remote `sudo` over
/// SSH triggers the local IR sensor, and whoever happens to be sitting at the
/// desk authenticates the remote attacker.
fn reject_remote_session() -> Result<(), String> {
    remote_session(env::var_os("PAM_RHOST").as_deref())
}

/// Unset is local; a value that isn't UTF-8 is remote (pinned by
/// `tests::a_non_utf8_rhost_is_remote_not_unset`).
fn remote_session(rhost: Option<&std::ffi::OsStr>) -> Result<(), String> {
    let Some(rhost) = rhost else {
        return Ok(());
    };
    let Some(rhost) = rhost.to_str() else {
        return Err(format!("remote session from {}", rhost.to_string_lossy()));
    };
    let rhost = rhost.trim();
    if rhost_is_local(rhost) {
        Ok(())
    } else {
        Err(format!("remote session from {rhost}"))
    }
}

/// Empty, exactly `localhost`, or a loopback IP address. Hostnames are never
/// resolved: that would let DNS decide (pinned by
/// `tests::only_loopback_addresses_and_localhost_are_local`).
fn rhost_is_local(rhost: &str) -> bool {
    rhost.is_empty()
        || rhost == "localhost"
        || rhost
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.to_canonical().is_loopback())
}

/// A scan that can't succeed: nobody at the camera over SSH, or a built-in
/// camera behind a closed lid. Checked before `FaceAuth::new`, which loads
/// both models, so the fallback to password is immediate. Never recorded as
/// a lockout failure: no face was presented.
fn skip_reason(config: &FaceAuthConfig) -> Option<SkipReason> {
    let proc_root = Path::new("/proc");
    if config.abort_if_ssh() && environment::under_ssh(proc_root, std::process::id()) {
        return Some(SkipReason::SshSession);
    }
    if config.abort_if_lid_closed() && environment::lid_closed(Path::new("/proc/acpi/button/lid")) {
        let docked_with_external_camera = config
            .device()
            .and_then(|device| capture::device_bus_path(&device))
            .is_ok_and(|bus| environment::camera_is_external(Path::new(&bus)));
        if !docked_with_external_camera {
            return Some(SkipReason::LidClosed);
        }
    }
    None
}

/// Give a freshly started screen locker time to be walked away from.
///
/// The locker runs PAM the moment it starts, so a user still at the camera
/// would be unlocked at once. Sleeps out the rest of `start_delay_ms`,
/// measured from the locker's own start time (our parent, since `pam_exec`
/// execs us directly, or its parent if that is a PAM helper, see
/// `environment::locker_pid`), so a retry on the same lock screen pays it
/// once. Not recorded against the lockout. If the age can't be read, scan
/// immediately.
fn wait_for_start_delay(config: &FaceAuthConfig) {
    let surface = environment::classify_pam_service(env::var("PAM_SERVICE").ok().as_deref());
    let delay = config.start_delay_for(surface);
    if delay.is_zero() {
        return;
    }
    let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    let locker = environment::locker_pid(Path::new("/proc"), std::os::unix::process::parent_id());
    let age = std::fs::read_to_string(format!("/proc/{locker}/stat"))
        .ok()
        .zip(std::fs::read_to_string("/proc/uptime").ok())
        .and_then(|(stat, uptime)| environment::process_age(&stat, &uptime, ticks.max(0) as u64));
    if let Some(remaining) = age.and_then(|age| delay.checked_sub(age)) {
        tracing::debug!(?surface, ?remaining, "waiting out the start delay");
        std::thread::sleep(remaining);
    }
}

/// A routine authentication outcome: no match, or a session this tool declines
/// to handle. Logged below the default level, because `pam_exec.so stdout`
/// relays our stderr to the user and this would otherwise show on every
/// failed attempt.
fn fail_auth(msg: &str) -> ! {
    tracing::info!("{msg}");
    std::process::exit(1)
}

/// A misconfiguration (no PAM user, broken config, missing model, unusable
/// camera): logged at error and to syslog, so an admin sees it without
/// `RUST_LOG`.
fn fail_setup(msg: &str) -> ! {
    tracing::error!("{msg}");
    audit(libc::LOG_ERR, msg);
    std::process::exit(1)
}

/// One line to syslog (authpriv, like pam_unix) per scan outcome. stderr
/// is lost on a lock screen and `RUST_LOG` is scrubbed there, so this is the
/// only record of what happened: `journalctl -t vinoauthface-auth`.
fn audit(priority: libc::c_int, msg: &str) {
    let Ok(msg) = std::ffi::CString::new(msg) else { return };
    unsafe {
        libc::openlog(c"vinoauthface-auth".as_ptr(), libc::LOG_PID, libc::LOG_AUTHPRIV);
        libc::syslog(priority, c"%s".as_ptr(), msg.as_ptr());
    }
}

/// Drop everything the caller put in the environment when running with
/// borrowed privileges: the set-group-ID `face-auth` group for lock screens
/// that run as the user (KScreenLocker, swaylock), or root from sudo's own
/// set-user-ID process. Only what `pam_exec` sets survives, and `PATH` is
/// pinned for anything that might still search it.
fn scrub_caller_environment() {
    let borrowed = unsafe { libc::getuid() != libc::geteuid() || libc::getgid() != libc::getegid() };
    if !borrowed {
        return;
    }
    let kept = environment_to_keep(|k| env::var_os(k), root_running_verify());
    let all: Vec<std::ffi::OsString> = env::vars_os().map(|(k, _)| k).collect();
    for key in all {
        env::remove_var(key);
    }
    for (key, value) in kept {
        env::set_var(key, value);
    }
    env::set_var("PATH", "/usr/sbin:/usr/bin:/sbin:/bin");
}

/// `sudo env RUST_LOG=... vinoauthface-auth --verify`: still "borrowed" (the
/// set-group-ID bit splits real and effective group), but the log filter is
/// root's own choice. pam_exec never passes `--verify` (the PAM line is
/// root-owned) and a lock screen's real uid isn't root.
fn root_running_verify() -> bool {
    caller_is_root() && env::args().nth(1).as_deref() == Some("--verify")
}

/// Real uid, not effective: under sudo the effective uid is 0 for any caller.
fn caller_is_root() -> bool {
    let uid = unsafe { libc::getuid() };
    uid == 0
}

/// What survives [`scrub_caller_environment`], read through `get` (pinned by
/// `tests::only_what_pam_exec_sets_survives_the_scrub`,
/// `tests::root_running_verify_keeps_its_log_filter`).
fn environment_to_keep(
    get: impl Fn(&str) -> Option<std::ffi::OsString>,
    root_verify: bool,
) -> Vec<(&'static str, std::ffi::OsString)> {
    const KEEP: [&str; 4] = ["PAM_USER", "PAM_SERVICE", "PAM_RHOST", "PAM_TTY"];
    KEEP.iter()
        .chain(root_verify.then_some(&"RUST_LOG"))
        .filter_map(|k| get(k).map(|v| (*k, v)))
        .collect()
}

/// Where the NPU driver caches compiled models; `deploy.sh` makes it
/// `root:root 0755`.
const NPU_CACHE_HOME: &str = "/var/cache/face-auth";

/// Pin the working directory and `HOME` before OpenVINO loads.
///
/// The NPU driver caches compiled blobs under `$HOME/.cache`, or a relative
/// `.cache` when `HOME` is unset. The blobs are checksummed, not
/// authenticated, so a planted one is a model the NPU runs: only root paths
/// (sudo, GDM, polkit) may write the cache. The lock screen, as the user,
/// reads it and misses rather than writes.
fn pin_process_context() {
    let _ = env::set_current_dir("/");
    env::set_var("HOME", NPU_CACHE_HOME);
}

/// A caller that is not root may only test its own face, so the
/// set-group-ID binary can't probe another account's templates or lockout
/// (pinned by `tests::another_user_may_not_probe_an_account`).
fn caller_may_authenticate(caller: u32, target_uid: u32) -> bool {
    caller == 0 || caller == target_uid
}

/// Why the PAM path stopped before loading any config or touching the camera.
#[derive(Debug, PartialEq)]
enum Refusal {
    /// [`fail_setup`]: PAM or NSS is broken, an admin should see it.
    Setup(String),
    /// [`fail_auth`]: a routine refusal.
    Auth(String),
}

/// The gates ahead of the config and the camera, in order: a `PAM_USER`
/// (never `USER`, `LOGNAME` or `id -un`, which name the caller or the
/// environment), an NSS lookup of it, a caller allowed to test that account,
/// and a local session. Every one fails closed. `remote` is
/// [`reject_remote_session`].
fn pre_scan(
    pam_user: Option<String>,
    caller_uid: u32,
    lookup: impl FnOnce(&str) -> anyhow::Result<user::UserInfo>,
    remote: impl FnOnce() -> Result<(), String>,
) -> Result<user::UserInfo, Refusal> {
    let username = match pam_user {
        Some(u) if !u.is_empty() => u,
        _ => {
            return Err(Refusal::Setup(
                "PAM_USER is not set; refusing to guess which account to authenticate".into(),
            ))
        }
    };
    let info = lookup(&username).map_err(|e| Refusal::Setup(format!("cannot authenticate '{username}': {e}")))?;
    if !caller_may_authenticate(caller_uid, info.uid) {
        return Err(Refusal::Auth(format!(
            "caller user ID {caller_uid} may not authenticate '{}'",
            info.name
        )));
    }
    remote().map_err(|reason| Refusal::Auth(format!("refusing face authentication for {reason}")))?;
    Ok(info)
}

/// The value, or `{prefix}{error}` on stderr and exit 2, for the command-line
/// modes.
fn or_exit<T>(result: Result<T, impl std::fmt::Display>, prefix: &str) -> T {
    result.unwrap_or_else(|e| {
        eprintln!("{prefix}{e}");
        std::process::exit(2)
    })
}

/// Interactive verification against a stored template, set up as the PAM
/// path is so timings match an unlock. Exits 0 on a match, 1 otherwise.
fn run_verify(name: &str) -> ! {
    if !caller_is_root() {
        eprintln!("--verify reads root-owned templates; re-run with sudo or pkexec");
        std::process::exit(2);
    }
    let info = or_exit(user::lookup(name), "");
    let config = or_exit(FaceAuthConfig::load_for_auth(&info.name), "config error: ");
    let window = config.scan_duration_ms();
    let interval = config.scan_interval_ms();
    let (mut auth, prepared) = or_exit(FaceAuth::new_for_scan(config, &info.name, |_| {}), "init error: ");
    match auth.authenticate_prepared(&info.name, prepared, window, interval) {
        Ok(true) => {
            println!("match");
            std::process::exit(0);
        }
        Ok(false) => {
            println!("no match");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    }
}

/// Compile both models as root so the NPU cache is filled before the first
/// unlock: the lock screen runs as the user and can't write it. Models,
/// backend and device are system-only settings, so the system config compiles
/// exactly what the PAM path will look up.
fn run_warm_cache() -> ! {
    if !caller_is_root() {
        eprintln!("--warm-cache writes the root-owned NPU cache; re-run with sudo");
        std::process::exit(2);
    }
    let config = or_exit(FaceAuthConfig::load_system(), "config error: ");
    if config.backend() != "openvino" {
        println!("backend is {}: nothing to cache", config.backend());
        std::process::exit(0);
    }
    let device = config.npu_device();
    // Without OpenVINO the backends fall back to tract and this would
    // "succeed" having compiled nothing.
    #[cfg(feature = "npu")]
    if face_auth_core::inference::find_openvino().is_none() {
        eprintln!("cannot compile the models for {device}: {}", face_auth_core::inference::OpenVinoMissing);
        std::process::exit(1);
    }
    let t = Instant::now();
    if let Err(e) = FaceAuth::new(config) {
        eprintln!("cannot compile the models for {device}: {e}");
        std::process::exit(1);
    }
    println!("compiled both models for {device} in {:.1?}", t.elapsed());
    std::process::exit(0);
}

/// Load OpenVINO from its fixed paths and list its devices. Reads no config
/// and no templates, so it is open to any caller.
fn run_openvino_devices() -> ! {
    #[cfg(feature = "npu")]
    match face_auth_core::inference::openvino_devices() {
        Ok((lib, devices)) => {
            println!("{}: {}", lib.display(), devices.join(" "));
            std::process::exit(0)
        }
        Err(e) => {
            eprintln!("{e:#}");
            std::process::exit(1)
        }
    }
    #[cfg(not(feature = "npu"))]
    {
        eprintln!("this build has no OpenVINO backend (tract only)");
        std::process::exit(1)
    }
}

/// Does the caller have templates? Exit 0 yes, 1 no, 2 error. For the tray,
/// which runs as the user and can't open the store. Answers only for the
/// real uid, from the system config's store path alone.
fn run_enrolled() -> ! {
    let info = or_exit(user::current(), "");
    let config = or_exit(FaceAuthConfig::load_system(), "config error: ");
    // A store we can't see into would read as "not enrolled": that's an error.
    let dir = config.embeddings_dir();
    if let Err(e) = std::fs::read_dir(&dir) {
        eprintln!("cannot read {}: {e}", dir.display());
        std::process::exit(2);
    }
    match EmbeddingStore::is_enrolled(&info.name, &dir) {
        Ok(true) => std::process::exit(0),
        Ok(false) => std::process::exit(1),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    }
}

/// Point stderr at /dev/null. Best effort: if it fails, errors show as text.
fn silence_stderr() {
    if let Ok(null) = std::fs::OpenOptions::new().write(true).open("/dev/null") {
        unsafe { libc::dup2(std::os::fd::AsRawFd::as_raw_fd(&null), libc::STDERR_FILENO) };
    }
}

fn main() {
    scrub_caller_environment();
    pin_process_context();

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("face_auth_core=error,vinoauthface_auth=error"));
    fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();

    let t0 = Instant::now();

    let argv: Vec<String> = env::args().skip(1).collect();
    if !argv.is_empty() {
        match argv.as_slice() {
            [flag, name] if flag == "--verify" => run_verify(name),
            [flag] if flag == "--warm-cache" => run_warm_cache(),
            [flag] if flag == "--enrolled" => run_enrolled(),
            // For the install scripts: the npu build loads OpenVINO at run
            // time, so ldd can't tell the builds apart.
            [flag] if flag == "--build" => {
                println!("{}", if cfg!(feature = "npu") { "openvino" } else { "tract" });
                std::process::exit(0)
            }
            [flag] if flag == "--openvino-devices" => run_openvino_devices(),
            _ => {
                eprintln!("usage: vinoauthface-auth            (PAM mode, reads PAM_USER)");
                eprintln!("       vinoauthface-auth --verify USER  (test a stored face, requires root)");
                eprintln!("       vinoauthface-auth --warm-cache   (compile the models into the NPU cache, requires root)");
                eprintln!("       vinoauthface-auth --enrolled     (exit 0 if you have enrolled a face)");
                eprintln!("       vinoauthface-auth --build        (print the backend built in: openvino or tract)");
                eprintln!("       vinoauthface-auth --openvino-devices (load OpenVINO, list what it can run on)");
                std::process::exit(2);
            }
        }
    }

    // With `pam_exec.so stdout`, stderr also reaches the lock screen as PAM
    // messages. Only the scan prompt is meant for the user; failures go to
    // `audit`.
    let relayed = prompt::relayed();
    if relayed {
        silence_stderr();
    }

    let caller = unsafe { libc::getuid() };
    let info = match pre_scan(env::var("PAM_USER").ok(), caller, user::lookup, reject_remote_session) {
        Ok(info) => info,
        Err(Refusal::Setup(msg)) => fail_setup(&msg),
        Err(Refusal::Auth(msg)) => fail_auth(&msg),
    };

    let config = match FaceAuthConfig::load_for_auth(&info.name) {
        Ok(c) => c,
        Err(e) => fail_setup(&format!("config error: {e}")),
    };

    if config.seat_check() {
        if let Err(reason) = seat::check(Path::new("/run/systemd"), info.uid) {
            fail_auth(&format!("refusing face authentication for '{}': {reason}", info.name));
        }
    }

    if let Some(reason) = skip_reason(&config) {
        fail_auth(&format!("skipping face authentication: {reason}"));
    }

    let scan_duration = config.scan_duration_ms();
    let scan_interval = config.scan_interval_ms();
    let pam_service = env::var("PAM_SERVICE").ok();
    let surface = environment::classify_pam_service(pam_service.as_deref());
    let confirm = config.require_confirmation_for(surface);
    let store_kind = if config.seal_embeddings() { "sealed" } else { "plaintext" };
    let service = pam_service.as_deref().unwrap_or("?");
    let outcome = |what: &str| {
        format!("{what} for '{}' (service {service}, {store_kind} templates)", info.name)
    };

    // The start delay runs alongside the model load, before the camera opens.
    let (mut auth, prepared) = match FaceAuth::new_for_scan(config, &info.name, wait_for_start_delay) {
        Ok(a) => a,
        Err(e) => fail_setup(&format!("init error: {e}")),
    };

    tracing::debug!(
        user = %info.name,
        window_ms = scan_duration,
        interval_ms = scan_interval,
        setup = ?t0.elapsed(),
        "starting scan"
    );

    let mut stdout = std::io::stdout();
    let relay = relayed.then_some(&mut stdout as &mut dyn std::io::Write);
    let scanning = prompt::ScanPrompt::show(env::var("PAM_TTY").ok().as_deref(), info.uid, relay);
    let result = auth.authenticate_prepared(&info.name, prepared, scan_duration, scan_interval);
    drop(scanning);
    tracing::debug!(total = ?t0.elapsed(), "scan finished");

    match result {
        Ok(true) if confirm => match check_confirmation(confirm::ask(&info.name), pam_service.as_deref()) {
            Ok(audit_what) => {
                audit(libc::LOG_INFO, &outcome(audit_what));
                std::process::exit(0)
            }
            Err((audit_what, fail_reason)) => {
                audit(libc::LOG_NOTICE, &outcome(audit_what));
                fail_auth(fail_reason)
            }
        },
        Ok(true) => {
            audit(libc::LOG_INFO, &outcome("face match"));
            std::process::exit(0)
        }
        Ok(false) => {
            audit(libc::LOG_NOTICE, &outcome("no face match"));
            fail_auth(&format!("face not recognised for '{}'", info.name))
        }
        Err(e) => fail_setup(&outcome(&format!("face authentication error: {e:#}"))),
    }
}

/// Evaluate confirmation outcome for a given `PAM_SERVICE`.
///
/// Only `polkit-1` (a desktop polkit agent dialog, which runs without a
/// controlling terminal) may proceed on [`confirm::Outcome::NoTerminal`].
/// CLI elevation tools (`sudo`, `su`, `doas`, `run0`, `pkexec`, etc.) must
/// fail closed if the caller detached from `/dev/tty` via `setsid`.
fn check_confirmation(
    outcome: confirm::Outcome,
    pam_service: Option<&str>,
) -> Result<&'static str, (&'static str, &'static str)> {
    match outcome {
        confirm::Outcome::Confirmed => Ok("face match, confirmed"),
        confirm::Outcome::NoTerminal if pam_service == Some("polkit-1") => {
            Ok("face match, no terminal")
        }
        confirm::Outcome::NoTerminal => Err((
            "face matched but no terminal to confirm",
            "face matched but was not confirmed (no controlling terminal)",
        )),
        confirm::Outcome::Declined => {
            Err(("face matched but not confirmed", "face matched but was not confirmed"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alice() -> user::UserInfo {
        user::UserInfo { name: "alice".into(), uid: 1000, home: "/home/alice".into() }
    }

    fn found(name: &str) -> anyhow::Result<user::UserInfo> {
        assert_eq!(name, "alice", "looked up a name PAM_USER didn't give");
        Ok(alice())
    }

    fn local() -> Result<(), String> {
        Ok(())
    }

    fn unreached<T>() -> T {
        panic!("a later gate ran after an earlier one refused")
    }

    #[test]
    fn the_target_or_root_from_a_local_session_gets_through() {
        assert_eq!(pre_scan(Some("alice".into()), 1000, found, local).unwrap().name, "alice");
        assert_eq!(pre_scan(Some("alice".into()), 0, found, local).unwrap().uid, 1000);
    }

    #[test]
    fn no_pam_user_is_a_setup_failure_before_any_lookup() {
        for pam_user in [None, Some(String::new())] {
            let got = pre_scan(pam_user, 0, |_| unreached(), unreached);
            assert!(matches!(got, Err(Refusal::Setup(ref m)) if m.contains("PAM_USER")), "{got:?}");
        }
    }

    #[test]
    fn an_unknown_account_is_a_setup_failure() {
        let got = pre_scan(Some("ghost".into()), 0, |_| anyhow::bail!("no such user"), unreached);
        assert!(matches!(got, Err(Refusal::Setup(ref m)) if m.contains("ghost")), "{got:?}");
    }

    #[test]
    fn another_user_may_not_probe_an_account() {
        let got = pre_scan(Some("alice".into()), 1001, found, unreached);
        assert_eq!(got.unwrap_err(), Refusal::Auth("caller user ID 1001 may not authenticate 'alice'".into()));
    }

    #[test]
    fn a_remote_session_is_refused_even_for_root() {
        let remote = || Err("remote session from 10.0.0.5".to_string());
        for caller in [0, 1000] {
            let got = pre_scan(Some("alice".into()), caller, found, remote);
            assert!(matches!(got, Err(Refusal::Auth(ref m)) if m.contains("10.0.0.5")), "{got:?}");
        }
    }

    /// What survives the scrub of a caller environment with PAM's variables
    /// and some of the caller's own.
    fn kept_keys(root_verify: bool) -> Vec<&'static str> {
        let caller_env = |k: &str| -> Option<std::ffi::OsString> {
            match k {
                "PAM_USER" => Some("alice".into()),
                "PAM_SERVICE" => Some("sudo".into()),
                "PAM_TTY" => Some("/dev/pts/0".into()),
                "RUST_LOG" | "LD_PRELOAD" | "HOME" | "FACE_AUTH_CONFIG" => Some("caller's".into()),
                _ => None,
            }
        };
        environment_to_keep(caller_env, root_verify).into_iter().map(|(k, _)| k).collect()
    }

    #[test]
    fn only_what_pam_exec_sets_survives_the_scrub() {
        assert_eq!(kept_keys(false), ["PAM_USER", "PAM_SERVICE", "PAM_TTY"]);
    }

    #[test]
    fn root_running_verify_keeps_its_log_filter() {
        assert_eq!(kept_keys(true), ["PAM_USER", "PAM_SERVICE", "PAM_TTY", "RUST_LOG"]);
    }

    #[test]
    fn only_loopback_addresses_and_localhost_are_local() {
        for local in ["", "localhost", "127.0.0.1", "127.1.2.3", "::1", "::ffff:127.0.0.1"] {
            assert!(rhost_is_local(local), "{local:?} should be local");
        }
        for remote in [
            "127.evil.example.com",
            "127.0.0.1.evil.example.com",
            "localhost.evil.example.com",
            "localhost.localdomain",
            "10.0.0.5",
            "::ffff:10.0.0.5",
            "fe80::1",
            "laptop",
        ] {
            assert!(!rhost_is_local(remote), "{remote:?} should be remote");
        }
    }

    #[test]
    fn a_non_utf8_rhost_is_remote_not_unset() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        assert!(remote_session(None).is_ok());
        assert!(remote_session(Some(OsStr::new(" 127.0.0.1 "))).is_ok());
        assert!(remote_session(Some(OsStr::new("10.0.0.5"))).is_err());
        assert!(remote_session(Some(OsStr::from_bytes(b"10.0.0.5\xff"))).is_err());
        assert!(remote_session(Some(OsStr::from_bytes(b"\xfflocalhost"))).is_err());
    }

    #[test]
    fn confirmation_without_terminal_fails_closed_except_for_polkit() {
        assert_eq!(
            check_confirmation(confirm::Outcome::Confirmed, Some("sudo")),
            Ok("face match, confirmed")
        );
        assert_eq!(
            check_confirmation(confirm::Outcome::NoTerminal, Some("polkit-1")),
            Ok("face match, no terminal")
        );
        for service in [
            Some("sudo"),
            Some("sudo-i"),
            Some("su"),
            Some("su-l"),
            Some("doas"),
            Some("run0"),
            Some("pkexec"),
            Some("login"),
            None,
        ] {
            assert!(
                check_confirmation(confirm::Outcome::NoTerminal, service).is_err(),
                "NoTerminal must fail closed for {service:?}"
            );
        }
        assert!(check_confirmation(confirm::Outcome::Declined, Some("polkit-1")).is_err());
    }
}
