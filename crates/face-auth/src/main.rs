//! PAM authentication helper, invoked via `pam_exec.so`.
//!
//! Exit 0 authenticates the user; any other status falls through to the next
//! module in the stack (normally a password prompt). Everything this process
//! reads from its environment is attacker-influenced except `PAM_USER`, which
//! `pam_exec` sets from the PAM handle itself.

mod confirm;

use face_auth_core::environment::{self, SkipReason};
use face_auth_core::error::FaceAuthError;
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
    let Ok(rhost) = env::var("PAM_RHOST") else {
        return Ok(());
    };
    let rhost = rhost.trim();
    let local = rhost.is_empty()
        || rhost == "localhost"
        || rhost == "localhost.localdomain"
        || rhost == "::1"
        || rhost.starts_with("127.");
    if local {
        Ok(())
    } else {
        Err(format!("remote session from {rhost}"))
    }
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
        // An external IR camera still sees a docked user with the lid shut.
        let external = capture::device_bus_path(&config.device())
            .is_ok_and(|bus| environment::camera_is_external(Path::new(&bus)));
        if !external {
            return Some(SkipReason::LidClosed);
        }
    }
    None
}

/// Give a freshly started screen locker time to be walked away from.
///
/// The locker runs PAM the moment it starts, so a user still sitting at the
/// camera after locking would be unlocked at once. Sleeps out the remainder of
/// `start_delay_ms`, measured from the locker's own start time (our parent,
/// since `pam_exec` execs us directly): a retry on the same lock screen sees
/// the same start time and does not pay the delay again. Not a failed attempt,
/// so nothing is recorded against the lockout. If the age can't be read, scan
/// straight away rather than stall.
fn wait_for_start_delay(config: &FaceAuthConfig) {
    let surface = environment::classify_pam_service(env::var("PAM_SERVICE").ok().as_deref());
    let delay = config.start_delay_for(surface);
    if delay.is_zero() {
        return;
    }
    let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    let ppid = std::os::unix::process::parent_id();
    let age = std::fs::read_to_string(format!("/proc/{ppid}/stat"))
        .ok()
        .zip(std::fs::read_to_string("/proc/uptime").ok())
        .and_then(|(stat, uptime)| environment::process_age(&stat, &uptime, ticks.max(0) as u64));
    if let Some(remaining) = age.and_then(|age| delay.checked_sub(age)) {
        tracing::debug!(?surface, ?remaining, "waiting out the start delay");
        std::thread::sleep(remaining);
    }
}

/// A routine authentication outcome: no match, or a session this tool declines
/// to handle. Logged below the default level, because `pam_exec` relays our
/// stderr to the terminal and this would otherwise print on every failed sudo.
fn fail_auth(msg: &str) -> ! {
    tracing::info!("{msg}");
    std::process::exit(1)
}

/// A misconfiguration: PAM did not supply a user, the config is broken, a model
/// is missing, the camera is unusable. These are rare, actionable, and useless
/// if silent — an admin has to be able to see them without setting RUST_LOG.
fn fail_setup(msg: &str) -> ! {
    tracing::error!("{msg}");
    std::process::exit(1)
}

/// Drop everything the caller put in the environment when running with
/// borrowed privileges: the set-group-ID `face-auth` group for lock screens
/// that run as the user (KScreenLocker, swaylock), or root from sudo's own
/// set-user-ID process. Only what `pam_exec` sets survives, and `PATH` is
/// pinned because `user::lookup` runs `getent` through it.
fn scrub_caller_environment() {
    let borrowed = unsafe { libc::getuid() != libc::geteuid() || libc::getgid() != libc::getegid() };
    if !borrowed {
        return;
    }
    const KEEP: [&str; 4] = ["PAM_USER", "PAM_SERVICE", "PAM_RHOST", "PAM_TTY"];
    let kept: Vec<(&str, std::ffi::OsString)> = KEEP
        .iter()
        .filter_map(|k| env::var_os(k).map(|v| (*k, v)))
        .collect();
    let all: Vec<std::ffi::OsString> = env::vars_os().map(|(k, _)| k).collect();
    for key in all {
        env::remove_var(key);
    }
    for (key, value) in kept {
        env::set_var(key, value);
    }
    env::set_var("PATH", "/usr/sbin:/usr/bin:/sbin:/bin");
}

/// Root-owned directory the Intel NPU driver may cache compiled models in.
/// Created by `deploy.sh`, `root:root 0755`.
const NPU_CACHE_HOME: &str = "/var/cache/face-auth";

/// Pin the working directory and `HOME` before OpenVINO loads.
///
/// The NPU driver caches compiled model blobs under `$HOME/.cache`, falling
/// back to a *relative* `.cache` when `HOME` is unset, so it used whatever
/// directory face-auth was started from. Under sudo that meant root reading
/// and writing a model cache in a directory the user controls, and the blobs
/// are only checksummed, not authenticated: a planted one is a model the NPU
/// runs. Pointing `HOME` at a root-owned directory means only root paths
/// (sudo, GDM, polkit) populate the cache, and the lock screen, running as
/// the user, can read it but gets a cache miss rather than a write.
fn pin_process_context() {
    let _ = env::set_current_dir("/");
    env::set_var("HOME", NPU_CACHE_HOME);
}

/// A caller that is not root may only test its own face. Without this, the
/// set-group-ID binary would let any user probe another account's templates and
/// drive that account's lockout.
fn caller_may_authenticate(target_uid: u32) -> bool {
    let caller = unsafe { libc::getuid() };
    caller == 0 || caller == target_uid
}

/// Interactive verification against a stored template. Prints a human-readable
/// result and exits 0 on a match, 1 otherwise.
fn run_verify(name: &str) -> ! {
    // Real user ID, not effective: under sudo the effective user ID is 0 even
    // though the caller is not root.
    if unsafe { libc::getuid() } != 0 {
        eprintln!("--verify reads root-owned templates; re-run with sudo or pkexec");
        std::process::exit(2);
    }
    let info = match user::lookup(name) {
        Ok(info) => info,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let config = match FaceAuthConfig::load_for_auth(&info.name) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("config error: {e}");
            std::process::exit(2);
        }
    };
    let window = config.scan_duration_ms();
    let interval = config.scan_interval_ms();
    let mut auth = match FaceAuth::new(config) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("init error: {e}");
            std::process::exit(2);
        }
    };
    match auth.authenticate_scan(&info.name, window, interval) {
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

/// Compile both models once as root so the NPU cache is populated before the
/// first unlock. The lock screen runs as the user and cannot write the cache,
/// so without this every lock-screen unlock after a deploy recompiles both
/// models on the CPU until something root (sudo, polkit) authenticates.
/// Models, backend and device are system-only settings, so the system config
/// compiles exactly what the PAM path will look up.
fn run_warm_cache() -> ! {
    if unsafe { libc::getuid() } != 0 {
        eprintln!("--warm-cache writes the root-owned NPU cache; re-run with sudo");
        std::process::exit(2);
    }
    let config = match FaceAuthConfig::load_system() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("config error: {e}");
            std::process::exit(2);
        }
    };
    if config.backend() != "openvino" {
        println!("backend is {}: nothing to cache", config.backend());
        std::process::exit(0);
    }
    let device = config.npu_device();
    let t = Instant::now();
    if let Err(e) = FaceAuth::new(config) {
        eprintln!("cannot compile the models for {device}: {e}");
        std::process::exit(1);
    }
    println!("compiled both models for {device} in {:.1?}", t.elapsed());
    std::process::exit(0);
}

/// Does the caller have templates? Exit 0 yes, 1 no, 2 error. For the tray's
/// status line, which runs as the user and cannot open the store. Answers
/// only for the real user ID, so it says nothing about any other account, and
/// reads the store path from the system config alone.
fn run_enrolled() -> ! {
    let info = match user::current() {
        Ok(info) => info,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let config = match FaceAuthConfig::load_system() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("config error: {e}");
            std::process::exit(2);
        }
    };
    // EmbeddingStore::load reads a missing file as "not enrolled", and a
    // store it cannot see into looks missing. That is an error here, not a no.
    let dir = config.embeddings_dir();
    if let Err(e) = std::fs::read_dir(&dir) {
        eprintln!("cannot read {}: {e}", dir.display());
        std::process::exit(2);
    }
    match EmbeddingStore::load(&info.name, &dir) {
        Ok(store) if !store.embeddings.is_empty() => std::process::exit(0),
        Ok(_) => std::process::exit(1),
        Err(e) if matches!(e.downcast_ref(), Some(FaceAuthError::NoEmbeddings)) => std::process::exit(1),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    }
}

fn main() {
    scrub_caller_environment();
    pin_process_context();

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("face_auth_core=error,face_auth=error"));
    fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();

    let t0 = Instant::now();

    // `--verify <user>` tests a stored face without going through PAM. It
    // reads the same root-owned templates the PAM path does, so it requires
    // root; it grants nothing a root caller did not already have.
    let argv: Vec<String> = env::args().skip(1).collect();
    if !argv.is_empty() {
        match argv.as_slice() {
            [flag, name] if flag == "--verify" => run_verify(name),
            [flag] if flag == "--warm-cache" => run_warm_cache(),
            [flag] if flag == "--enrolled" => run_enrolled(),
            _ => {
                eprintln!("usage: face-auth            (PAM mode, reads PAM_USER)");
                eprintln!("       face-auth --verify USER  (test a stored face, requires root)");
                eprintln!("       face-auth --warm-cache   (compile the models into the NPU cache, requires root)");
                eprintln!("       face-auth --enrolled     (exit 0 if you have enrolled a face)");
                std::process::exit(2);
            }
        }
    }

    // PAM_USER is the only authoritative identity here. USER, LOGNAME and
    // `id -un` are environment strings or the *invoking* account, not the
    // account being authenticated, and trusting them lets the wrong template
    // decide the answer.
    let username = match env::var("PAM_USER") {
        Ok(u) if !u.is_empty() => u,
        _ => fail_setup("PAM_USER is not set; refusing to guess which account to authenticate"),
    };

    // Resolve through NSS, which also rejects anything that is not a real,
    // well-formed account name before it becomes a path component.
    let info = match user::lookup(&username) {
        Ok(info) => info,
        Err(e) => fail_setup(&format!("cannot authenticate '{username}': {e}")),
    };

    if !caller_may_authenticate(info.uid) {
        fail_auth(&format!(
            "caller user ID {} may not authenticate '{}'",
            unsafe { libc::getuid() },
            info.name
        ));
    }

    if let Err(reason) = reject_remote_session() {
        fail_auth(&format!("refusing face authentication for {reason}"));
    }

    // System config only, plus a strictly-narrowing overlay from the user.
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

    wait_for_start_delay(&config);

    let scan_duration = config.scan_duration_ms();
    let scan_interval = config.scan_interval_ms();
    let surface = environment::classify_pam_service(env::var("PAM_SERVICE").ok().as_deref());
    let confirm = config.require_confirmation_for(surface);

    let mut auth = match FaceAuth::new(config) {
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

    let result = auth.authenticate_scan(&info.name, scan_duration, scan_interval);
    tracing::debug!(total = ?t0.elapsed(), "scan finished");

    match result {
        Ok(true) if confirm => match confirm::ask(&info.name) {
            confirm::Outcome::Confirmed | confirm::Outcome::NoTerminal => std::process::exit(0),
            confirm::Outcome::Declined => fail_auth("face matched but was not confirmed"),
        },
        Ok(true) => std::process::exit(0),
        Ok(false) => fail_auth(&format!("face not recognised for '{}'", info.name)),
        Err(e) => fail_setup(&format!("face authentication error: {e}")),
    }
}
