pub mod capture;
pub mod config;
pub mod error;
pub mod inference;
pub mod preprocess;
pub mod storage;
pub mod verify;

pub use crate::capture::Camera;
pub use crate::config::FaceAuthConfig;
use crate::inference::FaceEncoder;
use crate::storage::EmbeddingStore;
use crate::verify::verify_embedding;
use anyhow::Result;
use std::time::Instant;

pub struct FaceAuth {
    config: FaceAuthConfig,
    encoder: FaceEncoder,
}

impl FaceAuth {
    pub fn new(config: FaceAuthConfig) -> Result<Self> {
        let encoder = FaceEncoder::new(&config.model_path())?;
        Ok(Self { config, encoder })
    }
    
    pub fn authenticate_once(&mut self, user: &str) -> Result<bool> {
        // Fail fast if user isn't enrolled — don't touch the camera
        let t0 = Instant::now();
        let store = EmbeddingStore::load(user, &self.config.embeddings_dir())?;
        eprintln!("TIMING store_load: {:?}", t0.elapsed());

        let t1 = Instant::now();
        let frame = crate::capture::capture_ir_frame(&self.config.device(), self.config.capture_timeout_ms())?;
        eprintln!("TIMING capture: {:?}", t1.elapsed());

        let t2 = Instant::now();
        let mut frame = frame;
        crate::preprocess::histogram_equalize(&mut frame);
        let input = crate::preprocess::preprocess_ir_frame(&frame)?;
        eprintln!("TIMING preprocess: {:?}", t2.elapsed());

        let t3 = Instant::now();
        let embedding = self.encoder.encode(input.view())?;
        eprintln!("TIMING encode: {:?}", t3.elapsed());

        let result = verify_embedding(&embedding, &store, self.config.threshold());
        result
    }

    /// Continuous face scanning: opens the camera once and repeatedly captures
    /// + verifies frames for up to `duration_ms`. Returns `Ok(true)` on the
    /// first frame that matches; returns `Ok(false)` when the scan window
    /// elapses with no match.
    ///
    /// Camera-open failure is propagated immediately (fail fast) so the PAM
    /// module doesn't needlessly burn its entire timeout waiting for a locked
    /// device. Subsequent per-frame capture errors are retried (the camera
    /// stream might recover), but repeated consecutive failures return early.
    pub fn authenticate_scan(
        &mut self,
        user: &str,
        duration_ms: u64,
        interval_ms: u64,
    ) -> Result<bool> {
        // Fail fast if user isn't enrolled — don't touch the camera
        let t0 = Instant::now();
        let store = EmbeddingStore::load(user, &self.config.embeddings_dir())?;
        eprintln!("TIMING store_load: {:?}", t0.elapsed());

        // Open the camera ONCE for the entire scan window. A failure here
        // (device busy, permissions, not present) propagates immediately.
        let t_cam = Instant::now();
        let mut cam = Camera::open(&self.config.device())?;
        eprintln!("TIMING camera_open: {:?}", t_cam.elapsed());

        let deadline = Instant::now() + std::time::Duration::from_millis(duration_ms);
        let mut frame_num: usize = 0;
        let mut consecutive_capture_errors = 0u32;

        loop {
            let now = Instant::now();
            if now >= deadline {
                eprintln!(
                    "SCAN: window elapsed ({} frames processed over {:?})",
                    frame_num,
                    t0.elapsed()
                );
                return Ok(false);
            }

            frame_num += 1;
            let t_cap = Instant::now();
            let frame = match cam.capture_frame(self.config.capture_timeout_ms()) {
                Ok(f) => {
                    consecutive_capture_errors = 0;
                    f
                }
                Err(e) => {
                    consecutive_capture_errors += 1;
                    eprintln!(
                        "TIMING frame_{} capture_error: {:?} — {}",
                        frame_num,
                        t_cap.elapsed(),
                        e
                    );
                    // Fail fast if the camera is consistently unavailable;
                    // don't burn the PAM timeout on a dead device.
                    if consecutive_capture_errors >= 3 {
                        eprintln!(
                            "SCAN: aborting after {} consecutive capture failures",
                            consecutive_capture_errors
                        );
                        return Err(e);
                    }
                    // Otherwise sleep and retry within the window.
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    let sleep = std::time::Duration::from_millis(interval_ms)
                        .min(remaining);
                    std::thread::sleep(sleep);
                    continue;
                }
            };
            eprintln!("TIMING frame_{} capture: {:?}", frame_num, t_cap.elapsed());

            let t_pre = Instant::now();
            let mut frame = frame;
            crate::preprocess::histogram_equalize(&mut frame);
            let input = crate::preprocess::preprocess_ir_frame(&frame)?;
            eprintln!("TIMING frame_{} preprocess: {:?}", frame_num, t_pre.elapsed());

            let t_enc = Instant::now();
            let embedding = self.encoder.encode(input.view())?;
            eprintln!("TIMING frame_{} encode: {:?}", frame_num, t_enc.elapsed());

            let matched = verify_embedding(&embedding, &store, self.config.threshold())?;
            if matched {
                eprintln!(
                    "SCAN: match on frame {} after {:?} ({})",
                    frame_num,
                    t0.elapsed(),
                    frame_num
                );
                return Ok(true);
            }

            eprintln!(
                "SCAN: frame {} no match ({:?} elapsed, {:?} remaining)",
                frame_num,
                t0.elapsed(),
                deadline.saturating_duration_since(Instant::now())
            );

            // Sleep until the next capture interval, but never past the deadline.
            let remaining = deadline.saturating_duration_since(Instant::now());
            let sleep = std::time::Duration::from_millis(interval_ms).min(remaining);
            if !sleep.is_zero() {
                std::thread::sleep(sleep);
            }
        }
    }

    pub fn enroll(&mut self, user: &str, frames: usize, interval_ms: u64) -> Result<()> {
        let mut store = EmbeddingStore::default();
        let mut cam = Camera::open(&self.config.device())?;
        
        for i in 0..frames {
            println!("Capturing frame {}/{}...", i + 1, frames);
            let frame = cam.capture_frame(self.config.capture_timeout_ms())?;
            let mut frame = frame;
            crate::preprocess::histogram_equalize(&mut frame);
            let input = crate::preprocess::preprocess_ir_frame(&frame)?;
            let embedding = self.encoder.encode(input.view())?;
            store.add_embedding(embedding);
            
            if i < frames - 1 {
                std::thread::sleep(std::time::Duration::from_millis(interval_ms));
            }
        }
        
        store.save(user, &self.config.embeddings_dir())?;
        println!("Saved {} embeddings for user '{}'", frames, user);
        
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
        assert_eq!(config.capture_timeout_ms(), 5000);
        assert_eq!(config.scan_duration_ms(), 5000);
        assert_eq!(config.scan_interval_ms(), 200);
    }
}