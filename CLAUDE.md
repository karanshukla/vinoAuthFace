# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`authFace` — a Rust PAM authentication module that unlocks Linux (sudo, lock screen, polkit)
via an IR camera, Windows Hello-style. Static musl binary, no daemon, no systemd, no D-Bus.
Designed to work on immutable distros (Bazzite, Bluefin, Silverblue, Kinoite) with zero system
packages beyond what's already there. See README.md for the full feature/security writeup —
don't duplicate it here.

## Build & test commands

```bash
# Add the target once
rustup target add x86_64-unknown-linux-musl

# Build the two deployable binaries (static musl, CPU/tract backend)
cargo build --release --target x86_64-unknown-linux-musl -p face-auth -p face-enroll

# Build with the OpenVINO/NPU backend instead (requires OpenVINO runtime + headers)
cargo build --release --target x86_64-unknown-linux-musl -p face-auth -p face-enroll --features npu

# Run all tests (workspace-wide; face-auth-core holds nearly all of them)
cargo test --workspace

# Run a single test
cargo test -p face-auth-core storage::tests::save_then_load_round_trips_embeddings_and_tag

# Check without the slow release build
cargo check --workspace
```

There is no CI workflow and no clippy/rustfmt config committed — match existing style by hand.

To actually exercise a change end-to-end (not just unit tests), deploy and test on real
hardware: `sudo ./deploy.sh`, `face-enroll --user $USER`, `sudo true`. See README's
Troubleshooting section for `RUST_LOG=face_auth_core=debug` and direct-binary invocation
(`sudo env PAM_USER=$USER USER=$USER HOME=$HOME /usr/local/bin/face-auth`).

## Architecture

### Workspace layout

Five crates. All the logic lives in `face-auth-core`; the rest are thin CLI/PAM/debug shims:

- **`crates/face-auth-core`** — the library. Camera I/O, detection, inference, preprocessing,
  storage, verification, config, lockout. Everything below refers to files here unless noted.
- **`crates/face-auth`** (`src/main.rs`) — the PAM binary. No stdin/args; resolves the user via
  `PAM_USER` → `USER` → `LOGNAME` → `id -un` fallback chain, then calls `authenticate_scan`.
  Exit 0 = matched, exit 1 = anything else (PAM's `sufficient` line falls through to password).
- **`crates/face-enroll`** (`src/main.rs`) — the enrollment CLI (`clap`-based).
- **`crates/face-similarity-check`** (`src/main.rs`) — offline debug tool, not deployed by
  `deploy.sh`. Runs the same detect → crop → CLAHE → encode → cosine-similarity pipeline as a
  live auth attempt, but fed from image files (`image::open`, upscaled the same `*257` way
  `capture.rs` upscales raw camera bytes) instead of the IR camera — for gauging false-accept
  risk against photos of other people without needing a second person at the camera. Everything
  runs locally against the on-disk model/embeddings; only the printed similarity score is
  produced, nothing is transmitted anywhere.
- **`crates/face-camera-diag`** (`src/main.rs`) — offline camera discovery/diagnostic tool, also
  not deployed by `deploy.sh`. `list` enumerates every `/dev/video*` node with driver/card name
  (`VIDIOC_QUERYCAP`), resolved USB VID:PID (walks up sysfs from `capture::device_bus_path`), and
  current pixel format/resolution (`VIDIOC_G_FMT` via `capture::query_format`) — for figuring out
  which node is the IR sensor and what format it reports without reading through the whole
  README. `dump` captures one frame from a given device and writes it as a 16-bit PGM for visual
  inspection. Purely read-only against devices it's just listing; `dump` takes the target device
  the same way live face-auth would.

The `npu` Cargo feature (on `face-auth-core`, propagated through the other four crates) swaps
the inference backend from pure-Rust `tract-onnx` (CPU) to `openvino` (NPU/GPU/CPU via OpenVINO
runtime) — see `#[cfg(feature = "npu")]` in `inference.rs` and `detector.rs`. Backend selection
at runtime is `config.backend()` (`"tract"` default or `"openvino"`) plus `config.npu_device()`
(`"NPU"`/`"GPU"`/`"CPU"`), independent of which one was compiled in.

### Auth/enroll pipeline (`lib.rs`)

`FaceAuth` owns a `FaceDetector` + `FaceEncoder` (both loaded once at construction, from
`config.model_path()`/`config.detector_model_path()`). Both `authenticate_once` /
`authenticate_scan` (used by the PAM binary) and `enroll` / `enroll_append` (used by
`face-enroll`) run the same per-frame pipeline:

```
capture (V4L2, GREY) → content check (variance) → histogram equalize
  → detect (RetinaFace-derived ONNX) → crop to face (+30% margin) → normalize
  → encode (tract-onnx or OpenVINO, 512-d embedding) → cosine similarity vs stored embeddings
```

`authenticate_scan` polls this in a loop until `scan_duration_ms` elapses, pacing by
`scan_interval_ms` (auto-detected from the camera's native V4L2 frame interval if unset — see
`config.rs`'s `scan_interval_ms()`). It additionally requires **motion-based liveness**: the
first face-bearing frame only seeds a baseline (never encoded/matched), and a match is only
accepted once measurable pixel motion (`preprocess::frame_motion_fraction` ≥
`liveness_motion_threshold`) has been observed between consecutive face frames — defeats a
rigidly-held static photo. Every entry/exit point calls `config.verify_pinned_camera()` first
(no-op unless `pin-camera.sh` has been run) and consults `lockout::check` before doing any
camera work.

### Config layering (`config.rs`)

`FaceAuthConfig::load()` merges, lowest to highest priority: struct defaults →
`/etc/face-auth.toml` → `~/.config/face-auth.toml` → `FACE_AUTH_*` env vars (via the `config`
crate). Every field is `Option<T>` with a `fn field_name(&self) -> T` accessor supplying the
default — always add new settings this way (optional field + accessor with fallback), not by
making the raw field required, so old config files without the new key keep working.

### Recognition-model identity (`storage.rs`, `config.rs::model_tag()`)

Two interchangeable recognition models are supported (`mbf` default / `r50` opt-in, selected at
deploy time via `FACE_AUTH_RECOGNITION_MODEL`, see README's model table). They produce
numerically incompatible 512-d embedding spaces, so mixing them silently would corrupt matching.
`EmbeddingStore` (v2 binary format) tags each saved embeddings file with `model_tag` (the
`model_path` basename); `FaceAuth::check_model_tag` refuses to authenticate or `--improve`
against a store tagged for a different model. Legacy v1 files (no tag) and a fresh
`EmbeddingStore::default()` are treated as "unknown" and always pass — mismatch can only be
raised once both sides are actually known (`model_tag_matches`). Keep this permissive-on-unknown
behavior if you touch this path; it's what keeps existing installs from breaking on upgrade.

### Lockout (`lockout.rs`)

Per-user exponential backoff state (`lockout.bin`, next to `embeddings.bin`) tracked across
separate PAM invocations (each `face-auth` run is a fresh process). Only throttles the *face*
factor — never blocks PAM's password fallback — and caps the actual sleep at `max_tarpit_ms`
regardless of the computed cooldown, so a long lockout window still can't stall the password
prompt. `authenticate_scan` only counts a scan toward failure if a face was actually detected
during it (`face_ever_detected`) — an unattended `sudo` invocation with nobody in front of the
camera isn't a failed *attempt*.

### On-disk formats

Both `embeddings.bin` and `lockout.bin` are little-endian binary, versioned, written via
temp-file + `fs::rename` (atomic replace) with `0o600` file / `0o700` directory permissions set
explicitly rather than trusted to umask. Follow this pattern (version header, atomic write,
explicit permissions) for any new per-user state file.

### Deploy/uninstall scripts

`deploy.sh` and `uninstall.sh` are the actual "integration test" surface for anything touching
config defaults, model paths, or PAM — they encode where files land and how PAM stanzas are
patched (see README's PAM Integration and Model sections for the specifics: insertion points
per service, `.face-auth.bak` backups, SHA-256 checksum verification, SELinux policy
compile/load). If you change a default path or add a new required file, update both scripts and
`config/face-auth.toml.example` together, not just the Rust defaults — the deploy script is
often the only thing that actually creates these files on a target system.

`pin-camera.sh` is a separate, opt-in hardening step (frame-injection defense via USB bus-path
pinning) — read the README's "Camera identity / frame-injection" subsection before touching
`config.rs::verify_pinned_camera` or `capture.rs`'s `device_bus_path`/`device_capture_index`.

### Platform constraint

V4L2 ioctl numbers/struct layouts in `capture.rs` are hardcoded for x86_64. Porting to
aarch64 needs the `v4l` crate instead, not a quick tweak.
