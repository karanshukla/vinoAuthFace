//! What the tray shows about the install: camera, enrolment, backend, models,
//! the login screen's mode and each setting's current choice.

use crate::helper::{bash, Verb, LOGIN_MODE, SAFE_PATH, SETTING_MODE};
use face_auth_core::installed::enrolled;
use face_auth_core::{capture, update, Camera, FaceAuthConfig};
use std::collections::HashMap;
use std::path::Path;
use std::process::{Command, Stdio};

/// The optional settings window (`deploy.sh --with-settings`). When it is
/// installed, Settings opens it instead of the tray's own submenu.
pub use face_auth_core::paths::SETTINGS_WINDOW;

/// Face unlock at the KDE login screen (Plasma Login or SDDM), as `login-mode.sh` names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginMode {
    Off,
    Both,
    Face,
}

impl LoginMode {
    pub const ALL: [LoginMode; 3] = [LoginMode::Off, LoginMode::Both, LoginMode::Face];

    pub fn parse(status: &str) -> Option<LoginMode> {
        match status.trim() {
            "off" => Some(LoginMode::Off),
            "both" => Some(LoginMode::Both),
            "face" => Some(LoginMode::Face),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            LoginMode::Off => "Password only",
            LoginMode::Both => "Password and face",
            LoginMode::Face => "Face or password",
        }
    }

    pub fn verb(self) -> Verb {
        match self {
            LoginMode::Off => Verb::LoginOff,
            LoginMode::Both => Verb::LoginBoth,
            LoginMode::Face => Verb::LoginFace,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Status {
    /// The IR camera vinoauthface-auth would use, if one opens.
    pub camera: Option<String>,
    /// `None` when vinoauthface-auth is missing or could not answer.
    pub enrolled: Option<bool>,
    pub backend: String,
    /// The recognition and detector models, as `model_path` and
    /// `detector_model_path` name them.
    pub model: String,
    /// `None` without Plasma Login or SDDM, which hides the menu entry.
    pub login: Option<LoginMode>,
    /// Each setting's current choice id, from `setting-mode.sh status`; empty
    /// when it is missing (an install from before it), which hides the menu.
    pub settings: HashMap<String, String>,
    /// `SETTINGS_WINDOW` is installed.
    pub settings_window: bool,
}

impl Status {
    pub fn read() -> Status {
        let config = FaceAuthConfig::load_system().ok();
        // The system config's device is trusted as is: it is often
        // pin-camera.sh's /dev/face-auth-ir symlink, which has no sysfs node
        // of its own for the stricter IR checks to read.
        let camera = config.as_ref().and_then(|c| match &c.device {
            Some(device) => Camera::open(device).is_ok().then(|| device.clone()),
            None => capture::detect_ir_camera(),
        });
        let backend = match config.as_ref().map(|c| (c.backend(), c.npu_device())) {
            Some((b, device)) if b == "openvino" => format!("OpenVINO ({device})"),
            Some(_) => "tract (CPU)".into(),
            None => "unknown (cannot read /etc/face-auth.toml)".into(),
        };
        let model = config.as_ref().map_or("unknown".into(), |c| {
            model_names(&c.model_path(), &c.detector_model_path())
        });
        Status {
            camera,
            enrolled: enrolled(),
            backend,
            model,
            login: login_mode(),
            settings: settings(),
            settings_window: Path::new(SETTINGS_WINDOW).exists(),
        }
    }

    pub fn ready(&self) -> bool {
        self.camera.is_some() && self.enrolled == Some(true)
    }

    /// The Status submenu's read-only lines.
    pub fn lines(&self) -> Vec<String> {
        vec![
            match &self.camera {
                Some(device) => format!("Camera: {device}"),
                None => "Camera: no IR camera found".into(),
            },
            format!(
                "Face: {}",
                match self.enrolled {
                    Some(true) => "enrolled",
                    Some(false) => "not enrolled",
                    None => "unknown (check that vinoauthface is installed)",
                }
            ),
            format!("Backend: {}", self.backend),
            format!("Model: {}", self.model),
            format!("Version: {}", update::CURRENT),
            "Full check: sudo vinoauthface doctor".into(),
        ]
    }
}

/// "w600k_r50 (detector det_500m)": the file stems, which are what the model
/// table in docs/configuration.md lists.
fn model_names(model: &str, detector: &str) -> String {
    let stem = |path: &str| {
        Path::new(path)
            .file_stem()
            .map_or(path.to_string(), |n| n.to_string_lossy().into_owned())
    };
    format!("{} (detector {})", stem(model), stem(detector))
}

/// Reads /etc/pam.d, which needs no privileges.
fn login_mode() -> Option<LoginMode> {
    let out = Command::new(bash())
        .args([LOGIN_MODE, "status"])
        .env_clear()
        .env("PATH", SAFE_PATH)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    LoginMode::parse(&String::from_utf8_lossy(&out.stdout))
}

/// Reads /etc/face-auth.toml, which needs no privileges. "custom" is a value
/// the menu doesn't offer, set by hand.
fn settings() -> HashMap<String, String> {
    let out = Command::new(bash())
        .args([SETTING_MODE, "status"])
        .env_clear()
        .env("PATH", SAFE_PATH)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    let Ok(out) = out else { return HashMap::new() };
    parse_settings(&String::from_utf8_lossy(&out.stdout))
}

pub fn parse_settings(status: &str) -> HashMap<String, String> {
    status
        .lines()
        .filter_map(|l| l.split_once(' '))
        .map(|(k, v)| (k.to_string(), v.trim().to_string()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_lines_name_what_is_missing() {
        let s = Status {
            camera: None,
            enrolled: None,
            backend: "tract (CPU)".into(),
            model: "unknown".into(),
            login: None,
            settings: HashMap::new(),
            settings_window: false,
        };
        let lines = s.lines();
        assert_eq!(lines[0], "Camera: no IR camera found");
        assert_eq!(
            lines[1],
            "Face: unknown (check that vinoauthface is installed)"
        );
        assert_eq!(lines[2], "Backend: tract (CPU)");
        assert_eq!(lines[3], "Model: unknown");
        // ksni reads "_" as an access-key marker.
        assert!(lines.iter().all(|l| !l.contains('_')), "{lines:?}");

        let s = Status {
            camera: Some("/dev/video2".into()),
            enrolled: Some(true),
            backend: "OpenVINO (NPU)".into(),
            model: "w600k_r50 (detector det_500m)".into(),
            login: Some(LoginMode::Face),
            settings: HashMap::new(),
            settings_window: false,
        };
        assert!(s.ready());
        assert_eq!(s.lines()[..2], ["Camera: /dev/video2", "Face: enrolled"]);
    }

    #[test]
    fn login_modes_round_trip_through_the_script() {
        // login-mode.sh's status words, and its "none" without Plasma Login or SDDM.
        assert_eq!(LoginMode::parse("off\n"), Some(LoginMode::Off));
        assert_eq!(LoginMode::parse("both"), Some(LoginMode::Both));
        assert_eq!(LoginMode::parse("face"), Some(LoginMode::Face));
        assert_eq!(LoginMode::parse("none"), None);
        assert_eq!(LoginMode::parse(""), None);
        for m in LoginMode::ALL {
            assert_eq!(m.verb().arg(), format!("login-{m:?}").to_lowercase());
            assert!(!m.label().contains('_'));
        }
    }

    #[test]
    fn settings_status_parses_one_pair_per_line() {
        let parsed = parse_settings("security strict\nscan 5s\nconfirm custom\n");
        assert_eq!(parsed["security"], "strict");
        assert_eq!(parsed["confirm"], "custom");
        assert!(parse_settings("").is_empty());
    }

    #[test]
    fn model_names_are_the_file_stems() {
        assert_eq!(
            model_names(
                "/usr/local/share/face-auth/w600k_mbf.onnx",
                "/usr/local/share/face-auth/det_500m.onnx"
            ),
            "w600k_mbf (detector det_500m)"
        );
    }
}
