---
title: Settings window
nav_order: 8
---

# Settings window

`vinoauthface-settings` is an optional window that shows every key in `/etc/face-auth.toml`,
and the ones you may set in your own `~/.config/face-auth.toml`. The [tray](tray.md) is
unchanged and doesn't need it. Like the tray it runs as you and is never part of authentication.

Open it from the app launcher ("vinoAuthFace settings") or a terminal:

```bash
vinoauthface-settings                  # or --page matching|liveness|camera|models|security|you
```

## Install

`sudo ./deploy.sh --with-settings` builds and installs it (it needs the tray and a Rust
toolchain). Later deploys keep it; `--no-settings` removes it. It is built
for the host (glibc), not static musl like the other binaries, since the window loads the
desktop's Wayland and OpenGL libraries. Releases ship musl only, so a deploy from a downloaded
release with no Rust toolchain has no settings window; edit `/etc/face-auth.toml` by hand there.
`sudo ./update.sh` rebuilds it where it's installed, and `uninstall.sh` removes it.

| File | What |
|---|---|
| `/usr/local/bin/vinoauthface-settings` | The window. Runs as you |
| `/usr/local/share/applications/vinoauthface-settings.desktop` | Its launcher |

It needs the tray's root helper and polkit action (`--no-tray` skips both, and the window).

## Pages

| Page | Keys |
|---|---|
| Matching | `threshold`, `detector_threshold`, `min_face_size_ratio`, `scan_duration_ms`, `liveness.grace_ms`, `capture_timeout_ms`, `scan_interval_ms` |
| Liveness | `liveness.preset`, `liveness.motion_threshold`, `liveness.residual_motion_threshold`, `liveness.window_ms` |
| Camera | `device` (picked from the video devices present), `bind_camera`, and the camera pin, which it can remove but not set |
| Models | `model_path`, `detector_model_path`, `backend`, `npu_device` |
| Security | `guards.*`, `lockout.*`, `seal_embeddings`, `embeddings_dir`, `update_check` |
| Just you | Your own file: the thresholds, which only count when stricter than the computer's, the scan timing, your IR camera and `tray_idle_minutes` |

What each key does is in [configuration.md](configuration.md) and the comments in
[`config/face-auth.toml.example`](https://github.com/karanshukla/vinoAuthFace/blob/main/config/face-auth.toml.example).

## Saving

**Just you** saves as you change it: the file is yours, no password needed. A value looser than
the computer's says "Ignored", since authentication would ignore it anyway.

**Everything else** is for everyone on the computer, so changes wait in a bar at the bottom of
the window until you press **Apply…**, which asks for an administrator's password once for all of
them. **Discard** puts them back.

Apply runs `pkexec /usr/local/libexec/vinoauthface-helper write-config` (polkit action
`io.github.karanshukla.vinoauthface.write-config`, `auth_admin`) with the new file on stdin. It is
the one helper verb whose content comes from the caller, so the helper trusts none of it:

- The new file must parse and validate exactly as authentication reads it. A file face unlock
  would refuse is never written.
- It must have been made from the file as it is now. If something else changed
  `/etc/face-auth.toml` since the window read it (you by hand, the tray, `deploy.sh`), nothing is
  written; the window reads the file again when it regains focus.
- A `model_path`, `detector_model_path` or `embeddings_dir` that changes must be root's: it, and
  every directory above it, owned by root and writable by nobody else. Either decides whose face
  unlocks an account, so a path you could write to would make the password prompt a formality.
  The window says so before you press Apply.
- The camera pin can be removed, not set: `pin-camera.sh` reads it from sysfs.

The file is replaced in one rename, keeping its owner and mode. Edits are made line by line, as
`setting-mode.sh` makes them: a value is replaced where it is, or written over its commented-out
example line, and a key put back to its default is commented out. Comments and keys the window
doesn't change are left alone. The next unlock reads the new file; there is no service to restart.

## Your own models

The Models page has a **Check** button beside each model path. It loads the file as
authentication would (with tract on the CPU, whatever the backend) and says what it found: how
many values the recognition model gives per face (the shipped ones give 512), or whether a
detector is SCRFD, whose landmarks align the face, or box only. A model that doesn't load says
why.

To use one, copy it where only root can write, then pick it:

```bash
sudo install -m 644 my-model.onnx /usr/local/share/face-auth/
```

Templates are tagged with the models that made them (see
[configuration.md](configuration.md)), so a new recognition or detector model means everyone
enrols again. The Templates row says when a change would do that, and the notice after Apply
repeats it. Enrol from the tray's "Enrol again from scratch", or `sudo vinoauthface enroll --user
$USER`.
