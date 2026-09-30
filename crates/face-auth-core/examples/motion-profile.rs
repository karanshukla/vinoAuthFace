//! Diagnostic: liveness motion profile over a recorded IR clip.
//!
//! Feeds raw 8-bit greyscale frames through the same path as a scan (dark
//! strobe frames dropped, CLAHE, detect, face patch from the raw frame) and
//! prints `total`/`residual`/`shift` for each consecutive pair of face frames,
//! plus how long the liveness step took. For picking
//! `liveness_residual_motion_threshold` from real captures and real prints.
//!
//!   ffmpeg -i clip.mkv -f rawvideo -pix_fmt gray clip.gray
//!   cargo run --release --example motion-profile -- clip.gray 360 360 [GAP]
//!
//! `GAP` compares each face frame against the oldest of the last `GAP` face
//! frames (default 1, consecutive), the way `authenticate_scan` uses
//! `liveness_baseline_ms`. At the laptop's ~7.5 face frames/s, 8 is about 1 s.
use face_auth_core::capture::IrFrame;
use face_auth_core::detector::{assess_frame, FaceDetector, FrameQuality};
use face_auth_core::preprocess::{face_patch, histogram_equalize, motion_profile};
use face_auth_core::FaceAuthConfig;
use std::io::Read;
use std::time::{Duration, Instant};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (path, w, h, gap) = match args.as_slice() {
        [path, w, h] => (path, w, h, 1),
        [path, w, h, gap] => (path, w, h, gap.parse()?),
        _ => anyhow::bail!("usage: motion-profile FILE.gray WIDTH HEIGHT [GAP]"),
    };
    let (width, height): (u32, u32) = (w.parse()?, h.parse()?);
    anyhow::ensure!(gap >= 1, "GAP must be at least 1");

    let config = FaceAuthConfig::load()?;
    let mut detector = FaceDetector::new(
        &config.detector_model_path(),
        config.detector_threshold(),
        &config.backend(),
        &config.npu_device(),
    )?;

    let mut file = std::fs::File::open(path)?;
    let mut buf = vec![0u8; (width * height) as usize];
    let mut history: std::collections::VecDeque<(IrFrame, _)> = std::collections::VecDeque::new();
    let mut spent = Duration::ZERO;
    let mut pairs = 0u32;
    println!("{:>5} {:>7} {:>8} {:>7} {:>7}", "frame", "total", "residual", "local", "shift");

    let mut read = |file: &mut std::fs::File| -> Option<IrFrame> {
        file.read_exact(&mut buf).ok()?;
        Some(IrFrame { data: buf.iter().map(|&b| b as u16 * 257).collect(), width, height })
    };
    for n in 0.. {
        // Frames in pairs, keeping the brighter, as `capture_illuminated_frame`
        // does: a strobing emitter's dark frame can still pass `assess_frame`.
        let (Some(a), Some(b)) = (read(&mut file), read(&mut file)) else {
            break;
        };
        let raw = if b.mean_intensity() > a.mean_intensity() { b } else { a };
        if assess_frame(&raw) != FrameQuality::Ok {
            continue;
        }
        let mut eq = raw.clone();
        histogram_equalize(&mut eq);
        let Some(face_box) = detector.detect(&eq)? else {
            continue;
        };

        // Same pairing as `authenticate_scan`: both patches cut with the
        // earlier frame's box.
        let t = Instant::now();
        let profile = match history.front() {
            Some((prev_raw, prev_box)) => {
                Some(motion_profile(&face_patch(prev_raw, prev_box)?, &face_patch(&raw, prev_box)?))
            }
            None => None,
        };
        let dt = t.elapsed();
        if history.len() == gap {
            history.pop_front();
        }
        history.push_back((raw, face_box));

        if let Some(m) = profile {
            spent += dt;
            pairs += 1;
            println!("{n:>5} {:>7.4} {:>8.4} {:>7.4} ({:.2},{:.2})", m.total, m.residual, m.local, m.shift.0, m.shift.1);
        }
    }
    if pairs > 0 {
        println!("\n{pairs} pairs, liveness step {:?} per frame", spent / pairs);
    }
    Ok(())
}
