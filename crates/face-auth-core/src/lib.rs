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
use std::time::Instant;

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

    pub fn authenticate(&mut self, user: &str) -> Result<bool> {
        let t0 = Instant::now();
        let store = EmbeddingStore::load(user, &self.config.embeddings_dir())?;
        eprintln!("TIMING store_load: {:?}", t0.elapsed());

        let t1 = Instant::now();
        let frame = crate::capture::capture_ir_frame(&self.config.device(), self.config.capture_timeout_ms())?;
        eprintln!("TIMING capture: {:?}", t1.elapsed());

        // Quick content check on raw frame — rejects uniform/noise frames
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

        let result = verify_embedding(&embedding, &store, self.config.threshold());
        result
    }

    pub fn enroll(&mut self, user: &str, frames: usize, interval_ms: u64) -> Result<()> {
        let mut store = EmbeddingStore::default();
        let mut cam = Camera::open(&self.config.device())?;
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

        let saved = store.embeddings.len();
        store.save(user, &self.config.embeddings_dir())?;
        println!("Saved {} embeddings for user '{}'", saved, user);

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