pub mod capture;
pub mod config;
pub mod detector;
pub mod error;
pub mod inference;
pub mod preprocess;
pub mod storage;
pub mod verify;

pub use crate::capture::Camera;
pub use crate::config::FaceAuthConfig;
use crate::detector::FaceDetector;
use crate::inference::FaceEncoder;
use crate::storage::EmbeddingStore;
use crate::verify::verify_embedding;
use anyhow::Result;
use std::time::{Duration, Instant};

/// Fraction of the detected face box's own width/height added as margin on
/// each side before cropping, so the encoder sees the same kind of
/// forehead-to-chin framing MobileFaceNet-style models are trained on
/// instead of a razor-tight bounding box.
const FACE_CROP_MARGIN: f32 = 0.3;

pub struct FaceAuth {
    config: FaceAuthConfig,
    encoder: FaceEncoder,
    detector: FaceDetector,
}

impl FaceAuth {
    pub fn new(config: FaceAuthConfig) -> Result<Self> {
        let backend = config.backend();
        let device = config.npu_device();
        let encoder = FaceEncoder::new(&config.model_path(), &backend, &device)?;
        let detector = FaceDetector::new(
            &config.detector_model_path(),
            config.detector_threshold(),
            &backend,
            &device,
        )?;
        Ok(Self { config, encoder, detector })
    }

    pub fn authenticate_once(&mut self, user: &str) -> Result<bool> {
        let t0 = Instant::now();
        let store = EmbeddingStore::load(user, &self.config.embeddings_dir())?;
        eprintln!("TIMING store_load: {:?}", t0.elapsed());

        let t1 = Instant::now();
        let frame = crate::capture::capture_ir_frame(&self.config.device(), self.config.capture_timeout_ms())?;
        eprintln!("TIMING capture: {:?}", t1.elapsed());

        if !crate::detector::raw_frame_has_content(&frame) {
            return Err(crate::error::FaceAuthError::NoFaceDetected.into());
        }

        let t2 = Instant::now();
        let mut frame = frame;
        crate::preprocess::histogram_equalize(&mut frame);
        eprintln!("TIMING equalize: {:?}", t2.elapsed());

        let t_detect = Instant::now();
        let face_box = match self.detector.detect(&frame)? {
            Some(b) => b,
            None => return Err(crate::error::FaceAuthError::NoFaceDetected.into()),
        };
        eprintln!("TIMING detect: {:?}", t_detect.elapsed());

        let t3 = Instant::now();
        let cropped = crate::preprocess::crop_to_face(&frame, &face_box, FACE_CROP_MARGIN);
        let input = crate::preprocess::preprocess_ir_frame(&cropped)?;
        eprintln!("TIMING preprocess: {:?}", t3.elapsed());

        let t4 = Instant::now();
        let embedding = self.encoder.encode(input.view())?;
        eprintln!("TIMING encode: {:?}", t4.elapsed());

        verify_embedding(&embedding, &store, self.config.threshold())
    }

    pub fn authenticate_scan(
        &mut self,
        user: &str,
        duration_ms: u64,
        interval_ms: u64,
    ) -> Result<bool> {
        let t0 = Instant::now();
        let store = EmbeddingStore::load(user, &self.config.embeddings_dir())?;
        eprintln!("TIMING store_load: {:?}", t0.elapsed());

        let t_cam = Instant::now();
        let mut cam = Camera::open(&self.config.device())?;
        eprintln!("TIMING camera_open: {:?}", t_cam.elapsed());

        let deadline = Instant::now() + Duration::from_millis(duration_ms);
        let mut frame_num: usize = 0;
        let mut consecutive_errors = 0u32;
        let motion_threshold = self.config.liveness_motion_threshold();
        let mut prev_face_frame: Option<crate::capture::IrFrame> = None;
        let mut motion_observed = false;

        loop {
            if Instant::now() >= deadline {
                eprintln!("SCAN: window elapsed ({} frames)", frame_num);
                return Ok(false);
            }

            frame_num += 1;
            let t_cap = Instant::now();
            let frame = match cam.capture_frame(self.config.capture_timeout_ms()) {
                Ok(f) => {
                    consecutive_errors = 0;
                    f
                }
                Err(e) => {
                    consecutive_errors += 1;
                    eprintln!("SCAN: frame {} capture error — {}", frame_num, e);
                    if consecutive_errors >= 3 {
                        return Err(e);
                    }
                    let sleep = Duration::from_millis(interval_ms)
                        .min(deadline.saturating_duration_since(Instant::now()));
                    std::thread::sleep(sleep);
                    continue;
                }
            };
            eprintln!("TIMING frame_{} capture: {:?}", frame_num, t_cap.elapsed());

            if !crate::detector::raw_frame_has_content(&frame) {
                let sleep = Duration::from_millis(interval_ms)
                    .min(deadline.saturating_duration_since(Instant::now()));
                std::thread::sleep(sleep);
                continue;
            }

            let t2 = Instant::now();
            let mut frame = frame;
            crate::preprocess::histogram_equalize(&mut frame);
            eprintln!("TIMING frame_{} equalize: {:?}", frame_num, t2.elapsed());

            let face_box = match self.detector.detect(&frame)? {
                Some(b) => b,
                None => {
                    let sleep = Duration::from_millis(interval_ms)
                        .min(deadline.saturating_duration_since(Instant::now()));
                    std::thread::sleep(sleep);
                    continue;
                }
            };

            // The first frame with a detected face has no prior frame to diff
            // against, so it can never clear the motion-liveness gate below —
            // any match on it would be discarded anyway. Use it only to seed
            // the motion baseline and skip the (comparatively expensive)
            // encoder call entirely rather than run it on a result that's
            // structurally guaranteed to be thrown away.
            let is_first_face_frame = prev_face_frame.is_none();

            if let Some(prev) = &prev_face_frame {
                let motion = crate::preprocess::frame_motion_fraction(prev, &frame);
                if motion >= motion_threshold {
                    motion_observed = true;
                }
                tracing::debug!(frame_num, motion, motion_threshold, motion_observed, "liveness: motion check");
            }
            prev_face_frame = Some(frame.clone());

            if is_first_face_frame {
                eprintln!("SCAN: frame {} establishes motion baseline, skipping encode", frame_num);
                let sleep = Duration::from_millis(interval_ms)
                    .min(deadline.saturating_duration_since(Instant::now()));
                std::thread::sleep(sleep);
                continue;
            }

            let t3 = Instant::now();
            let cropped = crate::preprocess::crop_to_face(&frame, &face_box, FACE_CROP_MARGIN);
            let input = crate::preprocess::preprocess_ir_frame(&cropped)?;
            let embedding = self.encoder.encode(input.view())?;
            eprintln!("TIMING frame_{} encode: {:?}", frame_num, t3.elapsed());

            let matched = verify_embedding(&embedding, &store, self.config.threshold())?;
            if matched {
                if !motion_observed {
                    eprintln!(
                        "SCAN: frame {} matched embedding but no liveness motion observed yet — treating as possible spoof, continuing scan",
                        frame_num
                    );
                    let sleep = Duration::from_millis(interval_ms)
                        .min(deadline.saturating_duration_since(Instant::now()));
                    std::thread::sleep(sleep);
                    continue;
                }
                eprintln!("SCAN: match on frame {} after {:?}", frame_num, t0.elapsed());
                return Ok(true);
            }

            let remaining = deadline.saturating_duration_since(Instant::now());
            let sleep = Duration::from_millis(interval_ms).min(remaining);
            if !sleep.is_zero() {
                std::thread::sleep(sleep);
            }
        }
    }

    fn capture_embeddings(
        &mut self,
        cam: &mut Camera,
        store: &mut EmbeddingStore,
        frames: usize,
        interval_ms: u64,
    ) -> Result<()> {
        // Blank-frame retries shouldn't reuse `interval_ms` — that paces successful captures so
        // the user has time to shift pose/expression between shots, not how fast this sensor
        // produces its next frame. Query the camera's own reported frame interval the same way
        // `FaceAuthConfig::scan_interval_ms` does, so the retry cadence adapts to whatever
        // hardware this runs on instead of assuming this machine's timing.
        let retry_delay_ms = crate::capture::query_frame_interval_ms(&self.config.device()).unwrap_or(100);

        let mut captured = 0usize;
        let mut attempts = 0usize;
        let start = Instant::now();
        // A ceiling to guarantee termination if the camera is genuinely stuck (disconnected,
        // permanently dark), not a tight budget for ordinary blank-frame bursts — it scales with
        // this camera's own reported frame interval rather than a flat guess, so a sensor that
        // reports blank frames at a higher rate (faster native fps) gets proportionally more
        // retry headroom instead of being punished by a fixed attempt count.
        let deadline = start + Duration::from_millis(frames as u64 * (interval_ms + retry_delay_ms * 50));

        while captured < frames && Instant::now() < deadline {
            println!("Capturing frame {}/{} (attempt {})...", captured + 1, frames, attempts + 1);
            let frame = cam.capture_frame(self.config.capture_timeout_ms())?;
            attempts += 1;

            if !crate::detector::raw_frame_has_content(&frame) {
                eprintln!(
                    "No content in frame (variance={:.0}, threshold={:.0}, mean={:.1}, bytes={}, expected={}), retrying...",
                    crate::detector::frame_variance(&frame),
                    crate::detector::CONTENT_VARIANCE_THRESHOLD,
                    crate::detector::frame_mean(&frame),
                    frame.data.len(),
                    (frame.width * frame.height) as usize,
                );
                // Not interval_ms: a blank frame isn't the user needing time to move, it's the
                // sensor needing another poll cycle, so back off by its own native frame time.
                std::thread::sleep(Duration::from_millis(retry_delay_ms));
                continue;
            }

            let mut frame = frame;
            crate::preprocess::histogram_equalize(&mut frame);

            let face_box = match self.detector.detect(&frame)? {
                Some(b) => b,
                None => {
                    eprintln!("No face detected, retrying...");
                    std::thread::sleep(std::time::Duration::from_millis(interval_ms));
                    continue;
                }
            };

            let cropped = crate::preprocess::crop_to_face(&frame, &face_box, FACE_CROP_MARGIN);
            let input = crate::preprocess::preprocess_ir_frame(&cropped)?;
            let embedding = self.encoder.encode(input.view())?;
            store.add_embedding(embedding);
            captured += 1;

            if captured < frames {
                std::thread::sleep(std::time::Duration::from_millis(interval_ms));
            }
        }

        if store.embeddings.is_empty() {
            return Err(anyhow::anyhow!("No face detected in any frame during enrollment"));
        }

        Ok(())
    }

    pub fn enroll(&mut self, user: &str, frames: usize, interval_ms: u64) -> Result<()> {
        let mut store = EmbeddingStore::default();
        let mut cam = Camera::open(&self.config.device())?;
        self.capture_embeddings(&mut cam, &mut store, frames, interval_ms)?;

        let saved = store.embeddings.len();
        store.save(user, &self.config.embeddings_dir())?;
        println!("Saved {} embeddings for user '{}'", saved, user);

        Ok(())
    }

    pub fn enroll_append(&mut self, user: &str, frames: usize, interval_ms: u64) -> Result<()> {
        let mut store = match EmbeddingStore::load(user, &self.config.embeddings_dir()) {
            Ok(s) => s,
            Err(_) => EmbeddingStore::default(),
        };
        let existing = store.embeddings.len();
        let mut cam = Camera::open(&self.config.device())?;
        self.capture_embeddings(&mut cam, &mut store, frames, interval_ms)?;

        let total = store.embeddings.len();
        store.save(user, &self.config.embeddings_dir())?;
        println!("Added {} new embeddings for user '{}' ({} total)", total - existing, user, total);

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_config_defaults() {
        let config = FaceAuthConfig::default();
        assert_eq!(config.threshold(), 0.6);
        assert!(config.device().starts_with("/dev/video"));
    }
}