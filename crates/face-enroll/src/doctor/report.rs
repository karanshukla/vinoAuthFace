//! How the checks are shown: the terminal table, and the anonymous `--report` markdown.

use super::{file_name, Check, Status};
use face_auth_core::{cameras, capture, FaceAuthConfig};

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
    if let Ok(device) = config.device() {
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
    print!("{}", table(checks));
    exit_code(checks)
}

fn table(checks: &[Check]) -> String {
    let mut out = String::new();
    for c in checks {
        let tag = match c.status {
            Status::Ok => "ok  ",
            Status::Warn => "warn",
            Status::Fail => "FAIL",
            Status::Info => "info",
        };
        out.push_str(&format!("[{tag}] {:<18} {}\n", c.name, c.detail));
    }
    let fails = checks.iter().filter(|c| c.status == Status::Fail).count();
    let warns = checks.iter().filter(|c| c.status == Status::Warn).count();
    out.push_str(&format!("\n{fails} failed, {warns} warning(s)\n"));
    out
}

fn exit_code(checks: &[Check]) -> i32 {
    checks.iter().any(|c| c.status == Status::Fail) as i32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doctor::check;

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
    fn only_a_failed_check_makes_the_exit_code_non_zero() {
        let warned = vec![check(Status::Ok, "a", ""), check(Status::Warn, "b", ""), check(Status::Info, "c", "")];
        assert_eq!(exit_code(&warned), 0);
        let mut failed = warned;
        failed.push(check(Status::Fail, "d", ""));
        assert_eq!(exit_code(&failed), 1);
    }

    #[test]
    fn the_table_has_a_line_per_check_and_the_counts() {
        let checks = vec![check(Status::Ok, "camera", "/dev/video2"), check(Status::Fail, "sealing", "on, but no TPM found")];
        assert_eq!(
            table(&checks),
            "[ok  ] camera             /dev/video2\n[FAIL] sealing            on, but no TPM found\n\n1 failed, 0 warning(s)\n"
        );
    }

    #[test]
    fn pretty_name_from_os_release() {
        let os = "NAME=\"Ubuntu\"\nPRETTY_NAME=\"Ubuntu 24.04.1 LTS\"\nID=ubuntu\n";
        assert_eq!(os_pretty_name(os).as_deref(), Some("Ubuntu 24.04.1 LTS"));
        assert_eq!(os_pretty_name("ID=x\n"), None);
    }
}
