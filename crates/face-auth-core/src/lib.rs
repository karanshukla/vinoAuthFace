pub mod blazeface;
pub mod cameras;
pub mod capture;
pub mod config;
pub mod config_write;
pub mod detector;
pub mod environment;
pub mod error;
pub mod guided;
pub mod inference;
pub mod installed;
pub mod lockout;
pub mod model_files;
pub mod preprocess;
pub mod scrfd;
pub mod seal;
pub mod seat;
pub mod storage;
#[cfg(test)]
mod test_support;
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

/// Margin around the detected box before cropping, as a fraction of the box
/// per side: the encoder's training crops run forehead to chin.
pub const FACE_CROP_MARGIN: f32 = 0.3;

/// The encoder input for a detected face: warped onto the ArcFace template
/// when the detector found landmarks, otherwise the box crop with
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

/// Most face frames kept for `liveness_window_ms` whatever the frame rate:
/// about 1 s at 15 pairs a second.
const MAX_LIVENESS_FRAMES: usize = 16;

const MAX_CONSECUTIVE_CAPTURE_ERRORS: u32 = 3;

/// (pinned by `tests::liveness_is_off_only_when_both_thresholds_are_zero`)
fn liveness_disabled(motion_threshold: f32, residual_threshold: f32) -> bool {
    motion_threshold <= 0.0 && residual_threshold <= 0.0
}

/// `total` (against the oldest face frame in the window) defeats a static
/// photo; `local` (against the previous frame) one moved by hand (pinned by
/// `tests::rejects_a_static_photo_below_the_total_threshold`,
/// `tests::rejects_a_photo_moved_by_hand_below_the_local_threshold`).
fn motion_passes(total: f32, local: f32, motion_threshold: f32, residual_threshold: f32) -> bool {
    total >= motion_threshold && local >= residual_threshold
}

/// Once per scan, a match still waiting on liveness gets `grace` more time
/// (pinned by `tests::extends_a_pending_match_once`,
/// `tests::never_extends_a_scan_that_has_not_matched_or_has_no_grace`).
fn grace_extends_scan(liveness_pending: bool, already_extended: bool, grace: Duration) -> bool {
    liveness_pending && !already_extended && !grace.is_zero()
}

/// (pinned by `tests::keeps_a_face_frame_inside_the_window`,
/// `tests::keeps_the_baseline_frame_however_old`, `tests::caps_buffered_face_frames`)
fn drop_oldest_face(frames_kept: usize, oldest_age: Duration, window: Duration) -> bool {
    frames_kept > 1 && (oldest_age > window || frames_kept > MAX_LIVENESS_FRAMES)
}

/// A face frame as captured, before CLAHE: equalisation remaps each frame
/// differently and would add change of its own.
struct FaceFrame {
    at: Instant,
    raw: crate::capture::IrFrame,
    face_box: FaceBox,
}

/// Motion liveness across one scan's face frames.
struct Liveness {
    recent_faces: std::collections::VecDeque<FaceFrame>,
    window: Duration,
    motion_threshold: f32,
    residual_threshold: f32,
    motion_seen: bool,
}

impl Liveness {
    fn new(config: &FaceAuthConfig) -> Self {
        let motion_threshold = config.liveness_motion_threshold();
        let residual_threshold = config.liveness_residual_motion_threshold();
        Self {
            recent_faces: std::collections::VecDeque::new(),
            window: Duration::from_millis(config.liveness_window_ms()),
            motion_threshold,
            residual_threshold,
            motion_seen: liveness_disabled(motion_threshold, residual_threshold),
        }
    }

    /// Adds a face frame. `false` for the first one, which is only a baseline
    /// and is never encoded.
    fn observe(&mut self, frame_num: usize, face: FaceFrame) -> Result<bool> {
        while let Some(oldest) = self.recent_faces.front() {
            if !drop_oldest_face(self.recent_faces.len(), face.at.duration_since(oldest.at), self.window) {
                break;
            }
            self.recent_faces.pop_front();
        }
        let motion = match (self.recent_faces.front(), self.recent_faces.back()) {
            (Some(oldest), Some(prev)) => {
                let across = motion_since(oldest, &face.raw)?;
                let consecutive = if self.recent_faces.len() > 1 { motion_since(prev, &face.raw)? } else { across };
                Some((across, consecutive))
            }
            _ => None,
        };
        let now = face.at;
        self.recent_faces.push_back(face);
        let Some((across, consecutive)) = motion else {
            return Ok(false);
        };
        self.motion_seen |= motion_passes(across.total, consecutive.local, self.motion_threshold, self.residual_threshold);
        tracing::debug!(
            frame = frame_num,
            motion = across.total,
            span_ms = now.duration_since(self.recent_faces[0].at).as_millis() as u64,
            residual = consecutive.residual,
            local = consecutive.local,
            shift = ?consecutive.shift,
            motion_threshold = self.motion_threshold,
            residual_threshold = self.residual_threshold,
            motion_seen = self.motion_seen,
            "liveness"
        );
        Ok(true)
    }
}

/// Both patches are cut with the earlier frame's box: the detector's box
/// changes size from frame to frame, and differently scaled patches would
/// differ everywhere.
fn motion_since(earlier: &FaceFrame, raw: &crate::capture::IrFrame) -> Result<crate::preprocess::MotionProfile> {
    Ok(crate::preprocess::motion_profile(
        &crate::preprocess::face_patch(&earlier.raw, &earlier.face_box)?,
        &crate::preprocess::face_patch(raw, &earlier.face_box)?,
    ))
}

/// Progress for the interactive enrolment paths. The library never writes to
/// stdout: under `pam_exec` it would land on the terminal on every `sudo`.
pub type ProgressFn<'a> = &'a mut dyn FnMut(EnrollProgress);

#[derive(Debug, Clone)]
pub enum EnrollProgress {
    Capturing { captured: usize, wanted: usize, attempt: usize },
    NoContent,
    NoFace,
    FaceTooSmall,
    Captured { captured: usize, wanted: usize },
}

/// What a scan needs before its first frame, prepared by
/// [`FaceAuth::new_for_scan`]. `Ok(None)` means the user is locked out.
pub struct PreparedScan(Result<Option<(EmbeddingStore, Camera)>>);

fn scan_prelude(
    config: &FaceAuthConfig,
    user: &str,
    gate: impl FnOnce(&FaceAuthConfig),
) -> Result<Option<(EmbeddingStore, Camera)>> {
    gate(config);
    config.verify_pinned_camera()?;
    check_camera_binding(config, user)?;
    let embeddings_dir = config.embeddings_dir();
    if lockout::check(user, &embeddings_dir, &config.lockout_policy()).is_some() {
        return Ok(None);
    }
    let device = config.device()?;
    // Don't light the emitter for an account with no templates.
    let enrolled = EmbeddingStore::is_enrolled(user, &embeddings_dir).unwrap_or(false);
    std::thread::scope(|s| {
        let camera = enrolled.then(|| {
            s.spawn(|| {
                let mut camera = Camera::open(&device)?;
                camera.start_stream()?;
                Ok::<_, anyhow::Error>(camera)
            })
        });
        let t0 = Instant::now();
        let store = EmbeddingStore::load_with(user, &embeddings_dir, config.seal_embeddings());
        tracing::debug!(elapsed = ?t0.elapsed(), "store loaded");
        let store = store?;
        let camera = match camera {
            Some(handle) => handle.join().map_err(|_| anyhow::anyhow!("camera thread panicked"))??,
            None => Camera::open(&device)?,
        };
        Ok(Some((store, camera)))
    })
}

fn check_camera_binding(config: &FaceAuthConfig, user: &str) -> Result<()> {
    if !config.bind_camera() {
        return Ok(());
    }
    let enrolled = cameras::load(user, &config.embeddings_dir())?;
    let device = config.device()?;
    cameras::check(&enrolled, cameras::camera_id(&device).as_deref(), &device)
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

    /// Like [`FaceAuth::new`], but loads the models while another thread runs
    /// everything `authenticate_scan` needs before its first frame: `gate`,
    /// the pin, binding and lockout checks, then the template unseal alongside
    /// the camera's open and stream start. Nothing touches the camera until
    /// `gate` and the checks have passed. Hand the second value to
    /// [`FaceAuth::authenticate_prepared`].
    pub fn new_for_scan(
        config: FaceAuthConfig,
        user: &str,
        gate: impl FnOnce(&FaceAuthConfig) + Send,
    ) -> Result<(Self, PreparedScan)> {
        config.validate()?;
        let (backend, device) = (config.backend(), config.npu_device());
        let (models, prelude) = std::thread::scope(|s| {
            let prelude = s.spawn(|| scan_prelude(&config, user, gate));
            let models = FaceEncoder::new(&config.model_path(), &backend, &device).and_then(|encoder| {
                let detector = FaceDetector::new(
                    &config.detector_model_path(),
                    config.detector_threshold(),
                    &backend,
                    &device,
                )?;
                Ok((encoder, detector))
            });
            (models, prelude.join())
        });
        let (encoder, detector) = models?;
        let prelude = prelude.unwrap_or_else(|_| Err(anyhow::anyhow!("scan preparation panicked")));
        Ok((Self { config, encoder, detector }, PreparedScan(prelude)))
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
    /// not a failed attempt: the lockout doesn't count it. See `cameras`.
    fn check_camera_binding(&self, user: &str) -> Result<()> {
        check_camera_binding(&self.config, user)
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
        let prepared = PreparedScan(scan_prelude(&self.config, user, |_| {}));
        self.authenticate_prepared(user, prepared, duration_ms, interval_ms)
    }

    /// [`FaceAuth::authenticate_scan`] with the setup already done by
    /// [`FaceAuth::new_for_scan`].
    pub fn authenticate_prepared(
        &mut self,
        user: &str,
        prepared: PreparedScan,
        duration_ms: u64,
        interval_ms: u64,
    ) -> Result<bool> {
        let embeddings_dir = self.config.embeddings_dir();
        let Some((store, mut cam)) = prepared.0? else {
            return Ok(false);
        };
        self.check_model_tag(&store)?;
        let t0 = Instant::now();

        let mut deadline = Instant::now() + Duration::from_millis(duration_ms);
        let mut frame_num: usize = 0;
        let mut consecutive_errors = 0u32;
        let mut last_reject: Option<FrameQuality> = None;
        let mut attempt_counted = false;
        let mut liveness = Liveness::new(&self.config);
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
            if !attempt_counted {
                attempt_counted = true;
                if !record_pending_failure(user, &embeddings_dir) {
                    return Ok(false);
                }
            }

            if !liveness.observe(frame_num, FaceFrame { at: Instant::now(), raw, face_box })? {
                tracing::debug!(frame = frame_num, "liveness baseline");
                nap(deadline);
                continue;
            }

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
                if !liveness.motion_seen {
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

        // This run's captures, not the store's: an append must capture something.
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
        // Recorded even with `bind_camera` off, so turning it on needs no
        // re-enrolment.
        cameras::record(user, &self.config.embeddings_dir(), cameras::camera_id(&device).as_deref(), true)?;
        Ok(saved)
    }

    /// Guided enrolment (see [`guided`]): replace the user's templates, or
    /// add to them with `append`, from the poses its steps ask for. Returns
    /// the templates added and the total. Nothing is saved if it fails or
    /// `control` cancels.
    pub fn enroll_guided(
        &mut self,
        user: &str,
        append: bool,
        progress: &mut dyn FnMut(guided::GuidedProgress),
        control: &mut dyn FnMut() -> Option<guided::Control>,
    ) -> Result<(usize, usize)> {
        self.config.verify_pinned_camera()?;
        let mut store = if append {
            self.store_to_append_to(user)?
        } else {
            EmbeddingStore::default()
        };
        let existing = store.embeddings.len();
        let most: usize = guided::Step::ALL.iter().map(|s| s.wanted()).sum();
        crate::storage::check_room(existing, most)?;
        let device = self.config.device()?;
        let mut cam = Camera::open(&device)?;
        for embedding in self.capture_guided(&mut cam, progress, control)? {
            store.add_embedding(embedding);
        }
        let total = store.embeddings.len();
        store.save_with(
            user,
            &self.config.embeddings_dir(),
            &self.config.model_tag(),
            self.config.seal_embeddings(),
        )?;
        cameras::record(user, &self.config.embeddings_dir(), cameras::camera_id(&device).as_deref(), !append)?;
        Ok((total - existing, total))
    }

    /// The user's templates to add to. Only "nothing enrolled yet" starts
    /// from empty: saving over a store that failed to load would discard it.
    /// A store from another model is refused, since its vectors can't be mixed.
    fn store_to_append_to(&self, user: &str) -> Result<EmbeddingStore> {
        let store = match EmbeddingStore::load(user, &self.config.embeddings_dir()) {
            Ok(s) => s,
            Err(e) if matches!(e.downcast_ref::<FaceAuthError>(), Some(FaceAuthError::NoEmbeddings)) => {
                EmbeddingStore::default()
            }
            Err(e) => return Err(e.context("refusing to append: existing embeddings unreadable")),
        };
        self.check_model_tag(&store)?;
        Ok(store)
    }

    /// Add to the user's templates, for coverage across lighting and angles.
    pub fn enroll_append(
        &mut self,
        user: &str,
        frames: usize,
        interval_ms: u64,
        progress: ProgressFn<'_>,
    ) -> Result<(usize, usize)> {
        self.config.verify_pinned_camera()?;
        let mut store = self.store_to_append_to(user)?;

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

/// Count the attempt as failed at the first face, so a scan that errors out or
/// is killed (whoever runs `sudo` can kill its `pam_exec` child) is still
/// counted; a match resets it. A scan with no face is never an attempt.
///
/// `false` when another process holds the state lock: going on uncounted is
/// the parallel-scan bypass the lock closes. Any other write failure goes on
/// uncounted rather than becoming an error (pinned by
/// `tests::a_held_lock_declines_the_scan`,
/// `tests::a_failed_write_proceeds_uncounted_by_design`).
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

    fn liveness_config(motion: f32, residual: f32) -> FaceAuthConfig {
        FaceAuthConfig {
            liveness_motion_threshold: Some(motion),
            liveness_residual_motion_threshold: Some(residual),
            ..FaceAuthConfig::default()
        }
    }

    fn face_frame(seed: u32) -> FaceFrame {
        let (width, height) = (120u32, 120u32);
        let data = (0..width * height)
            .map(|i| (i.wrapping_mul(2654435761u32.wrapping_add(seed.wrapping_mul(0x9E37_79B9))) >> 16) as u16)
            .collect();
        FaceFrame {
            at: Instant::now(),
            raw: crate::capture::IrFrame { data, width, height },
            face_box: FaceBox { x1: 0.1, y1: 0.1, x2: 0.9, y2: 0.9, landmarks: None },
        }
    }

    #[test]
    fn the_first_face_frame_is_only_a_baseline() {
        let mut liveness = Liveness::new(&liveness_config(0.05, 0.0));
        assert!(!liveness.observe(1, face_frame(1)).unwrap());
        assert!(liveness.observe(2, face_frame(1)).unwrap());
    }

    #[test]
    fn motion_is_not_seen_until_a_frame_changes() {
        let mut liveness = Liveness::new(&liveness_config(0.05, 0.0));
        liveness.observe(1, face_frame(1)).unwrap();
        liveness.observe(2, face_frame(1)).unwrap();
        assert!(!liveness.motion_seen);

        liveness.observe(3, face_frame(2)).unwrap();
        assert!(liveness.motion_seen);
    }

    #[test]
    fn liveness_with_both_thresholds_zero_starts_with_motion_seen() {
        assert!(Liveness::new(&liveness_config(0.0, 0.0)).motion_seen);
        assert!(!Liveness::new(&liveness_config(0.05, 0.0)).motion_seen);
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

    #[test]
    fn a_counted_attempt_proceeds_and_a_match_clears_it() {
        let dir = crate::test_support::tmpdir("counted");
        std::fs::create_dir(dir.join("alice")).unwrap();
        assert!(record_pending_failure("alice", &dir));
        assert!(record_pending_failure("alice", &dir));
        assert_eq!(lockout::peek("alice", &dir, &lockout::LockoutPolicy::default()).0, 2);
        record_success("alice", &dir);
        assert_eq!(lockout::peek("alice", &dir, &lockout::LockoutPolicy::default()).0, 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_failed_write_proceeds_uncounted_by_design() {
        let dir = crate::test_support::tmpdir("nostore");
        assert!(record_pending_failure("alice", &dir));
        assert!(!dir.join("alice").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_held_lock_declines_the_scan() {
        use std::os::fd::AsRawFd;
        let dir = crate::test_support::tmpdir("held");
        let lockout_dir = dir.join("alice/lockout");
        std::fs::create_dir_all(&lockout_dir).unwrap();
        let held = std::fs::File::create(lockout_dir.join("state.lock")).unwrap();
        assert_eq!(unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_EX) }, 0);
        assert!(!record_pending_failure("alice", &dir));
        drop(held);
        assert_eq!(lockout::peek("alice", &dir, &lockout::LockoutPolicy::default()).0, 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
