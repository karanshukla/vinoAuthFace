use crate::user;
use anyhow::{bail, Context, Result};
use config::{Config, Environment, File};
use serde::Deserialize;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

pub const SYSTEM_CONFIG_PATH: &str = "/etc/face-auth.toml";

/// Default embeddings location. Must stay root-owned and 0700 — see deploy.sh.
pub const DEFAULT_EMBEDDINGS_DIR: &str = "/var/lib/face-auth";

// Bounds applied to every config source. A threshold near zero accepts any
// face at all, so the floor is enforced rather than merely documented.
const THRESHOLD_RANGE: std::ops::RangeInclusive<f32> = 0.3..=1.0;
const DETECTOR_THRESHOLD_RANGE: std::ops::RangeInclusive<f32> = 0.05..=1.0;
const CAPTURE_TIMEOUT_RANGE: std::ops::RangeInclusive<u64> = 100..=30_000;
const SCAN_DURATION_RANGE: std::ops::RangeInclusive<u64> = 500..=30_000;
const SCAN_INTERVAL_RANGE: std::ops::RangeInclusive<u64> = 0..=5_000;
const LIVENESS_MOTION_RANGE: std::ops::RangeInclusive<f32> = 0.0..=1.0;

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
            scan_interval_ms: Some(0),
            backend: None,
            npu_device: None,
            liveness_motion_threshold: None,
            pinned_camera_path: None,
            pinned_camera_index: None,
            lockout_threshold: None,
            lockout_base_delay_ms: None,
            lockout_max_delay_ms: None,
        }
    }
}

const USER_CONFIG_MAX_BYTES: u64 = 64 * 1024;

/// Read a user-owned file, refusing to follow a symlink at the final component.
///
/// Used for config files under a user's home: the authentication helper runs
/// as root, and a symlink there would otherwise aim root's read at a file the
/// user cannot open themselves. `O_NOFOLLOW` only covers the last component,
/// so the owner check catches a symlinked parent (`~/.config -> /root/.config`)
/// landing on a file the user does not own. `O_NONBLOCK` keeps a FIFO from
/// hanging PAM.
fn read_user_file(path: &Path, owner_uid: u32) -> std::io::Result<String> {
    use std::io::{Error, ErrorKind, Read};
    use std::os::unix::fs::MetadataExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(Error::new(ErrorKind::InvalidInput, "not a regular file"));
    }
    if meta.uid() != owner_uid {
        return Err(Error::new(ErrorKind::PermissionDenied, "not owned by the user"));
    }
    if meta.len() > USER_CONFIG_MAX_BYTES {
        return Err(Error::new(ErrorKind::InvalidData, "too large"));
    }
    let mut buf = String::new();
    file.take(USER_CONFIG_MAX_BYTES).read_to_string(&mut buf)?;
    Ok(buf)
}

impl FaceAuthConfig {
    /// Full layered load: system file, then the calling user's file, then
    /// `FACE_AUTH_*` environment overrides.
    ///
    /// Every source here is writable by whoever runs the process, so this is
    /// for unprivileged tools only — `face-enroll` and the settings GUI. The
    /// authentication path must use [`FaceAuthConfig::load_for_auth`].
    pub fn load() -> Result<Self> {
        let mut builder = Config::builder();

        let system_config = PathBuf::from(SYSTEM_CONFIG_PATH);
        if system_config.exists() {
            builder = builder.add_source(File::from(system_config));
        }

        if let Some(config_dir) = dirs::config_dir() {
            let user_config = config_dir.join("face-auth.toml");
            if user_config.exists() {
                builder = builder.add_source(File::from(user_config));
            }
        }

        // try_parsing so numeric keys coerce from their string env values.
        builder = builder.add_source(Environment::with_prefix("FACE_AUTH").try_parsing(true));

        let config: FaceAuthConfig = builder.build()?.try_deserialize()?;
        config.validate()?;
        Ok(config)
    }

    /// Configuration for the PAM authentication path.
    ///
    /// Trust model: only the root-owned system file may decide *what* is
    /// checked — camera device, models, and the embeddings directory. The
    /// target user's own config is consulted for comfort settings, and for
    /// thresholds it may only ever tighten the system value, never relax it.
    /// The environment is ignored outright.
    ///
    /// Without this split, anything running as the user (malware that never
    /// learned their password) could drop a `~/.config/face-auth.toml` with a
    /// permissive threshold, or repoint `embeddings_dir` at a directory it
    /// controls, and turn their next `sudo` into root.
    pub fn load_for_auth(username: &str) -> Result<Self> {
        let mut builder = Config::builder();
        let system_config = PathBuf::from(SYSTEM_CONFIG_PATH);
        if system_config.exists() {
            builder = builder.add_source(File::from(system_config));
        }
        let mut config: FaceAuthConfig = builder.build()?.try_deserialize()?;
        config.validate().context("invalid system config")?;

        if let Some(overlay) = load_user_overlay(username) {
            config.apply_user_overlay(&overlay);
        }

        Ok(config)
    }

    /// Merge the safe subset of a user's config over a system baseline.
    fn apply_user_overlay(&mut self, overlay: &FaceAuthConfig) {
        // Thresholds: accept only values at least as strict as the system's.
        if let Some(t) = overlay.threshold {
            if t.is_finite() && t >= self.threshold() && THRESHOLD_RANGE.contains(&t) {
                self.threshold = Some(t);
            } else {
                tracing::debug!(
                    requested = t,
                    floor = self.threshold(),
                    "ignoring user threshold: would weaken authentication"
                );
            }
        }
        if let Some(t) = overlay.detector_threshold {
            if t.is_finite()
                && t >= self.detector_threshold()
                && DETECTOR_THRESHOLD_RANGE.contains(&t)
            {
                self.detector_threshold = Some(t);
            }
        }

        if let Some(t) = overlay.liveness_motion_threshold {
            if t.is_finite()
                && t >= self.liveness_motion_threshold()
                && LIVENESS_MOTION_RANGE.contains(&t)
            {
                self.liveness_motion_threshold = Some(t);
            }
        }

        // Timing preferences cannot weaken a match decision, only how long the
        // user is willing to wait, so they are honoured within bounds.
        if let Some(v) = overlay.capture_timeout_ms {
            if CAPTURE_TIMEOUT_RANGE.contains(&v) {
                self.capture_timeout_ms = Some(v);
            }
        }
        if let Some(v) = overlay.scan_duration_ms {
            if SCAN_DURATION_RANGE.contains(&v) {
                self.scan_duration_ms = Some(v);
            }
        }
        if let Some(v) = overlay.scan_interval_ms {
            if SCAN_INTERVAL_RANGE.contains(&v) {
                self.scan_interval_ms = Some(v);
            }
        }

        // Which IR sensor to use is a preference on a multi-camera machine, so
        // it is honoured — but only after confirming the path really is an IR
        // capture device here. Taken unchecked it would let an unprivileged
        // setting aim authentication at any video source at all.
        if let Some(dev) = overlay.device.as_deref() {
            if crate::capture::is_ir_capture_device(dev) {
                self.device = Some(dev.to_string());
            } else {
                tracing::warn!(device = dev, "ignoring user camera: not an IR capture device");
            }
        }

        // model_path, detector_model_path and embeddings_dir stay system
        // policy: each decides what gets compared against what. So do the
        // camera pin and the lockout policy: a user must not be able to unpin
        // the camera or lift their own throttle.
    }

    pub fn validate(&self) -> Result<()> {
        let t = self.threshold();
        if !t.is_finite() || !THRESHOLD_RANGE.contains(&t) {
            bail!(
                "threshold {} outside safe range {}..={}",
                t,
                THRESHOLD_RANGE.start(),
                THRESHOLD_RANGE.end()
            );
        }
        let d = self.detector_threshold();
        if !d.is_finite() || !DETECTOR_THRESHOLD_RANGE.contains(&d) {
            bail!("detector_threshold {} outside safe range", d);
        }
        if !matches!(self.backend().as_str(), "tract" | "openvino") {
            bail!("backend must be \"tract\" or \"openvino\", not {:?}", self.backend());
        }
        let m = self.liveness_motion_threshold();
        if !m.is_finite() || !LIVENESS_MOTION_RANGE.contains(&m) {
            bail!("liveness_motion_threshold {} outside 0.0..=1.0", m);
        }
        if !self.embeddings_dir().is_absolute() {
            bail!("embeddings_dir must be an absolute path");
        }
        Ok(())
    }

    pub fn device(&self) -> String {
        self.device
            .clone()
            .or_else(crate::capture::detect_ir_camera)
            .unwrap_or_else(|| "/dev/video0".to_string())
    }

    pub fn threshold(&self) -> f32 {
        self.threshold.unwrap_or(0.6)
    }

    pub fn model_path(&self) -> String {
        self.model_path
            .clone()
            .unwrap_or_else(|| "/usr/local/share/face-auth/w600k_mbf.onnx".to_string())
    }

    /// Identity stored with saved embeddings, so changing `model_path` without
    /// re-enrolling is a clear error instead of comparisons across two
    /// incompatible embedding spaces. The file name only: the same model at a
    /// different path is the same model.
    pub fn model_tag(&self) -> String {
        let path = self.model_path();
        Path::new(&path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or(path)
    }

    pub fn capture_timeout_ms(&self) -> i32 {
        self.capture_timeout_ms
            .unwrap_or(5000)
            .clamp(*CAPTURE_TIMEOUT_RANGE.start(), *CAPTURE_TIMEOUT_RANGE.end()) as i32
    }

    pub fn embeddings_dir(&self) -> PathBuf {
        self.embeddings_dir
            .clone()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_EMBEDDINGS_DIR))
    }

    pub fn detector_model_path(&self) -> String {
        self.detector_model_path
            .clone()
            .unwrap_or_else(|| "/usr/local/share/face-auth/version-slim-320.onnx".to_string())
    }

    pub fn detector_threshold(&self) -> f32 {
        self.detector_threshold.unwrap_or(0.5)
    }

    pub fn scan_duration_ms(&self) -> u64 {
        self.scan_duration_ms
            .unwrap_or(5000)
            .clamp(*SCAN_DURATION_RANGE.start(), *SCAN_DURATION_RANGE.end())
    }

    /// Extra delay between scan attempts.
    ///
    /// Defaults to zero: the loop is already paced by the camera, which
    /// delivers 15 frames a second while a single attempt costs ~235ms of
    /// inference, so it cannot spin. The old 200ms default added most of a
    /// second across a handful of attempts for nothing.
    pub fn scan_interval_ms(&self) -> u64 {
        self.scan_interval_ms
            .unwrap_or(0)
            .clamp(*SCAN_INTERVAL_RANGE.start(), *SCAN_INTERVAL_RANGE.end())
    }
}

impl FaceAuthConfig {
    /// Backoff after repeated face-match failures. See `lockout::check`: it
    /// never blocks the password fallback, only how fast face attempts retry.
    /// Inference backend: "tract" (default, pure-Rust CPU) or "openvino"
    /// (needs a build with the `npu` feature; runs on `npu_device()`).
    pub fn backend(&self) -> String {
        self.backend.clone().unwrap_or_else(|| "tract".to_string())
    }

    /// OpenVINO device when `backend() == "openvino"`: "NPU", "GPU" or "CPU".
    pub fn npu_device(&self) -> String {
        self.npu_device.clone().unwrap_or_else(|| "NPU".to_string())
    }

    /// Minimum fraction of pixels that must change between consecutive face
    /// frames in a scan before a match is accepted. See
    /// `preprocess::frame_motion_fraction`. Zero disables the check.
    pub fn liveness_motion_threshold(&self) -> f32 {
        self.liveness_motion_threshold.unwrap_or(0.01)
    }

    /// If `pin-camera.sh` has pinned a camera, check that `device()` still
    /// resolves to that exact bus path and V4L2 index before trusting a frame.
    ///
    /// This is the enforcement point. The udev rule keeps `/dev/face-auth-ir`
    /// pointed at the right hardware, but this re-derives the identity from
    /// sysfs on every call and fails closed on any mismatch, independent of
    /// that rule. VID/PID is deliberately not compared: any device can claim
    /// it. A no-op until a camera is pinned.
    pub fn verify_pinned_camera(&self) -> Result<()> {
        let (Some(pinned_path), Some(pinned_index)) =
            (self.pinned_camera_path.as_deref(), self.pinned_camera_index)
        else {
            return Ok(());
        };

        let device = self.device();
        let path = crate::capture::device_bus_path(&device)?;
        let index = crate::capture::device_capture_index(&device)?;
        if path != pinned_path || index != pinned_index {
            bail!(
                "camera identity mismatch: pinned to {pinned_path} (index {pinned_index}), but \
                 {device} resolves to {path} (index {index}); refusing to trust its frames. \
                 If you replaced or moved the hardware on purpose, re-run pin-camera.sh."
            );
        }
        Ok(())
    }

    pub fn lockout_policy(&self) -> crate::lockout::LockoutPolicy {
        let d = crate::lockout::LockoutPolicy::default();
        crate::lockout::LockoutPolicy {
            threshold: self.lockout_threshold.unwrap_or(d.threshold).max(1),
            base_delay_ms: self.lockout_base_delay_ms.unwrap_or(d.base_delay_ms),
            max_delay_ms: self.lockout_max_delay_ms.unwrap_or(d.max_delay_ms),
            max_tarpit_ms: d.max_tarpit_ms,
        }
    }
}

/// Read `~/.config/face-auth.toml` for the account being authenticated.
///
/// Resolved through NSS rather than `$HOME`, which under `pam_exec` belongs to
/// whoever invoked the stack rather than to the target user. Any failure is
/// non-fatal: the system configuration alone is a valid setup.
fn load_user_overlay(username: &str) -> Option<FaceAuthConfig> {
    let info = user::lookup(username)
        .map_err(|e| tracing::debug!("no passwd entry for '{username}': {e}"))
        .ok()?;

    let path = info.home.join(".config/face-auth.toml");
    let contents = read_user_file(&path, info.uid)
        .map_err(|e| tracing::debug!("no usable user config at {}: {e}", path.display()))
        .ok()?;

    // The parse error is not logged: it quotes the offending line, and under
    // the set-user-ID lock-screen path the caller reads our stderr.
    match toml::from_str::<FaceAuthConfig>(&contents) {
        Ok(cfg) => Some(cfg),
        Err(_) => {
            tracing::warn!("ignoring malformed user config {}", path.display());
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_file_must_be_owned_by_the_user_and_not_a_symlink() {
        let dir = std::env::temp_dir().join(format!("face-auth-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("face-auth.toml");
        std::fs::write(&file, "threshold = 0.7\n").unwrap();
        let link = dir.join("link.toml");
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&file, &link).unwrap();
        let me = unsafe { libc::getuid() };

        assert!(read_user_file(&file, me).is_ok());
        assert_eq!(
            read_user_file(&file, me.wrapping_add(1)).unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied,
            "a file owned by someone else must not be read on the user's behalf"
        );
        assert!(read_user_file(&link, me).is_err(), "symlink must not be followed");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn system_baseline() -> FaceAuthConfig {
        FaceAuthConfig {
            threshold: Some(0.6),
            detector_threshold: Some(0.5),
            embeddings_dir: Some(DEFAULT_EMBEDDINGS_DIR.to_string()),
            device: Some("/dev/video3".to_string()),
            model_path: Some("/usr/local/share/face-auth/w600k_mbf.onnx".to_string()),
            ..FaceAuthConfig::default()
        }
    }

    #[test]
    fn user_overlay_cannot_weaken_threshold() {
        let mut cfg = system_baseline();
        let overlay = FaceAuthConfig {
            threshold: Some(0.1),
            ..FaceAuthConfig::default()
        };
        cfg.apply_user_overlay(&overlay);
        assert_eq!(
            cfg.threshold(),
            0.6,
            "user must not be able to relax matching"
        );
    }

    #[test]
    fn user_overlay_may_only_raise_liveness_threshold() {
        let mut cfg = system_baseline();
        cfg.apply_user_overlay(&FaceAuthConfig {
            liveness_motion_threshold: Some(0.0),
            ..FaceAuthConfig::default()
        });
        assert_eq!(cfg.liveness_motion_threshold(), 0.01);

        cfg.apply_user_overlay(&FaceAuthConfig {
            liveness_motion_threshold: Some(0.05),
            ..FaceAuthConfig::default()
        });
        assert_eq!(cfg.liveness_motion_threshold(), 0.05);
    }

    #[test]
    fn user_overlay_may_tighten_threshold() {
        let mut cfg = system_baseline();
        let overlay = FaceAuthConfig {
            threshold: Some(0.8),
            ..FaceAuthConfig::default()
        };
        cfg.apply_user_overlay(&overlay);
        assert_eq!(cfg.threshold(), 0.8);
    }

    #[test]
    fn user_overlay_cannot_redirect_lookups() {
        let mut cfg = system_baseline();
        let overlay = FaceAuthConfig {
            embeddings_dir: Some("/home/mallory/faces".to_string()),
            model_path: Some("/home/mallory/evil.onnx".to_string()),
            detector_model_path: Some("/home/mallory/evil2.onnx".to_string()),
            lockout_threshold: Some(u32::MAX),
            pinned_camera_path: Some("/sys/devices/evil".to_string()),
            pinned_camera_index: Some(9),
            ..FaceAuthConfig::default()
        };
        cfg.apply_user_overlay(&overlay);
        assert_eq!(cfg.embeddings_dir(), PathBuf::from(DEFAULT_EMBEDDINGS_DIR));
        assert!(cfg.model_path().starts_with("/usr/local/share"));
        assert!(cfg.detector_model_path().starts_with("/usr/local/share"));
        assert_eq!(cfg.lockout_policy().threshold, 5);
        assert!(cfg.pinned_camera_path.is_none() && cfg.pinned_camera_index.is_none());
    }

    #[test]
    fn user_overlay_rejects_a_camera_that_is_not_an_ir_device() {
        // No such node exists in the test environment, so the validation in
        // is_ir_capture_device must reject it and keep the system choice.
        let mut cfg = system_baseline();
        let overlay = FaceAuthConfig {
            device: Some("/dev/video99".to_string()),
            ..FaceAuthConfig::default()
        };
        cfg.apply_user_overlay(&overlay);
        assert_eq!(cfg.device, Some("/dev/video3".to_string()));

        for bogus in ["/etc/passwd", "../dev/video0", "/dev/../etc/passwd", "video0"] {
            let mut cfg = system_baseline();
            cfg.apply_user_overlay(&FaceAuthConfig {
                device: Some(bogus.to_string()),
                ..FaceAuthConfig::default()
            });
            assert_eq!(cfg.device, Some("/dev/video3".to_string()), "accepted {bogus}");
        }
    }

    #[test]
    fn user_overlay_honours_timing_within_bounds() {
        let mut cfg = system_baseline();
        let overlay = FaceAuthConfig {
            scan_duration_ms: Some(8000),
            scan_interval_ms: Some(999_999),
            ..FaceAuthConfig::default()
        };
        cfg.apply_user_overlay(&overlay);
        assert_eq!(cfg.scan_duration_ms(), 8000);
        assert_eq!(cfg.scan_interval_ms(), 0, "out-of-range value ignored");
    }

    #[test]
    fn validate_rejects_permissive_threshold() {
        let cfg = FaceAuthConfig {
            threshold: Some(0.0),
            ..FaceAuthConfig::default()
        };
        assert!(cfg.validate().is_err());

        let cfg = FaceAuthConfig {
            threshold: Some(f32::NAN),
            ..FaceAuthConfig::default()
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_rejects_relative_embeddings_dir() {
        let cfg = FaceAuthConfig {
            embeddings_dir: Some("relative/path".to_string()),
            ..FaceAuthConfig::default()
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn empty_config_deserialises_to_all_defaults() {
        // load_for_auth builds from zero sources when /etc/face-auth.toml is
        // absent. If that errored, face-auth would refuse to start on a system
        // with no system config rather than falling back to defaults.
        let cfg: FaceAuthConfig = Config::builder().build().unwrap().try_deserialize().unwrap();
        assert!(cfg.validate().is_ok());
        assert_eq!(cfg.threshold(), 0.6);
        assert_eq!(cfg.embeddings_dir(), PathBuf::from(DEFAULT_EMBEDDINGS_DIR));
    }

    #[test]
    fn model_tag_is_the_file_name() {
        let cfg = FaceAuthConfig {
            model_path: Some("/opt/models/w600k_r50.onnx".to_string()),
            ..FaceAuthConfig::default()
        };
        assert_eq!(cfg.model_tag(), "w600k_r50.onnx");
        assert_eq!(FaceAuthConfig::default().model_tag(), "w600k_mbf.onnx");
    }

    #[test]
    fn unpinned_camera_is_not_checked() {
        assert!(FaceAuthConfig::default().verify_pinned_camera().is_ok());
    }

    #[test]
    fn pinned_camera_fails_closed_when_it_cannot_be_resolved() {
        let cfg = FaceAuthConfig {
            device: Some("/dev/video-does-not-exist".to_string()),
            pinned_camera_path: Some("/sys/devices/pci0000:00/usb3/3-7/3-7:1.2".to_string()),
            pinned_camera_index: Some(0),
            ..FaceAuthConfig::default()
        };
        assert!(cfg.verify_pinned_camera().is_err());
    }

    #[test]
    fn validate_rejects_unknown_backend() {
        let cfg = FaceAuthConfig { backend: Some("cuda".to_string()), ..FaceAuthConfig::default() };
        assert!(cfg.validate().is_err());
        let cfg = FaceAuthConfig { backend: Some("openvino".to_string()), ..FaceAuthConfig::default() };
        assert!(cfg.validate().is_ok());
    }

    /// Every key the shipped example sets or documents must parse, through
    /// both paths that read TOML: the `toml` crate (the PAM user overlay) and
    /// the `config` crate (face-enroll and the tools). Uncommenting the
    /// documented keys also catches the example drifting from the struct.
    #[test]
    fn example_config_parses_through_both_loaders() {
        let raw = include_str!("../../../config/face-auth.toml.example");
        let uncommented: String = raw
            .lines()
            .map(|l| match l.strip_prefix("# ") {
                Some(rest) if rest.split_once(" = ").is_some_and(|(k, _)| {
                    !k.is_empty() && k.chars().all(|c| c.is_ascii_lowercase() || c == '_')
                }) => rest.split(" #").next().unwrap_or(rest).to_string(),
                _ => l.to_string(),
            })
            .collect::<Vec<_>>()
            .join("\n");

        for text in [raw, uncommented.as_str()] {
            let via_toml: FaceAuthConfig = toml::from_str(text).expect("toml crate parses example");
            via_toml.validate().expect("example is valid");
            let via_config: FaceAuthConfig = Config::builder()
                .add_source(config::File::from_str(text, config::FileFormat::Toml))
                .build()
                .unwrap()
                .try_deserialize()
                .expect("config crate parses example");
            assert_eq!(via_config.threshold(), via_toml.threshold());
        }

        let full: FaceAuthConfig = toml::from_str(&uncommented).unwrap();
        assert!(full.pinned_camera_index.is_some() && full.lockout_threshold.is_some());
        assert_eq!(full.backend(), "tract");
    }

    #[test]
    fn defaults_are_valid() {
        assert!(FaceAuthConfig::default().validate().is_ok());
    }
}
