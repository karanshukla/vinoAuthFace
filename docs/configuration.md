---
title: Configuration and enrolment
nav_order: 3
---

# Configuration and enrolment

Every configuration key is documented in
[`config/face-auth.toml.example`](https://github.com/karanshukla/vinoAuthFace/blob/main/config/face-auth.toml.example),
organized into everyday settings, liveness, lockout, guards, install/hardware, and advanced options.
This page explains which config files are trusted in each context, how enrolment works, and how to
choose between recognition models. If you prefer a GUI, the [settings window](settings.md) lets you
edit both system and user configs directly.

## Config sources

The PAM authentication path and the unprivileged offline tools follow different trust rules.

### PAM authentication (`vinoauthface-auth`)

Applies to `sudo`, lock screens, login screens, and polkit prompts:

| Source | Effect |
|---|---|
| `/etc/face-auth.toml` (root-owned) | Authoritative for all settings |
| `~/.config/face-auth.toml` | May only make authentication **stricter** (see [user overrides](#what-a-user-may-override-at-the-login-prompt)) |
| `FACE_AUTH_*` environment variables | **Ignored entirely** |

### Root enrolment (`vinoauthface enroll` and `improve`)

When run as root (the standard workflow), only `/etc/face-auth.toml` and explicit command-line flags
are read. Any `~/.config/face-auth.toml` or `FACE_AUTH_*` variables preserved by `sudo -E` or
`doas` `keepenv` are ignored. Any `--device` argument must point to an IR capture node or match the
`device` already configured in `/etc/face-auth.toml`.

### Offline tools and non-root enrolment

For offline diagnostic tools, or when enrolling as a normal user into a custom `--embeddings-dir`,
standard configuration layering applies in order of precedence: `FACE_AUTH_*` environment variables,
then `~/.config/face-auth.toml`, then `/etc/face-auth.toml`. Environment variable names match the
flat key names (for example, `FACE_AUTH_CAPTURE_TIMEOUT_MS` for the capture timeout and
`FACE_AUTH_LIVENESS_MOTION_THRESHOLD` for the liveness motion threshold).

Example `/etc/face-auth.toml`:

```toml
threshold = 0.6
scan_duration_ms = 5000
liveness.preset = "standard"
guards.start_delay_ms = 100
```

### Grouped keys

Related settings use dotted prefixes: `liveness.*`, `lockout.*`, and `guards.*` (which controls when
a scan runs: `seat_check`, `abort_if_ssh`, `abort_if_lid_closed`, `start_delay_ms`,
`start_delay_scope`, and `require_confirmation_elevation`).

Write these as dotted keys rather than TOML `[liveness]` table headers. `deploy.sh` and
`pin-camera.sh` append top-level keys to the end of `/etc/face-auth.toml`, and placing a `[table]`
header above them would put those appended keys inside that table.

Legacy flat key names (`liveness_motion_threshold`, `lockout_threshold`, `seat_check`, and similar)
are still supported. If both forms are present, the flat key takes precedence.

`liveness.preset` provides a shortcut for configuring motion liveness:

| Preset | Motion threshold | Residual threshold | Behaviour |
|---|---|---|---|
| `off` | `0` | `0` | Disables the motion check; a still photo or screen replay can unlock |
| `standard` | `0.01` | `0` | Default; stationary faces still pass via natural drift over the liveness window |
| `strict` | `0.01` | `0.3` | Also rejects hand-moved photos; may fail if you hold completely still |

Setting an individual `liveness.*` threshold overrides that specific value from the preset.

### What a user may override at the login prompt

A user's `~/.config/face-auth.toml` is resolved from their home directory via `getent passwd`, must
be a regular file owned by that user, and is applied strictly as a narrowing overlay:

| Keys | Behaviour during PAM authentication |
|---|---|
| `threshold`, `detector_threshold`, `liveness.motion_threshold`, `liveness.residual_motion_threshold`, `liveness.preset`, `min_face_size_ratio` | Applied only if **>= the system value**. Lower values are ignored, so setting `liveness.preset = "off"` in a user file has no effect. |
| `device` | Applied only if the path is a real IR capture device on this machine (an IR-named sysfs node or physical greyscale sensor that opens in a supported format). |
| `scan_duration_ms`, `scan_interval_ms`, `capture_timeout_ms`, `liveness.grace_ms` | Applied within built-in bounds. |
| `model_path`, `detector_model_path`, `embeddings_dir`, `pinned_camera_*`, `lockout.*`, `seal_embeddings`, `backend`, `npu_device`, `liveness.window_ms`, `bind_camera`, `guards.*` | **Ignored** (system policy only). |

This prevents unprivileged code running under your user account from writing a permissive
`~/.config/face-auth.toml` to weaken `sudo` authentication. To loosen matching thresholds, edit
`/etc/face-auth.toml` as root.

### Leave `device` unset

UVC cameras typically expose a metadata node alongside the capture node under the same card name
(for example, `/dev/video2` captures frames while `/dev/video3` is metadata-only). Auto-detection
probes each candidate and picks the first real capture node in a supported format, whereas manual
device paths are easy to get wrong. `pin-camera.sh` sets `device` automatically when pinning. If no
valid IR device is found, vinoAuthFace declines authentication and PAM falls back to your password.

## Enrolment

Because face templates are owned by root, enrolment requires `sudo`:

```bash
sudo vinoauthface enroll --user $USER           # replace templates with a new capture (30 frames)
sudo vinoauthface improve --user $USER          # append frames for additional lighting or angles
sudo vinoauthface enroll --guided --user $USER  # capture six measured poses (~10 templates)
```

`--guided` (available for both `enroll` and `improve`) keeps only frames matching the requested
poses; see [settings.md](settings.md#guided-enrolment).

Capturing 30 frames provides good pose and expression coverage in a single sitting, and running
`improve` in different lighting conditions is the best way to boost accuracy further. Each user can
store up to 256 embeddings. If an `enroll` or `improve` run would exceed 256 embeddings, it aborts
before opening the camera; re-enrol from scratch once your gallery is full.

Both `enroll` and `improve` accept `--user`, `--frames`, `--interval`, `--device`, `--threshold`,
`--model`, `--embeddings-dir`, and `-v`.

> Note: While the CLI binary is named `vinoauthface`, the config files (`face-auth.toml`),
> `FACE_AUTH_*` environment variables, `/var/lib/face-auth`, and the `face-auth` group retain their
> `face-auth` names.

**When to re-enrol:** Always re-enrol after switching recognition models (vinoAuthFace detects the
mismatch and refuses old templates) or after preprocessing pipeline changes. Templates created by
older versions of this fork or upstream may still work, though re-enrolling is recommended.

Enrolment requires root because whoever can write a face template controls which face unlocks the
account; a user's own session must not be able to modify templates directly.

## Recognition model: mbf vs r50

| Property | `mbf` | `r50` |
|---|---|---|
| Backbone | MobileFaceNet | ResNet50 |
| InsightFace pack | `buffalo_sc` | `buffalo_l` |
| Model size | ~13 MB | ~175 MB |
| Encode latency (NPU benchmark) | ~1.2 ms/frame | ~4.2 ms/frame |
| Genuine-match similarity (same benchmark) | mean 0.815, min 0.759 | mean 0.875, min 0.830 |

`r50` provides a noticeably wider match margin for a few extra milliseconds per frame, which is
negligible inside the camera-paced scan loop. At the default `threshold = 0.6`, `r50` accepted 0 of
7.7 million stranger pairs and `mbf` accepted 1 in 1.9 million on a near-IR benchmark dataset (see
[Match accuracy against strangers](security.md#match-accuracy-against-strangers)).

`deploy.sh` defaults to `r50` for NPU (OpenVINO) builds and `mbf` for CPU (`tract`) builds. On
existing installs, `deploy.sh` keeps your current model if `model_path` is already configured or any
user is already enrolled. To pick a model explicitly:

```bash
sudo FACE_AUTH_RECOGNITION_MODEL=r50 ./deploy.sh   # or mbf
```

**Switching models requires re-enrolling.** Both models output 512-dimensional embeddings, but their
embedding spaces are incompatible and cannot be compared. Each template file stores the tag of the
model that created it, and vinoAuthFace declines to authenticate or run `improve` when the tag does
not match the configured model. Legacy template files created before model tagging existed carry no
tag and are treated as compatible.

See [architecture.md](architecture.md#models) for model sources and licensing terms.
