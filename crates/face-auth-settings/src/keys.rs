//! Every key in `face-auth.toml`, and editing one in a file's text without
//! disturbing the rest of it.
//!
//! The file is hand-edited and heavily commented (the shipped example is
//! mostly comments), and deploy.sh and pin-camera.sh append plain keys to its
//! end. So edits are line by line, as `setting-mode.sh` makes them: a value
//! is replaced where it is, or written over its commented-out example line
//! (`# liveness.motion_threshold = 0.01`), or else added at the end of the
//! top-level keys. A key set back to its default is commented out, the way the
//! example shows a default. Comments and other keys are left as they were.

mod lines;
mod values;

pub use lines::edit;
pub use values::{boolean, float, int, string};

/// One setting, under each name it can be spelled with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Key {
    /// The top-level spelling (`liveness_motion_threshold`), which wins
    /// over the grouped one. `None` for `liveness.preset`, which has none.
    pub flat: Option<&'static str>,
    /// The table and the key in it (`liveness`, `motion_threshold`): the
    /// spelling the example uses, and so the one written.
    pub group: Option<(&'static str, &'static str)>,
}

const fn flat(name: &'static str) -> Key {
    Key { flat: Some(name), group: None }
}

const fn grouped(flat: &'static str, table: &'static str, name: &'static str) -> Key {
    Key { flat: Some(flat), group: Some((table, name)) }
}

pub const DEVICE: Key = flat("device");
pub const THRESHOLD: Key = flat("threshold");
pub const MODEL_PATH: Key = flat("model_path");
pub const EMBEDDINGS_DIR: Key = flat("embeddings_dir");
pub const CAPTURE_TIMEOUT_MS: Key = flat("capture_timeout_ms");
pub const DETECTOR_MODEL_PATH: Key = flat("detector_model_path");
pub const DETECTOR_THRESHOLD: Key = flat("detector_threshold");
pub const SCAN_DURATION_MS: Key = flat("scan_duration_ms");
pub const SCAN_INTERVAL_MS: Key = flat("scan_interval_ms");
pub const BACKEND: Key = flat("backend");
pub const NPU_DEVICE: Key = flat("npu_device");
pub const MIN_FACE_SIZE_RATIO: Key = flat("min_face_size_ratio");
pub const PINNED_CAMERA_PATH: Key = flat("pinned_camera_path");
pub const PINNED_CAMERA_INDEX: Key = flat("pinned_camera_index");
pub const SEAL_EMBEDDINGS: Key = flat("seal_embeddings");
pub const BIND_CAMERA: Key = flat("bind_camera");
pub const UPDATE_CHECK: Key = flat("update_check");
pub const TRAY_IDLE_MINUTES: Key = flat("tray_idle_minutes");

pub const LIVENESS_PRESET: Key = Key { flat: None, group: Some(("liveness", "preset")) };
pub const LIVENESS_MOTION: Key = grouped("liveness_motion_threshold", "liveness", "motion_threshold");
pub const LIVENESS_RESIDUAL: Key =
    grouped("liveness_residual_motion_threshold", "liveness", "residual_motion_threshold");
pub const LIVENESS_WINDOW_MS: Key = grouped("liveness_window_ms", "liveness", "window_ms");
pub const LIVENESS_GRACE_MS: Key = grouped("liveness_grace_ms", "liveness", "grace_ms");

pub const LOCKOUT_THRESHOLD: Key = grouped("lockout_threshold", "lockout", "threshold");
pub const LOCKOUT_BASE_DELAY_MS: Key = grouped("lockout_base_delay_ms", "lockout", "base_delay_ms");
pub const LOCKOUT_MAX_DELAY_MS: Key = grouped("lockout_max_delay_ms", "lockout", "max_delay_ms");

pub const SEAT_CHECK: Key = grouped("seat_check", "guards", "seat_check");
pub const ABORT_IF_SSH: Key = grouped("abort_if_ssh", "guards", "abort_if_ssh");
pub const ABORT_IF_LID_CLOSED: Key = grouped("abort_if_lid_closed", "guards", "abort_if_lid_closed");
pub const START_DELAY_MS: Key = grouped("start_delay_ms", "guards", "start_delay_ms");
pub const START_DELAY_SCOPE: Key = grouped("start_delay_scope", "guards", "start_delay_scope");
pub const REQUIRE_CONFIRMATION: Key =
    grouped("require_confirmation_elevation", "guards", "require_confirmation_elevation");

/// Every key `FaceAuthConfig` reads. A test holds this to the struct.
pub const ALL: [Key; 32] = [
    DEVICE,
    THRESHOLD,
    MODEL_PATH,
    EMBEDDINGS_DIR,
    CAPTURE_TIMEOUT_MS,
    DETECTOR_MODEL_PATH,
    DETECTOR_THRESHOLD,
    SCAN_DURATION_MS,
    SCAN_INTERVAL_MS,
    BACKEND,
    NPU_DEVICE,
    MIN_FACE_SIZE_RATIO,
    PINNED_CAMERA_PATH,
    PINNED_CAMERA_INDEX,
    SEAL_EMBEDDINGS,
    BIND_CAMERA,
    UPDATE_CHECK,
    TRAY_IDLE_MINUTES,
    LIVENESS_PRESET,
    LIVENESS_MOTION,
    LIVENESS_RESIDUAL,
    LIVENESS_WINDOW_MS,
    LIVENESS_GRACE_MS,
    LOCKOUT_THRESHOLD,
    LOCKOUT_BASE_DELAY_MS,
    LOCKOUT_MAX_DELAY_MS,
    SEAT_CHECK,
    ABORT_IF_SSH,
    ABORT_IF_LID_CLOSED,
    START_DELAY_MS,
    START_DELAY_SCOPE,
    REQUIRE_CONFIRMATION,
];

impl Key {
    /// The name a new line is written with.
    pub fn written(self) -> String {
        match (self.group, self.flat) {
            (Some((table, name)), _) => format!("{table}.{name}"),
            (None, Some(flat)) => flat.to_string(),
            (None, None) => unreachable!("every key has a spelling"),
        }
    }

    /// The value set in a parsed file, flat spelling first, as
    /// `FaceAuthConfig::fold_sections` reads it.
    pub fn value(self, table: &toml::Table) -> Option<&toml::Value> {
        self.flat.and_then(|flat| table.get(flat)).or_else(|| {
            let (group, name) = self.group?;
            table.get(group)?.as_table()?.get(name)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use face_auth_core::FaceAuthConfig;

    /// `ALL` names every field `FaceAuthConfig` reads: a key added to the
    /// struct and not here would be a setting the window can't show.
    #[test]
    fn every_config_field_has_a_key() {
        let fields = format!("{:?}", FaceAuthConfig::from_toml("").unwrap());
        let inner = fields.trim_start_matches("FaceAuthConfig {").trim_end_matches('}');
        let mut missing = Vec::new();
        let fields: Vec<&str> = inner.split(", ").filter_map(|f| f.split_once(':').map(|(n, _)| n.trim())).collect();
        assert!(fields.len() > 30, "couldn't read the fields from {inner}");
        for field in fields {
            if matches!(field, "liveness" | "lockout" | "guards") {
                continue;
            }
            if !ALL.iter().any(|k| k.flat == Some(field)) {
                missing.push(field);
            }
        }
        assert!(missing.is_empty(), "no Key for {missing:?}");
    }
}
