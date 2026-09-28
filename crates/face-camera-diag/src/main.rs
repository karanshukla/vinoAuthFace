//! Camera discovery and sanity check. Not installed by deploy.sh.
//!
//! `list` shows every V4L2 node with driver, card, VID:PID, current format,
//! whether its name looks like an IR sensor, and which node face-auth would
//! pick on its own. It only queries capabilities and format, so it is safe
//! against a camera in use. `dump` captures one illuminated frame and writes
//! it as a 16-bit PGM for a visual check.

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
    /// List every V4L2 node with driver, card, VID:PID and pixel format (default).
    List,
    /// Capture one frame and write it as a 16-bit PGM.
    Dump {
        #[arg(long, help = "Video device to capture from, e.g. /dev/video2")]
        device: String,
        #[arg(long, default_value = "frame.pgm", help = "Output PGM path")]
        out: PathBuf,
        #[arg(long, default_value = "5000", help = "Capture timeout in ms")]
        timeout_ms: i32,
    },
}

fn main() -> anyhow::Result<()> {
    match Args::parse().command.unwrap_or(Command::List) {
        Command::List => list_devices(),
        Command::Dump { device, out, timeout_ms } => dump_frame(&device, &out, timeout_ms),
    }
}

fn list_devices() -> anyhow::Result<()> {
    let devices = capture::list_video_devices();
    if devices.is_empty() {
        println!("No /dev/video* nodes under /sys/class/video4linux.");
        return Ok(());
    }

    let chosen = capture::detect_ir_camera();
    println!(
        "{:<14} {:<10} {:<28} {:<10} {:<14} NOTES",
        "DEVICE", "DRIVER", "CARD", "VID:PID", "FORMAT"
    );
    for device in &devices {
        let (driver, card) = match capture::query_caps(device) {
            Ok(c) => (c.driver, c.card),
            Err(e) => ("?".to_string(), format!("<unavailable: {e}>")),
        };
        let vid_pid = capture::usb_ids(device)
            .map(|(vid, pid)| format!("{vid}:{pid}"))
            .unwrap_or_else(|_| "-".to_string());
        let format = capture::query_format(device)
            .map(|(w, h, fourcc)| format!("{w}x{h} {}", capture::fourcc_to_string(fourcc)))
            .unwrap_or_else(|_| "-".to_string());

        let mut notes = Vec::new();
        if capture::name_suggests_ir(&card) {
            notes.push("IR name");
        }
        if chosen.as_deref() == Some(device.as_str()) {
            notes.push("<- auto-detect picks this");
        }
        println!(
            "{:<14} {:<10} {:<28} {:<10} {:<14} {}",
            device,
            driver,
            card,
            vid_pid,
            format,
            notes.join(", ")
        );
    }
    if chosen.is_none() {
        println!("\nAuto-detect found no usable IR capture node; set `device` in /etc/face-auth.toml.");
    }
    Ok(())
}

fn dump_frame(device: &str, out: &PathBuf, timeout_ms: i32) -> anyhow::Result<()> {
    let frame = capture::capture_ir_frame(device, timeout_ms)?;
    println!("Captured {}x{} frame from {device}", frame.width, frame.height);

    let mut file = std::io::BufWriter::new(std::fs::File::create(out)?);
    write!(file, "P5\n{} {}\n65535\n", frame.width, frame.height)?;
    for sample in &frame.data[..(frame.width * frame.height) as usize] {
        file.write_all(&sample.to_be_bytes())?;
    }
    file.flush()?;
    println!("Wrote {}", out.display());
    Ok(())
}
