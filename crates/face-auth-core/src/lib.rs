pub mod capture;
pub mod config;
pub mod detector;
pub mod environment;
pub mod error;
pub mod inference;
pub mod lockout;
pub mod preprocess;
pub mod seal;
pub mod seat;
pub mod storage;
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
                "templates were enrolled with a different recognition model ({}) than the one \
                 configured ({current}); re-run face-enroll for this model",
                store.model_tag.as_deref().unwrap_or("unknown")
            );
        }
        Ok(())
    }

    /// Single-shot verification. The PAM path uses
    /// [`FaceAuth::authenticate_scan`].
    pub fn authenticate_once(&mut self, user: &str) -> Result<bool> {
        self.config.verify_pinned_camera()?;
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
            &self.config.device(),
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

        let face = crate::preprocess::crop_to_face(&frame, &face_box, FACE_CROP_MARGIN)?;
        let input = crate::preprocess::preprocess_ir_frame(&face)?;
        let embedding = self.encoder.encode(input.view())?;
        tracing::debug!(elapsed = ?t0.elapsed(), "authenticate_once complete");

        let matched = verify_embedding(&embedding, &store, self.config.threshold())?;
        record_attempt(user, &embeddings_dir, matched);
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

        let mut cam = Camera::open(&self.config.device())?;
        tracing::debug!(elapsed = ?t0.elapsed(), "camera open");

        let deadline = Instant::now() + Duration::from_millis(duration_ms);
        let mut frame_num: usize = 0;
        let mut consecutive_errors = 0u32;
        let mut last_reject: Option<FrameQuality> = None;
        // Only a scan that saw a face counts toward lockout: an unattended
        // `sudo` with nobody at the camera is not a failed attempt.
        let mut face_seen = false;
        // Motion liveness: a match only counts once real motion has been seen
        // between consecutive face frames. `total` defeats a static photo;
        // `residual` (motion a rigid shift can't explain) defeats one moved
        // by hand.
        let motion_threshold = self.config.liveness_motion_threshold();
        let residual_threshold = self.config.liveness_residual_motion_threshold();
        let mut prev_face: Option<(crate::capture::IrFrame, FaceBox)> = None;
        let mut motion_seen = motion_threshold <= 0.0 && residual_threshold <= 0.0;

        // Wait out the remainder of the interval without overrunning the window.
        let nap = |deadline: Instant| {
            let sleep =
                Duration::from_millis(interval_ms).min(deadline.saturating_duration_since(Instant::now()));
            if !sleep.is_zero() {
                std::thread::sleep(sleep);
            }
        };

        loop {
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
                if face_seen {
                    record_attempt(user, &embeddings_dir, false);
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
                    if consecutive_errors >= 3 {
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
            face_seen = true;

            // The first face frame has nothing to diff against, so it can never
            // pass the liveness gate. Use it as the baseline and skip encoding.
            // Both patches are cut with the earlier frame's box: the detector's
            // box wobbles in size from frame to frame, and two differently
            // scaled patches would differ everywhere.
            let motion = match &prev_face {
                Some((prev_raw, prev_box)) => Some(crate::preprocess::motion_profile(
                    &crate::preprocess::face_patch(prev_raw, prev_box)?,
                    &crate::preprocess::face_patch(&raw, prev_box)?,
                )),
                None => None,
            };
            prev_face = Some((raw, face_box));
            let Some(motion) = motion else {
                tracing::debug!(frame = frame_num, "liveness baseline");
                nap(deadline);
                continue;
            };
            motion_seen |= motion.total >= motion_threshold && motion.residual >= residual_threshold;
            tracing::debug!(
                frame = frame_num,
                motion = motion.total,
                residual = motion.residual,
                shift = ?motion.shift,
                motion_threshold,
                residual_threshold,
                motion_seen,
                "liveness"
            );

            let face = crate::preprocess::crop_to_face(&frame, &face_box, FACE_CROP_MARGIN)?;
            let input = crate::preprocess::preprocess_ir_frame(&face)?;
            let embedding = self.encoder.encode(input.view())?;

            if verify_embedding(&embedding, &store, self.config.threshold())? {
                if !motion_seen {
                    tracing::debug!(frame = frame_num, "match without liveness motion yet; continuing");
                    nap(deadline);
                    continue;
                }
                tracing::debug!(frame = frame_num, elapsed = ?t0.elapsed(), "match");
                record_attempt(user, &embeddings_dir, true);
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

            let face = crate::preprocess::crop_to_face(&frame, &face_box, FACE_CROP_MARGIN)?;
            let input = crate::preprocess::preprocess_ir_frame(&face)?;
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
        let mut store = EmbeddingStore::default();
        let mut cam = Camera::open(&self.config.device())?;
        self.capture_embeddings(&mut cam, &mut store, frames, interval_ms, progress)?;

        let saved = store.embeddings.len();
        store.save_with(
            user,
            &self.config.embeddings_dir(),
            &self.config.model_tag(),
            self.config.seal_embeddings(),
        )?;
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
        let mut cam = Camera::open(&self.config.device())?;
        self.capture_embeddings(&mut cam, &mut store, frames, interval_ms, progress)?;

        let total = store.embeddings.len();
        store.save_with(
            user,
            &self.config.embeddings_dir(),
            &self.config.model_tag(),
            self.config.seal_embeddings(),
        )?;
        Ok((total - existing, total))
    }
}

/// Lockout bookkeeping must never turn a result into an error: a write that
/// fails just means the backoff does not advance this time.
fn record_attempt(user: &str, embeddings_dir: &std::path::Path, matched: bool) {
    let result = if matched {
        lockout::record_success(user, embeddings_dir)
    } else {
        lockout::record_failure(user, embeddings_dir)
    };
    if let Err(e) = result {
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
}
