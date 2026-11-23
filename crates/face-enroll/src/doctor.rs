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

mod report;
mod verdicts;

pub use report::{facts, markdown, report};
use verdicts::*;

/// Kept in step with `PAM_SERVICES` in deploy.sh. deploy.sh hooks only one
/// of the two KDE slots, kde-smartcard when fprintd owns kde-fingerprint.
const PAM_SERVICES: &[&str] = &[
    "sudo",
    "swaylock",
    "hyprlock",
    "gdm-password",
    "polkit-1",
    "kde-fingerprint",
    "kde-smartcard",
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

/// Where the Intel NPU driver installs its compiler library, and where
/// `deploy.sh` puts the one `ovfetch npu install` fetched when the driver
/// package shipped none (`face_auth_core::inference::NPU_PREFIX_DIR`).
const NPU_LIB_DIRS: &[&str] = &[
    "/usr/lib64",
    "/usr/lib/x86_64-linux-gnu",
    "/usr/lib",
    "/usr/local/lib",
    face_auth_core::paths::NPU_PREFIX_DIR,
];
/// The compiler's name by driver: 1.32 loads the first, 1.35 the second.
const NPU_COMPILER_LIBS: &[&str] = &["libnpu_driver_compiler.so", "libopenvino_intel_npu_compiler.so"];

fn npu_check() -> Check {
    let accel = std::fs::read_dir("/dev/accel")
        .map(|d| d.filter_map(Result::ok).filter(|e| e.file_name().to_string_lossy().starts_with("accel")).count())
        .unwrap_or(0);
    let compiler = NPU_LIB_DIRS.iter().any(|d| {
        std::fs::read_dir(d).is_ok_and(|mut e| {
            e.any(|f| {
                f.is_ok_and(|f| {
                    let name = f.file_name().to_string_lossy().into_owned();
                    NPU_COMPILER_LIBS.iter().any(|lib| name.starts_with(lib))
                })
            })
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

fn sha256_file(path: &str) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;

    let mut hasher = Sha256::new();
    let mut file = std::fs::File::open(path)?;
    let mut buffer = [0; 8192];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

fn model_check(name: &str, path: &str) -> Check {
    model_verdict(name, path, pinned_sha(PINNED_MODELS, &file_name(path)).as_deref(), sha256_file(path))
}

/// `etc` is where sudoers lives (`/etc` outside tests).
pub fn run(pam_dir: &Path, etc: &Path) -> Vec<Check> {
    let mut out = vec![check(Status::Info, "version", crate::VERSION)];
    out.extend(pam_checks(pam_dir));
    out.push(ownership_verdict("config", Path::new(SYSTEM_CONFIG_PATH)));

    let config = match FaceAuthConfig::load_system().and_then(|c| c.validate().map(|_| c)) {
        Ok(c) => c,
        Err(e) => {
            out.push(check(Status::Fail, "config", format!("{e:#}")));
            return out;
        }
    };

    out.push(update_check(&config));
    out.extend(model_and_backend_checks(&config));
    let root_enrolled = enrolment_checks(&config, &mut out);
    out.extend(sudo_check(pam_dir, etc, root_enrolled));
    out.push(selinux_check());
    out.push(camera_check(&config));
    pin_and_seal(&config, out)
}

fn pam_checks(pam_dir: &Path) -> Vec<Check> {
    let mut out: Vec<Check> = PAM_SERVICES
        .iter()
        .filter_map(|service| Some(pam_verdict(service, &std::fs::read_to_string(pam_dir.join(service)).ok()?)))
        .collect();
    if no_service_hooked(&out) {
        out.push(check(Status::Fail, "PAM", "no service is hooked; run deploy.sh"));
    }
    out
}

fn update_check(config: &FaceAuthConfig) -> Check {
    if !config.update_check() {
        return check(Status::Info, "update", "check disabled (update_check = false)");
    }
    let latest = update::release_number(crate::VERSION).and_then(|_| update::fetch_latest_tag());
    update_verdict(crate::VERSION, latest.as_deref())
}

fn model_and_backend_checks(config: &FaceAuthConfig) -> Vec<Check> {
    let mut out = vec![
        model_check("recognition model", &config.model_path()),
        model_check("detector model", &config.detector_model_path()),
    ];
    #[cfg(feature = "npu")]
    let openvino = face_auth_core::inference::find_openvino();
    #[cfg(not(feature = "npu"))]
    let openvino: Option<std::path::PathBuf> = None;
    out.push(backend_verdict(&config.backend(), &config.npu_device(), cfg!(feature = "npu"), openvino.as_deref()));
    if cfg!(feature = "npu") && config.backend() == "openvino" && config.npu_device() == "NPU" {
        out.push(npu_check());
    }
    out
}

/// The store and everyone in it. Returns whether root is enrolled, `None`
/// when the store could not be listed.
fn enrolment_checks(config: &FaceAuthConfig, out: &mut Vec<Check>) -> Option<bool> {
    let dir = config.embeddings_dir();
    out.push(ownership_verdict("template store", &dir));
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
            // No camera at all is reported once, by the camera check.
            if let Ok(device) = config.device() {
                out.push(binding_verdict(config.bind_camera(), &device, cameras::camera_id(&device).as_deref(), &bound));
            }
            let policy = config.lockout_policy();
            let locked: Vec<(String, u32, std::time::Duration)> = stored
                .iter()
                .filter_map(|(user, _)| match lockout::peek(user, &dir, &policy) {
                    (failures, Some(left)) => Some((user.clone(), failures, left)),
                    _ => None,
                })
                .collect();
            out.push(lockout_verdict(&locked));
            return Some(stored.iter().any(|(user, _)| user == "root"));
        }
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied && unsafe { libc::geteuid() } != 0 => {
            out.push(check(Status::Info, "enrolment", "store is root-only; re-run with sudo to count enrolled accounts"))
        }
        Err(e) => out.push(check(Status::Fail, "enrolment", format!("{}: {e}", dir.display()))),
    }
    None
}

fn sudo_check(pam_dir: &Path, etc: &Path, root_enrolled: Option<bool>) -> Option<Check> {
    if !pam_dir.join("sudo").exists() {
        return None;
    }
    match read_sudoers(etc) {
        Ok(files) => {
            let flag = sudo_prompts_for_target(&files);
            // Unknown enrolment (not root) only matters when the flag is set.
            Some(sudo_verdict(flag.as_deref(), root_enrolled.unwrap_or(false)))
        }
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            Some(check(Status::Info, "sudo", "sudoers is root-only; re-run with sudo to check targetpw"))
        }
        Err(_) => None,
    }
}

fn camera_check(config: &FaceAuthConfig) -> Check {
    let Ok(device) = config.device() else {
        return check(
            Status::Fail,
            "camera",
            "no IR camera detected; see `vinoauthface-camera-diag list`, or set `device`",
        );
    };
    match capture::query_format(&device) {
        Ok((w, h, fourcc)) => camera_format_verdict(&device, w, h, &capture::fourcc_to_string(fourcc)),
        Err(e) => check(Status::Fail, "camera", format!("{device}: {e:#}")),
    }
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

fn file_name(path: &str) -> String {
    Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

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
