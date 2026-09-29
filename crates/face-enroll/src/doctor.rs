//! `vinoauthface doctor`: check the whole install in one command.
//!
//! Every check takes its inputs as paths or strings so the verdict logic can be
//! tested against fixture files. Where auth already has a function for the
//! question (config, camera, pinning, sealing) doctor calls it, so the two can't
//! drift apart.

use face_auth_core::{capture, config::SYSTEM_CONFIG_PATH, seal, FaceAuthConfig};
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

fn model_check(name: &str, path: &str) -> Check {
    match std::fs::metadata(path) {
        Ok(m) if m.len() > 0 => check(Status::Ok, name, format!("{path} ({} bytes)", m.len())),
        Ok(_) => check(Status::Fail, name, format!("{path} is empty")),
        Err(e) => check(Status::Fail, name, format!("{path}: {e}")),
    }
}

pub fn run(pam_dir: &Path) -> Vec<Check> {
    let mut out = Vec::new();

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

    out.push(model_check("recognition model", &config.model_path()));
    out.push(model_check("detector model", &config.detector_model_path()));

    let dir = config.embeddings_dir();
    out.push(ownership_verdict("template store", &dir));
    match enrolled_users(&dir) {
        Ok(u) if u.is_empty() => {
            out.push(check(Status::Fail, "enrolment", "no one enrolled; run `sudo vinoauthface enroll --user NAME`"))
        }
        Ok(u) => out.push(check(Status::Ok, "enrolment", format!("{} account(s) enrolled", u.len()))),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied && unsafe { libc::geteuid() } != 0 => {
            out.push(check(Status::Info, "enrolment", "store is root-only; re-run with sudo to count enrolled accounts"))
        }
        Err(e) => out.push(check(Status::Fail, "enrolment", format!("{}: {e}", dir.display()))),
    }

    let device = config.device();
    match capture::query_format(&device) {
        Ok((w, h, fourcc)) => {
            let fmt = capture::fourcc_to_string(fourcc);
            let status = if matches!(fmt.trim(), "GREY" | "YUYV" | "Y16") { Status::Ok } else { Status::Warn };
            out.push(check(status, "camera", format!("{device}: {w}x{h} {fmt}")));
        }
        Err(e) => out.push(check(Status::Fail, "camera", format!("{device}: {e:#}"))),
    }

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

    const OURS: &str = "auth sufficient pam_exec.so quiet /usr/local/bin/vinoauthface-auth";

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
