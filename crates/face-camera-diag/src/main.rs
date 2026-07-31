//! Camera discovery/diagnostic tool, not deployed by deploy.sh (same category as
//! face-similarity-check). Lists every V4L2 video node with driver/card/VID:PID/pixel-format
//! info so someone can sanity-check their own camera setup — which node is the IR sensor, what
//! pixel format it reports, whether it even has a resolvable VID:PID — without reading through
//! the whole README or reverse-engineering journalctl output. `dump` additionally grabs one
//! frame and writes it as a 16-bit PGM for visual inspection.
//!
//! Read-only against devices it's just listing (VIDIOC_QUERYCAP/G_FMT); `dump` takes the target
//! device over for capture the same way live face-auth would.

use clap::{Parser, Subcommand};
use face_auth_core::capture;
use std::io::Write;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "face-camera-diag", about = "Discover and sanity-check V4L2 cameras for face-auth")]
struct Args {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// List all V4L2 video nodes with driver, card, VID:PID, and pixel format (default).
    List,
    /// Capture one frame from a device and write it as a 16-bit PGM for visual inspection.
    Dump {
        #[arg(long, help = "Video device to capture from, e.g. /dev/video3")]
        device: String,
        #[arg(long, default_value = "frame.pgm", help = "Output PGM file path")]
        out: PathBuf,
        #[arg(long, default_value = "5000", help = "Capture timeout in ms")]
        timeout_ms: i32,
    },
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    match args.command.unwrap_or(Command::List) {
        Command::List => list_devices(),
        Command::Dump { device, out, timeout_ms } => dump_frame(&device, &out, timeout_ms),
    }
}

fn list_devices() -> anyhow::Result<()> {
    let devices = capture::list_video_devices();
    if devices.is_empty() {
        println!("No /dev/video* nodes found under /sys/class/video4linux.");
        return Ok(());
    }

    println!(
        "{:<14} {:<10} {:<24} {:<10} {:<12} {}",
        "DEVICE", "DRIVER", "CARD", "VID:PID", "FORMAT", "IR GUESS"
    );
    for device in &devices {
        let caps = capture::query_caps(device);
        let (driver, card) = match &caps {
            Ok(c) => (c.driver.clone(), c.card.clone()),
            Err(e) => ("?".to_string(), format!("<unavailable: {}>", e)),
        };

        let vid_pid = capture::usb_ids(device)
            .map(|(vid, pid)| format!("{}:{}", vid, pid))
            .unwrap_or_else(|_| "-".to_string());

        let format = match capture::query_format(device) {
            Ok((w, h, fourcc)) => format!("{}x{} {}", w, h, capture::fourcc_to_string(fourcc)),
            Err(_) => "-".to_string(),
        };

        let ir_guess = if card.to_lowercase().contains("ir") || card.to_lowercase().contains("infrared") {
            "likely"
        } else {
            ""
        };

        println!(
            "{:<14} {:<10} {:<24} {:<10} {:<12} {}",
            device, driver, card, vid_pid, format, ir_guess
        );
    }
    Ok(())
}

fn dump_frame(device: &str, out: &PathBuf, timeout_ms: i32) -> anyhow::Result<()> {
    let frame = capture::capture_ir_frame(device, timeout_ms)?;
    println!(
        "Captured {}x{} frame from {} ({} samples)",
        frame.width, frame.height, device, frame.data.len()
    );

    let mut file = std::fs::File::create(out)?;
    write!(file, "P5\n{} {}\n65535\n", frame.width, frame.height)?;
    for sample in &frame.data {
        file.write_all(&sample.to_be_bytes())?;
    }
    println!("Wrote {}", out.display());
    Ok(())
}
