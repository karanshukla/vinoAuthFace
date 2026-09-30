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

mod stats;

use clap::Parser;
use face_auth_core::capture::IrFrame;
use face_auth_core::detector::FaceDetector;
use face_auth_core::inference::FaceEncoder;
use face_auth_core::storage::EmbeddingStore;
use face_auth_core::{preprocess, verify, FaceAuthConfig};
use std::path::{Path, PathBuf};

#[derive(Parser, Debug)]
#[command(
    name = "vinoauthface-similarity-check",
    about = "Score photos against an enrolled user's templates, without a live camera"
)]
struct Args {
    #[arg(short, long, help = "Enrolled username to compare against")]
    user: String,

    #[arg(
        help = "Path(s) to face photo(s) to test",
        required_unless_present_any = ["genuine_dir", "impostor_dir"]
    )]
    images: Vec<PathBuf>,

    #[arg(long, help = "Directory of photos of the enrolled user (batch mode)")]
    genuine_dir: Option<PathBuf>,

    #[arg(long, help = "Directory of photos of other people (batch mode)")]
    impostor_dir: Option<PathBuf>,

    #[arg(
        long,
        value_name = "START:END:STEP",
        help = "Batch mode: FAR/FRR at each threshold"
    )]
    sweep: Option<String>,

    #[arg(
        long,
        value_name = "FILE",
        help = "Batch mode: write per-image scores as CSV"
    )]
    csv: Option<PathBuf>,

    #[arg(long, help = "Embeddings directory (overrides config)")]
    embeddings_dir: Option<String>,

    #[arg(long, help = "Recognition model path (overrides config)")]
    model: Option<String>,

    #[arg(
        long,
        help = "Threshold to report pass/fail against (overrides config)"
    )]
    threshold: Option<f32>,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("face_auth_core=error"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();

    let mut config = FaceAuthConfig::load()?;
    if let Some(dir) = &args.embeddings_dir {
        config.embeddings_dir = Some(dir.clone());
    }
    if let Some(model) = &args.model {
        config.model_path = Some(model.clone());
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

    let mut score_of = |path: &Path| -> anyhow::Result<Result<f32, String>> {
        let img = match image::open(path) {
            Ok(i) => i,
            Err(e) => return Ok(Err(format!("could not open image ({e})"))),
        };

        // Widen by 257 like capture.rs does, so the pipeline sees data shaped
        // exactly like a live frame.
        let luma = img.to_luma8();
        let (width, height) = luma.dimensions();
        let data = luma
            .into_raw()
            .into_iter()
            .map(|b| b as u16 * 257)
            .collect();
        let mut frame = IrFrame {
            data,
            width,
            height,
        };
        preprocess::histogram_equalize(&mut frame);

        let Some(face_box) = detector.detect(&frame)? else {
            return Ok(Err("no face detected, skipped".into()));
        };
        let input = face_auth_core::face_input(&frame, &face_box)?;
        let embedding = encoder.encode(input.view())?;
        Ok(Ok(verify::max_similarity(&embedding, &store)?))
    };

    if args.genuine_dir.is_some() || args.impostor_dir.is_some() {
        return run_batch(&args, threshold, &mut score_of);
    }

    for path in &args.images {
        let label = path.display();
        match score_of(path)? {
            Err(why) => println!("{label}: {why}"),
            Ok(score) => {
                let verdict = if score >= threshold {
                    "PASS (would match)"
                } else {
                    "fail"
                };
                println!("{label}: similarity {score:.4} vs {threshold:.4} -> {verdict}");
            }
        }
    }

    Ok(())
}

type Scorer<'a> = dyn FnMut(&Path) -> anyhow::Result<Result<f32, String>> + 'a;

/// Every regular file in `dir`, sorted so runs are comparable.
fn list_files(dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_file() {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

fn score_dir(dir: &Path, score_of: &mut Scorer) -> anyhow::Result<(Vec<(PathBuf, f32)>, usize)> {
    let mut scored = Vec::new();
    let mut skipped = 0;
    for path in list_files(dir)? {
        match score_of(&path)? {
            Ok(score) => scored.push((path, score)),
            Err(why) => {
                eprintln!("{}: {why}", path.display());
                skipped += 1;
            }
        }
    }
    Ok((scored, skipped))
}

fn run_batch(args: &Args, threshold: f32, score_of: &mut Scorer) -> anyhow::Result<()> {
    let mut genuine = Vec::new();
    let mut impostor = Vec::new();
    if let Some(dir) = &args.genuine_dir {
        let (scored, skipped) = score_dir(dir, score_of)?;
        eprintln!("genuine: {} scored, {skipped} skipped", scored.len());
        genuine = scored;
    }
    if let Some(dir) = &args.impostor_dir {
        let (scored, skipped) = score_dir(dir, score_of)?;
        eprintln!("impostor: {} scored, {skipped} skipped", scored.len());
        impostor = scored;
    }

    if let Some(path) = &args.csv {
        let mut out = String::from("kind,path,score\n");
        for (kind, rows) in [("genuine", &genuine), ("impostor", &impostor)] {
            for (p, s) in rows {
                out.push_str(&format!("{kind},{},{s:.4}\n", p.display()));
            }
        }
        std::fs::write(path, out)?;
    }

    let g: Vec<f32> = genuine.iter().map(|r| r.1).collect();
    let i: Vec<f32> = impostor.iter().map(|r| r.1).collect();

    println!("Threshold: {threshold:.4}");
    if !g.is_empty() {
        println!(
            "TAR: {:.2}% ({} genuine)",
            100.0 * stats::accept_rate(&g, threshold),
            g.len()
        );
    }
    if !i.is_empty() {
        println!(
            "FAR: {:.2}% ({} impostor)",
            100.0 * stats::accept_rate(&i, threshold),
            i.len()
        );
    }
    if let Some((t, eer)) = stats::equal_error_rate(&g, &i) {
        println!("EER: {:.2}% at threshold {t:.4}", 100.0 * eer);
    }

    if let Some(spec) = &args.sweep {
        let thresholds = stats::parse_sweep(spec).map_err(anyhow::Error::msg)?;
        println!("\nthreshold,far,frr");
        for t in thresholds {
            let far = stats::accept_rate(&i, t);
            let frr = 1.0 - stats::accept_rate(&g, t);
            println!("{t:.4},{far:.4},{frr:.4}");
        }
    }
    Ok(())
}
