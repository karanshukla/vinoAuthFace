---
title: Settings window
nav_order: 8
---

# Settings window

`vinoauthface-settings` is an optional window for every key in `/etc/face-auth.toml` and your own
`~/.config/face-auth.toml`. It runs as you and is never part of authentication. The
[tray](tray.md) doesn't need it.

```bash
vinoauthface-settings    # or --page face|matching|liveness|camera|models|security|you
```

## Install

```bash
sudo ./deploy.sh --with-settings
```

It needs the tray and a Rust toolchain, and builds for glibc, not musl: the window loads the
desktop's Wayland and OpenGL libraries. Releases ship musl only, so without a toolchain there's
no window. Later deploys and `update.sh` keep it; `--no-settings` and `uninstall.sh` remove it.

| File | What |
|---|---|
| `/usr/local/bin/vinoauthface-settings` | The window |
| `/usr/local/share/applications/vinoauthface-settings.desktop` | Its launcher |

## Pages

| Page | Keys |
|---|---|
| Face | Not a key: guided enrolment, below |
| Matching | `threshold`, `detector_threshold`, `min_face_size_ratio`, `scan_duration_ms`, `liveness.grace_ms`, `capture_timeout_ms`, `scan_interval_ms` |
| Liveness | `liveness.preset`, `liveness.motion_threshold`, `liveness.residual_motion_threshold`, `liveness.window_ms` |
| Camera | `device`, `bind_camera`, the camera pin (remove only) |
| Models | `model_path`, `detector_model_path`, `backend`, `npu_device` |
| Security | `guards.*`, `lockout.*`, `seal_embeddings`, `embeddings_dir`, `update_check` |
| Just you | Your own file: stricter thresholds, scan timing, your IR camera, `tray_idle_minutes` |

Each key is described in [configuration.md](configuration.md) and
[`config/face-auth.toml.example`](https://github.com/karanshukla/vinoAuthFace/blob/main/config/face-auth.toml.example).

## Saving

**Just you** saves as you change it. A value looser than the computer's shows as "Ignored", as
authentication ignores it.

**The other pages** apply to everyone, so changes wait for **Apply…**: one administrator
password for all of them. **Discard** drops them.

Apply runs `pkexec /usr/local/libexec/vinoauthface-helper write-config` (`auth_admin`) with the
new file on stdin. The helper doesn't trust it:

- It must validate as authentication reads it.
- It must be based on the current file. If the file changed since the window read it, nothing is
  written.
- A new `model_path`, `detector_model_path` or `embeddings_dir` must be root's all the way up.
  Each decides whose face unlocks, so a path you could write would bypass the password prompt.
- The camera pin can be removed, not set.

The file is replaced in one rename, keeping its owner and mode. Only the changed lines move: a
value is replaced in place or over its commented-out example, and a default is commented out.
There's no service to restart.

## Guided enrolment

The Face page enrols you in six steps instead of 30 frames of one pose: straight on, a slight turn
each way, a slight tilt up and down, then glasses off or on. It keeps about ten templates; matching
takes the best one, so varied templates cover more of how you're seen.

Poses are measured from the detector's five landmarks, relative to your own straight-on pose. A frame
counts once the pose is held for two frames with the eyes at least 30 px apart, and near-copies of
a kept frame are skipped. Your first turn sets which side is which, so a mirrored image works. A
step not done in 20 s is skipped; only straight on is required.

| Button | Runs |
|---|---|
| Start, or Enrol again (confirmed) | `vinoauthface enroll --guided`, replacing your templates |
| Add poses | `vinoauthface improve --guided`, keeping them |

Both run as root through pkexec (`enrol-guided`, `retrain-guided`, `auth_admin`). Skip and Stop
go to the enrolment's stdin; Stop saves nothing. There's no camera preview. In a terminal:
`sudo vinoauthface enroll --guided --user $USER` (Enter skips, `cancel` stops).

Off-angle templates can raise impostor scores, so measure before and after with
`vinoauthface-similarity-check` (see #19).

## Your own models

**Check**, beside each model path, loads the model with tract on the CPU and reports what it
found: values per face (the shipped models give 512), or whether a detector gives the five face points.

Copy a model where only root can write before picking it:

```bash
sudo install -m 644 my-model.onnx /usr/local/share/face-auth/
```

Templates are tagged with their models (see [configuration.md](configuration.md)), so a new
model means everyone enrols again. The Templates row warns before Apply.
