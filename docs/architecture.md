# Architecture

## How it works

```
PAM (sudo / gdm-password / swaylock / polkit-1 / kde-fingerprint / cosmic-greeter)
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
  ├─ Face detection (Ultra-Light-Fast-Generic-Face-Detector), anchor-decoded box
  ├─ Crop to the face (+30% margin), resize to 112×112, normalise to [-1, 1]
  ├─ Encode (tract or OpenVINO; MobileFaceNet or ResNet50, 512-d embedding)
  ├─ Motion liveness across consecutive face frames
  ├─ Cosine similarity vs stored templates (default threshold 0.6)
  └─ Exit 0 (match) or exit 1 (no match → password prompt)
```

## Models

Recognition uses InsightFace **`w600k_mbf.onnx`** (MobileFaceNet @ WebFace600K, ~13 MB, 512-d
output) from the `buffalo_sc` pack by default, or `w600k_r50.onnx` from `buffalo_l`
([configuration.md](configuration.md#recognition-model-mbf-default-vs-r50)). Detection uses
**`version-slim-320.onnx`** from
[Linzaer/Ultra-Light-Fast-Generic-Face-Detector-1MB](https://github.com/Linzaer/Ultra-Light-Fast-Generic-Face-Detector-1MB),
a separate project.

**Licensing differs.** The detector is MIT. The recognition weights are not: InsightFace's model
zoo is licensed for non-commercial research use only (see `model_zoo/README.md` and
`python-package/README.md` in the InsightFace repo). Only InsightFace's library *code* is MIT.

Neither model is bundled. `deploy.sh` downloads both and verifies a pinned SHA-256 before
installing: the recognition model from InsightFace's GitHub releases, the detector from a specific
commit of its own repo.

## Relationship to upstream

A fork of [Peter Falkingham's authFace](https://github.com/pfalkingham/authFace), kept in sync
with upstream. On top of upstream it adds:

| Feature | Docs |
|---|---|
| Motion liveness: a match only counts after non-rigid motion of the face between consecutive frames | [security.md](security.md#presentation-attacks-something-held-up-to-the-real-camera) |
| Lockout: exponential backoff after repeated failed matches, never blocking the password | [troubleshooting.md](troubleshooting.md#face-auth-stopped-being-tried-after-a-few-failures) |
| Camera pinning by physical USB port, against frame injection | [security.md](security.md#frame-injection-a-fake-camera) |
| Face crop before encoding (detector boxes decoded) | above |
| OpenVINO/NPU backend (optional build) | [install.md](install.md#openvino--npu-backend) |
| mbf or r50 recognition model, templates tagged by model | [configuration.md](configuration.md#recognition-model-mbf-default-vs-r50) |
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
deploy.sh / uninstall.sh        # Install and removal
pin-camera.sh                   # Pins the camera by USB bus path
```
