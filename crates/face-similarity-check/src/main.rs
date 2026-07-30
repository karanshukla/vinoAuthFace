//! Offline check: for a set of face photos, would each pass as a match against an enrolled
//! user's stored embeddings? Runs the exact same detect -> crop -> CLAHE -> encode -> cosine
//! similarity pipeline as a live authentication attempt, just fed from image files instead of
//! the IR camera — useful for gauging false-accept risk (a photo of someone else, a look-alike,
//! etc.) without needing a second person physically present at the camera.
//!
//! Everything here runs locally against the on-disk model and embeddings; nothing is sent
//! anywhere. Only the computed similarity score is printed, never image content.

use clap::Parser;
use face_auth_core::capture::IrFrame;
use face_auth_core::detector::FaceDetector;
use face_auth_core::inference::FaceEncoder;
use face_auth_core::storage::EmbeddingStore;
use face_auth_core::{preprocess, verify, FaceAuthConfig, FACE_CROP_MARGIN};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "face-similarity-check",
    about = "Check whether photo(s) would pass as a face-auth match against an enrolled user, without needing a live camera"
)]
struct Args {
    #[arg(short, long, help = "Enrolled username to compare against")]
    user: String,

    #[arg(help = "Path(s) to face photo(s) to test", required = true)]
    images: Vec<PathBuf>,

    #[arg(long, help = "Embeddings directory (overrides config)")]
    embeddings_dir: Option<String>,

    #[arg(long, help = "Recognition model path (overrides config)")]
    model: Option<String>,

    #[arg(long, help = "Similarity threshold to evaluate pass/fail against (overrides config)")]
    threshold: Option<f32>,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("face_auth_core=error"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let mut config = FaceAuthConfig::load()?;
    if let Some(dir) = args.embeddings_dir {
        config.embeddings_dir = Some(dir);
    }
    if let Some(model) = args.model {
        config.model_path = Some(model);
    }
    if let Some(t) = args.threshold {
        config.threshold = Some(t);
    }

    let store = EmbeddingStore::load(&args.user, &config.embeddings_dir())?;

    // Same guard FaceAuth::check_model_tag applies to live auth: comparing across recognition
    // models is meaningless, not just less accurate, so refuse rather than print a misleading
    // number.
    let current_tag = config.model_tag();
    if !store.model_tag_matches(&current_tag) {
        anyhow::bail!(
            "Enrolled embeddings were created with a different recognition model ({:?}) than the \
             one currently configured ({current_tag}) — results wouldn't be comparable. Re-run \
             face-enroll first, or pass --model to match the model the embeddings were saved with.",
            store.model_tag
        );
    }

    let backend = config.backend();
    let npu_device = config.npu_device();
    let mut detector = FaceDetector::new(
        &config.detector_model_path(),
        config.detector_threshold(),
        &backend,
        &npu_device,
    )?;
    let mut encoder = FaceEncoder::new(&config.model_path(), &backend, &npu_device)?;
    let threshold = config.threshold();

    println!("Threshold: {:.4}", threshold);
    println!();

    for path in &args.images {
        let label = path.display();

        let img = match image::open(path) {
            Ok(i) => i,
            Err(e) => {
                println!("{label}: could not open image ({e})");
                continue;
            }
        };

        // Same *257 upscale capture.rs applies to raw camera bytes, so this feeds the rest of
        // the pipeline data shaped exactly like a live IR frame (256 real levels, u16-ranged).
        let luma = img.to_luma8();
        let (width, height) = luma.dimensions();
        let data: Vec<u16> = luma.into_raw().into_iter().map(|b| b as u16 * 257).collect();
        let mut frame = IrFrame { data, width, height };

        preprocess::clahe(&mut frame);

        let face_box = match detector.detect(&frame)? {
            Some(b) => b,
            None => {
                println!("{label}: no face detected, skipped");
                continue;
            }
        };

        let cropped = preprocess::crop_to_face(&frame, &face_box, FACE_CROP_MARGIN);
        let input = preprocess::preprocess_ir_frame(&cropped)?;
        let embedding = encoder.encode(input.view())?;
        let score = verify::max_similarity(&embedding, &store)?;

        let verdict = if score >= threshold { "PASS (would match)" } else { "fail (would not match)" };
        println!("{label}: similarity {score:.4} vs threshold {threshold:.4} -> {verdict}");
    }

    Ok(())
}
