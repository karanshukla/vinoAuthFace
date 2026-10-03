pub mod cameras;
pub mod capture;
pub mod config;
pub mod detector;
pub mod environment;
pub mod error;
pub mod inference;
pub mod lockout;
pub mod preprocess;
pub mod scrfd;
pub mod seal;
pub mod seat;
pub mod storage;
pub mod update;
pub mod user;
pub mod verify;

pub use crate::capture::Camera;
pub use crate::config::FaceAuthConfig;
use crate::detector::{assess_frame, FaceBox, FaceDetector, FrameQuality};
use crate::error::FaceAuthError;
use crate::inference::FaceEncoder;
use crate::storage::EmbeddingStore;
use crate::verify::verify_embedding;
use anyhow::Result;
use std::time::{Duration, Instant};

/// Margin added around the detected face box before cropping, as a fraction of
/// the box's own size per side, so the encoder sees forehead-to-chin framing
/// like its training crops rather than a razor-tight box. Public so offline
/// tooling can reproduce the exact crop.
pub const FACE_CROP_MARGIN: f32 = 0.3;

/// The encoder input for a detected face: warped onto the ArcFace template
/// when the detector found landmarks (SCRFD), otherwise the box crop with
/// `FACE_CROP_MARGIN`. Public so offline tooling encodes exactly as auth does.
pub fn face_input(
    frame: &crate::capture::IrFrame,
    face_box: &FaceBox,
) -> Result<tract_onnx::prelude::tract_ndarray::Array3<f32>> {
    match face_box.landmarks {
        Some(_) => crate::scrfd::align(frame, face_box),
        None => crate::preprocess::preprocess_ir_frame(&crate::preprocess::crop_to_face(frame, face_box, FACE_CROP_MARGIN)?),
    }
}

/// Most face frames kept for `liveness_window_ms`, whatever the camera's
/// frame rate: about 1 s at 15 pairs a second, and a bound on memory.
const MAX_LIVENESS_FRAMES: usize = 16;

/// Capture errors in a row before a scan gives up and reports the error.
const MAX_CONSECUTIVE_CAPTURE_ERRORS: u32 = 3;

/// Both thresholds at zero turn motion liveness off.
fn liveness_disabled(motion_threshold: f32, residual_threshold: f32) -> bool {
    motion_threshold <= 0.0 && residual_threshold <= 0.0
}

/// `total` (against the oldest face frame in the window) defeats a static
/// photo; `local` (against the previous frame) defeats one moved by hand.
fn motion_passes(total: f32, local: f32, motion_threshold: f32, residual_threshold: f32) -> bool {
    total >= motion_threshold && local >= residual_threshold
}

/// Once per scan, a face that matched but hasn't moved enough gets `grace`
/// more time. A scan that never matched never extends.
fn grace_extends_scan(liveness_pending: bool, already_extended: bool, grace: Duration) -> bool {
    liveness_pending && !already_extended && !grace.is_zero()
}

/// The oldest face frame is dropped once it falls outside the liveness window
/// or the buffer is over `MAX_LIVENESS_FRAMES`; the newest is always kept as
/// the baseline.
fn drop_oldest_face(frames_kept: usize, oldest_age: Duration, window: Duration) -> bool {
    frames_kept > 1 && (oldest_age > window || frames_kept > MAX_LIVENESS_FRAMES)
}

/// Progress reporting for the interactive enrolment paths.
///
/// The library never writes to stdout itself — `face-auth` runs under
/// `pam_exec`, where stray output lands on the user's terminal on every
/// `sudo`. Callers that *are* interactive supply a sink.
pub type ProgressFn<'a> = &'a mut dyn FnMut(EnrollProgress);

#[derive(Debug, Clone)]
pub enum EnrollProgress {
    Capturing { captured: usize, wanted: usize, attempt: usize },
    NoContent,
    NoFace,
    FaceTooSmall,
    Captured { captured: usize, wanted: usize },
}

pub struct FaceAuth {
    config: FaceAuthConfig,
    encoder: FaceEncoder,
    detector: FaceDetector,
}

impl FaceAuth {
    pub fn new(config: FaceAuthConfig) -> Result<Self> {
        config.validate()?;
        let (backend, device) = (config.backend(), config.npu_device());
        let encoder = FaceEncoder::new(&config.model_path(), &backend, &device)?;
        let detector = FaceDetector::new(
            &config.detector_model_path(),
            config.detector_threshold(),
            &backend,
            &device,
        )?;
        Ok(Self { config, encoder, detector })
    }

    pub fn config(&self) -> &FaceAuthConfig {
        &self.config
    }

    /// Refuse a store enrolled under a different recognition model than the
    /// configured one. Unknown-origin (v1) stores always pass.
    fn check_model_tag(&self, store: &EmbeddingStore) -> Result<()> {
        let current = self.config.model_tag();
        if !store.model_tag_matches(&current) {
            anyhow::bail!(
                "templates were enrolled with a different recognition model or detector ({}) than \
                 the one configured ({current}); re-enrol with `sudo vinoauthface enroll`",
                store.model_tag.as_deref().unwrap_or("unknown")
            );
        }
        Ok(())
    }

    /// Refuse a camera `user` didn't enrol on, before it is opened. An error,
    /// not a failed attempt: nothing is recorded against the lockout, and PAM
    /// falls through to the password. See `cameras`.
    fn check_camera_binding(&self, user: &str) -> Result<()> {
        if !self.config.bind_camera() {
            return Ok(());
        }
        let enrolled = cameras::load(user, &self.config.embeddings_dir())?;
        let device = self.config.device()?;
        cameras::check(&enrolled, cameras::camera_id(&device).as_deref(), &device)
    }

    /// Single-shot verification. The PAM path uses
    /// [`FaceAuth::authenticate_scan`].
    pub fn authenticate_once(&mut self, user: &str) -> Result<bool> {
        self.config.verify_pinned_camera()?;
        self.check_camera_binding(user)?;
        let embeddings_dir = self.config.embeddings_dir();
        if lockout::check(user, &embeddings_dir, &self.config.lockout_policy()).is_some() {
            return Ok(false);
        }

        let t0 = Instant::now();
        let store = EmbeddingStore::load_with(
            user,
            &self.config.embeddings_dir(),
            self.config.seal_embeddings(),
        )?;
        self.check_model_tag(&store)?;
        tracing::debug!(elapsed = ?t0.elapsed(), "store loaded");

        let t1 = Instant::now();
        let frame = crate::capture::capture_ir_frame(
            &self.config.device()?,
            self.config.capture_timeout_ms(),
        )?;
        tracing::debug!(elapsed = ?t1.elapsed(), "frame captured");

        let quality = assess_frame(&frame);
        if quality != FrameQuality::Ok {
            tracing::debug!(%quality, "frame rejected before inference");
            return Err(FaceAuthError::NoFaceDetected.into());
        }

        let mut frame = frame;
        crate::preprocess::histogram_equalize(&mut frame);

        let Some(face_box) = self.detector.detect(&frame)? else {
            return Err(FaceAuthError::NoFaceDetected.into());
        };
        if face_box.is_smaller_than(self.config.min_face_size_ratio()) {
            tracing::debug!(ratio = face_box.size_ratio(), "face too small");
            return Err(FaceAuthError::NoFaceDetected.into());
        }
        if !record_pending_failure(user, &embeddings_dir) {
            return Ok(false);
        }

        let input = face_input(&frame, &face_box)?;
        let embedding = self.encoder.encode(input.view())?;
        tracing::debug!(elapsed = ?t0.elapsed(), "authenticate_once complete");

        let matched = verify_embedding(&embedding, &store, self.config.threshold())?;
        if matched {
            record_success(user, &embeddings_dir);
        }
        Ok(matched)
    }

    /// Keep capturing until a frame matches or the scan window closes.
    pub fn authenticate_scan(
        &mut self,
        user: &str,
        duration_ms: u64,
        interval_ms: u64,
    ) -> Result<bool> {
        self.config.verify_pinned_camera()?;
        self.check_camera_binding(user)?;
        let embeddings_dir = self.config.embeddings_dir();
        if lockout::check(user, &embeddings_dir, &self.config.lockout_policy()).is_some() {
            return Ok(false);
        }

        let t0 = Instant::now();
        let store = EmbeddingStore::load_with(
            user,
            &self.config.embeddings_dir(),
            self.config.seal_embeddings(),
        )?;
        self.check_model_tag(&store)?;
        tracing::debug!(elapsed = ?t0.elapsed(), "store loaded");

        let mut cam = Camera::open(&self.config.device()?)?;
        tracing::debug!(elapsed = ?t0.elapsed(), "camera open");

        let mut deadline = Instant::now() + Duration::from_millis(duration_ms);
        let mut frame_num: usize = 0;
        let mut consecutive_errors = 0u32;
        let mut last_reject: Option<FrameQuality> = None;
        // Only a scan that saw a face counts toward lockout: an unattended
        // `sudo` with nobody at the camera is not a failed attempt. Counted
        // on the first face, not at the end (see `record_pending_failure`).
        let mut counted = false;
        // A match only counts once motion has been seen (see `motion_passes`).
        let motion_threshold = self.config.liveness_motion_threshold();
        let residual_threshold = self.config.liveness_residual_motion_threshold();
        let window = Duration::from_millis(self.config.liveness_window_ms());
        let mut recent_faces: std::collections::VecDeque<(Instant, crate::capture::IrFrame, FaceBox)> =
            std::collections::VecDeque::new();
        let mut motion_seen = liveness_disabled(motion_threshold, residual_threshold);
        let grace = Duration::from_millis(self.config.liveness_grace_ms());
        let mut liveness_pending = false;
        let mut extended = false;

        let nap = |deadline: Instant| {
            let sleep =
                Duration::from_millis(interval_ms).min(deadline.saturating_duration_since(Instant::now()));
            if !sleep.is_zero() {
                std::thread::sleep(sleep);
            }
        };

        loop {
            if Instant::now() >= deadline && grace_extends_scan(liveness_pending, extended, grace) {
                extended = true;
                deadline += grace;
                tracing::debug!(frames = frame_num, grace_ms = grace.as_millis() as u64, "matched, liveness pending; extending scan");
            }
            if Instant::now() >= deadline {
                // If nothing ever reached the detector, say why: "no match" and
                // "the illuminator never fired" need very different fixes.
                match last_reject {
                    Some(q) => tracing::debug!(
                        frames = frame_num,
                        "scan window elapsed; no frame passed quality checks — last: {q}"
                    ),
                    None => tracing::debug!(frames = frame_num, "scan window elapsed without a match"),
                }
                return Ok(false);
            }

            frame_num += 1;
            let frame = match cam.capture_illuminated_frame(self.config.capture_timeout_ms()) {
                Ok(f) => {
                    consecutive_errors = 0;
                    f
                }
                Err(e) => {
                    consecutive_errors += 1;
                    tracing::warn!(frame = frame_num, error = %e, "capture failed");
                    if consecutive_errors >= MAX_CONSECUTIVE_CAPTURE_ERRORS {
                        return Err(e);
                    }
                    nap(deadline);
                    continue;
                }
            };

            let quality = assess_frame(&frame);
            if quality != FrameQuality::Ok {
                tracing::trace!(frame = frame_num, %quality, "frame rejected");
                last_reject = Some(quality);
                nap(deadline);
                continue;
            }

            // Liveness compares the frame as captured; CLAHE remaps each
            // frame differently and would add change of its own.
            let raw = frame.clone();
            let mut frame = frame;
            crate::preprocess::histogram_equalize(&mut frame);

            let Some(face_box) = self.detector.detect(&frame)? else {
                nap(deadline);
                continue;
            };
            if face_box.is_smaller_than(self.config.min_face_size_ratio()) {
                tracing::trace!(frame = frame_num, ratio = face_box.size_ratio(), "face too small");
                nap(deadline);
                continue;
            }
            if !counted {
                counted = true;
                if !record_pending_failure(user, &embeddings_dir) {
                    return Ok(false);
                }
            }

            // The first face frame is only a baseline and is never encoded.
            // Both patches are cut with the earlier frame's box: the detector's
            // box wobbles in size from frame to frame, and two differently
            // scaled patches would differ everywhere.
            let now = Instant::now();
            while let Some(oldest) = recent_faces.front() {
                if !drop_oldest_face(recent_faces.len(), now.duration_since(oldest.0), window) {
                    break;
                }
                recent_faces.pop_front();
            }
            let profile = |(_, old_raw, old_box): &(Instant, crate::capture::IrFrame, FaceBox)| {
                anyhow::Ok(crate::preprocess::motion_profile(
                    &crate::preprocess::face_patch(old_raw, old_box)?,
                    &crate::preprocess::face_patch(&raw, old_box)?,
                ))
            };
            let motion = match (recent_faces.front(), recent_faces.back()) {
                (Some(oldest), Some(prev)) => {
                    let across = profile(oldest)?;
                    let consecutive = if recent_faces.len() > 1 { profile(prev)? } else { across };
                    Some((across, consecutive))
                }
                _ => None,
            };
            recent_faces.push_back((now, raw, face_box));
            let Some((across, consecutive)) = motion else {
                tracing::debug!(frame = frame_num, "liveness baseline");
                nap(deadline);
                continue;
            };
            motion_seen |= motion_passes(across.total, consecutive.local, motion_threshold, residual_threshold);
            tracing::debug!(
                frame = frame_num,
                motion = across.total,
                span_ms = now.duration_since(recent_faces[0].0).as_millis() as u64,
                residual = consecutive.residual,
                local = consecutive.local,
                shift = ?consecutive.shift,
                motion_threshold,
                residual_threshold,
                motion_seen,
                "liveness"
            );

            // Degenerate landmarks are one bad frame, not a broken setup.
            let input = match face_input(&frame, &face_box) {
                Ok(input) => input,
                Err(e) => {
                    tracing::debug!(frame = frame_num, error = %e, "face skipped");
                    nap(deadline);
                    continue;
                }
            };
            let embedding = self.encoder.encode(input.view())?;

            if verify_embedding(&embedding, &store, self.config.threshold())? {
                if !motion_seen {
                    tracing::debug!(frame = frame_num, "match without liveness motion yet; continuing");
                    liveness_pending = true;
                    nap(deadline);
                    continue;
                }
                tracing::debug!(frame = frame_num, elapsed = ?t0.elapsed(), "match");
                record_success(user, &embeddings_dir);
                return Ok(true);
            }

            nap(deadline);
        }
    }

    fn capture_embeddings(
        &mut self,
        cam: &mut Camera,
        store: &mut EmbeddingStore,
        frames: usize,
        interval_ms: u64,
        progress: ProgressFn<'_>,
    ) -> Result<()> {
        let mut captured = 0usize;
        let mut attempts = 0usize;
        let max_attempts = frames.saturating_mul(3);
        let before = store.embeddings.len();
        let mut last_reject: Option<FrameQuality> = None;

        while captured < frames && attempts < max_attempts {
            attempts += 1;
            progress(EnrollProgress::Capturing { captured, wanted: frames, attempt: attempts });

            let frame = cam.capture_illuminated_frame(self.config.capture_timeout_ms())?;

            let quality = assess_frame(&frame);
            if quality != FrameQuality::Ok {
                last_reject = Some(quality);
                progress(EnrollProgress::NoContent);
                std::thread::sleep(Duration::from_millis(interval_ms));
                continue;
            }

            let mut frame = frame;
            crate::preprocess::histogram_equalize(&mut frame);

            let Some(face_box) = self.detector.detect(&frame)? else {
                progress(EnrollProgress::NoFace);
                std::thread::sleep(Duration::from_millis(interval_ms));
                continue;
            };
            if face_box.is_smaller_than(self.config.min_face_size_ratio()) {
                progress(EnrollProgress::FaceTooSmall);
                std::thread::sleep(Duration::from_millis(interval_ms));
                continue;
            }

            let input = match face_input(&frame, &face_box) {
                Ok(input) => input,
                Err(e) => {
                    tracing::debug!(error = %e, "face skipped");
                    std::thread::sleep(Duration::from_millis(interval_ms));
                    continue;
                }
            };
            let embedding = self.encoder.encode(input.view())?;
            store.add_embedding(embedding);
            captured += 1;
            progress(EnrollProgress::Captured { captured, wanted: frames });

            if captured < frames {
                std::thread::sleep(Duration::from_millis(interval_ms));
            }
        }

        // Check what *this* run produced. Testing the whole store would let an
        // append silently succeed having captured nothing.
        if store.embeddings.len() == before {
            match last_reject {
                Some(q) => anyhow::bail!(
                    "no usable frame in {} attempts: {}\n\
                     Run `cargo run --example frame-stats` to see what the sensor is \
                     delivering.",
                    attempts,
                    q
                ),
                None => anyhow::bail!(
                    "no face detected in any of {} attempts — check the camera is the IR \
                     sensor and that your face is lit and in frame",
                    attempts
                ),
            }
        }

        Ok(())
    }

    /// Replace the user's enrolled embeddings.
    pub fn enroll(
        &mut self,
        user: &str,
        frames: usize,
        interval_ms: u64,
        progress: ProgressFn<'_>,
    ) -> Result<usize> {
        self.config.verify_pinned_camera()?;
        crate::storage::check_room(0, frames)?;
        let mut store = EmbeddingStore::default();
        let device = self.config.device()?;
        let mut cam = Camera::open(&device)?;
        self.capture_embeddings(&mut cam, &mut store, frames, interval_ms, progress)?;

        let saved = store.embeddings.len();
        store.save_with(
            user,
            &self.config.embeddings_dir(),
            &self.config.model_tag(),
            self.config.seal_embeddings(),
        )?;
        // Recorded whether or not `bind_camera` is on, so turning it on later
        // needs no re-enrolment.
        cameras::record(user, &self.config.embeddings_dir(), cameras::camera_id(&device).as_deref(), true)?;
        Ok(saved)
    }

    /// Append to the user's enrolled embeddings, improving coverage across
    /// lighting and angles.
    pub fn enroll_append(
        &mut self,
        user: &str,
        frames: usize,
        interval_ms: u64,
        progress: ProgressFn<'_>,
    ) -> Result<(usize, usize)> {
        self.config.verify_pinned_camera()?;
        let mut store = match EmbeddingStore::load(user, &self.config.embeddings_dir()) {
            Ok(s) => s,
            // Only "nothing enrolled yet" starts from empty. Any other error
            // (corrupt or unreadable file) must not silently discard what is
            // already there — saving would overwrite it.
            Err(e) if matches!(e.downcast_ref::<FaceAuthError>(), Some(FaceAuthError::NoEmbeddings)) => {
                EmbeddingStore::default()
            }
            Err(e) => return Err(e.context("refusing to append: existing embeddings unreadable")),
        };

        // Appending under a different model would mix incompatible vectors
        // into the gallery. Switching models is a fresh enrolment.
        self.check_model_tag(&store)?;

        let existing = store.embeddings.len();
        crate::storage::check_room(existing, frames)?;
        let device = self.config.device()?;
        let mut cam = Camera::open(&device)?;
        self.capture_embeddings(&mut cam, &mut store, frames, interval_ms, progress)?;

        let total = store.embeddings.len();
        store.save_with(
            user,
            &self.config.embeddings_dir(),
            &self.config.model_tag(),
            self.config.seal_embeddings(),
        )?;
        cameras::record(user, &self.config.embeddings_dir(), cameras::camera_id(&device).as_deref(), false)?;
        Ok((total - existing, total))
    }
}

/// Count the attempt as failed as soon as a face is seen, before anything can
/// match. A scan that then errors out, or is killed (whoever runs `sudo` can
/// kill its `pam_exec` child), has already been counted; a match resets it.
///
/// `false` when another process holds the state lock: the scan must stop,
/// since going on uncounted is the bypass this closes. A write that fails for
/// any other reason never turns a result into an error; the backoff just
/// doesn't advance this time.
fn record_pending_failure(user: &str, embeddings_dir: &std::path::Path) -> bool {
    match lockout::record_failure(user, embeddings_dir) {
        Ok(()) => true,
        Err(e) if e.is::<lockout::LockBusy>() => {
            tracing::warn!("{e}; declining this scan");
            false
        }
        Err(e) => {
            tracing::warn!("could not update lockout state: {e}");
            true
        }
    }
}

fn record_success(user: &str, embeddings_dir: &std::path::Path) {
    if let Err(e) = lockout::record_success(user, embeddings_dir) {
        tracing::warn!("could not update lockout state: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_are_sane() {
        let config = FaceAuthConfig::default();
        assert_eq!(config.threshold(), 0.6);
        assert!(config.validate().is_ok());
    }

    const MOTION: f32 = 0.01;
    const RESIDUAL: f32 = 0.3;

    #[test]
    fn accepts_motion_at_both_thresholds() {
        assert!(motion_passes(MOTION, RESIDUAL, MOTION, RESIDUAL));
    }

    #[test]
    fn rejects_a_static_photo_below_the_total_threshold() {
        assert!(!motion_passes(MOTION / 2.0, RESIDUAL, MOTION, RESIDUAL));
    }

    #[test]
    fn rejects_a_photo_moved_by_hand_below_the_local_threshold() {
        assert!(!motion_passes(MOTION, RESIDUAL / 2.0, MOTION, RESIDUAL));
    }

    #[test]
    fn liveness_is_off_only_when_both_thresholds_are_zero() {
        assert!(liveness_disabled(0.0, 0.0));
        assert!(!liveness_disabled(MOTION, 0.0));
        assert!(!liveness_disabled(0.0, RESIDUAL));
    }

    #[test]
    fn extends_a_pending_match_once() {
        let grace = Duration::from_millis(4000);
        assert!(grace_extends_scan(true, false, grace));
        assert!(!grace_extends_scan(true, true, grace));
    }

    #[test]
    fn never_extends_a_scan_that_has_not_matched_or_has_no_grace() {
        assert!(!grace_extends_scan(false, false, Duration::from_millis(4000)));
        assert!(!grace_extends_scan(true, false, Duration::ZERO));
    }

    #[test]
    fn keeps_a_face_frame_inside_the_window() {
        let window = Duration::from_millis(1000);
        assert!(!drop_oldest_face(5, window, window));
        assert!(drop_oldest_face(5, window + Duration::from_millis(1), window));
    }

    #[test]
    fn keeps_the_baseline_frame_however_old() {
        let window = Duration::from_millis(1000);
        assert!(!drop_oldest_face(1, window * 10, window));
    }

    #[test]
    fn caps_buffered_face_frames() {
        let window = Duration::from_millis(1000);
        assert!(!drop_oldest_face(MAX_LIVENESS_FRAMES, Duration::ZERO, window));
        assert!(drop_oldest_face(MAX_LIVENESS_FRAMES + 1, Duration::ZERO, window));
    }
}
