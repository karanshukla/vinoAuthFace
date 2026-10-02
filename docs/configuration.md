# Configuration and enrolment

Every key is documented in [`config/face-auth.toml.example`](../config/face-auth.toml.example),
which is grouped: everyday settings, liveness, lockout, guards, install/hardware, advanced. This
page covers which config sources are trusted where, enrolment, and choosing a recognition model.

## Config sources

The authentication path and the unprivileged tools trust different things.

**During PAM authentication** (`vinoauthface-auth`: sudo, lock screen, login, polkit):

| Source | Effect |
|--------|--------|
| `/etc/face-auth.toml` (root-owned) | Authoritative for everything |
| `~/.config/face-auth.toml` | May only make authentication **stricter**, see below |
| `FACE_AUTH_*` environment | **Ignored entirely** |

**For `vinoauthface enroll` and `improve` run as root** (the normal case), only
`/etc/face-auth.toml` and the command-line flags count: a `~/.config/face-auth.toml` or `FACE_AUTH_*`
carried over by `sudo -E` or doas `keepenv` is ignored. A `--device` must be an IR capture node, or
the `device` already set in `/etc/face-auth.toml`.

**For the offline tools, and enrolment as a normal user into your own `--embeddings-dir`**, the
usual layering applies: environment variables, then `~/.config/face-auth.toml`, then
`/etc/face-auth.toml`. Environment variable names follow the flat field names, so the capture
timeout is `FACE_AUTH_CAPTURE_TIMEOUT_MS` and the liveness motion threshold is
`FACE_AUTH_LIVENESS_MOTION_THRESHOLD`.

Example `/etc/face-auth.toml`:

```toml
threshold = 0.6
scan_duration_ms = 5000
liveness.preset = "standard"
guards.start_delay_ms = 100
```

### Grouped keys

Related settings share a prefix: `liveness.*`, `lockout.*` and `guards.*` (when a scan runs at all:
`seat_check`, `abort_if_ssh`, `abort_if_lid_closed`, `start_delay_ms`, `start_delay_scope`,
`require_confirmation_elevation`). Write them dotted, as the example does, rather than as `[liveness]`
headers: `deploy.sh` and `pin-camera.sh` append plain keys to the end of the file, and after a
table header those would land inside the table.

The old flat names (`liveness_motion_threshold`, `lockout_threshold`, `seat_check`, ...) still work.
If both are set, the flat key wins.

`liveness.preset` is the quick way to set motion liveness:

| Preset | Motion threshold | Residual threshold | Use |
|--------|------------------|--------------------|-----|
| `off` | 0 | 0 | No motion check. A photo or screen replay can unlock |
| `standard` | 0.01 | 0 | The default. Still faces pass by drifting over the window |
| `strict` | 0.01 | 0.3 | Also rejects a photo moved by hand; may fail if you sit very still |

An individual `liveness.*` value overrides the preset for that value.

### What a user may override at the login prompt

A user's own config is read from *their* home (resolved via `getent passwd`), must be a regular
file they own, and is applied as a narrowing overlay:

| Key | At the login prompt |
|-----|--------------------|
| `threshold`, `detector_threshold`, `liveness.motion_threshold`, `liveness.residual_motion_threshold`, `liveness.preset`, `min_face_size_ratio` | Honoured only if **>= the system value**. A lower number is ignored, so `preset = "off"` in a user file does nothing |
| `device` | Honoured only if the path is a real IR capture device on this machine (IR-looking sysfs name, or a physical greyscale sensor, and opens in a supported format) |
| `scan_duration_ms`, `scan_interval_ms`, `capture_timeout_ms`, `liveness.grace_ms` | Honoured within built-in bounds |
| `model_path`, `detector_model_path`, `embeddings_dir`, `pinned_camera_*`, `lockout.*`, `seal_embeddings`, `backend`, `npu_device`, `liveness.window_ms`, `bind_camera`, `guards.*` | **Ignored**: system policy only |

This stops code running as you, which doesn't know your password, from writing a permissive
`~/.config/face-auth.toml` and turning your next `sudo` into a root shell. To *loosen* matching,
edit `/etc/face-auth.toml` as root.

### Leave `device` unset

UVC cameras normally expose a metadata node right beside the capture node under the same name
(`/dev/video2` captures, `/dev/video3` doesn't). Auto-detection opens each candidate and takes the
first that really is a capture device in a supported format. A hand-written path often gets this
wrong. `pin-camera.sh` sets `device` for you. If nothing qualifies, there's no fallback node: face
auth declines and PAM asks for the password.

## Enrolment

Face templates are root-owned, so enrolment needs root:

```bash
sudo vinoauthface enroll --user $USER    # replace templates with a new capture (30 frames)
sudo vinoauthface improve --user $USER   # append, for other lighting or angles
```

30 frames gives enough pose and expression variation from one sitting. Running `improve` in
different lighting is the biggest gain beyond that. A user can store at most 256 embeddings;
an enrolment or `improve` that would go past that fails before the camera opens, so re-enrol
from scratch once the gallery is full.

Both take `--user`, `--frames`, `--interval`, `--device`, `--threshold`, `--model` and
`--embeddings-dir`; `-v` works on either.

The command is `vinoauthface`, but the config files (`face-auth.toml`), `FACE_AUTH_*` variables,
`/var/lib/face-auth` and the `face-auth` group keep their names.

**Re-enrol** after switching recognition model (vinoAuthFace refuses the old templates and says so)
and after any change to the preprocessing pipeline. Templates from older versions of this fork or
upstream may still match, but re-enrolling is the safer bet.

Enrolment isn't user-writable on purpose: whatever can write a face template decides whose face
unlocks that account. If your own login could rewrite it, so could anything running as you.

## Recognition model: mbf vs r50

| | `mbf` | `r50` |
|---|---|---|
| Backbone | MobileFaceNet | ResNet50 |
| Pack | `buffalo_sc` | `buffalo_l` |
| Size | ~13 MB | ~175 MB |
| Encode latency (NPU benchmark) | ~1.2 ms/frame | ~4.2 ms/frame |
| Genuine-match similarity (same benchmark) | mean 0.815, min 0.759 | mean 0.875, min 0.830 |

`r50` gives a wider match margin for a few milliseconds per frame, which the camera-paced scan
loop hides. `deploy.sh` picks `r50` for an NPU (OpenVINO) build and `mbf` for a CPU (tract) build.
An existing install keeps its model: the NPU default isn't applied when the config already sets
`model_path` or someone is enrolled. Choose explicitly with:

```bash
sudo FACE_AUTH_RECOGNITION_MODEL=r50 ./deploy.sh   # or mbf
```

**Switching models requires re-enrolling.** The two produce incompatible embedding spaces with the
same 512-d shape, so comparing across them is meaningless rather than just less accurate. Every
template file records the model that produced it, and vinoAuthFace refuses to authenticate or
`improve` against a mismatch. Files from before this existed carry no tag and are treated as
compatible.

Model sources and licensing: [architecture.md](architecture.md#models).
