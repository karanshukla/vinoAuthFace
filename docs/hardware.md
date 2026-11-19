---
title: Hardware and diagnostics
nav_order: 5
---

# Hardware and diagnostics

## Compatibility

vinoAuthFace communicates with cameras strictly over V4L2 via the `uvcvideo` kernel driver. It does
not integrate with `libcamera`, so cameras behind other kernel stacks (such as Intel IPU6 or MIPI
CSI) are unsupported regardless of their reported pixel formats.

Within `uvcvideo`, you need an IR capture node outputting `GREY`, `YUYV`, or `Y16` for the
screen-spoof resistance described in [security.md](security.md) to hold. The pixel format is queried
from the driver rather than assumed. For `Y16` streams from 10- or 12-bit sensors whose samples sit
in the lower bits, capture automatically scales intensities using the highest bit observed during
the scan, so both low- and high-aligned `Y16` sensors work.

| Camera stack | `vinoauthface-camera-diag list` output | Support |
|---|---|---|
| UVC IR (`uvcvideo` + `GREY`/`YUYV`/`Y16` IR node) | `DRIVER=uvcvideo`, with a node reporting `GREY`, `YUYV`, or `Y16` | ✅ Supported |
| UVC RGB-only (`uvcvideo`, no IR node) | `DRIVER=uvcvideo`, colour nodes only | ⚠️ Never auto-detected, and MJPEG is rejected. Setting a `YUYV` node manually as `device` will capture frames, but without IR there is no screen-spoof resistance. |
| Intel IPU6 / MIPI / libcamera | Camera does not appear as a standard `uvcvideo` node | ❌ Not supported |

Currently **x86_64 only**: V4L2 ioctl numbers and struct layouts are hardcoded for x86_64.

### Reports

| Laptop | IR camera | Driver | Tier | Notes |
|---|---|---|---|---|
| Unconfirmed model, FHD webcam with IR | `2b7e:55c0`, IR on `/dev/video2`, 360x360 `GREY` | `uvcvideo` | ✅ Supported | Reference hardware for this fork |

If vinoAuthFace works (or fails) on your hardware, please
[open an issue](https://github.com/karanshukla/vinoAuthFace/issues/new) with the output of
`sudo vinoauthface doctor --report` (which strips usernames, paths, and hostnames), or
`vinoauthface-camera-diag list` if vinoAuthFace is not installed. Table format borrowed from
[Visage](https://github.com/sovren-software/visage)'s hardware documentation.

## vinoauthface-camera-diag

`vinoauthface-camera-diag` is a standalone offline diagnostic tool that is not installed by
`deploy.sh`. Download it from [Releases](https://github.com/karanshukla/vinoAuthFace/releases):

```bash
curl -fLO https://github.com/karanshukla/vinoAuthFace/releases/latest/download/vinoauthface-camera-diag-x86_64-unknown-linux-musl
chmod +x vinoauthface-camera-diag-x86_64-unknown-linux-musl
```

Or build it from source with `cargo build --release -p face-camera-diag`.

### Listing V4L2 devices

`list` inspects every V4L2 node and prints its driver, card name, USB `VID:PID`, active pixel
format, and which node auto-detection would choose:

```text
$ vinoauthface-camera-diag list
DEVICE         DRIVER     CARD                         VID:PID    FORMAT         NOTES
/dev/video0    uvcvideo   Integrated_Webcam_FHD: Integrat 2b7e:55c0  1920x1080 MJPG
/dev/video1    uvcvideo   Integrated_Webcam_FHD: Integrat 2b7e:55c0  -
/dev/video2    uvcvideo   Integrated_Webcam_FHD: Integrat 2b7e:55c0  360x360 GREY   <- auto-detect picks this
/dev/video3    uvcvideo   Integrated_Webcam_FHD: Integrat 2b7e:55c0  -
```

Nodes reporting `GREY`, `YUYV`, or `Y16` can be captured by vinoAuthFace. A `-` in the format column
usually indicates a paired metadata node. In the example above, the sysfs card name does not mention
IR (sysfs truncates card names at 32 bytes), yet auto-detection still identifies `/dev/video2`
because it is a physical camera streaming native greyscale, which standard RGB webcams do not do.

### Dumping a test frame

`dump` captures a single frame as a 16-bit PGM image so you can verify the camera produces a lit IR
image instead of noise or a blank frame:

```bash
vinoauthface-camera-diag dump --device /dev/video2 --out frame.pgm
```

## Other tools

From a source checkout, several Cargo examples help diagnose camera behaviour and performance:

| Command | Output |
|---|---|
| `cargo run --example detect-camera` | Explains why each V4L2 node is or is not recognized as IR |
| `cargo run --example frame-stats` | Prints per-frame brightness to identify strobing IR illuminators |
| `cargo run --release --example bench` | Measures per-stage pipeline timings |

### `vinoauthface-similarity-check`

`vinoauthface-similarity-check` runs image files through the same recognition pipeline and scores
them against your enrolled templates, letting you evaluate false-accept risk without needing another
person in front of the camera. Because templates are root-owned, run it with `sudo`:

```bash
sudo vinoauthface-similarity-check --user $USER photo1.jpg photo2.png
```

In batch mode, you can measure overall pipeline accuracy across directories of genuine (your own)
and impostor (other people's) photos. It reports True Accept Rate (TAR), False Accept Rate (FAR),
and the Equal Error Rate (EER) at your configured threshold. Images with no detectable face are
skipped and counted on stderr:

```bash
sudo vinoauthface-similarity-check --user $USER --genuine-dir me/ --impostor-dir others/ \
  --sweep 0.30:0.70:0.02 --csv scores.csv
```

- `--sweep` prints `threshold,far,frr` CSV rows across the specified range so runs can be compared.
- `--csv` writes the individual score for every processed image.
- You can pass either `--genuine-dir` or `--impostor-dir` on its own to measure only that rate.
