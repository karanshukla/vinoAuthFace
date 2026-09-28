# Hardware and diagnostics

## Compatibility

face-auth only speaks V4L2 via `uvcvideo`. There's no libcamera integration, so a camera behind a
different kernel stack (Intel IPU6, MIPI CSI) is unreachable whatever format it reports. Within
`uvcvideo` it needs an IR capture node (`GREY`, `YUYV` or `Y16`) for the spoof resistance in
[security.md](security.md) to hold. The pixel format is read from the driver, not assumed.

| Camera stack | `face-camera-diag list` output | Support |
|---|---|---|
| UVC IR (`uvcvideo` + GREY/YUYV/Y16 IR node) | `DRIVER=uvcvideo`, a node reports `GREY`/`YUYV`/`Y16` | ✅ Supported |
| UVC RGB-only (`uvcvideo`, no IR node) | `DRIVER=uvcvideo`, only a colour-format node exists | ⚠️ Never auto-detected, and MJPEG is refused. A YUYV node set explicitly as `device` will capture, but with no IR there's no spoof resistance |
| Intel IPU6 / MIPI / libcamera | camera doesn't appear as a plain `uvcvideo` node | ❌ Not supported |

x86_64 only: V4L2 ioctl numbers and struct layouts are hardcoded.

### Reports

| Laptop | IR camera | Driver | Tier | Notes |
|---|---|---|---|---|
| Unconfirmed model, FHD webcam with IR | `2b7e:55c0`, IR on `/dev/video2`, 360x360 GREY | `uvcvideo` | ✅ Supported | Reference hardware for this fork |

If face-auth works (or doesn't) on yours, please
[open an issue](https://github.com/karanshukla/vinoAuthFace/issues/new) with your
`face-camera-diag list` output. Table format borrowed from
[Visage](https://github.com/sovren-software/visage)'s hardware docs.

## face-camera-diag

A small offline tool, not installed by `deploy.sh`. Grab it from
[Releases](https://github.com/karanshukla/vinoAuthFace/releases):

```bash
curl -fLO https://github.com/karanshukla/vinoAuthFace/releases/latest/download/face-camera-diag-x86_64-unknown-linux-musl
chmod +x face-camera-diag-x86_64-unknown-linux-musl
```

or build it with `cargo build --release -p face-camera-diag`.

`list` shows every V4L2 node with driver, card name, USB VID:PID, current format, and which one
auto-detect would pick:

```
$ face-camera-diag list
DEVICE         DRIVER     CARD                         VID:PID    FORMAT         NOTES
/dev/video0    uvcvideo   Integrated_Webcam_FHD: Integrat 2b7e:55c0  1920x1080 MJPG
/dev/video1    uvcvideo   Integrated_Webcam_FHD: Integrat 2b7e:55c0  -
/dev/video2    uvcvideo   Integrated_Webcam_FHD: Integrat 2b7e:55c0  360x360 GREY   <- auto-detect picks this
/dev/video3    uvcvideo   Integrated_Webcam_FHD: Integrat 2b7e:55c0  -
```

`GREY`, `YUYV` or `Y16` is a format face-auth can capture. `-` usually means a paired metadata
node. The card name here says nothing about IR (sysfs truncates it at 32 bytes); auto-detect
still finds the sensor because it's a physical node streaming native greyscale, which RGB webcams
never do.

`dump` captures one frame as a 16-bit PGM, to confirm you're seeing a lit IR image and not noise
or a black frame:

```bash
face-camera-diag dump --device /dev/video2 --out frame.pgm
```

## Other tools

From a source checkout:

| Command | Shows |
|---|---|
| `cargo run --example detect-camera` | Why each node is or isn't treated as IR |
| `cargo run --example frame-stats` | Per-frame brightness, for spotting a strobing illuminator |
| `cargo run --release --example bench` | Per-stage timings |

`face-similarity-check` scores photos against your enrolled templates through the same pipeline,
for gauging false-accept risk without a second person at the camera. Templates are root-owned, so
it needs sudo:

```bash
sudo face-similarity-check --user $USER photo1.jpg photo2.png
```
