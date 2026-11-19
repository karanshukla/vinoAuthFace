---
title: Architecture
nav_order: 10
---

# Architecture

## How it works

```text
PAM (sudo / gdm-password / swaylock / hyprlock / polkit-1 / kde-fingerprint or kde-smartcard / cosmic-greeter)
  │
  ▼
vinoauthface-auth (static binary, set-group-ID face-auth)
  ├─ Drop caller environment if running with elevated privileges
  ├─ Resolve PAM_USER via getent (never USER/LOGNAME); non-root callers may only authenticate themselves
  ├─ Refuse if PAM_RHOST names a remote host
  ├─ Load /etc/face-auth.toml + strictly narrowing user overlay
  ├─ Verify pinned camera identity (if configured) and check failed-attempt lockout
  ├─ V4L2 capture (auto-detected IR node; GREY/YUYV/Y16), keeping the brighter of each frame pair
  │   └─ poll() with 5 s timeout; exits cleanly if the camera hangs
  ├─ Reject dark/flat frames and apply CLAHE equalization
  ├─ Face detection (SCRFD det_500m): bounding box and five landmarks
  ├─ Align: similarity-warp landmarks onto ArcFace's 112×112 template and normalize to [-1, 1]
  ├─ Encode (tract or OpenVINO; MobileFaceNet or ResNet50, 512-d embedding)
  ├─ Check motion liveness across face frames up to 1 s apart
  ├─ Compute cosine similarity against stored templates (default threshold 0.6)
  └─ Exit 0 (match) or exit 1 (no match → password prompt)
```

## Models

### Recognition and detection

Face recognition uses InsightFace weights:

- **`w600k_r50.onnx`** (ResNet50 trained on WebFace600K, ~175 MB, 512-d output, from the `buffalo_l`
  pack) on NPU builds.
- **`w600k_mbf.onnx`** (MobileFaceNet, ~13 MB, 512-d output, from the `buffalo_sc` pack) on CPU
  builds (see [configuration.md](configuration.md#recognition-model-mbf-vs-r50)).

Face detection uses InsightFace **`det_500m.onnx`** (SCRFD, ~2.5 MB, from `buffalo_sc`), which
outputs a bounding box and five facial landmarks (eyes, nose tip, and mouth corners). Warping the
face so those landmarks align with the reference coordinates expected by the recognition models—as
opposed to cropping directly from the bounding box—raised median same-person similarity on recorded
IR clips from 0.66 to 0.86, and from 0.29 to 0.71 while wearing glasses
([#27](https://github.com/karanshukla/vinoAuthFace/issues/27)).

Installations from before SCRFD used the box-only `version-slim-320.onnx` detector
([Linzaer/Ultra-Light-Fast-Generic-Face-Detector-1MB](https://github.com/Linzaer/Ultra-Light-Fast-Generic-Face-Detector-1MB),
MIT license); vinoAuthFace falls back to it if `det_500m.onnx` is absent. Because the detector is
recorded in each template's model tag, switching detectors requires re-enrolling.

`detector_model_path` also accepts a BlazeFace-style five-point detector (896 anchors, 128×128
input, outputs named `regressors` and `classificators`). vinoAuthFace detects which of the three
formats a model uses by inspecting its outputs and decodes each accordingly (`blazeface.rs` performs
a second refinement pass on a face crop). No BlazeFace model is shipped with vinoAuthFace.

In `tract` 0.21, SCRFD's `Resize` upsampling nodes (which specify target sizes with empty scales)
were evaluated as no-ops, so `detector.rs` rewrites those nodes to explicit ×2 scales before loading.
This rewrite remains in place on `tract` 0.23 and has not yet been re-tested for removal.

### Licensing

InsightFace's model zoo weights—both the recognition models (`w600k_mbf.onnx`, `w600k_r50.onnx`) and
the SCRFD detector (`det_500m.onnx`)—are licensed for **non-commercial research use only** (see
`model_zoo/README.md` and `python-package/README.md` in the InsightFace repository). Only
InsightFace's library source code is MIT-licensed
([#83](https://github.com/karanshukla/vinoAuthFace/issues/83)).

No model files are bundled in this repository. `deploy.sh` downloads them from InsightFace's GitHub
releases and verifies pinned SHA-256 checksums before installation.

## Why it is built this way

**Designed for immutable distros and laptops with Intel NPUs.** Most Linux face-authentication tools
rely on a background daemon communicating over D-Bus ([gaze](https://github.com/gundulabs/gaze),
Visage, Biopass) or a Python runtime (Howdy), installed via the system package manager. On immutable
systems such as Bazzite, Bluefin, Fedora Silverblue, or Kinoite, that requires package layering and
reboots—or fails outright (for example, `gaze`'s
[installer](https://github.com/gundulabs/gaze/blob/main/docs/public/install.sh) refuses to run on
`rpm-ostree` systems). vinoAuthFace instead ships as a standalone static musl binary in
`/usr/local` with no daemon, systemd units, D-Bus service, or system package dependencies.

**Intentionally IR-only.** There is no RGB webcam path, hybrid mode, or colour fallback. Resistance
to phone-screen spoofing relies on active near-infrared sensing, and IR illumination also works in
the dark—matching how Windows Hello operates. Adding optional Intel NPU inference lets the system
run stronger recognition models like `w600k_r50` quickly during unlock.

If your machine does not have an IR camera, or you prefer a full enrolment wizard and daemon-based
setup, consider Gaze, Facelock, or Howdy instead.

## Relationship to upstream

vinoAuthFace is a fork of [Peter Falkingham's authFace](https://github.com/pfalkingham/authFace),
kept in sync with upstream. Additions in this fork include:

| Feature | Documentation |
|---|---|
| Motion liveness (including opt-in non-rigid residual motion check) | [security.md](security.md#presentation-attacks-something-held-up-to-the-real-camera) |
| Exponential failed-match lockout that never blocks password fallback | [troubleshooting.md](troubleshooting.md#face-auth-stopped-being-tried-after-a-few-failures) |
| Physical USB port camera pinning against frame injection | [security.md](security.md#frame-injection-a-fake-camera) |
| Face detection and landmark alignment before encoding | [Models](#models) |
| Optional OpenVINO / Intel NPU inference backend | [install.md](install.md#openvino--npu-backend) |
| Choice of `mbf` or `r50` recognition model with model-tagged templates | [configuration.md](configuration.md#recognition-model-mbf-vs-r50) |
| `polkit-1`, Bitwarden biometric unlock, and KDE lock/login screen support | [pam.md](pam.md) |
| `YUYV` and `Y16` sensor support and auto-detection for IR nodes not named "IR" | [hardware.md](hardware.md) |
| Prebuilt GitHub Release binaries and CI running real deploy/uninstall tests | [install.md](install.md) |
| `vinoauthface-camera-diag` and `vinoauthface-similarity-check` tools | [hardware.md](hardware.md#vinoauthface-camera-diag) |
| Optional tray icon for status, enrolment, retraining, test scans, and settings | [tray.md](tray.md) |

Upstream's GTK settings GUI and GNOME scan-indicator extension are omitted; the optional tray icon
([tray.md](tray.md)) and settings window ([settings.md](settings.md)) replace them without needing a
root-to-user status channel. See
[CHANGELOG.md](https://github.com/karanshukla/vinoAuthFace/blob/main/CHANGELOG.md) for the complete
history, including the upstream security hardening pass.

## Project structure

```text
crates/
  face-auth-core/          # Core library
    src/
      capture.rs           # V4L2 capture, formats, IR auto-detect, sysfs identity
      config.rs            # Layered config, narrowing PAM overlay, camera pin verification
      blazeface.rs         # BlazeFace-style five-point detector: anchors, decode, two passes
      detector.rs          # Frame quality gates, detector format dispatch, slim box decode
      error.rs             # Error types
      inference.rs         # tract / OpenVINO encoder
      installed.rs         # Installed helper paths, --enrolled query, pkexec exit codes
      lib.rs               # FaceAuth: authentication scan (liveness, lockout) and enrolment
      lockout.rs           # Per-user failed-match exponential backoff
      preprocess.rs        # CLAHE, face alignment/crop, resize/normalize, motion profile
      storage.rs           # Versioned, model-tagged, atomic template I/O
      user.rs              # NSS lookup and username validation
      verify.rs            # Cosine similarity matching
    examples/              # detect-camera, frame-stats, bench
  face-auth/               # PAM binary (reads PAM_USER only)
  face-enroll/             # Enrolment and diagnostics CLI (vinoauthface)
  face-camera-diag/        # Standalone camera discovery tool (list, dump)
  face-similarity-check/   # Offline photo FAR/TAR evaluation tool
  face-auth-tray/          # Tray icon, vinoauthface-helper (pkexec), polkit policy, desktop files
  face-auth-settings/      # Settings window: window.rs (frame), window/pages.rs, window/rows.rs
config/face-auth.toml.example   # Documented configuration template
selinux/face-auth.te            # SELinux policy source
selinux/face-auth.fc            # SELinux file contexts (NPU node, store, lockout, NPU cache)
install.sh                      # One-line installer: downloads release bundle and runs deploy.sh --prebuilt
deploy.sh / uninstall.sh        # Installation (build or download + files) and removal
setup.sh                        # PAM, login screen, Bitwarden, SELinux (installed as vinoauthface-setup)
upgrade.sh                      # Release upgrader (installed as vinoauthface-upgrade)
login-mode.sh                   # KDE login screen mode switcher (used by deploy.sh and tray helper)
setting-mode.sh                 # Tray Settings submenu writer (used by tray helper)
pin-camera.sh                   # Pins the IR camera by physical USB bus path
```
