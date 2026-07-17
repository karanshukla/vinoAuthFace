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

pub struct FaceAuth {
    config: FaceAuthConfig,
    encoder: FaceEncoder,
    detector: FaceDetector,
}

impl FaceAuth {
    pub fn new(config: FaceAuthConfig) -> Result<Self> {
        let encoder = FaceEncoder::new(&config.model_path())?;
        let detector = FaceDetector::new(
            &config.detector_model_path(),
            config.detector_threshold(),
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
        if !self.detector.detect(&frame)? {
            return Err(crate::error::FaceAuthError::NoFaceDetected.into());
        }
        eprintln!("TIMING detect: {:?}", t_detect.elapsed());

        let t3 = Instant::now();
        let input = crate::preprocess::preprocess_ir_frame(&frame)?;
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

            if !self.detector.detect(&frame)? {
                let sleep = Duration::from_millis(interval_ms)
                    .min(deadline.saturating_duration_since(Instant::now()));
                std::thread::sleep(sleep);
                continue;
            }

            let t3 = Instant::now();
            let input = crate::preprocess::preprocess_ir_frame(&frame)?;
            let embedding = self.encoder.encode(input.view())?;
            eprintln!("TIMING frame_{} encode: {:?}", frame_num, t3.elapsed());

            let matched = verify_embedding(&embedding, &store, self.config.threshold())?;
            if matched {
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
        let mut captured = 0usize;
        let mut attempts = 0usize;
        let max_attempts = frames * 3;

        while captured < frames && attempts < max_attempts {
            println!("Capturing frame {}/{} (attempt {})...", captured + 1, frames, attempts + 1);
            let frame = cam.capture_frame(self.config.capture_timeout_ms())?;
            attempts += 1;

            if !crate::detector::raw_frame_has_content(&frame) {
                eprintln!("No content in frame, retrying...");
                std::thread::sleep(std::time::Duration::from_millis(interval_ms));
                continue;
            }

            let mut frame = frame;
            crate::preprocess::histogram_equalize(&mut frame);

            if !self.detector.detect(&frame)? {
                eprintln!("No face detected, retrying...");
                std::thread::sleep(std::time::Duration::from_millis(interval_ms));
                continue;
            }

            let input = crate::preprocess::preprocess_ir_frame(&frame)?;
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