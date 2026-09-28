# authFace: IR Camera Face Unlock for Linux

A fork of [Peter Falkingham's authFace](https://github.com/pfalkingham/authFace), kept in sync
with upstream. It adds motion liveness, a failed-match lockout, camera pinning, an optional
OpenVINO/NPU backend, a choice of recognition model, polkit-1 and Bitwarden unlock, prebuilt
release binaries, and camera diagnostics. See [What this fork adds](#what-this-fork-adds).

**Windows Hello-style biometric login for Linux.** IR camera facial authentication via PAM.
Works on **immutable distros** (Bazzite, Bluefin, Fedora Silverblue, Fedora Kinoite, etc.) with
zero system packages, daemons, or layering.

- **Face unlock for sudo, lock screen (GNOME/Sway), `gdm-password` and polkit prompts**
- **~2 seconds** from camera poll to authenticated
- **Static musl binary**: no dependencies, no runtime
- **No daemon, no systemd units, no D-Bus**
- **Immutable-first**: everything fits in `/usr/local`, `/etc` and `/var/lib`

**Jump to:** [Quick Start](#quick-start) · [Requirements](#requirements) ·
[Deployment](#deployment) · [Configuration](#configuration) · [Enrollment](#enrollment) ·
[Diagnosing your camera](#diagnosing-your-camera) ·
[Hardware compatibility](#hardware-compatibility) · [Troubleshooting](#troubleshooting) ·
[Security & Limitations](#security--limitations)

## Quick Start

```bash
# 1. Install (PAM, models, binaries). Builds from source if it finds cargo,
#    otherwise downloads checksum-verified release binaries.
sudo ./deploy.sh

# 2. Enrol your face (templates are root-owned, so this needs sudo)
sudo face-enroll --user $USER

# 3. Test sudo
sudo -k && sudo true   # triggers IR camera, exit 0

# 4. Recommended: pin the camera (face-enroll prints the exact command)
sudo ./pin-camera.sh /dev/videoN
```

## What this fork adds

| Feature | Where |
|---|---|
| **Motion liveness**: a match only counts after real pixel motion between consecutive face frames, so a rigidly held photo fails | [Security](#security--limitations), `liveness_motion_threshold` |
| **Lockout**: exponential backoff after repeated failed matches, without ever blocking the password fallback | [Security](#security--limitations), `lockout_*` |
| **Camera pinning**: binds face-auth to the camera's physical USB port, closing frame injection by a spoofed device | [Pin camera](#pin-camera-recommended) |
| **Face crop before encoding**: detector boxes are decoded and the encoder sees the face, not the whole frame | [How It Works](#how-it-works) |
| **OpenVINO/NPU backend** (optional build) | [Building](#openvino--npu-backend) |
| **mbf or r50 recognition model**, with templates tagged by model so a mismatch is refused | [Model](#recognition-model-mbf-default-vs-r50) |
| **polkit-1 and Bitwarden** system-auth unlock | [PAM Integration](#pam-integration) |
| **YUYV and Y16 sensors**, and auto-detect for IR nodes without "IR" in their name | [Hardware](#hardware-compatibility) |
| **Prebuilt release binaries**, CI running real deploy/uninstall cycles | [Deployment](#deployment) |
| **`face-camera-diag`** and **`face-similarity-check`** offline tools | [Diagnosing](#diagnosing-your-camera) |

The GTK settings GUI and GNOME scan-indicator extension from upstream are not included: this
fork is PAM and CLI only.

## Upstream Merges & Security Pass

Upstream merged improvements from [SamVivan1/authFace](https://github.com/SamVivan1/authFace)
(multi-IR-camera detection, distro-aware PAM, `pam_exec.so quiet`, automated and pinned detector
download), followed by a security pass over the whole tree. See [CHANGELOG.md](CHANGELOG.md) for
the full list. The changes that affect how you use it:

- **Enrolment needs root** (`sudo face-enroll`). Face templates are authentication data; when the
  store was world-writable, any local user could enrol a face for an account that had not
  enrolled yet, then log in as it.
- **The login prompt trusts only `/etc/face-auth.toml`.** Your own config can make matching
  stricter, never looser. See [Configuration](#configuration).
- **`face-auth` requires `PAM_USER`** rather than falling back to `USER`, `LOGNAME` or
  `id -un`, and refuses remote (`PAM_RHOST`) sessions.

Upgrading re-secures an existing template store in place (including this fork's old
world-writable one), so the store itself needs no re-enrolment. Pipeline changes might,
though: see [Enrollment](#enrollment).

## Requirements

### Hardware

- **IR camera** (Windows Hello compatible). Pixel format (GREY, YUYV or Y16) is read from the
  driver, not assumed.
- **Linux kernel** with `uvcvideo` (standard on all distros)

Not sure your camera stack works at all? See [Hardware compatibility](#hardware-compatibility).
Not sure which `/dev/video*` node is the IR sensor? See
[Diagnosing your camera](#diagnosing-your-camera).

### Software (target system, where you deploy)

- PAM with `pam_exec.so` (standard on all distros)
- SELinux (Fedora/Bluefin/Silverblue): deploy script installs policy automatically
- `policycoreutils` for SELinux policy compilation (installed by default on Fedora)

### Software (build system)

None required. If `sudo ./deploy.sh` finds no Rust toolchain and no local build, it downloads
the static musl binaries from this project's
[GitHub Releases](https://github.com/karanshukla/vinoAuthFace/releases), verified against the
release's `SHA256SUMS`. Build from source for an unreleased change or the NPU backend.

## Building from Source

### Core auth (static musl, no runtime deps)

```bash
# Install Rust if needed
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Add musl target
rustup target add x86_64-unknown-linux-musl

# Clone and build
git clone https://github.com/karanshukla/vinoAuthFace.git
cd vinoAuthFace
cargo build --release --locked --target x86_64-unknown-linux-musl -p face-auth -p face-enroll

# Deploy
sudo ./deploy.sh
```

### OpenVINO / NPU backend

The `npu` feature swaps pure-Rust `tract` inference for OpenVINO on an Intel NPU, GPU or CPU.
It links OpenVINO's glibc libraries, so it cannot be a static musl build and CI cannot build it.
`deploy.sh` handles it for you: if it finds OpenVINO (a system RPM/DEB, or an extracted archive
under `~/.local/opt` or `/opt/intel`) and a Rust toolchain, it builds with `--features npu`,
sets `backend = "openvino"` in `/etc/face-auth.toml`, and for archive installs registers the
runtime libraries with `ldconfig` (PAM runs `face-auth` without your shell profile). Pick the
device with `npu_device = "NPU" | "GPU" | "CPU"`.

### Without installing a toolchain (container build)

If `deploy.sh` finds no toolchain and the release download fails, it prints this command for
you to run. It does not run it for you: `deploy.sh` runs under `sudo`, and rootless podman
driven through `sudo -u` often fails on a missing `XDG_RUNTIME_DIR`.

```bash
podman run --rm -v "$PWD":/src:Z -w /src docker.io/library/rust:alpine \
  sh -c 'apk add --no-cache musl-dev && \
         cargo build --release --locked --target x86_64-unknown-linux-musl \
           -p face-auth -p face-enroll'
sudo ./deploy.sh
```

`rust:alpine` targets musl natively, so the result is the same static binary. Run the
container as your own user (not under `sudo`) so the files in `target/` stay yours.

### On immutable distros via distrobox

```bash
distrobox create --image registry.fedoraproject.org/fedora:latest --name authface-dev
distrobox enter authface-dev

# Build deps (once). Fedora does not package a musl std for Rust, so use rustup,
# not the distro `rust` package.
sudo dnf install -y gcc musl-gcc cmake
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y \
  --target x86_64-unknown-linux-musl
source "$HOME/.cargo/env"

cd ~/Projects/vinoAuthFace
cargo build --release --locked --target x86_64-unknown-linux-musl -p face-auth -p face-enroll

# Exit container, then deploy on host
exit
sudo ./deploy.sh
```

> **`sudo ./deploy.sh` and `cargo`:** if you installed Rust with rustup, `cargo` lives in
> `~/.cargo/bin`, which is not on root's `PATH`. The script looks there for the invoking user
> and runs the build as that user, so it does not leave root-owned files in `target/`.

## Deployment

```bash
sudo ./deploy.sh
```

| Step | What | Details |
|------|------|---------|
| Build | Picks the first that applies | OpenVINO + cargo: NPU build. Prebuilt binaries in `target/`: use them (`FACE_AUTH_FORCE_BUILD=1` to rebuild). Cargo: static musl build. Otherwise: download and checksum-verify the release binaries |
| Binaries | Installs to `/usr/local/bin` | `face-auth` (set-group-ID `face-auth`, so lock screens running as the user can read templates; a non-root caller can only authenticate itself) + `face-enroll` |
| Models | Downloads and SHA-256 verifies | Recognition model (`w600k_mbf.onnx` by default) and `version-slim-320.onnx` detector, to `/usr/local/share/face-auth/`. A copy in `models/` is used first, and verified too |
| Config | Installs default config | `/etc/face-auth.toml`, kept if it already exists |
| PAM | Patches PAM service files | Adds `sufficient` `pam_exec.so quiet` to `sudo`, `gdm-password`, `swaylock`, `polkit-1`, `kde-fingerprint` |
| Bitwarden | Only if installed | Adds Bitwarden's polkit unlock action |
| SELinux | Compiles and loads policy | Allows `xdm_t` to mmap camera for lock-screen auth |
| Storage | Secures template store | `/var/lib/face-auth`, `root:face-auth` `2750`, templates `0640`, per-user `lockout/` `2770`. An existing store is re-secured in place |

GDM lock-screen patching is **distro-aware**: after `pam_selinux_permit.so` on
Fedora/Bluefin/Silverblue, after `#%PAM-1.0` on Ubuntu/Debian. Each PAM file is backed up with
a `.face-auth.bak` suffix.

### Pin camera (recommended)

```bash
sudo ./pin-camera.sh /dev/videoN
```

Run it after enrolment and a test unlock both work: it pins whatever device you give it, and
`face-enroll` prints the exact command with the camera it used. It writes a udev rule for a
stable `/dev/face-auth-ir` symlink and records the camera's physical identity in
`/etc/face-auth.toml`. See [Security & Limitations](#security--limitations) for what this does
and does not defend against. Re-run it if you replace the hardware on purpose.

### Uninstall

```bash
# Remove everything (binaries, models, config, PAM changes, camera pin)
sudo ./uninstall.sh

# ...including face templates
sudo ./uninstall.sh --purge
```

Restores PAM backups, removes the `polkit-1` override if `deploy.sh` created it, and removes the
Bitwarden action only if `deploy.sh` installed it. Also cleans up leftovers from older installs
that had the GTK GUI.

## Configuration

The authentication path and the unprivileged tools trust different things.

**During PAM authentication** (`face-auth`, i.e. sudo / lock screen / login / polkit):

| Source | Effect |
|--------|--------|
| `/etc/face-auth.toml` (root-owned) | Authoritative for everything |
| `~/.config/face-auth.toml` | May only make authentication **stricter**, see below |
| `FACE_AUTH_*` environment | **Ignored entirely** |

**For `face-enroll` and the offline tools**, the usual layering applies: environment
variables, then `~/.config/face-auth.toml`, then `/etc/face-auth.toml`.

### What a user may override at the login prompt

A user's own config is read (resolved via `getent passwd`, so it is *their* home and not whoever
happened to invoke the PAM stack), but it is applied as a narrowing overlay:

| Key | At the login prompt |
|-----|--------------------|
| `threshold`, `detector_threshold`, `liveness_motion_threshold` | Honoured only if **>= the system value**. A lower number is ignored. |
| `device` | Honoured only if the path is a real IR capture device on this machine (IR-looking sysfs name, or a physical greyscale sensor, and opens in a supported format). |
| `scan_duration_ms`, `scan_interval_ms`, `capture_timeout_ms` | Honoured within built-in bounds. |
| `model_path`, `detector_model_path`, `embeddings_dir`, `pinned_camera_*`, `lockout_*`, `backend`, `npu_device` | **Ignored**: system policy only. |

This is what stops code running as you, which does not know your password, from writing a
permissive `~/.config/face-auth.toml` and turning your next `sudo` into a root shell. To
*loosen* matching, edit `/etc/face-auth.toml` as root.

Example `/etc/face-auth.toml`:
```toml
threshold = 0.6
capture_timeout_ms = 5000
scan_duration_ms = 5000
liveness_motion_threshold = 0.01
```

Every key is documented in [`config/face-auth.toml.example`](config/face-auth.toml.example).
Environment variable names follow the field names, so the capture timeout is
`FACE_AUTH_CAPTURE_TIMEOUT_MS` (not `FACE_AUTH_CAPTURE_TIMEOUT`).

> **Leave `device` unset unless you must pin it.** UVC cameras normally expose a metadata node
> right beside the capture node under the same name (e.g. `/dev/video2` captures and
> `/dev/video3` does not). Auto-detection opens each candidate and takes the first that really
> is a capture device in a supported format, which gets this right; a hand-written path often
> does not. `pin-camera.sh` sets `device` for you.

## Enrollment

Face templates are root-owned (`/var/lib/face-auth`), so enrolment is a privileged operation:

```bash
# Replace existing templates with a new capture (30 frames)
sudo face-enroll --user $USER

# Append templates to improve recognition across lighting/angles
sudo face-enroll --improve --user $USER
```

`--frames` defaults to 30: enough pose and expression variation from one sitting for reliable
matching. Run `--improve` again in different lighting for the biggest gain beyond that.

CLI options: `--frames`, `--interval`, `--device`, `--threshold`, `--model`,
`--embeddings-dir`, `--improve`, `-v`.

**When to re-enrol:** after switching recognition model (face-auth refuses the old templates
and says so), and after any change to the preprocessing pipeline. Templates made by older
versions of this fork or upstream may still match, but re-enrolling is the safer bet.

Why this is not user-writable: whatever can write a face template decides whose face unlocks
that account. If your own login could rewrite it, then so could anything running as you, and a
stolen browser session would become a root shell at the next `sudo`.

## PAM Integration

The deploy script adds a `sufficient` `pam_exec.so quiet` line to:

| Service | File | Insertion point |
|---------|------|----------------|
| `sudo` | `/etc/pam.d/sudo` | After `#%PAM-1.0` |
| `gdm-password` | `/etc/pam.d/gdm-password` | After `pam_selinux_permit.so` (Fedora) / after `#%PAM-1.0` (Ubuntu/Debian) |
| `swaylock` | `/etc/pam.d/swaylock` | After `#%PAM-1.0` |
| `polkit-1` | `/etc/pam.d/polkit-1` | After `#%PAM-1.0` |
| `kde-fingerprint` | `/etc/pam.d/kde-fingerprint` | Above the first `auth` line. KScreenLocker runs this slot alongside the password field |

`sufficient` means: if face-auth exits 0, the user is authenticated immediately. If it fails
(no match, no camera, timeout, lockout), PAM falls through to the password prompt. `quiet`
suppresses `pam_exec` chatter so the unlock UI stays clean.

**`polkit-1`** covers every polkit `auth_self` prompt system-wide: `pkexec`, package-manager
GUIs, settings changes, and apps like Bitwarden that ask polkit to re-authenticate you. Most
distros ship only the vendor default in `/usr/lib/pam.d/polkit-1`; `deploy.sh` copies it to
`/etc/pam.d/polkit-1` first so there is something to patch, and `uninstall.sh` deletes that copy
again rather than "restoring" a file that never existed.

There is no visual cue while scanning (Windows Hello shows a camera icon). Auth either succeeds
within the scan window or falls through to the normal password prompt.

### Bitwarden biometric unlock

If a Bitwarden desktop client is installed (native, Flatpak or Snap), `deploy.sh` also installs
Bitwarden's polkit action (`/usr/share/polkit-1/actions/com.bitwarden.Bitwarden.policy`). Flatpak
and Snap builds are sandboxed and cannot write it themselves. The content is transcribed from
Bitwarden's own source (`os-biometrics-linux.service.ts` in
[`bitwarden/clients`](https://github.com/bitwarden/clients)), not downloaded. Then enable
**Settings → Unlock with system authentication** in Bitwarden.

Not covered: KWallet. It unlocks once at login from the typed password and stays unlocked across
screen locks, so there is nothing for face-auth to gate.

## How It Works

```
PAM (sudo / gdm-password / swaylock / polkit-1 / kde-fingerprint)
  │
  ▼
face-auth (static binary)
  ├─ Resolve PAM_USER via getent (refuses to guess from USER/LOGNAME)
  ├─ Refuse if PAM_RHOST names a remote host
  ├─ Load /etc/face-auth.toml + strictly-narrowing user overlay
  ├─ Verify the pinned camera identity (if pinned), check lockout
  ├─ V4L2 capture (auto-detected node; GREY/YUYV/Y16), brighter of each frame pair
  │   └─ poll() with 5s timeout; exits cleanly if the camera hangs
  ├─ Reject dark/flat frames, CLAHE equalisation
  ├─ Face detection (Ultra-Light-Fast-Generic-Face-Detector), anchor-decoded box
  ├─ Crop to the face (+30% margin), resize to 112×112, normalise to [-1, 1]
  ├─ Encode (tract or OpenVINO; MobileFaceNet or ResNet50, 512-d embedding)
  ├─ Motion liveness across consecutive face frames
  ├─ Cosine similarity vs stored templates (default threshold 0.6)
  └─ Exit 0 (match) or exit 1 (no match → password prompt)
```

## Model

Uses InsightFace **`w600k_mbf.onnx`** (MobileFaceNet @ WebFace600K, ~13 MB, 512-d output) from
the `buffalo_sc` pack by default for recognition, plus **`version-slim-320.onnx`** from
[Linzaer/Ultra-Light-Fast-Generic-Face-Detector-1MB](https://github.com/Linzaer/Ultra-Light-Fast-Generic-Face-Detector-1MB)
(a different project, not InsightFace) for detection. The detector is MIT-licensed. **The
recognition model's weights are not MIT**: InsightFace's model zoo is licensed for
non-commercial research use only (see `model_zoo/README.md` and `python-package/README.md` in
the InsightFace repo). Only InsightFace's library *code* is MIT.

Neither model is bundled. `deploy.sh` downloads both and verifies a pinned SHA-256 before
installing: the recognition model from InsightFace's GitHub releases, the detector from a
specific commit (not a moving branch) of its own repo.

### Recognition model: mbf (default) vs r50

| | `mbf` (default) | `r50` |
|---|---|---|
| Backbone | MobileFaceNet | ResNet50 |
| Pack | `buffalo_sc` | `buffalo_l` |
| Size | ~13 MB | ~175 MB |
| Encode latency (NPU benchmark) | ~1.2ms/frame | ~4.2ms/frame |
| Genuine-match similarity (same benchmark) | mean 0.815, min 0.759 | mean 0.875, min 0.830 |

`r50` gives a wider match margin for a few milliseconds per frame, which the camera-paced scan
loop hides. Install it with:

```bash
sudo FACE_AUTH_RECOGNITION_MODEL=r50 ./deploy.sh
```

**Switching models requires re-enrolling.** The two produce incompatible embedding spaces with
the same 512-d shape, so comparing across them is meaningless rather than just less accurate.
Every template file records the model that produced it, and face-auth refuses to authenticate
or `--improve` against a mismatch. Files from before this existed carry no tag and are treated
as compatible.

## SELinux

On Fedora/Bluefin/Silverblue with SELinux enforcing, the GNOME lock screen runs in the `xdm_t`
domain, which cannot `mmap` video devices by default. The deploy script installs a minimal
policy module:

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

`face-camera-diag` is a small offline tool that is not installed by `deploy.sh`. Grab it from
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

A `FORMAT` of `GREY`, `YUYV` or `Y16` is one face-auth can capture. `-` usually means a paired
metadata node. Note the card name here says nothing about IR (sysfs truncates it at 32 bytes);
auto-detect still finds the sensor because it is a physical node streaming native greyscale,
which RGB webcams never do.

`dump` captures one frame and writes it as a 16-bit PGM, to confirm you are looking at a lit IR
image and not noise or a black frame:

```bash
face-camera-diag dump --device /dev/video2 --out frame.pgm
```

From a source checkout there are also `cargo run --example detect-camera` (why each node is or
is not treated as IR), `cargo run --example frame-stats` (per-frame brightness, for spotting a
strobing illuminator), and `cargo run --release --example bench` (per-stage timings).

`face-similarity-check` scores photos against your enrolled templates through the same pipeline,
for gauging false-accept risk without a second person at the camera:

```bash
sudo face-similarity-check --user $USER photo1.jpg photo2.png
```

## Hardware compatibility

face-auth only speaks V4L2 via `uvcvideo`. There is no libcamera integration, so a camera behind
a different kernel stack (Intel IPU6, MIPI CSI) is unreachable whatever format it reports.
Within `uvcvideo` it needs an IR capture node (`GREY`, `YUYV` or `Y16`) for the spoof
resistance in [Security & Limitations](#security--limitations) to hold.

| Camera stack | `face-camera-diag list` output | Support |
|---|---|---|
| UVC IR (`uvcvideo` + GREY/YUYV/Y16 IR node) | `DRIVER=uvcvideo`, a node reports `GREY`/`YUYV`/`Y16` | ✅ Supported |
| UVC RGB-only (`uvcvideo`, no IR node) | `DRIVER=uvcvideo`, only a colour-format node exists | ⚠️ Never auto-detected, and MJPEG is refused. A YUYV node set explicitly as `device` will capture, but with no IR there is no spoof resistance |
| Intel IPU6 / MIPI / libcamera | camera does not appear as a plain `uvcvideo` node | ❌ Not supported |

### Reports

| Laptop | IR camera | Driver | Tier | Notes |
|---|---|---|---|---|
| Unconfirmed model, FHD webcam with IR | `2b7e:55c0`, IR on `/dev/video2`, 360x360 GREY | `uvcvideo` | ✅ Supported | Reference hardware for this fork. Enrolment and auth verified end to end before the upstream resync; after it, auto-detect and capture verified. |

Table format borrowed from [Visage](https://github.com/sovren-software/visage)'s hardware docs.
If face-auth works (or doesn't) on yours, please
[open an issue](https://github.com/karanshukla/vinoAuthFace/issues/new) with your
`face-camera-diag list` output.

## Troubleshooting

```bash
# Which camera will it use?
face-camera-diag list

# Grant video group access (log out/in after)
sudo usermod -aG video $USER

# Debug output from a live sudo attempt
sudo -k; RUST_LOG=face_auth_core=debug,face_auth=debug sudo true

# Check PAM logs
journalctl | grep -i "pam_exec\|face-auth"

# SELinux denials
journalctl -k | grep face-auth | grep denied

# Test a stored face directly (skips PAM; needs root to read templates)
sudo face-auth --verify $USER
echo $?   # 0 = match, 1 = no match, 2 = error
```

### "PAM_USER is not set"

`face-auth` no longer guesses the account from `USER`/`LOGNAME`. If you are invoking it by hand,
use `--verify` rather than setting `PAM_USER` yourself.

### "reports pixel format ... expected GREY, YUYV or Y16"

The selected device is not an IR sensor: it is an ordinary RGB webcam, or the metadata node next
to the real capture node. Let auto-detection pick one, or check `face-camera-diag list`.

### "camera identity mismatch"

The camera is pinned and `device` now resolves to a different physical port or node. If you
replaced or moved the hardware on purpose, re-run `sudo ./pin-camera.sh`.

### "no face detected" every time

Most Windows Hello IR modules **strobe their illuminator**, emitting a lit frame and a near-black
one alternately. `cargo run --example frame-stats` shows what yours does. authFace captures
frames in pairs and keeps the brighter, so strobing is handled; if *every* frame is dark, the
illuminator is not firing and no software will help.

### Face auth stopped being tried after a few failures

That is the lockout. After 5 failed matches (a face was seen and rejected) face-auth skips the
camera with a doubling cooldown, up to 5 minutes, and PAM goes straight to the password. The
next successful face match resets it. Tune it with `lockout_*` in
`/etc/face-auth.toml`.

### My threshold change did nothing

A user config may only make matching *stricter*. To loosen it, lower `threshold` in
`/etc/face-auth.toml` as root. See [Configuration](#configuration).

## Security & Limitations

### Trust model

- **Face templates are root-owned.** `/var/lib/face-auth` is `root:face-auth` mode `2750`, with
  templates at `0640`. Whatever can write a template decides whose face unlocks that account, so
  enrolment goes through `sudo`.
- **`face-auth` is set-group-ID `face-auth`, not set-user-ID root.** Lock screens (KScreenLocker,
  swaylock) run PAM as the user, so the binary borrows a group that can read templates and write
  only `<user>/lockout/` (`2770`). A bug in it exposes templates and lockout counters, never root.
  Nothing else has the group, and the store is closed to everyone else, so a user cannot reach
  their own lockout state to reset it. With borrowed privileges it drops the caller's environment
  and only lets a non-root caller authenticate itself.
- **The PAM path trusts only `/etc/face-auth.toml`.** A user's own config may make matching
  stricter, never looser, and may not redirect model or template paths, unpin the camera, or
  lift the lockout. `FACE_AUTH_*` environment variables are ignored during authentication.
- **Identity comes from `PAM_USER` only.** `face-auth` refuses to run if PAM did not set it.
- **Remote sessions are refused.** If `PAM_RHOST` names a non-local host, face authentication is
  declined; the camera is at the console, so otherwise whoever sits at the desk would
  authenticate an SSH session.

### Presentation attacks (something held up to the real camera)

- **Screens are blocked by sensor physics.** OLED and most LCD panels emit essentially no near-IR
  and barely reflect the camera's illuminator, so a phone showing your photo produces no
  face-shaped IR signal. Measured: zero detections across 100 attempts against a phone screen.
- **Motion liveness** requires pixel-level motion between consecutive face frames before a match
  counts, which defeats a rigidly held static image.
- **Printed photos remain an open risk.** Paper does reflect some NIR, and a gently moved print
  could pass the motion check. There is no structured-light or depth check. High-quality
  IR-visible prints or 3D masks may bypass verification. Treat face unlock as a convenience over
  a password you still have, not a stronger factor.

### Frame injection (a fake camera)

By default face-auth trusts frames from whatever `device` resolves to. A USB device claiming the
real camera's VID/PID (just a string; any device can) could feed replayed frames.
`pin-camera.sh` closes this by pinning the camera's physical USB port path and V4L2 index, read
from sysfs, which a spoofed device cannot occupy at the same time as the real one. face-auth
re-verifies that identity on every authentication and enrolment, independent of the udev rule.
Opt-in. This is separate from the presentation defences above; you want both.

Auto-detect only ever considers IR-named nodes or physical greyscale sensors, and never
virtual (v4l2loopback) nodes, whose format any local user can set.

### Other limitations

- **Rate limiting covers the face factor only.** The lockout throttles repeated face attempts;
  PAM's password fallback is untouched, so wire `pam_faillock` for that separately.
- **`sufficient` bypasses the rest of the auth stack.** A successful match satisfies
  authentication outright, so the strength of the stack becomes the strength of the face match.
- **SELinux policy scope:** the lock-screen policy grants `xdm_t` mmap access to all V4L2
  devices. Narrowing it requires custom udev device types.
- **x86_64 only:** V4L2 ioctl numbers and struct layouts are hardcoded. ARM/aarch64 requires
  the `v4l` crate.
- **Model integrity:** both models are pinned by SHA-256 and the detector URL is pinned to a
  commit. `deploy.sh` aborts on mismatch. Release binaries are verified against `SHA256SUMS`.

## Project Structure

```
vinoAuthFace/
  crates/
    face-auth-core/          # Core library
      src/
        capture.rs           # V4L2 capture, formats, IR auto-detect, sysfs identity
        config.rs            # Layered config + narrowing overlay for PAM, camera pin check
        detector.rs          # Frame quality gates + face detection with box decode
        error.rs             # Error types
        inference.rs         # tract / OpenVINO encoder
        lib.rs               # FaceAuth: auth scan (liveness, lockout) + enrolment
        lockout.rs           # Per-user failed-match backoff
        preprocess.rs        # CLAHE, face crop, resize/normalise, motion fraction
        storage.rs           # Template I/O (versioned, model-tagged, atomic, 0640)
        user.rs              # NSS lookup + username validation
        verify.rs            # Cosine similarity
      examples/              # detect-camera, frame-stats, bench
    face-auth/               # PAM binary (PAM_USER only)
    face-enroll/             # Enrolment CLI
    face-camera-diag/        # Camera discovery tool (list, dump)
    face-similarity-check/   # Offline photo FAR tool
  config/
    face-auth.toml.example   # Documented config template
  selinux/
    face-auth.te             # SELinux policy source
  deploy.sh                  # Installer
  pin-camera.sh              # Pins the camera by USB bus path
  uninstall.sh               # Removal script (--purge)
```

## License

MIT, for this code. A fork of [pfalkingham/authFace](https://github.com/pfalkingham/authFace)
(MIT). The face detector `version-slim-320.onnx` is MIT. The recognition models
(`w600k_mbf.onnx`, `w600k_r50.onnx`) are InsightFace model-zoo weights, licensed for
non-commercial research use only; see [Model](#model).
