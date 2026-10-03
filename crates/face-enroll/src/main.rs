use clap::{Args, CommandFactory, Parser, Subcommand};
use clap_complete::Shell;
use face_auth_core::{user, EnrollProgress, FaceAuth, FaceAuthConfig};
use tracing_subscriber::{fmt, EnvFilter};

mod doctor;

/// Enough pose and expression variation from one sitting; fewer match less reliably.
const DEFAULT_FRAMES: usize = 30;

/// The release tag the build was stamped with (`VINOAUTHFACE_VERSION`: CI, or deploy.sh
/// building a release), or "dev".
pub const VERSION: &str = match option_env!("VINOAUTHFACE_VERSION") {
    Some(v) => v,
    None => "dev",
};

#[derive(Parser, Debug)]
#[command(
    name = "vinoauthface",
    version = VERSION,
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
    /// Check the whole install: PAM hooks, models, templates, camera, sealing
    Doctor {
        /// Print a Markdown summary for a GitHub issue instead: distro, kernel,
        /// camera, models, backend and each check's status. Leaves out
        /// usernames, home paths, hostnames, enrolment counts and the camera's
        /// USB bus path.
        #[arg(long)]
        report: bool,
    },
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

    #[arg(short, long, help = "Number of frames to capture", default_value_t = DEFAULT_FRAMES)]
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
        Command::Doctor { report } => {
            let checks = doctor::run(std::path::Path::new("/etc/pam.d"), std::path::Path::new("/etc"));
            if report {
                print!("{}", doctor::markdown(&doctor::facts(), &checks));
                return Ok(());
            }
            std::process::exit(doctor::report(&checks));
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

    // Canonical name: `getent passwd 0` would otherwise enrol into a directory "0".
    let info = user::lookup(&args.user)?;

    // The system store is root-owned; say so up front rather
    // than failing on EACCES. An explicit --embeddings-dir is the caller's business.
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

    // As root, only root's own choices count: /etc/face-auth.toml and this
    // command line. `sudo -E` or doas `keepenv` would otherwise let the
    // caller's ~/.config/face-auth.toml and FACE_AUTH_* pick the camera, model
    // and store root writes templates for.
    let mut config = if euid == 0 { FaceAuthConfig::load_system()? } else { FaceAuthConfig::load()? };

    if let Some(device) = args.device {
        if euid == 0 {
            check_device(&device, config.device.as_deref(), face_auth_core::capture::is_ir_capture_device)?;
        }
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
    let device = config.device()?;

    println!("Enrolling '{}'", info.name);
    println!("  camera:     {device}");
    println!("  model:      {}", config.model_path());
    println!("  templates:  {}", config.embeddings_dir().display());
    if config.seal_embeddings() {
        let sealed = face_auth_core::seal::available();
        println!("  sealing:    {}", if sealed { "TPM" } else { "requested, but no TPM found; NOT sealed" });
    }
    println!();

    let pinned = config.pinned_camera_path.is_some();
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

/// A `--device` given to root must be an IR capture node, or the one
/// `/etc/face-auth.toml` already names (root's own choice, which may be a
/// pin-camera.sh symlink or a test loopback the IR checks can't vouch for).
fn check_device(device: &str, system: Option<&str>, is_ir: impl Fn(&str) -> bool) -> anyhow::Result<()> {
    if system == Some(device) || is_ir(device) {
        return Ok(());
    }
    anyhow::bail!(
        "--device {device} is not an IR capture device on this machine; see \
         `vinoauthface-camera-diag list`, or set `device` in /etc/face-auth.toml to use it anyway"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_device_must_be_ir_or_the_system_choice() {
        let never_ir = |_: &str| false;
        assert!(check_device("/dev/video0", None, never_ir).is_err());
        assert!(check_device("/dev/video0", Some("/dev/video10"), never_ir).is_err());
        assert!(check_device("/dev/video10", Some("/dev/video10"), never_ir).is_ok());
        assert!(check_device("/dev/video2", None, |d: &str| d == "/dev/video2").is_ok());
    }
}
