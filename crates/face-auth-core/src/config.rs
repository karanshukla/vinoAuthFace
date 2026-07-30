use config::{Config, File, Environment};
use dirs;
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Deserialize, Clone)]
pub struct FaceAuthConfig {
    pub device: Option<String>,
    pub threshold: Option<f32>,
    pub model_path: Option<String>,
    pub embeddings_dir: Option<String>,
    pub capture_timeout_ms: Option<u64>,
    pub detector_model_path: Option<String>,
    pub detector_threshold: Option<f32>,
    pub scan_duration_ms: Option<u64>,
    pub scan_interval_ms: Option<u64>,
    pub backend: Option<String>,
    pub npu_device: Option<String>,
    pub liveness_motion_threshold: Option<f32>,
    pub pinned_camera_path: Option<String>,
    pub pinned_camera_index: Option<u32>,
    pub lockout_threshold: Option<u32>,
    pub lockout_base_delay_ms: Option<u64>,
    pub lockout_max_delay_ms: Option<u64>,
}

impl Default for FaceAuthConfig {
    fn default() -> Self {
        Self {
            device: None,
            threshold: Some(0.6),
            model_path: None,
            embeddings_dir: None,
            capture_timeout_ms: Some(5000),
            detector_model_path: None,
            detector_threshold: Some(0.5),
            scan_duration_ms: Some(5000),
            scan_interval_ms: None,
            backend: Some("tract".to_string()),
            npu_device: Some("NPU".to_string()),
            liveness_motion_threshold: Some(0.01),
            pinned_camera_path: None,
            pinned_camera_index: None,
            lockout_threshold: Some(5),
            lockout_base_delay_ms: Some(2_000),
            lockout_max_delay_ms: Some(300_000),
        }
    }
}

impl FaceAuthConfig {
    pub fn load() -> anyhow::Result<Self> {
        let mut builder = Config::builder();

        // System config (lower priority)
        let system_config = PathBuf::from("/etc/face-auth.toml");
        if system_config.exists() {
            builder = builder.add_source(File::from(system_config));
        }

        // User config (higher priority, overrides system)
        if let Some(config_dir) = dirs::config_dir() {
            let user_config = config_dir.join("face-auth.toml");
            if user_config.exists() {
                builder = builder.add_source(File::from(user_config));
            }
        }

        builder = builder.add_source(Environment::with_prefix("FACE_AUTH"));

        let config: FaceAuthConfig = builder.build()?.try_deserialize()?;
        Ok(config)
    }

    pub fn device(&self) -> String {
        self.device
            .clone()
            .or_else(|| detect_ir_camera())
            .unwrap_or_else(|| "/dev/video3".to_string())
    }

    pub fn threshold(&self) -> f32 {
        self.threshold.unwrap_or(0.6)
    }

    pub fn model_path(&self) -> String {
        self.model_path
            .clone()
            .or_else(|| {
                std::env::var("FACE_AUTH_MODEL_PATH").ok()
            })
            .unwrap_or_else(|| "/usr/local/share/face-auth/w600k_mbf.onnx".to_string())
    }

    /// Identity tag stored alongside saved embeddings (see `storage::EmbeddingStore`) so a
    /// `model_path` change without re-enrolling is caught as a clear error instead of silently
    /// comparing embeddings from two numerically incompatible recognition models. Just the
    /// filename, not the full path — swapping `/usr/local/share/face-auth/w600k_r50.onnx` for
    /// a copy at a different path shouldn't look like a different model.
    pub fn model_tag(&self) -> String {
        std::path::Path::new(&self.model_path())
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| self.model_path())
    }

    pub fn capture_timeout_ms(&self) -> i32 {
        self.capture_timeout_ms.unwrap_or(5000) as i32
    }

    pub fn embeddings_dir(&self) -> PathBuf {
        self.embeddings_dir
            .clone()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/var/lib/face-auth"))
    }

    pub fn detector_model_path(&self) -> String {
        self.detector_model_path
            .clone()
            .or_else(|| std::env::var("FACE_AUTH_DETECTOR_MODEL_PATH").ok())
            .unwrap_or_else(|| "/usr/local/share/face-auth/version-slim-320.onnx".to_string())
    }

    pub fn detector_threshold(&self) -> f32 {
        self.detector_threshold.unwrap_or(0.5)
    }

    pub fn scan_duration_ms(&self) -> u64 {
        self.scan_duration_ms.unwrap_or(5000)
    }

    /// Delay between scan attempts. If explicitly configured (`scan_interval_ms` in TOML or
    /// `FACE_AUTH_SCAN_INTERVAL_MS`), that value always wins. Otherwise this queries the
    /// currently configured camera's native frame interval via V4L2 and uses that directly —
    /// there's no point sleeping longer than the driver's own frame rate, and different
    /// hardware reports very different native rates, so no single hardcoded default is portable
    /// across machines. Falls back to a conservative 100ms if the query fails (e.g. driver
    /// doesn't report `V4L2_CAP_TIMEPERFRAME`).
    pub fn scan_interval_ms(&self) -> u64 {
        self.scan_interval_ms
            .or_else(|| crate::capture::query_frame_interval_ms(&self.device()))
            .unwrap_or(100)
    }

    /// Inference backend: "tract" (default, pure-Rust CPU) or "openvino" (requires the `npu`
    /// build feature; runs on the device named by `npu_device()`).
    pub fn backend(&self) -> String {
        self.backend.clone().unwrap_or_else(|| "tract".to_string())
    }

    /// OpenVINO device string used when `backend() == "openvino"`, e.g. "NPU", "GPU", "CPU".
    pub fn npu_device(&self) -> String {
        self.npu_device.clone().unwrap_or_else(|| "NPU".to_string())
    }

    /// Minimum fraction of pixels that must change between consecutive face frames (during a
    /// scan) for the subject to be considered live rather than a static photo. See
    /// `preprocess::frame_motion_fraction`.
    pub fn liveness_motion_threshold(&self) -> f32 {
        self.liveness_motion_threshold.unwrap_or(0.01)
    }

    /// If a camera has been pinned (via `pin-camera.sh`), verifies that `device()` currently
    /// resolves to that exact physical bus path and V4L2 function index before any frame from
    /// it is trusted for authentication or enrollment. This is the actual enforcement point:
    /// `pin-camera.sh`'s udev rule keeps a stable symlink pointed at the pinned hardware, but
    /// this check doesn't depend on that rule being correct or even present — it re-derives the
    /// device's real identity from sysfs every time and refuses to proceed on any mismatch,
    /// fail-closed. VID/PID isn't part of the comparison: any USB device can claim the real
    /// camera's VID/PID, but it can't occupy the real camera's exact physical port at the same
    /// time. Does nothing (returns `Ok`) if no camera has been pinned — pinning is opt-in so
    /// existing installs aren't broken by upgrading.
    pub fn verify_pinned_camera(&self) -> anyhow::Result<()> {
        let (Some(pinned_path), Some(pinned_index)) =
            (self.pinned_camera_path.as_ref(), self.pinned_camera_index)
        else {
            return Ok(());
        };

        let device = self.device();
        let actual_path = crate::capture::device_bus_path(&device)?;
        let actual_index = crate::capture::device_capture_index(&device)?;

        if &actual_path != pinned_path || actual_index != pinned_index {
            anyhow::bail!(
                "Camera identity mismatch: pinned to {} (index {}), but {} currently resolves to \
                 {} (index {}) — refusing to trust frames from an unrecognized camera. If you \
                 intentionally replaced or moved the hardware, re-run pin-camera.sh.",
                pinned_path, pinned_index, device, actual_path, actual_index
            );
        }
        Ok(())
    }

    /// Backoff policy applied after repeated face-match failures for a user. See
    /// `lockout::check` for how this is enforced — it never blocks PAM's password
    /// fallback, only throttles how fast repeated face-auth attempts can be retried.
    pub fn lockout_policy(&self) -> crate::lockout::LockoutPolicy {
        let defaults = crate::lockout::LockoutPolicy::default();
        crate::lockout::LockoutPolicy {
            threshold: self.lockout_threshold.unwrap_or(defaults.threshold),
            base_delay_ms: self.lockout_base_delay_ms.unwrap_or(defaults.base_delay_ms),
            max_delay_ms: self.lockout_max_delay_ms.unwrap_or(defaults.max_delay_ms),
            max_tarpit_ms: defaults.max_tarpit_ms,
        }
    }
}

fn detect_ir_camera() -> Option<String> {
    use std::fs;
    let base = std::path::Path::new("/sys/class/video4linux");
    if let Ok(entries) = fs::read_dir(base) {
        for entry in entries.flatten() {
            let name_path = entry.path().join("name");
            if let Ok(name) = fs::read_to_string(&name_path) {
                if name.to_lowercase().contains("ir") || name.to_lowercase().contains("infrared") {
                    if let Some(device_name) = entry.file_name().to_str() {
                        return Some(format!("/dev/{}", device_name));
                    }
                }
            }
        }
    }
    None
}