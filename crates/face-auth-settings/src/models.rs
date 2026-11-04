//! Loading a model the way authentication would, to say whether it works
//! before anyone has to re-enrol with it. Always with tract on the CPU: the
//! window is never an OpenVINO build.

use face_auth_core::detector::{DetectorKind, FaceDetector};
use face_auth_core::inference::FaceEncoder;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tract_onnx::prelude::tract_ndarray::Array3;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Kind {
    Encoder,
    Detector,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Check {
    Running,
    Done(Result<String, String>),
}

pub type Checks = Arc<Mutex<HashMap<(Kind, String), Check>>>;

pub fn start(checks: &Checks, kind: Kind, path: String, ctx: &eframe::egui::Context) {
    checks.lock().unwrap().insert((kind, path.clone()), Check::Running);
    let checks = Arc::clone(checks);
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let result = check(kind, &path).map_err(|e| format!("{e:#}"));
        checks.lock().unwrap().insert((kind, path), Check::Done(result));
        ctx.request_repaint();
    });
}

fn check(kind: Kind, path: &str) -> anyhow::Result<String> {
    match kind {
        Kind::Encoder => {
            let mut encoder = FaceEncoder::new(path, "tract", "CPU")?;
            let face = Array3::<f32>::zeros((3, 112, 112));
            let values = encoder.encode(face.view())?.len();
            let note = if values == 512 { "" } else { ", where the shipped models give 512" };
            Ok(format!("Loads, and gives {values} values per face{note}."))
        }
        Kind::Detector => {
            let detector = FaceDetector::new(path, 0.5, "tract", "CPU")?;
            // Any kind but the slim one gives the five points; matched this way
            // so a new detector format needs nothing here.
            Ok(match detector.kind() {
                DetectorKind::Slim => "Loads as a box-only detector: faces are cropped, not aligned.".into(),
                _with_points => "Loads, with five face points: faces are aligned by them.".into(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_that_isnt_a_model_says_so() {
        let dir = std::env::temp_dir().join(format!("vinoauthface-settings-check-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let junk = dir.join("junk.onnx");
        std::fs::write(&junk, b"not a model").unwrap();
        for kind in [Kind::Encoder, Kind::Detector] {
            assert!(check(kind, junk.to_str().unwrap()).is_err());
            assert!(check(kind, "/no/such/model.onnx").unwrap_err().to_string().contains("not found"));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The shipped models, where deploy.sh has installed them (not in CI).
    #[test]
    fn the_shipped_models_check_out() {
        let encoder = "/usr/local/share/face-auth/w600k_mbf.onnx";
        if std::path::Path::new(encoder).exists() {
            assert_eq!(check(Kind::Encoder, encoder).unwrap(), "Loads, and gives 512 values per face.");
        }
        let detector = "/usr/local/share/face-auth/det_500m.onnx";
        if std::path::Path::new(detector).exists() {
            assert_eq!(
                check(Kind::Detector, detector).unwrap(),
                "Loads, with five face points: faces are aligned by them."
            );
            // A detector in the recognition slot can load, but doesn't give 512.
            let wrong = check(Kind::Encoder, detector).unwrap_or_else(|e| e.to_string());
            assert!(!wrong.contains("gives 512 values"), "{wrong}");
        }
    }
}
