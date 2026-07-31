# authFace — IR Camera Face Unlock for Linux

**Windows Hello–style biometric login for Linux.** IR camera facial authentication via PAM — works on **immutable distros** (Bazzite, Bluefin, Fedora Silverblue, Fedora Kinoite, etc.) with zero system packages, daemons, or layering.

- **Face unlock for sudo, lock screen (GNOME/Sway), and `gdm-password`**
- **~2 seconds** from camera poll to authenticated
- **Static musl binary** — no dependencies, no runtime
- **No daemon, no systemd units, no D-Bus**
- **Immutable-first** — everything fits in `/usr/local` and `~/.local`, no `/usr` modifications needed

**Jump to:** [Quick Start](#quick-start) · [Requirements](#requirements) ·
[Deployment](#deployment) · [Configuration](#configuration) · [Enrollment](#enrollment) ·
[Diagnosing your camera](#diagnosing-your-camera) · [Troubleshooting](#troubleshooting) ·
[Security & Limitations](#security--limitations)

## Features

- **Windows Hello–compatible IR camera support** — auto-detects GREY, YUYV, or Y16 pixel formats, no RGB camera needed
- **Automatic password fallback** — if face auth fails, times out, or no camera, PAM falls through to password
- **Static musl binary** (~20 MB, zero runtime dependencies) — copy to any Linux system
- **No daemon, no systemd, no D-Bus** — just `pam_exec.so` triggered by PAM
- **Configurable** via `/etc/face-auth.toml`, `~/.config/face-auth.toml`, or environment variables
- **Built-in capture timeout** (5s default) — camera hang won't lock you out
- **Works on immutable distros** — no `rpm-ostree layer`, no package installs, no `/usr` modification

## Quick Start

```bash
# 1. Install core authentication (PAM, models, binaries)
sudo ./deploy.sh

# 2. Enroll your face
face-enroll --user $USER

# 3. Test sudo
sudo true              # triggers IR camera → exit 0
```

## Requirements

### Hardware

- **IR camera** (Windows Hello compatible, e.g. Shinetech ASUS FHD webcam) — pixel format
  (GREY, YUYV, or Y16) is auto-detected from the driver, not assumed
- **Linux kernel** with `uvcvideo` (standard on all distros)

Not sure which `/dev/video*` node is your IR camera, or whether it's a format face-auth
understands? Build `face-camera-diag` and run `list` — see
[Diagnosing your camera](#diagnosing-your-camera).

### Software (target system — where you deploy)

- PAM with `pam_exec.so` (standard on all distros)
- SELinux (Fedora/Bluefin/Silverblue) — deploy script installs policy automatically
- `policycoreutils` for SELinux policy compilation (installed by default on Fedora)

### Software (build system — where you compile)

None required. `sudo ./deploy.sh` downloads prebuilt static musl binaries from this project's
[GitHub Releases](https://github.com/karanshukla/vinoAuthFace/releases) (checksum-verified) if
it can't find a Rust toolchain. Only build from source yourself if you want a change that isn't
in a release yet, or the NPU/OpenVINO backend (which CI can't build — see below).

## Building from Source

Skip this section if `sudo ./deploy.sh` already worked — it only builds from source when it has
to. Build manually if you want an unreleased change, or the OpenVINO/NPU backend.

### Core auth (static musl — no runtime deps)

```bash
# Install Rust if needed
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Add musl target
rustup target add x86_64-unknown-linux-musl

# Clone and build
git clone https://github.com/pfalkingham/authFace.git
cd authFace
cargo build --release --target x86_64-unknown-linux-musl -p face-auth -p face-enroll

# Deploy
sudo ./deploy.sh
```

### On immutable distros via distrobox

```bash
# Create a Fedora development container
distrobox create --image docker.io/library/fedora:40 --name authface-dev
distrobox enter authface-dev

# Inside the container, install build deps (once)
sudo dnf install -y rust cargo gcc gcc-c++ musl-gcc cmake

# Clone and build
cd ~/Projects
git clone https://github.com/pfalkingham/authFace.git
cd authFace
cargo build --release --target x86_64-unknown-linux-musl -p face-auth -p face-enroll

# Exit container, then deploy on host
exit
sudo ./deploy.sh
```

## Deployment

### Core (PAM authentication)

```bash
sudo ./deploy.sh
```

| Step | What | Details |
|------|------|---------|
| Build | Compiles if `cargo` is available | Else uses pre-built binaries in `target/`, else downloads a checksum-verified release from GitHub |
| Binaries | Installs to `/usr/local/bin` | `face-auth` + `face-enroll` |
| Model | Downloads from InsightFace | `w600k_mbf.onnx` (~13 MB) to `/usr/local/share/face-auth/` |
| Config | Installs default config | `/etc/face-auth.toml` |
| PAM | Patches PAM service files | Adds `sufficient` `pam_exec.so` to `sudo`, `gdm-password`, `swaylock` |
| SELinux | Compiles and loads policy | Allows `xdm_t` to mmap camera for lock-screen auth |
| Storage | Creates embeddings directory | `/var/lib/face-auth/<user>/` with sticky bit |

Each PAM file is backed up with a `.face-auth.bak` suffix.

### Pin camera (recommended)

```bash
sudo ./pin-camera.sh
```

Confirm the camera works first (`face-enroll` succeeds, `sudo true` unlocks) — this pins
whatever `device` is currently configured, so run it after enrollment, not before. See
[Security & Limitations](#security--limitations) for what this does and does not defend
against.

### Uninstall

```bash
# Remove everything (core + models + config)
sudo ./uninstall.sh

# Remove everything including face embeddings
sudo ./uninstall.sh --purge
```

Restores PAM backups, removes binaries, models, config, and SELinux policy.

## Configuration

Priority (highest first):

1. **Environment variables**: `FACE_AUTH_DEVICE`, `FACE_AUTH_THRESHOLD`, `FACE_AUTH_MODEL_PATH`, `FACE_AUTH_EMBEDDINGS_DIR`, `FACE_AUTH_CAPTURE_TIMEOUT`
2. **User config**: `~/.config/face-auth.toml`
3. **System config**: `/etc/face-auth.toml`
4. **Defaults**: auto-detected camera, threshold 0.6, 5s capture timeout

Example `/etc/face-auth.toml`:
```toml
device = "/dev/video3"
threshold = 0.6
model_path = "/usr/local/share/face-auth/w600k_mbf.onnx"
embeddings_dir = "/var/lib/face-auth"
capture_timeout_ms = 5000
```

`scan_interval_ms` (delay between scan attempts during `authenticate_scan`) is not hardcoded:
if unset, it's queried from the camera's own reported native frame interval via V4L2
(`VIDIOC_G_PARM`), so it adapts automatically to whatever hardware it's running on rather than
assuming one machine's frame rate. Set `scan_interval_ms` explicitly (TOML or
`FACE_AUTH_SCAN_INTERVAL_MS`) to override.

## Enrollment

```bash
# Replace existing embeddings with new capture (default: 30 frames)
face-enroll --user $USER

# Append new embeddings to improve recognition across lighting/angles
face-enroll --improve --user $USER
```

`--frames` defaults to 30: enough pose/expression variation from a single sitting for
reliable matching against a fixed similarity threshold. Fewer frames enroll faster but
match less reliably; run `--improve` again in different lighting/angles for the biggest
accuracy gain beyond that.

CLI options: `--frames`, `--interval`, `--device`, `--threshold`, `--model`, `--improve`, `-v`.

## PAM Integration

The deploy script adds a `sufficient` `pam_exec.so` line to:

| Service | File | Insertion point |
|---------|------|----------------|
| `sudo` | `/etc/pam.d/sudo` | After `#%PAM-1.0` |
| `gdm-password` | `/etc/pam.d/gdm-password` | After `pam_selinux_permit.so` |
| `swaylock` | `/etc/pam.d/swaylock` | After `#%PAM-1.0` |
| `polkit-1` | `/etc/pam.d/polkit-1` | After `#%PAM-1.0` |

`sufficient` means: if face-auth exits 0, the user is authenticated immediately.
If it fails (no match, no camera, timeout), PAM falls through to password prompt.

No `timeout`, `setenv`, or `env_pass` flags are needed — face-auth reads the camera
(not stdin) and resolves `PAM_USER` via its own fallback chain.

**`polkit-1`** covers every polkit `auth_self` prompt system-wide, not just one app —
`pkexec`, package-manager GUIs, desktop-settings changes, and any app (Bitwarden included,
see below) that asks polkit to re-authenticate the active user. This is the same PAM
service most distros already wire a fingerprint reader into via `system-auth`; face-auth
is added as a second `sufficient` biometric method ahead of it, not a replacement. If
`/etc/pam.d/polkit-1` doesn't already exist as an admin override (the common case — most
distros ship only the vendor default under `/usr/lib/pam.d/polkit-1`), `deploy.sh`
materializes one first so there's something to back up and patch; `uninstall.sh` removes
that file outright in this case rather than trying to restore a prior state that never
existed, falling back to the vendor default again.

Known UX gap: sudo's terminal prompt and polkit's graphical dialog give no visual cue that
a biometric method is available or being attempted (unlike Windows Hello, which shows a
camera icon while scanning) — auth just silently succeeds within the scan window or falls
through to the normal password prompt. Purely cosmetic; doesn't affect whether it works.

### Bitwarden biometric unlock

If a Bitwarden desktop client is detected (native binary, Flatpak, or Snap), `deploy.sh`
also installs Bitwarden's own polkit action file
(`/usr/share/polkit-1/actions/com.bitwarden.Bitwarden.policy`) — required for Flatpak/Snap
installs, which are sandboxed and can't write it themselves. The content is transcribed
verbatim from Bitwarden's official source (`os-biometrics-linux.service.ts` in
[`bitwarden/clients`](https://github.com/bitwarden/clients)), not downloaded from a URL, so
there's no separate checksum step. Once installed, enable **File → Settings → Unlock with
system authentication** in Bitwarden — it'll prompt through `polkit-1` like any other
polkit action, so a face scan (or fingerprint, or password) satisfies it.

Not covered: KWallet. Its auto-unlock happens once at login (`pam_kwallet5.so` capturing
the typed password), not at the lock screen, and most desktops leave the wallet unlocked
across screen lock/unlock regardless of auth method — so there's nothing for face-auth to
gate there today. Making face-auth *drive* KWallet's own unlock would need KWallet to grow
a pluggable-auth backend, which it doesn't have yet (there's an open upstream feature
request for FIDO2/biometric-backed KWallet storage, not implemented as of this writing).

## How It Works

```
PAM (sudo / gdm-password / swaylock / polkit-1)
  │
  ▼
face-auth (static binary)
  ├─ V4L2 capture from IR camera (auto-detected device + pixel format: GREY/YUYV/Y16)
  │   └─ poll() with 5s timeout — exits cleanly if camera hangs
  ├─ Histogram equalization
  ├─ Face detection (RetinaFace-derived ONNX model)
  ├─ Resize to 112×112, normalize to [-1, 1]
  ├─ tract-onnx inference (MobileFaceNet, 512-d embedding)
  ├─ Cosine similarity vs stored embeddings (default threshold 0.6)
  └─ Exit 0 (match) or exit 1 (no match → password prompt)
```

## Model

Uses InsightFace **`w600k_mbf.onnx`** (MobileFaceNet @ WebFace600K, ~13 MB, 512-d output)
from the `buffalo_sc` model pack by default, plus **`version-slim-320.onnx`** for face
detection. Licensed under MIT (InsightFace is MIT-licensed).

The models are **not bundled** in this repository. `deploy.sh` downloads them directly from
InsightFace's official GitHub releases and verifies the SHA-256 checksum.

### Recognition model: mbf (default) vs r50

`deploy.sh` can install either recognition model from InsightFace's model zoo:

| | `mbf` (default) | `r50` |
|---|---|---|
| Backbone | MobileFaceNet | ResNet50 |
| Pack | `buffalo_sc` | `buffalo_l` |
| Size | ~13 MB | ~175 MB |
| Encode latency (this project's NPU benchmark) | ~1.2ms/frame | ~4.2ms/frame |
| Genuine-match similarity (same benchmark) | mean 0.815, min 0.759 | mean 0.875, min 0.830 |

`r50` is InsightFace's larger, more accurate recognition model — noticeably wider match
margin in local NPU benchmarking, for a few extra milliseconds per frame that don't show up
in practice (the scan loop is paced by the camera's own frame interval either way, not by
encode time). The only real cost is a one-time ~1s NPU compile-cache-miss the first time it's
ever loaded (subsequent loads are back to double-digit ms, same as `mbf`).

Install it with:

```bash
FACE_AUTH_RECOGNITION_MODEL=r50 sudo -E ./deploy.sh
```

**Switching models requires re-enrolling** (`face-enroll --user $USER`) — `mbf` and `r50`
produce numerically incompatible embedding spaces despite the same 512-d shape, so cosine
similarity between them would be meaningless, not just less accurate. To make this a loud
failure instead of a silent one, every saved embeddings file is tagged with the recognition
model that produced it (`storage::EmbeddingStore`'s `model_tag`, the model's filename);
authenticating or `--improve`-enrolling against a mismatched `model_path` is refused with an
explicit error rather than comparing embeddings across models. Existing installs are
unaffected — files saved before this existed have no tag and are treated as compatible with
whatever's currently configured, same as always.

## SELinux

On Fedora/Bluefin/Silverblue with SELinux enforcing, the GNOME lock screen runs in the
`xdm_t` domain. This domain cannot `mmap` video devices by default. The deploy script
installs a minimal policy module:

```
allow xdm_t v4l_device_t:chr_file map;
```

To remove: `sudo semodule -r face_auth`

If the deploy script reported missing SELinux tools:
```bash
sudo dnf install -y policycoreutils
sudo checkmodule -M -m -o face_auth.mod selinux/face-auth.te
sudo semodule_package -o face_auth.pp -m face_auth.mod
sudo semodule -i face_auth.pp
```

## Diagnosing your camera

Not sure which `/dev/video*` node is the IR camera, or whether face-auth will understand its
pixel format? `face-camera-diag` is a small offline tool for exactly that — it's not installed
by `deploy.sh`, so grab it once, either as a prebuilt binary from
[Releases](https://github.com/karanshukla/vinoAuthFace/releases) (no Rust toolchain needed):

```bash
curl -fLO https://github.com/karanshukla/vinoAuthFace/releases/latest/download/face-camera-diag-x86_64-unknown-linux-musl
chmod +x face-camera-diag-x86_64-unknown-linux-musl
```

or by building it from source:

```bash
cargo build --release -p face-camera-diag
```

`list` enumerates every V4L2 node with its driver, card name, USB VID:PID, and current
resolution/pixel format (substitute `./target/release/face-camera-diag` below for the downloaded
binary's name if you didn't build from source):

```bash
./target/release/face-camera-diag list

DEVICE         DRIVER     CARD                     VID:PID    FORMAT       IR GUESS
/dev/video0    uvcvideo   Integrated_Webcam_FHD    2b7e:55c0  1920x1080 YUYV
/dev/video2    uvcvideo   Integrated_Webcam_FHD_IR 2b7e:55c0  360x360 GREY  likely
/dev/video3    uvcvideo   Integrated_Webcam_FHD_IR 2b7e:55c0  -
```

A `FORMAT` of `GREY`, `YUYV`, or `Y16` is one face-auth can capture; anything else (or `-`,
which usually means a paired metadata node rather than an actual capture device) isn't. The
`IR GUESS` column is a heuristic based on the card name, not authoritative — cross-check against
what `sudo ./deploy.sh` / `config.device()` actually picks.

`dump` captures one frame from a specific device and writes it as a 16-bit PGM you can open in
any image viewer that supports the format (e.g. GIMP), to confirm you're actually looking at a
face-shaped IR image and not noise or a black frame:

```bash
./target/release/face-camera-diag dump --device /dev/video2 --out frame.pgm
```

## Troubleshooting

```bash
# List available IR cameras
ls /sys/class/video4linux/*/name

# Grant video group access (log out/in after)
sudo usermod -aG video $USER

# Debug output
RUST_LOG=face_auth_core=debug sudo -k && sudo true

# Check PAM logs
journalctl | grep -i "pam_exec\|face-auth"

# SELinux denials
journalctl -k | grep face-auth | grep denied

# Test binary directly (skips PAM)
sudo env PAM_USER=$USER USER=$USER HOME=$HOME /usr/local/bin/face-auth
echo $?   # 0 = success, 1 = failure

# Increase capture timeout (default 5000ms)
FACE_AUTH_CAPTURE_TIMEOUT=10000 sudo -k && sudo true
```

## Security & Limitations

- **Anti-spoofing:** Uses an active-NIR IR camera (not RGB), which resists casual photo
  spoofing structurally rather than statistically. Two layers:
  - **Screen-based spoofing (phone/tablet showing a photo or video of the victim) is blocked
    by sensor physics, not software**: OLED/most LCD panels emit essentially no near-infrared
    and barely reflect the camera's own IR illuminator, so a screen held up to the camera
    produces no face-shaped IR signal at all — confirmed empirically (zero detections across
    100 capture attempts against a phone screen). The attack fails at the face-detection
    stage, before recognition or liveness ever runs.
  - **Motion-based liveness check** requires observed pixel-level motion between consecutive
    face-detected frames before accepting a match, defeating a rigidly-held static image.
  - Does not perform structured-light or dot-projection depth checks (no depth-capable
    hardware). **Printed photos remain an open risk** — paper does reflect some NIR unlike
    OLED, and a gently-moved (not perfectly rigid) printed photo could pass the motion check.
    High-quality IR-transparent prints or 3D masks may also bypass verification. Untested;
    revisit if/when calibration data against real printed photos becomes available.
- **Camera identity / frame-injection:** By default face-auth opens whatever V4L2 device
  `device` resolves to and trusts frames from it — a USB device that claims the real camera's
  VID/PID (which any device can do; it's just a string) could be substituted and used to feed
  synthetic or replayed frames. Run `sudo ./pin-camera.sh` once to close this: it pins
  face-auth to the camera's exact physical USB port path and V4L2 function index (not VID/PID),
  read from sysfs — a property a spoofed device can't replicate without physically intercepting
  that exact internal bus segment. It writes a udev rule (`/etc/udev/rules.d/99-face-auth-
  camera.rules`) so `device` can point at a stable `/dev/face-auth-ir` symlink instead of a
  `/dev/videoN` index that isn't guaranteed stable across reboots, and records the pinned
  identity in `face-auth.toml` (`pinned_camera_path`, `pinned_camera_index`) so face-auth
  re-verifies it directly from sysfs on every authenticate/enroll call — independent of the
  udev rule staying correct. Opt-in and unset by default, so existing installs aren't affected
  until you run it. This closes the *injection* vector specifically; it's unrelated to the
  motion-liveness and IR-physics checks above, which defend against *presentation* attacks
  (something held up to the real, legitimate camera) — you want both.
- **SELinux policy scope:** The lock-screen policy grants `xdm_t` mmap access to all
  V4L2 devices. This is a trade-off for drop-in compatibility; narrowing it requires
  custom udev device types.
- **x86_64 only:** V4L2 ioctl numbers and struct layouts are hardcoded for x86_64.
  ARM/aarch64 requires switching to the `v4l` crate.
- **Model integrity:** `deploy.sh` verifies SHA-256 checksum and aborts on mismatch.

## Project Structure

```
authFace/
  crates/
    face-auth-core/          # Core library
      src/
        capture.rs           # V4L2 capture + poll() timeout
        config.rs            # Layered config (system → user → env)
        detector.rs          # Face detection (RetinaFace-based ONNX model)
        error.rs             # Error types
        inference.rs         # tract-onnx model loading + encoding
        lib.rs               # FaceAuth struct, auth + enroll + scan
        preprocess.rs        # Histogram equalize, resize, normalize
        storage.rs           # Binary embedding I/O (versioned, atomic)
        verify.rs            # Cosine similarity
    face-auth/               # PAM binary (stdin-less, PAM_USER fallback)
    face-enroll/             # Enrollment CLI
    face-similarity-check/   # Offline FAR debug tool (photos vs enrolled embeddings)
    face-camera-diag/        # Offline camera discovery/diagnostic tool (list, dump)
  config/
    face-auth.toml.example   # Documented config template
  selinux/
    face-auth.te             # SELinux policy source
  deploy.sh                  # Core auth installer
  pin-camera.sh              # Pins device by USB bus path (frame-injection defense)
  uninstall.sh               # Removal script (--purge flag)
```

## License

MIT
