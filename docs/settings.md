---
title: Settings window
nav_order: 8
---

# Settings window

`vinoauthface-settings` is an optional graphical interface for viewing and editing every key in
`/etc/face-auth.toml` and your personal `~/.config/face-auth.toml`. It runs as your user and plays
no part in authentication. The [tray icon](tray.md) works with or without it.

```bash
vinoauthface-settings    # or --page face|matching|liveness|camera|models|security|you|about
```

## Install

```bash
curl -fsSL https://github.com/karanshukla/vinoAuthFace/releases/latest/download/install.sh | sudo bash -s -- --with-settings
sudo ./deploy.sh --with-settings   # or build and install from a checkout
```

The settings window requires the tray (it uses the tray's root helper). Unlike the static musl core
binaries, `vinoauthface-settings` is linked against glibc so it can load your desktop's Wayland and
OpenGL libraries. Release builds target glibc 2.35 and run on Ubuntu 22.04, Debian 12, and newer
distributions. Subsequent deploys, upgrades, and `update.sh` preserve it; pass `--no-settings` or
run `uninstall.sh` to remove it.

| Installed file | Purpose |
|---|---|
| `/usr/local/bin/vinoauthface-settings` | Settings window executable |
| `/usr/local/share/applications/vinoauthface-settings.desktop` | Application launcher |

## Pages

| Page | Covers |
|---|---|
| **Face** | [Guided enrolment](#guided-enrolment) (not a config key) |
| **Matching** | `threshold`, `detector_threshold`, `min_face_size_ratio`, `scan_duration_ms`, `liveness.grace_ms`, `capture_timeout_ms`, `scan_interval_ms` |
| **Liveness** | `liveness.preset`, `liveness.motion_threshold`, `liveness.residual_motion_threshold`, `liveness.window_ms` |
| **Camera** | `device`, `bind_camera`, and removing an existing camera pin |
| **Models** | `model_path`, `detector_model_path`, `backend`, `npu_device`, plus [model and cache cleanup](#disk-space) |
| **Security** | `guards.*`, `lockout.*`, `seal_embeddings`, `embeddings_dir`, `update_check` |
| **Personal** | Per-user `~/.config/face-auth.toml`: stricter thresholds, scan timing, IR camera override, and `tray_idle_minutes` |
| **About** | Installed version, manual **Check for updates**, and links to vinopilot.ca and other vinoPilot apps |

On the **About** page, nothing contacts the network until you click **Check for updates**. When a
new release is available, **Update to vN** prompts for administrator authentication and runs the
tray helper's `upgrade` action (identical to the tray menu). Local `dev` builds display the newest
release version without offering to overwrite the local build. Setting `update_check = false` hides
the update row.

Every configuration key is documented in [configuration.md](configuration.md) and
[`config/face-auth.toml.example`](https://github.com/karanshukla/vinoAuthFace/blob/main/config/face-auth.toml.example).

## Saving

Changes on the **Personal** page save immediately to `~/.config/face-auth.toml`. Any threshold
looser than the system configuration is labelled "Ignored" because PAM authentication disregards it.

Changes on all other pages apply system-wide and wait until you click **Apply…** (one administrator
prompt saves all pending edits) or **Discard**.

Clicking **Apply…** runs `pkexec /usr/local/libexec/vinoauthface-helper write-config` (`auth_admin`)
and pipes the proposed file over stdin. The helper validates the payload before writing:

- The config must parse and validate identically to how `vinoauthface-auth` loads it.
- The edit must be based on the current file on disk; if `/etc/face-auth.toml` changed after the
  window loaded it, the write is rejected.
- Any modified `model_path`, `detector_model_path`, or `embeddings_dir` must be root-owned at every
  directory component so an unprivileged user cannot redirect authentication to files they control.
- Existing camera pins can be removed, but new camera pins must be set from a terminal with
  `sudo vinoauthface-pin-camera` (`pin-camera.sh`).

`/etc/face-auth.toml` is replaced via an atomic rename that preserves ownership and permissions.
Edits are applied minimally in place (replacing existing lines, uncommenting matching example lines,
or commenting out keys returned to default). Changes take effect immediately with no service to
restart.

## Guided enrolment

The **Face** page walks you through six poses instead of capturing 30 frames of a single pose:
straight on, slight left turn, slight right turn, slight tilt up, slight tilt down, and glasses off
or on. It saves roughly ten diverse templates; because matching compares against your best-matching
template, varied poses improve recognition across everyday angles.

Poses are measured from the detector's five facial landmarks relative to your straight-on pose. A
frame is accepted once you hold the target pose for two frames with eyes at least 30 pixels apart,
and near-duplicates of already kept frames are skipped. Your first horizontal turn establishes left
versus right so mirrored camera feeds work automatically. Each step times out and skips after 20
seconds; only the initial straight-on pose is required.

| Button | Command |
|---|---|
| **Start** or **Enrol again** (confirmed) | `vinoauthface enroll --guided` (replaces existing templates) |
| **Add poses** | `vinoauthface improve --guided` (appends to existing templates) |

Both actions run as root through `pkexec` (`enrol-guided` and `retrain-guided`, both `auth_admin`).
**Skip** and **Stop** send commands to the enrolment process's stdin, and **Stop** exits without
saving. There is no live camera preview, so camera frames stay in the root process. To run guided
enrolment in a terminal, use `sudo vinoauthface enroll --guided --user $USER` (press Enter to skip
a step, or type `cancel` to abort).

Because off-angle templates can occasionally increase impostor similarity scores, consider checking
before and after with `vinoauthface-similarity-check` (see #19).

## Your own models

Model paths are displayed read-only in the window for now; edit `model_path` or
`detector_model_path` directly in `/etc/face-auth.toml` to change them. Clicking **Check** next to
either path loads the model with `tract` on the CPU and reports its properties (embedding dimension,
which is 512 for the shipped models, or whether a detector outputs the required five landmarks).

Install custom models in a root-owned location before configuring them:

```bash
sudo install -m 644 my-model.onnx /usr/local/share/face-auth/
```

Because templates are tagged with the models that generated them (see
[configuration.md](configuration.md)), switching models requires everyone to re-enrol. The
**Templates** row shows which models generated your stored templates. Selecting **OpenVINO** under
**Inference** requires an NPU-enabled build, which `deploy.sh` installs automatically when OpenVINO
is present.

## Disk space

The bottom section of the **Models** page summarizes disk usage and lets you reclaim space:

- **Unused models:** Lists `.onnx` files in `/usr/local/share/face-auth` that are neither the active
  recognition model nor the active detector model in `/etc/face-auth.toml`, along with their total
  size. Unused files accumulate when switching between shipped models (`deploy.sh` leaves older
  models on disk) or after testing custom models. Clicking **Delete…** then **Delete** runs
  `pkexec /usr/local/libexec/vinoauthface-helper delete-unused-models` (`auth_admin`). The helper
  accepts no file paths; it computes the unused set itself from `/etc/face-auth.toml`, deletes only
  regular files directly inside `/usr/local/share/face-auth` (skipping symlinks and keeping active
  models wherever they point), and deletes nothing if the config fails to load. `deploy.sh` will
  re-download a shipped model automatically if you switch back later.
- **Compiled cache:** Shows the size of OpenVINO's compiled model cache in `/var/cache/face-auth`.
  **Clear** invokes the helper's `clear-cache` verb, emptying the contents while keeping the
  directory. The next unlock recompiles the cache (making that first scan slower), or you can
  rebuild it immediately with `sudo vinoauthface-auth --warm-cache`.

Active models and the compiled cache are removed automatically by `sudo vinoauthface-uninstall`.
Face templates in `/var/lib/face-auth` are never modified from this window and are only removed with
`sudo vinoauthface-uninstall --purge`.
