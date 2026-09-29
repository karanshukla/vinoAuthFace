use clap::{Args, CommandFactory, Parser, Subcommand};
use clap_complete::Shell;
use face_auth_core::{user, EnrollProgress, FaceAuth, FaceAuthConfig};
use tracing_subscriber::{fmt, EnvFilter};

#[derive(Parser, Debug)]
#[command(
    name = "vinoauthface",
    about = "vinoAuthFace: IR camera face unlock",
    after_help = "Templates live under a root-owned directory, so enrol and improve must be run with sudo."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,

    #[arg(short, long, global = true, help = "Verbose output")]
    verbose: bool,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Enrol a face, replacing any existing templates
    Enroll(Capture),
    /// Capture more frames and append them to the existing templates
    Improve(Capture),
    #[command(hide = true, about = "Print a shell completion script and exit")]
    Completions {
        #[arg(value_name = "SHELL")]
        shell: Shell,
    },
}

#[derive(Args, Debug)]
struct Capture {
    #[arg(short, long, help = "Username to enrol")]
    user: String,

    /// 30 gives enough pose and expression variation from one sitting for
    /// reliable matching; fewer enrol faster but match less reliably.
    #[arg(short, long, help = "Number of frames to capture", default_value = "30")]
    frames: usize,

    #[arg(long, help = "Interval between frames (ms)", default_value = "400")]
    interval: u64,

    #[arg(long, help = "Camera device path (overrides config)")]
    device: Option<String>,

    #[arg(long, help = "Similarity threshold (overrides config)")]
    threshold: Option<f32>,

    #[arg(long, help = "Model path (overrides config)")]
    model: Option<String>,

    #[arg(long, help = "Embeddings directory (overrides config)")]
    embeddings_dir: Option<String>,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let filter = if cli.verbose {
        EnvFilter::new("face_auth_core=debug,vinoauthface=debug")
    } else {
        EnvFilter::new("face_auth_core=warn,vinoauthface=info")
    };
    match cli.command {
        Command::Completions { shell } => {
            clap_complete::generate(shell, &mut Cli::command(), "vinoauthface", &mut std::io::stdout());
            Ok(())
        }
        Command::Enroll(args) => {
            fmt().with_env_filter(filter).with_writer(std::io::stderr).init();
            run(args, false)
        }
        Command::Improve(args) => {
            fmt().with_env_filter(filter).with_writer(std::io::stderr).init();
            run(args, true)
        }
    }
}

fn run(args: Capture, improve: bool) -> anyhow::Result<()> {
    if args.frames == 0 {
        anyhow::bail!("--frames must be at least 1");
    }

    // Resolve to the canonical account name. `getent passwd 0` succeeds and
    // would otherwise enrol into a directory literally named "0", which the
    // authentication path (looking up "root") never reads — a silent no-op.
    let info = user::lookup(&args.user)?;

    // The system template directory is root-owned 0700 so that no unprivileged
    // process can plant a face for an account. Writing there needs root; say so
    // clearly rather than failing later on EACCES. An explicit --embeddings-dir
    // is the caller's own business, so it is left alone.
    let euid = unsafe { libc::geteuid() };
    if euid != 0 && args.embeddings_dir.is_none() {
        anyhow::bail!(
            "enrolment writes to a root-owned template store; re-run with:\n\
             \n    sudo vinoauthface {} --user {}\n\
             \nOr pass --embeddings-dir to write somewhere you own.",
            if improve { "improve" } else { "enroll" },
            info.name
        );
    }

    let mut config = FaceAuthConfig::load()?;

    if let Some(device) = args.device {
        config.device = Some(device);
    }
    if let Some(model) = args.model {
        config.model_path = Some(model);
    }
    if let Some(dir) = args.embeddings_dir {
        config.embeddings_dir = Some(dir);
    }
    if let Some(threshold) = args.threshold {
        config.threshold = Some(threshold);
    }
    config.validate()?;

    println!("Enrolling '{}'", info.name);
    println!("  camera:     {}", config.device());
    println!("  model:      {}", config.model_path());
    println!("  templates:  {}", config.embeddings_dir().display());
    if config.seal_embeddings() {
        let sealed = face_auth_core::seal::available();
        println!("  sealing:    {}", if sealed { "TPM" } else { "requested, but no TPM found; NOT sealed" });
    }
    println!();

    let pinned = config.pinned_camera_path.is_some();
    let device = config.device();
    let mut auth = FaceAuth::new(config)?;

    let mut progress = |p: EnrollProgress| match p {
        EnrollProgress::Capturing { captured, wanted, attempt } => {
            println!("Capturing frame {}/{} (attempt {})...", captured + 1, wanted, attempt);
        }
        EnrollProgress::NoContent => println!("  nothing in frame, retrying..."),
        EnrollProgress::NoFace => println!("  no face detected, retrying..."),
        EnrollProgress::FaceTooSmall => println!("  face too small, move closer..."),
        EnrollProgress::Captured { captured, wanted } => {
            println!("  captured {captured}/{wanted}");
        }
    };

    if improve {
        let (added, total) =
            auth.enroll_append(&info.name, args.frames, args.interval, &mut progress)?;
        println!("\nAdded {} embeddings for '{}' ({} total)", added, info.name, total);
    } else {
        let saved = auth.enroll(&info.name, args.frames, args.interval, &mut progress)?;
        println!("\nSaved {} embeddings for '{}'", saved, info.name);
        if !pinned {
            println!("\nThat confirms {device} is the right camera. Recommended next step:");
            println!("  sudo ./pin-camera.sh {device}");
            println!("so a spoofed USB device claiming the same VID/PID cannot inject frames.");
        }
    }

    Ok(())
}
