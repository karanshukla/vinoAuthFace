//! Diagnostic: how well recorded IR clips match a gallery enrolled from
//! another clip, through the same detect → crop-or-align → encode path as a
//! scan. For comparing detectors (and so alignment, #27) without enrolling.
//!
//!   ffmpeg -i clip.mkv -f rawvideo -pix_fmt gray clip.gray
//!   cargo run --release --example clip-similarity -- DETECTOR.onnx W H GALLERY.gray PROBE.gray...
//!
//! The gallery is the first 30 face frames of GALLERY (an enrolment's worth);
//! its remaining frames are scored as a probe too. Prints, per probe, the
//! best-match similarity's spread and the share of frames at or above the
//! configured threshold. Recognition model and threshold come from the usual
//! config.
use face_auth_core::capture::IrFrame;
use face_auth_core::detector::{assess_frame, FaceDetector, FrameQuality};
use face_auth_core::inference::FaceEncoder;
use face_auth_core::preprocess::histogram_equalize;
use face_auth_core::storage::EmbeddingStore;
use face_auth_core::verify::max_similarity;
use face_auth_core::{face_input, FaceAuthConfig};
use std::io::Read;
use std::time::{Duration, Instant};

const GALLERY_FRAMES: usize = 30;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [detector_path, w, h, gallery, probes @ ..] = args.as_slice() else {
        anyhow::bail!("usage: clip-similarity DETECTOR.onnx WIDTH HEIGHT GALLERY.gray PROBE.gray...");
    };
    let (width, height): (u32, u32) = (w.parse()?, h.parse()?);
    let config = FaceAuthConfig::load()?;
    let mut detector =
        FaceDetector::new(detector_path, config.detector_threshold(), &config.backend(), &config.npu_device())?;
    let mut encoder = FaceEncoder::new(&config.model_path(), &config.backend(), &config.npu_device())?;
    println!("detector {detector_path} ({:?}), threshold {}", detector.kind(), config.threshold());

    let (mut spent, mut faces) = (Duration::ZERO, 0u32);
    let mut embed = |path: &str| -> anyhow::Result<(Vec<Vec<f32>>, usize)> {
        let mut file = std::fs::File::open(path)?;
        let mut buf = vec![0u8; (width * height) as usize];
        let mut read = |file: &mut std::fs::File| -> Option<IrFrame> {
            file.read_exact(&mut buf).ok()?;
            Some(IrFrame { data: buf.iter().map(|&b| b as u16 * 257).collect(), width, height })
        };
        let (mut out, mut frames) = (Vec::new(), 0);
        // Pairs, keeping the brighter, as `capture_illuminated_frame` does.
        while let (Some(a), Some(b)) = (read(&mut file), read(&mut file)) {
            frames += 1;
            let mut frame = if b.mean_intensity() > a.mean_intensity() { b } else { a };
            if assess_frame(&frame) != FrameQuality::Ok {
                continue;
            }
            histogram_equalize(&mut frame);
            let t = Instant::now();
            let Some(face) = detector.detect(&frame)? else { continue };
            let Ok(input) = face_input(&frame, &face) else { continue };
            out.push(encoder.encode(input.view())?);
            spent += t.elapsed();
            faces += 1;
        }
        Ok((out, frames))
    };

    let (mut first, frames) = embed(gallery)?;
    anyhow::ensure!(first.len() > GALLERY_FRAMES, "{gallery}: only {} face frames", first.len());
    let rest = first.split_off(GALLERY_FRAMES);
    let store = EmbeddingStore { embeddings: first, model_tag: None };
    let report = |name: &str, embeddings: &[Vec<f32>], frames: usize| -> anyhow::Result<()> {
        let mut scores: Vec<f32> = embeddings.iter().map(|e| max_similarity(e, &store)).collect::<anyhow::Result<_>>()?;
        scores.sort_by(f32::total_cmp);
        let pass = scores.iter().filter(|&&s| s >= config.threshold()).count();
        let at = |q: f32| scores.get(((scores.len() - 1) as f32 * q) as usize).copied().unwrap_or(f32::NAN);
        println!(
            "{name}: {}/{frames} face frames, min {:.3} p10 {:.3} median {:.3} max {:.3}, {pass} ({:.0}%) >= threshold",
            scores.len(),
            at(0.0),
            at(0.1),
            at(0.5),
            at(1.0),
            100.0 * pass as f32 / scores.len().max(1) as f32
        );
        Ok(())
    };
    report(&format!("{gallery} (rest)"), &rest, frames)?;
    for probe in probes {
        let (embeddings, frames) = embed(probe)?;
        report(probe, &embeddings, frames)?;
    }
    println!("detect + align/crop + encode: {:?} per face frame (this machine)", spent / faces.max(1));
    Ok(())
}
