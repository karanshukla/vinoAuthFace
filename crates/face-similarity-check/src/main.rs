//! Offline false-accept check. Not installed by deploy.sh.
//!
//! For each photo, runs the same equalise -> detect -> crop -> encode ->
//! cosine-similarity pipeline as a live attempt, fed from an image file
//! instead of the IR camera, and prints the score against an enrolled user.
//! Useful for gauging how close a photo of someone else gets without a second
//! person at the camera. Runs locally; only the score is printed.
//!
//! Templates are root-owned, so this needs sudo unless --embeddings-dir points
//! somewhere readable.

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
    about = "Score photos against an enrolled user's templates, without a live camera"
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

    #[arg(long, help = "Threshold to report pass/fail against (overrides config)")]
    threshold: Option<f32>,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("face_auth_core=error"));
    tracing_subscriber::fmt().with_env_filter(filter).with_writer(std::io::stderr).init();

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
    config.validate()?;

    let store = EmbeddingStore::load(&args.user, &config.embeddings_dir())?;

    // Scores across recognition models are meaningless, not just less
    // accurate. Refuse rather than print a misleading number.
    let current_tag = config.model_tag();
    if !store.model_tag_matches(&current_tag) {
        anyhow::bail!(
            "templates were enrolled with {} but {current_tag} is configured; pass --model to \
             match, or re-enrol",
            store.model_tag.as_deref().unwrap_or("unknown")
        );
    }

    let (backend, device) = (config.backend(), config.npu_device());
    let mut detector = FaceDetector::new(
        &config.detector_model_path(),
        config.detector_threshold(),
        &backend,
        &device,
    )?;
    let mut encoder = FaceEncoder::new(&config.model_path(), &backend, &device)?;
    let threshold = config.threshold();
    println!("Threshold: {threshold:.4}\n");

    for path in &args.images {
        let label = path.display();
        let img = match image::open(path) {
            Ok(i) => i,
            Err(e) => {
                println!("{label}: could not open image ({e})");
                continue;
            }
        };

        // Widen by 257 like capture.rs does, so the pipeline sees data shaped
        // exactly like a live frame.
        let luma = img.to_luma8();
        let (width, height) = luma.dimensions();
        let data = luma.into_raw().into_iter().map(|b| b as u16 * 257).collect();
        let mut frame = IrFrame { data, width, height };
        preprocess::histogram_equalize(&mut frame);

        let Some(face_box) = detector.detect(&frame)? else {
            println!("{label}: no face detected, skipped");
            continue;
        };
        let face = preprocess::crop_to_face(&frame, &face_box, FACE_CROP_MARGIN)?;
        let input = preprocess::preprocess_ir_frame(&face)?;
        let embedding = encoder.encode(input.view())?;
        let score = verify::max_similarity(&embedding, &store)?;

        let verdict = if score >= threshold { "PASS (would match)" } else { "fail" };
        println!("{label}: similarity {score:.4} vs {threshold:.4} -> {verdict}");
    }

    Ok(())
}
