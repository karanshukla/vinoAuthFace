# Architecture

## How it works

```
PAM (sudo / gdm-password / swaylock / polkit-1 / kde-fingerprint or kde-smartcard / cosmic-greeter)
  │
  ▼
vinoauthface-auth (static binary, set-group-ID face-auth)
  ├─ Drop the caller's environment if running with borrowed privileges
  ├─ Resolve PAM_USER via getent (never USER/LOGNAME); a non-root caller may only be itself
  ├─ Refuse if PAM_RHOST names a remote host
  ├─ Load /etc/face-auth.toml + strictly-narrowing user overlay
  ├─ Verify the pinned camera identity (if pinned), check lockout
  ├─ V4L2 capture (auto-detected node; GREY/YUYV/Y16), brighter of each frame pair
  │   └─ poll() with 5s timeout; exits cleanly if the camera hangs
  ├─ Reject dark/flat frames, CLAHE equalisation
  ├─ Face detection (SCRFD det_500m): box and five landmarks
  ├─ Align: similarity-warp the landmarks onto ArcFace's 112×112 template, normalise to [-1, 1]
  ├─ Encode (tract or OpenVINO; MobileFaceNet or ResNet50, 512-d embedding)
  ├─ Motion liveness across face frames up to 1 s apart
  ├─ Cosine similarity vs stored templates (default threshold 0.6)
  └─ Exit 0 (match) or exit 1 (no match → password prompt)
```

## Models

Recognition uses InsightFace **`w600k_r50.onnx`** (ResNet50 @ WebFace600K, ~175 MB, 512-d
output) from the `buffalo_l` pack on an NPU build, and **`w600k_mbf.onnx`** (MobileFaceNet, ~13 MB)
from `buffalo_sc` on a CPU build
([configuration.md](configuration.md#recognition-model-mbf-vs-r50)). Detection uses
InsightFace **`det_500m.onnx`** (SCRFD, ~2.5 MB) from the same `buffalo_sc` pack: a box plus five
landmarks (eyes, nose tip, mouth corners). The face is warped so those land on the fixed points
the recognition models were trained on, instead of being cropped from the box, which on recorded
IR clips raised same-person similarity from a median of 0.66 to 0.86, and from 0.29 to 0.71 for
the same person in glasses ([#27](https://github.com/karanshukla/vinoAuthFace/issues/27)).
Installs from before SCRFD used the box-only `version-slim-320.onnx`
([Linzaer/Ultra-Light-Fast-Generic-Face-Detector-1MB](https://github.com/Linzaer/Ultra-Light-Fast-Generic-Face-Detector-1MB),
MIT); vinoAuthFace falls back to it when `det_500m.onnx` isn't installed. The detector is part of
the templates' model tag, so switching means re-enrolling.

tract 0.21 mis-evaluated SCRFD's upsampling (`Resize` given sizes and empty scales resizes
nothing), so `detector.rs` rewires those nodes to explicit ×2 scales before loading. The rewrite
is still applied on tract 0.23; whether 0.23 still needs it hasn't been checked.

**Licensing.** InsightFace's model zoo, recognition *and* SCRFD weights, is licensed for
non-commercial research use only (see `model_zoo/README.md` and `python-package/README.md` in the
InsightFace repo). Only InsightFace's library *code* is MIT
([#83](https://github.com/karanshukla/vinoAuthFace/issues/83)).

No model is bundled. `deploy.sh` downloads them from InsightFace's GitHub releases and verifies a
pinned SHA-256 before installing.

## Relationship to upstream

A fork of [Peter Falkingham's authFace](https://github.com/pfalkingham/authFace), kept in sync
with upstream. On top of upstream it adds:

| Feature | Docs |
|---|---|
| Motion liveness: a match only counts after motion of the face (non-rigid, opt-in) | [security.md](security.md#presentation-attacks-something-held-up-to-the-real-camera) |
| Lockout: exponential backoff after repeated failed matches, never blocking the password | [troubleshooting.md](troubleshooting.md#face-auth-stopped-being-tried-after-a-few-failures) |
| Camera pinning by physical USB port, against frame injection | [security.md](security.md#frame-injection-a-fake-camera) |
| Face crop before encoding (detector boxes decoded) | above |
| OpenVINO/NPU backend (optional build) | [install.md](install.md#openvino--npu-backend) |
| mbf or r50 recognition model, templates tagged by model | [configuration.md](configuration.md#recognition-model-mbf-vs-r50) |
| polkit-1, Bitwarden and KDE lock screen unlock | [pam.md](pam.md) |
| YUYV and Y16 sensors, auto-detect for IR nodes not named "IR" | [hardware.md](hardware.md) |
| Prebuilt release binaries; CI running real deploy/uninstall cycles | [install.md](install.md) |
| `vinoauthface-camera-diag` and `vinoauthface-similarity-check` | [hardware.md](hardware.md#vinoauthface-camera-diag) |
| Tray icon: status, enrol, retrain, test scan, uninstall | [tray.md](tray.md) |

Upstream's GTK settings GUI and GNOME scan-indicator extension aren't included. The optional
tray icon ([tray.md](tray.md)) replaces both, without a root-to-user status channel. The full history, including the upstream security pass, is in
[CHANGELOG.md](../CHANGELOG.md).

## Project structure

```
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
      preprocess.rs        # CLAHE, face crop, resize/normalise, motion profile
      storage.rs           # Template I/O (versioned, model-tagged, atomic)
      user.rs              # NSS lookup + username validation
      verify.rs            # Cosine similarity
    examples/              # detect-camera, frame-stats, bench
  face-auth/               # PAM binary (PAM_USER only)
  face-enroll/             # Enrolment CLI
  face-camera-diag/        # Camera discovery tool (list, dump)
  face-similarity-check/   # Offline photo FAR tool
  face-auth-tray/          # Tray icon + vinoauthface-helper (pkexec), polkit policy, desktop files
config/face-auth.toml.example   # Documented config template
selinux/face-auth.te            # SELinux policy source
selinux/face-auth.fc            # SELinux file labels (NPU node, store, lockout, NPU cache)
deploy.sh / uninstall.sh        # Install and removal
login-mode.sh                   # Plasma login screen mode (deploy.sh and the tray helper run it)
setting-mode.sh                 # the tray's Settings menu: the few config keys it may set (the tray helper runs it)
pin-camera.sh                   # Pins the camera by USB bus path
```
