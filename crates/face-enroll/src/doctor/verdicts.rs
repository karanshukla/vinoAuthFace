//! What each check concludes from what was gathered. Pure: strings and numbers in, a `Check` out.

use super::{check, Check, Status};
use face_auth_core::{cameras, update};

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
                "{l} is available (installed {current}). Update: `sudo vinoauthface-upgrade`; notes at {}",
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
/// binary can't run fails every attempt before the camera opens. An npu build
/// loads OpenVINO at run time (`openvino`: the library it would load), and
/// without one runs the same models on tract.
pub fn backend_verdict(backend: &str, npu_device: &str, npu_built: bool, openvino: Option<&std::path::Path>) -> Check {
    match (backend, openvino) {
        ("openvino", _) if !npu_built => check(
            Status::Fail,
            "backend",
            "openvino configured, but this build has no `npu` feature; every scan falls to the password",
        ),
        ("openvino", None) => check(
            Status::Warn,
            "backend",
            "openvino configured, but OpenVINO isn't installed; scans run on tract (CPU) instead",
        ),
        ("openvino", Some(lib)) => check(Status::Ok, "backend", format!("openvino on {npu_device} ({})", lib.display())),
        (other, _) => check(Status::Ok, "backend", format!("{other} (CPU)")),
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
            "no NPU compiler library found; if the driver ships none, nothing compiles for the NPU \
             (`ovfetch detect`; `ovfetch npu install` fetches it, then `sudo ./deploy.sh` installs it for PAM)",
        ),
        _ => check(Status::Ok, "NPU", "/dev/accel present, driver compiler found"),
    }
}

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

/// Every PAM verdict is `Info` when no service file carries our line.
pub fn no_service_hooked(pam: &[Check]) -> bool {
    !pam.iter().any(|c| c.status != Status::Info)
}

/// The formats `capture.rs` reads; anything else is worth a warning.
pub fn camera_format_verdict(device: &str, width: u32, height: u32, fourcc: &str) -> Check {
    let status = if matches!(fourcc.trim(), "GREY" | "YUYV" | "Y16") { Status::Ok } else { Status::Warn };
    check(status, "camera", format!("{device}: {width}x{height} {fourcc}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doctor::PINNED_MODELS;

    const OURS: &str = "auth sufficient pam_exec.so quiet stdout /usr/local/bin/vinoauthface-auth";

    #[test]
    fn one_hooked_service_is_enough_and_none_is_a_failure() {
        let info = || check(Status::Info, "PAM sudo", "");
        assert!(no_service_hooked(&[]));
        assert!(no_service_hooked(&[info(), info()]));
        assert!(!no_service_hooked(&[info(), check(Status::Ok, "PAM polkit-1", "")]));
        assert!(!no_service_hooked(&[check(Status::Warn, "PAM sudo", "")]));
    }

    #[test]
    fn a_format_capture_reads_is_ok_and_any_other_warns() {
        for fourcc in ["GREY", "YUYV", "Y16 "] {
            assert_eq!(camera_format_verdict("/dev/video2", 640, 360, fourcc).status, Status::Ok, "{fourcc}");
        }
        let mjpg = camera_format_verdict("/dev/video2", 640, 360, "MJPG");
        assert_eq!(mjpg.status, Status::Warn);
        assert_eq!(mjpg.detail, "/dev/video2: 640x360 MJPG");
    }

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
        let lib = std::path::Path::new("/usr/local/lib/face-auth/openvino/libopenvino_c.so");
        assert_eq!(backend_verdict("openvino", "NPU", false, None).status, Status::Fail);
        assert_eq!(backend_verdict("openvino", "NPU", true, Some(lib)).status, Status::Ok);
        assert_eq!(backend_verdict("openvino", "NPU", true, None).status, Status::Warn);
        assert_eq!(backend_verdict("tract", "NPU", false, None).status, Status::Ok);
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
    fn kde_slot_line_is_hooked() {
        let kde = "auth [success=done new_authtok_reqd=done default=die] pam_exec.so quiet stdout /usr/local/bin/vinoauthface-auth";
        let c = format!("{kde}\nauth substack smartcard-auth\n");
        assert_eq!(pam_verdict("kde-smartcard", &c).status, Status::Ok);
    }

    #[test]
    fn fingerprint_first_is_info() {
        let c = format!("auth sufficient pam_fprintd.so\n{OURS}\n");
        assert_eq!(pam_verdict("sudo", &c).status, Status::Info);
    }
}
