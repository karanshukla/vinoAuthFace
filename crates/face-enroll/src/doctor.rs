//! `vinoauthface doctor`: check the whole install in one command.
//!
//! Every check takes its inputs as paths or strings so the verdict logic can be
//! tested against fixture files. Where auth already has a function for the
//! question (config, camera, pinning, sealing) doctor calls it, so the two can't
//! drift apart.

use face_auth_core::{
    cameras, capture, config::SYSTEM_CONFIG_PATH, lockout, seal, storage::EmbeddingStore, update, FaceAuthConfig,
};
use std::os::unix::fs::MetadataExt;
use std::path::Path;

/// Kept in step with `PAM_SERVICES` in deploy.sh.
const PAM_SERVICES: &[&str] = &[
    "sudo",
    "swaylock",
    "gdm-password",
    "polkit-1",
    "kde-fingerprint",
    "plasmalogin-fingerprint",
    "cosmic-greeter",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    Warn,
    Fail,
    Info,
}

pub struct Check {
    pub status: Status,
    pub name: String,
    pub detail: String,
}

fn check(status: Status, name: impl Into<String>, detail: impl Into<String>) -> Check {
    Check { status, name: name.into(), detail: detail.into() }
}

/// Verdict for the update check. `latest` is `None` when the fetch failed.
pub fn update_verdict(current: &str, latest: Option<&str>) -> Check {
    let name = "update";
    if update::release_number(current).is_none() {
        return check(Status::Info, name, format!("{current} is not a release build; not checked"));
    }
    match latest {
        None => check(Status::Info, name, "could not reach GitHub to check (offline?)"),
        Some(l) if update::is_newer(current, l) => check(
            Status::Info,
            name,
            format!(
                "{l} is available (installed {current}). Update: git pull, then `sudo ./deploy.sh`; notes at {}",
                update::RELEASES_URL
            ),
        ),
        Some(_) => check(Status::Ok, name, format!("{current} is the latest release")),
    }
}

/// Verdict for one PAM service file's contents.
pub fn pam_verdict(service: &str, contents: &str) -> Check {
    let name = format!("PAM {service}");
    let auth: Vec<&str> = contents
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with("auth") && !l.starts_with('#'))
        .collect();
    let ours: Vec<usize> = auth
        .iter()
        .enumerate()
        .filter(|(_, l)| l.contains("pam_exec.so") && l.contains("face-auth"))
        .map(|(i, _)| i)
        .collect();

    let Some(&first) = ours.first() else {
        return check(Status::Info, name, "not hooked (deploy.sh hooks it if the service exists)");
    };
    if ours.len() > 1 {
        return check(Status::Warn, name, "hooked more than once; every login scans twice");
    }
    let (before, after) = (&auth[..first], &auth[first + 1..]);
    if let Some(l) = after.iter().find(|l| l.contains("pam_nologin") || l.contains("pam_faillock")) {
        return check(Status::Warn, name, format!("hooked above `{l}`, which should run first"));
    }
    if before.iter().any(|l| l.contains("pam_fprintd") && l.contains("sufficient")) {
        return check(Status::Info, name, "hooked, but fingerprint is tried first");
    }
    check(Status::Ok, name, "hooked")
}

/// Owned by root and not writable by group or other.
pub fn ownership_verdict(name: &str, path: &Path) -> Check {
    match std::fs::metadata(path) {
        Err(e) => check(Status::Fail, name, format!("{}: {e}", path.display())),
        Ok(m) if m.uid() != 0 => {
            check(Status::Fail, name, format!("{} is owned by uid {}, not root", path.display(), m.uid()))
        }
        Ok(m) if m.mode() & 0o022 != 0 => {
            check(Status::Fail, name, format!("{} is group/other-writable ({:o})", path.display(), m.mode() & 0o777))
        }
        Ok(_) => check(Status::Ok, name, format!("{} root-owned, not g/o-writable", path.display())),
    }
}

/// Enrolled account names: one subdirectory each.
pub fn enrolled_users(dir: &Path) -> std::io::Result<Vec<String>> {
    let mut users: Vec<String> = std::fs::read_dir(dir)?
        .filter_map(Result::ok)
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    users.sort();
    Ok(users)
}

/// Templates enrolled with another recognition model are refused at auth, so
/// every scan for that account falls through to the password. An unknown tag
/// (a v1 file) passes, as it does at auth.
pub fn model_tag_verdict(current: &str, stored: &[(String, Option<String>)]) -> Check {
    let stale: Vec<String> = stored
        .iter()
        .filter_map(|(user, tag)| match tag {
            Some(t) if t != current => Some(format!("{user} ({t})")),
            _ => None,
        })
        .collect();
    if stale.is_empty() {
        check(Status::Ok, "model tag", format!("templates match {current}"))
    } else {
        check(
            Status::Fail,
            "model tag",
            format!("enrolled with another model than {current}: {}; re-enrol them", stale.join(", ")),
        )
    }
}

/// Auth builds the encoder with `config.backend()` before any scan; one this
/// binary can't run fails every attempt before the camera opens.
pub fn backend_verdict(backend: &str, npu_device: &str, npu_built: bool) -> Check {
    match backend {
        "openvino" if !npu_built => check(
            Status::Fail,
            "backend",
            "openvino configured, but this build has no `npu` feature; every scan falls to the password",
        ),
        "openvino" => check(Status::Ok, "backend", format!("openvino on {npu_device}")),
        other => check(Status::Ok, "backend", format!("{other} (CPU)")),
    }
}

/// Would auth accept the current camera for each enrolled account? An
/// account with no recorded camera accepts any (`cameras::check`).
pub fn binding_verdict(on: bool, device: &str, live: Option<&str>, users: &[(String, Vec<String>)]) -> Check {
    if !on {
        return check(Status::Info, "camera binding", "off (bind_camera = false)");
    }
    let refused: Vec<&str> = users
        .iter()
        .filter(|(_, ids)| cameras::check(ids, live, device).is_err())
        .map(|(user, _)| user.as_str())
        .collect();
    let unbound = users.iter().filter(|(_, ids)| ids.is_empty()).count();
    let live = live.unwrap_or("no USB ID");
    if !refused.is_empty() {
        check(
            Status::Fail,
            "camera binding",
            format!("{device} ({live}) isn't a camera {} enrolled on; re-enrol", refused.join(", ")),
        )
    } else if unbound > 0 {
        check(
            Status::Info,
            "camera binding",
            format!("{unbound} account(s) not bound to a camera yet (bound at next enrolment)"),
        )
    } else {
        check(Status::Ok, "camera binding", format!("{device} ({live}) matches every enrolment"))
    }
}

/// Accounts in a face cooldown right now. Not a failure: the password still
/// works, and the cooldown ends on its own.
pub fn lockout_verdict(locked: &[(String, u32, std::time::Duration)]) -> Check {
    if locked.is_empty() {
        return check(Status::Ok, "lockout", "no account is cooling down");
    }
    let list: Vec<String> = locked
        .iter()
        .map(|(user, failures, left)| format!("{user} ({failures} failures, {}s left)", left.as_secs().max(1)))
        .collect();
    check(
        Status::Warn,
        "lockout",
        format!("face unlock paused after failed scans: {}; the password still works", list.join(", ")),
    )
}

/// `enforce` is `/sys/fs/selinux/enforce`'s contents (`None`: no SELinux),
/// `modules` the output of `semodule -l` (`None`: couldn't run it). Without the
/// module, lock screens can't reach the camera; sudo still works.
pub fn selinux_verdict(enforce: Option<&str>, modules: Option<&str>) -> Check {
    let name = "SELinux";
    match enforce.map(str::trim) {
        None => check(Status::Info, name, "not enabled; no policy needed"),
        Some("1") => match modules {
            None => check(Status::Info, name, "enforcing; re-run with sudo to check the face_auth module"),
            Some(m) if m.lines().any(|l| l.split_whitespace().next() == Some("face_auth")) => {
                check(Status::Ok, name, "enforcing, face_auth module loaded")
            }
            Some(_) => check(
                Status::Warn,
                name,
                "enforcing without the face_auth module: lock screens can't reach the camera (sudo still works); re-run deploy.sh",
            ),
        },
        Some(_) => check(Status::Info, name, "permissive; lock screens work without the module"),
    }
}

/// Whether sudo asks for the target's password rather than the caller's
/// (`targetpw`, `rootpw`, `runaspw`), across sudoers files in the order sudo
/// reads them. Then `PAM_USER` is root, who usually has no face enrolled.
pub fn sudo_prompts_for_target(files: &[String]) -> Option<String> {
    let mut on: Option<String> = None;
    for contents in files {
        for line in contents.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            let Some(rest) = line.strip_prefix("Defaults") else { continue };
            // `Defaults:user`, `Defaults>runas` and friends scope it; a scoped
            // setting still changes whose password some sudo calls ask for.
            let rest = rest.trim_start_matches(|c: char| c != ' ' && c != '\t');
            for opt in rest.split(',').map(str::trim) {
                match opt {
                    "targetpw" | "rootpw" | "runaspw" => on = Some(opt.to_string()),
                    "!targetpw" | "!rootpw" | "!runaspw" if on.as_deref() == Some(&opt[1..]) => on = None,
                    _ => {}
                }
            }
        }
    }
    on
}

pub fn sudo_verdict(flag: Option<&str>, root_enrolled: bool) -> Check {
    match flag {
        Some(f) if !root_enrolled => check(
            Status::Warn,
            "sudo",
            format!("`Defaults {f}`: sudo authenticates root, who has no face enrolled, so sudo always asks for the password"),
        ),
        Some(f) => check(Status::Info, "sudo", format!("`Defaults {f}`: sudo matches root's face")),
        None => check(Status::Ok, "sudo", "authenticates the calling user"),
    }
}

/// sudoers and its drop-ins, in the order sudo reads them. `includedir` skips
/// names with a `.` or ending in `~`.
fn read_sudoers(etc: &Path) -> std::io::Result<Vec<String>> {
    let mut files = vec![std::fs::read_to_string(etc.join("sudoers"))?];
    if let Ok(dir) = std::fs::read_dir(etc.join("sudoers.d")) {
        let mut names: Vec<_> = dir
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| !n.contains('.') && !n.ends_with('~'))
            .collect();
        names.sort();
        for n in names {
            if let Ok(c) = std::fs::read_to_string(etc.join("sudoers.d").join(n)) {
                files.push(c);
            }
        }
    }
    Ok(files)
}

/// Where the Intel NPU driver installs its compiler library.
const NPU_LIB_DIRS: &[&str] = &["/usr/lib64", "/usr/lib/x86_64-linux-gnu", "/usr/lib", "/usr/local/lib"];

/// With `npu_device = "NPU"`, OpenVINO needs the kernel driver's
/// `/dev/accel` node and the user-space driver's compiler library; without
/// either, every model compile fails and every scan falls to the password.
pub fn npu_verdict(accel_nodes: usize, compiler: bool) -> Check {
    match (accel_nodes, compiler) {
        (0, _) => check(
            Status::Fail,
            "NPU",
            "no /dev/accel node (intel_vpu driver not loaded?); set npu_device = \"CPU\" or fix the driver",
        ),
        (_, false) => check(
            Status::Warn,
            "NPU",
            "no libnpu_driver_compiler.so found; if the driver ships no compiler, nothing compiles for the NPU (`ovfetch detect`)",
        ),
        _ => check(Status::Ok, "NPU", "/dev/accel present, driver compiler found"),
    }
}

fn npu_check() -> Check {
    let accel = std::fs::read_dir("/dev/accel")
        .map(|d| d.filter_map(Result::ok).filter(|e| e.file_name().to_string_lossy().starts_with("accel")).count())
        .unwrap_or(0);
    let compiler = NPU_LIB_DIRS.iter().any(|d| {
        std::fs::read_dir(d).is_ok_and(|mut e| {
            e.any(|f| f.is_ok_and(|f| f.file_name().to_string_lossy().starts_with("libnpu_driver_compiler.so")))
        })
    });
    npu_verdict(accel, compiler)
}

fn selinux_check() -> Check {
    let enforce = std::fs::read_to_string("/sys/fs/selinux/enforce").ok();
    let modules = (enforce.as_deref().map(str::trim) == Some("1"))
        .then(|| std::process::Command::new("semodule").arg("-l").output().ok())
        .flatten()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
    selinux_verdict(enforce.as_deref(), modules.as_deref())
}

/// The same pins `deploy.sh` verifies downloads against (`sha256sum` format).
const PINNED_MODELS: &str = include_str!("../../../config/models.sha256");

/// The pinned SHA-256 for a model file name, if it is one we ship.
pub fn pinned_sha(pins: &str, file_name: &str) -> Option<String> {
    pins.lines().find_map(|l| {
        let (sha, name) = l.split_once("  ")?;
        (name.trim() == file_name).then(|| sha.to_string())
    })
}

pub fn model_verdict(name: &str, path: &str, pinned: Option<&str>, actual: std::io::Result<String>) -> Check {
    match (actual, pinned) {
        (Err(e), _) => check(Status::Fail, name, format!("{path}: {e}")),
        (Ok(sha), Some(want)) if sha == want => check(Status::Ok, name, format!("{path} (checksum matches)")),
        (Ok(sha), Some(want)) => check(
            Status::Fail,
            name,
            format!("{path} doesn't match its pinned checksum ({sha}, want {want}); re-run deploy.sh"),
        ),
        (Ok(_), None) => check(Status::Info, name, format!("{path} isn't a model deploy.sh pins; not verified")),
    }
}

fn sha256_file(path: &str) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    std::io::copy(&mut std::fs::File::open(path)?, &mut hasher)?;
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

fn model_check(name: &str, path: &str) -> Check {
    model_verdict(name, path, pinned_sha(PINNED_MODELS, &file_name(path)).as_deref(), sha256_file(path))
}

/// `etc` is where sudoers lives (`/etc` outside tests).
pub fn run(pam_dir: &Path, etc: &Path) -> Vec<Check> {
    let mut out = vec![check(Status::Info, "version", crate::VERSION)];

    for service in PAM_SERVICES {
        if let Ok(contents) = std::fs::read_to_string(pam_dir.join(service)) {
            out.push(pam_verdict(service, &contents));
        }
    }
    if !out.iter().any(|c| c.status != Status::Info) {
        out.push(check(Status::Fail, "PAM", "no service is hooked; run deploy.sh"));
    }

    out.push(ownership_verdict("config", Path::new(SYSTEM_CONFIG_PATH)));

    let config = match FaceAuthConfig::load_system().and_then(|c| c.validate().map(|_| c)) {
        Ok(c) => c,
        Err(e) => {
            out.push(check(Status::Fail, "config", format!("{e:#}")));
            return out;
        }
    };

    if config.update_check() {
        let latest = update::release_number(crate::VERSION).and_then(|_| update::fetch_latest_tag());
        out.push(update_verdict(crate::VERSION, latest.as_deref()));
    } else {
        out.push(check(Status::Info, "update", "check disabled (update_check = false)"));
    }

    out.push(model_check("recognition model", &config.model_path()));
    out.push(model_check("detector model", &config.detector_model_path()));
    out.push(backend_verdict(&config.backend(), &config.npu_device(), cfg!(feature = "npu")));
    if cfg!(feature = "npu") && config.backend() == "openvino" && config.npu_device() == "NPU" {
        out.push(npu_check());
    }

    let dir = config.embeddings_dir();
    out.push(ownership_verdict("template store", &dir));
    let mut root_enrolled = None;
    match enrolled_users(&dir) {
        Ok(u) if u.is_empty() => {
            out.push(check(Status::Fail, "enrolment", "no one enrolled; run `sudo vinoauthface enroll --user NAME`"))
        }
        Ok(u) => {
            out.push(check(Status::Ok, "enrolment", format!("{} account(s) enrolled", u.len())));
            let mut stored = Vec::new();
            for user in u {
                match EmbeddingStore::stored_model_tag(&user, &dir) {
                    Ok(tag) => stored.push((user, tag)),
                    Err(e) => out.push(check(Status::Fail, "templates", format!("{user}: {e:#}"))),
                }
            }
            out.push(model_tag_verdict(&config.model_tag(), &stored));
            let bound: Vec<(String, Vec<String>)> = stored
                .iter()
                .map(|(user, _)| (user.clone(), cameras::load(user, &dir).unwrap_or_default()))
                .collect();
            let device = config.device();
            out.push(binding_verdict(config.bind_camera(), &device, cameras::camera_id(&device).as_deref(), &bound));
            let policy = config.lockout_policy();
            let locked: Vec<(String, u32, std::time::Duration)> = stored
                .iter()
                .filter_map(|(user, _)| match lockout::peek(user, &dir, &policy) {
                    (failures, Some(left)) => Some((user.clone(), failures, left)),
                    _ => None,
                })
                .collect();
            out.push(lockout_verdict(&locked));
            root_enrolled = Some(stored.iter().any(|(user, _)| user == "root"));
        }
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied && unsafe { libc::geteuid() } != 0 => {
            out.push(check(Status::Info, "enrolment", "store is root-only; re-run with sudo to count enrolled accounts"))
        }
        Err(e) => out.push(check(Status::Fail, "enrolment", format!("{}: {e}", dir.display()))),
    }

    if pam_dir.join("sudo").exists() {
        match read_sudoers(etc) {
            Ok(files) => {
                let flag = sudo_prompts_for_target(&files);
                // Unknown enrolment (not root) only matters when the flag is set.
                out.push(sudo_verdict(flag.as_deref(), root_enrolled.unwrap_or(false)));
            }
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                out.push(check(Status::Info, "sudo", "sudoers is root-only; re-run with sudo to check targetpw"))
            }
            Err(_) => {}
        }
    }
    out.push(selinux_check());

    // `config.device()` falls back to /dev/video0 when detection finds
    // nothing, which reads as a missing node rather than a missing camera.
    let Some(device) = config.device.clone().or_else(capture::detect_ir_camera) else {
        out.push(check(
            Status::Fail,
            "camera",
            "no IR camera detected; see `vinoauthface-camera-diag list`, or set `device`",
        ));
        return pin_and_seal(&config, out);
    };
    match capture::query_format(&device) {
        Ok((w, h, fourcc)) => {
            let fmt = capture::fourcc_to_string(fourcc);
            let status = if matches!(fmt.trim(), "GREY" | "YUYV" | "Y16") { Status::Ok } else { Status::Warn };
            out.push(check(status, "camera", format!("{device}: {w}x{h} {fmt}")));
        }
        Err(e) => out.push(check(Status::Fail, "camera", format!("{device}: {e:#}"))),
    }

    pin_and_seal(&config, out)
}

fn pin_and_seal(config: &FaceAuthConfig, mut out: Vec<Check>) -> Vec<Check> {
    if config.pinned_camera_path.is_some() {
        match config.verify_pinned_camera() {
            Ok(()) => out.push(check(Status::Ok, "camera pin", "matches current sysfs")),
            Err(e) => out.push(check(Status::Fail, "camera pin", format!("{e:#}"))),
        }
    } else {
        out.push(check(Status::Info, "camera pin", "not pinned (see pin-camera.sh)"));
    }

    if config.seal_embeddings() {
        if seal::available() {
            out.push(check(Status::Ok, "sealing", "on, TPM present"));
        } else {
            out.push(check(Status::Fail, "sealing", "on, but no TPM found"));
        }
    } else {
        out.push(check(Status::Info, "sealing", "off"));
    }

    out
}

/// What `--report` says about the machine. Nothing here may identify the
/// person or the machine: no usernames, home paths, hostnames, enrolment
/// counts or sysfs bus paths.
pub struct Facts {
    pub rows: Vec<(&'static str, String)>,
}

pub fn facts() -> Facts {
    let mut rows = vec![("version", crate::VERSION.to_string())];
    let os = std::fs::read_to_string("/etc/os-release").ok().and_then(|s| os_pretty_name(&s));
    rows.push(("distro", os.unwrap_or_else(|| "unknown".into())));
    let kernel = std::fs::read_to_string("/proc/sys/kernel/osrelease").map(|s| s.trim().to_string());
    rows.push(("kernel", kernel.unwrap_or_else(|_| "unknown".into())));
    rows.push(("build", if cfg!(feature = "npu") { "npu (OpenVINO)" } else { "tract (CPU)" }.into()));

    let Ok(config) = FaceAuthConfig::load_system() else {
        return Facts { rows };
    };
    let backend = match config.backend().as_str() {
        "openvino" => format!("openvino on {}", config.npu_device()),
        other => other.to_string(),
    };
    rows.push(("backend", backend));
    for (label, path) in [("recognition model", config.model_path()), ("detector model", config.detector_model_path())] {
        rows.push((label, file_name(&path)));
    }
    if let Some(device) = config.device.clone().or_else(capture::detect_ir_camera) {
        if let Ok(caps) = capture::query_caps(&device) {
            rows.push(("camera", format!("{} ({})", caps.card, caps.driver)));
        }
        rows.push(("USB ID", cameras::camera_id(&device).unwrap_or_else(|| "none".into())));
        if let Ok((w, h, fourcc)) = capture::query_format(&device) {
            rows.push(("format", format!("{w}x{h} {}", capture::fourcc_to_string(fourcc).trim())));
        }
    } else {
        rows.push(("camera", "no IR camera detected".into()));
    }
    Facts { rows }
}

fn file_name(path: &str) -> String {
    Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

fn os_pretty_name(os_release: &str) -> Option<String> {
    os_release
        .lines()
        .find_map(|l| l.strip_prefix("PRETTY_NAME="))
        .map(|v| v.trim().trim_matches('"').to_string())
}

/// Markdown for pasting into an issue: the facts, then each check's status by
/// name. Check details are left out, since they name accounts and paths.
pub fn markdown(facts: &Facts, checks: &[Check]) -> String {
    let mut out = String::from("### vinoauthface doctor report\n\n| | |\n|---|---|\n");
    for (k, v) in &facts.rows {
        out.push_str(&format!("| {k} | {} |\n", v.replace('|', "/")));
    }
    out.push_str("\n| Check | Status |\n|---|---|\n");
    for c in checks.iter().filter(|c| c.name != "version") {
        let status = match c.status {
            Status::Ok => "ok",
            Status::Warn => "warn",
            Status::Fail => "**FAIL**",
            Status::Info => "info",
        };
        out.push_str(&format!("| {} | {status} |\n", c.name));
    }
    out
}

/// Prints the table and returns the process exit code: 1 if anything failed.
pub fn report(checks: &[Check]) -> i32 {
    for c in checks {
        let tag = match c.status {
            Status::Ok => "ok  ",
            Status::Warn => "warn",
            Status::Fail => "FAIL",
            Status::Info => "info",
        };
        println!("[{tag}] {:<18} {}", c.name, c.detail);
    }
    let fails = checks.iter().filter(|c| c.status == Status::Fail).count();
    let warns = checks.iter().filter(|c| c.status == Status::Warn).count();
    println!("\n{fails} failed, {warns} warning(s)");
    (fails > 0) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    const OURS: &str = "auth sufficient pam_exec.so quiet stdout /usr/local/bin/vinoauthface-auth";

    #[test]
    fn binding_refuses_a_camera_nobody_enrolled_on() {
        let users = vec![
            ("alice".to_string(), vec!["2b7e:55c0".to_string()]),
            ("bob".to_string(), vec![]),
        ];
        assert_eq!(binding_verdict(true, "/dev/video2", Some("2b7e:55c0"), &users).status, Status::Info);
        let v = binding_verdict(true, "/dev/video2", Some("046d:085e"), &users);
        assert_eq!(v.status, Status::Fail);
        assert!(v.detail.contains("alice") && !v.detail.contains("bob"));
        assert_eq!(binding_verdict(false, "/dev/video2", None, &users).status, Status::Info);
        assert_eq!(binding_verdict(true, "/dev/video2", Some("2b7e:55c0"), &users[..1]).status, Status::Ok);
    }

    #[test]
    fn update_verdicts() {
        assert_eq!(update_verdict("v2", Some("v3")).status, Status::Info);
        assert!(update_verdict("v2", Some("v3")).detail.contains("v3 is available"));
        assert_eq!(update_verdict("v3", Some("v3")).status, Status::Ok);
        assert_eq!(update_verdict("v3", Some("v2")).status, Status::Ok);
        assert!(update_verdict("v2", None).detail.contains("could not reach"));
        assert!(update_verdict("dev", Some("v9")).detail.contains("not a release build"));
    }

    #[test]
    fn pins_cover_every_shipped_model() {
        for name in ["w600k_mbf.onnx", "w600k_r50.onnx", "det_500m.onnx", "version-slim-320.onnx"] {
            let sha = pinned_sha(PINNED_MODELS, name).unwrap_or_else(|| panic!("{name} not pinned"));
            assert!(sha.len() == 64 && sha.bytes().all(|b| b.is_ascii_hexdigit()), "{name}: {sha}");
        }
        assert_eq!(pinned_sha(PINNED_MODELS, "custom.onnx"), None);
    }

    #[test]
    fn model_checksum_verdicts() {
        let ok = || Ok("ab".to_string());
        assert_eq!(model_verdict("m", "/p", Some("ab"), ok()).status, Status::Ok);
        assert_eq!(model_verdict("m", "/p", Some("cd"), ok()).status, Status::Fail);
        assert_eq!(model_verdict("m", "/p", None, ok()).status, Status::Info);
        let missing = std::io::Error::from(std::io::ErrorKind::NotFound);
        assert_eq!(model_verdict("m", "/p", Some("ab"), Err(missing)).status, Status::Fail);
    }

    #[test]
    fn stale_model_tag_fails_and_unknown_passes() {
        let stored = vec![
            ("alice".to_string(), Some("w600k_mbf.onnx".to_string())),
            ("bob".to_string(), None),
        ];
        assert_eq!(model_tag_verdict("w600k_mbf.onnx", &stored).status, Status::Ok);
        let v = model_tag_verdict("w600k_r50.onnx", &stored);
        assert_eq!(v.status, Status::Fail);
        assert!(v.detail.contains("alice") && !v.detail.contains("bob"));
    }

    #[test]
    fn openvino_without_npu_build_fails() {
        assert_eq!(backend_verdict("openvino", "NPU", false).status, Status::Fail);
        assert_eq!(backend_verdict("openvino", "NPU", true).status, Status::Ok);
        assert_eq!(backend_verdict("tract", "NPU", false).status, Status::Ok);
    }

    #[test]
    fn lockout_warns_with_time_left() {
        assert_eq!(lockout_verdict(&[]).status, Status::Ok);
        let v = lockout_verdict(&[("alice".into(), 7, std::time::Duration::from_millis(30_500))]);
        assert_eq!(v.status, Status::Warn);
        assert!(v.detail.contains("alice (7 failures, 30s left)"), "{}", v.detail);
    }

    #[test]
    fn selinux_verdicts() {
        assert_eq!(selinux_verdict(None, None).status, Status::Info);
        assert_eq!(selinux_verdict(Some("0\n"), None).status, Status::Info);
        assert_eq!(selinux_verdict(Some("1\n"), None).status, Status::Info);
        let loaded = "abrt\nface_auth\nzoneminder\n";
        assert_eq!(selinux_verdict(Some("1\n"), Some(loaded)).status, Status::Ok);
        // Older semodule prints a version column.
        assert_eq!(selinux_verdict(Some("1"), Some("face_auth\t1.0\n")).status, Status::Ok);
        assert_eq!(selinux_verdict(Some("1"), Some("face_auth_other\n")).status, Status::Warn);
    }

    #[test]
    fn sudoers_targetpw() {
        let suse = "Defaults targetpw   # ask for the password of the target user\nALL ALL=(ALL) ALL\n";
        assert_eq!(sudo_prompts_for_target(&[suse.into()]).as_deref(), Some("targetpw"));
        assert_eq!(sudo_prompts_for_target(&["Defaults env_reset, rootpw\n".into()]).as_deref(), Some("rootpw"));
        assert_eq!(sudo_prompts_for_target(&["Defaults:alice runaspw\n".into()]).as_deref(), Some("runaspw"));
        // A later drop-in turns it off again.
        assert_eq!(sudo_prompts_for_target(&[suse.into(), "Defaults !targetpw\n".into()]), None);
        assert_eq!(sudo_prompts_for_target(&["# Defaults targetpw\n".into()]), None);
        assert_eq!(sudo_prompts_for_target(&["Defaults env_reset,mail_badpass\n".into()]), None);
        assert_eq!(sudo_verdict(Some("targetpw"), false).status, Status::Warn);
        assert_eq!(sudo_verdict(Some("targetpw"), true).status, Status::Info);
        assert_eq!(sudo_verdict(None, false).status, Status::Ok);
    }

    #[test]
    fn npu_needs_node_and_compiler() {
        assert_eq!(npu_verdict(0, true).status, Status::Fail);
        assert_eq!(npu_verdict(1, false).status, Status::Warn);
        assert_eq!(npu_verdict(1, true).status, Status::Ok);
    }

    #[test]
    fn report_has_status_but_no_details() {
        let facts = Facts { rows: vec![("camera", "Integrated IR | Camera (uvcvideo)".into())] };
        let checks = vec![
            check(Status::Ok, "enrolment", "2 account(s) enrolled"),
            check(Status::Fail, "model tag", "enrolled with another model: alice (w600k_mbf.onnx)"),
        ];
        let md = markdown(&facts, &checks);
        assert!(md.contains("| camera | Integrated IR / Camera (uvcvideo) |"), "{md}");
        assert!(md.contains("| model tag | **FAIL** |"), "{md}");
        assert!(!md.contains("alice") && !md.contains("2 account"), "{md}");
    }

    #[test]
    fn pretty_name_from_os_release() {
        let os = "NAME=\"Ubuntu\"\nPRETTY_NAME=\"Ubuntu 24.04.1 LTS\"\nID=ubuntu\n";
        assert_eq!(os_pretty_name(os).as_deref(), Some("Ubuntu 24.04.1 LTS"));
        assert_eq!(os_pretty_name("ID=x\n"), None);
    }

    #[test]
    fn missing_is_info() {
        assert_eq!(pam_verdict("sudo", "auth include common-auth\n").status, Status::Info);
    }

    #[test]
    fn hooked_is_ok() {
        assert_eq!(pam_verdict("sudo", &format!("#%PAM-1.0\n{OURS}\nauth include common-auth\n")).status, Status::Ok);
    }

    #[test]
    fn commented_line_does_not_count() {
        assert_eq!(pam_verdict("sudo", &format!("# {OURS}\n")).status, Status::Info);
    }

    #[test]
    fn double_insertion_warns() {
        assert_eq!(pam_verdict("sudo", &format!("{OURS}\n{OURS}\n")).status, Status::Warn);
    }

    #[test]
    fn above_faillock_warns() {
        let c = format!("{OURS}\nauth required pam_faillock.so preauth\n");
        assert_eq!(pam_verdict("sudo", &c).status, Status::Warn);
        let c = format!("auth required pam_faillock.so preauth\n{OURS}\n");
        assert_eq!(pam_verdict("sudo", &c).status, Status::Ok);
    }

    #[test]
    fn fingerprint_first_is_info() {
        let c = format!("auth sufficient pam_fprintd.so\n{OURS}\n");
        assert_eq!(pam_verdict("sudo", &c).status, Status::Info);
    }

    #[test]
    fn enrolled_users_lists_directories_only() {
        let d = std::env::temp_dir().join(format!("doctor-test-{}", std::process::id()));
        std::fs::create_dir_all(d.join("bob")).unwrap();
        std::fs::create_dir_all(d.join("alice")).unwrap();
        std::fs::write(d.join("stray"), "").unwrap();
        assert_eq!(enrolled_users(&d).unwrap(), vec!["alice", "bob"]);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn ownership_rejects_non_root_owner() {
        // A temp dir we created is owned by us; root-only holds when tests run as root.
        let d = std::env::temp_dir();
        let c = ownership_verdict("x", &d);
        if unsafe { libc::geteuid() } != 0 {
            assert_eq!(c.status, Status::Fail);
        }
    }
}
