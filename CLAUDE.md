# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`authFace`: a Rust PAM authentication module that unlocks Linux (sudo, lock screen, polkit)
via an IR camera, Windows Hello-style. Static musl binary, no daemon, no systemd, no D-Bus.
Designed to work on immutable distros (Bazzite, Bluefin, Silverblue, Kinoite) with zero system
packages beyond what's already there. User docs live in `docs/`; README.md is only the pitch,
quick start and an index. Don't duplicate them here, and when a change alters user-visible
behaviour, update the matching page.

## Build & test commands

```bash
# Add the target once
rustup target add x86_64-unknown-linux-musl

# Build the two deployable binaries (static musl, CPU/tract backend)
cargo build --release --locked --target x86_64-unknown-linux-musl -p face-auth -p face-enroll

# OpenVINO/NPU backend. Host (glibc) target, never musl: it links OpenVINO's
# .so files. Needs the OpenVINO runtime; deploy.sh provisions it with ovfetch.
cargo build --release --locked -p face-auth -p face-enroll \
  --features face-auth-core/npu,face-auth/npu,face-enroll/npu

# Run all tests (workspace-wide; face-auth-core holds nearly all of them)
cargo test --workspace --locked

# Run a single test
cargo test -p face-auth-core storage::tests::round_trips

# What CI gates on, besides tests
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo deny check
```

### CI and repo rules

- `ci.yml` (every PR, and pushes to main): `test` (unit tests + a from-source musl build of all
  six binaries), `clippy` (`-D warnings`), `deny` (`deny.toml`: crates.io only, license
  allow-list, advisories), and `deploy-script`, which runs a real `sudo ./deploy.sh` /
  `./uninstall.sh` cycle through every build path, including the checksum-verified download via
  a `file://` override (`FACE_AUTH_DEPLOY_RELEASE_BASE`, deploy.sh-only, not a config option).
- `release.yml` publishes static musl binaries as a GitHub pre-release on `v*` tags. Versions
  are consecutive (`v2`, `v3`), cut with `scripts/release.sh`; see `docs/releasing.md`.
- `ci.yml`'s `npu` job builds, lints and tests the `npu` feature against an OpenVINO that
  ovfetch provisions, then runs the ovfetch deploy path (compiling on OpenVINO's CPU plugin,
  since runners have no NPU). It isn't a required check. `release.yml` still ships musl only.
- `guard.yml` fails any non-owner PR touching security-relevant paths (see SECURITY.md). It runs
  from main's copy via `pull_request_target`, so changes to it only take effect once merged.
- The `main` ruleset requires a PR plus `test`, `clippy`, `deny`, `deploy-script` and `guard`.
  Dependabot PRs always trip `guard` by design and merge through the owner bypass.
- rustfmt is deliberately *not* enforced: upstream isn't fmt-clean, and reformatting its files
  would make every resync conflict. Match the surrounding style by hand.
- Every Actions `uses:` is pinned to a commit SHA with a version comment.
- A `deny.toml` advisory ignore needs a written reason next to it.
- tract 0.21 can't run SCRFD's `Resize` (sizes + empty scales) correctly; `detector.rs`'s
  `fix_empty_resize_scales` patches the graph. Re-check it if tract is ever moved.
- tract stays on 0.21 and openvino on 0.11 (Dependabot ignores their minor/major bumps): moving
  either is a deliberate port. tract 0.22+ changed the plan types `detector.rs`/`inference.rs`
  are built on.

To actually exercise a change end-to-end (not just unit tests), deploy and test on real
hardware: `sudo ./deploy.sh`, `sudo vinoauthface enroll --user $USER`, `sudo -k && sudo true`.
After that, `sudo ./update.sh` rebuilds and swaps only the binaries (same backend as installed),
which is the quick loop for code changes; anything touching models, config or PAM needs
`deploy.sh` again. Keep its install modes and paths in step with `deploy.sh`. Test a
stored face outside PAM with `sudo vinoauthface-auth --verify $USER` (add
`RUST_LOG=face_auth_core=debug` for scores). Without enrolling, `vinoauthface-camera-diag list` and
`cargo run --release --example bench` exercise capture, detection and encoding on the real
camera.

## Architecture

### Workspace layout

Six crates. All the logic lives in `face-auth-core`; the rest are thin CLI/PAM/debug/tray shims:

- **`crates/face-auth-core`**: The library. Camera I/O, detection, inference, preprocessing,
  storage, verification, config, lockout. Everything below refers to files here unless noted.
- **`crates/face-auth`** (`src/main.rs`): The PAM binary. Identity comes from `PAM_USER` only
  (resolved and validated through `user::lookup`, never `USER`/`LOGNAME`/`id -un`), refuses
  remote `PAM_RHOST` sessions, loads config via `FaceAuthConfig::load_for_auth`, then calls
  `authenticate_scan`. Exit 0 = matched, exit 1 = anything else (PAM's `sufficient` line falls
  through to password). `vinoauthface-auth --verify USER` (root only) runs the same scan outside PAM;
  `vinoauthface-auth --enrolled` (tray status) answers for the caller's own user ID only.
- **`crates/face-enroll`** (`src/main.rs`): The enrollment CLI (`clap`-based).
- **`crates/face-similarity-check`** (`src/main.rs`): Offline debug tool, not deployed by
  `deploy.sh`. Runs the same CLAHE → detect → crop → encode → cosine-similarity pipeline as a
  live auth attempt, but fed from image files (`image::open`, upscaled the same `*257` way
  `capture.rs` upscales raw camera bytes) instead of the IR camera, for gauging false-accept
  risk against photos of other people without needing a second person at the camera. Everything
  runs locally against the on-disk model/embeddings; only the printed similarity score is
  produced, nothing is transmitted anywhere. Templates are root-owned, so it needs sudo.
- **`crates/face-camera-diag`** (`src/main.rs`): Offline camera discovery/diagnostic tool, also
  not deployed by `deploy.sh`. `list` enumerates every `/dev/video*` node with driver/card name
  (`VIDIOC_QUERYCAP`), resolved USB VID:PID (walks up sysfs from `capture::device_bus_path`), and
  current pixel format/resolution (`VIDIOC_G_FMT` via `capture::query_format`), for figuring out
  which node is the IR sensor and what format it reports without reading through the
  docs. `dump` captures one frame from a given device and writes it as a 16-bit PGM for visual
  inspection. Purely read-only against devices it's just listing; `dump` takes the target device
  the same way live vinoauthface-auth would.
- **`crates/face-auth-tray`**: The tray icon (installed by default, `deploy.sh --no-tray` skips it, see `docs/tray.md`),
  which replaces upstream's GTK GUI. Two binaries: `vinoauthface-tray` (per-user, `ksni`
  StatusNotifierItem, never in the auth path; spots scans by `vinoauthface-auth` in `/proc`, reads
  enrolment via `vinoauthface-auth --enrolled`) and `vinoauthface-helper`, the only thing its polkit policy
  lets pkexec run. The helper takes one verb (`enrol|retrain|uninstall`), no flags, and the target
  user from `PKEXEC_UID` only; keep it that way, and keep ksni/zbus out of it. `data/` holds the
  policy, desktop files and the generated icon (`FACE_AUTH_BLESS_ICONS=1 cargo test -p
  face-auth-tray` after changing `icon.rs`).

The `npu` Cargo feature (on `face-auth-core`, propagated through the other five crates) swaps
the inference backend from pure-Rust `tract-onnx` (CPU) to `openvino` (NPU/GPU/CPU via OpenVINO
runtime); see `#[cfg(feature = "npu")]` in `inference.rs` and `detector.rs`. Backend selection
at runtime is `config.backend()` (`"tract"` default or `"openvino"`) plus `config.npu_device()`
(`"NPU"`/`"GPU"`/`"CPU"`), independent of which one was compiled in.

### Auth/enroll pipeline (`lib.rs`)

`FaceAuth` owns a `FaceDetector` + `FaceEncoder` (both loaded once at construction, from
`config.model_path()`/`config.detector_model_path()`). `authenticate_scan` (the PAM path and
`vinoauthface-auth --verify`), `authenticate_once` (single-shot, no in-tree caller) and `enroll` /
`enroll_append` (`vinoauthface enroll`) all run the same per-frame pipeline:

```
capture (V4L2, GREY/YUYV/Y16, brighter of a frame pair) → assess_frame (mean + variance gates)
  → CLAHE (preprocess::histogram_equalize) → detect (SCRFD det_500m: box + 5 landmarks, `scrfd.rs`)
  → align to the ArcFace template (`face_input`; box crop +30% if the legacy slim detector has no
    landmarks) → normalize → encode (tract or OpenVINO, 512-d embedding)
  → cosine similarity vs stored embeddings
```

`authenticate_scan` polls this in a loop until `scan_duration_ms` elapses (plus, once,
`liveness_grace_ms` if a frame matched but was held back for liveness). The loop is paced by
the camera itself; `scan_interval_ms` is an extra delay that defaults to 0. It additionally
requires **motion-based liveness**: the first face-bearing frame only seeds a baseline (never
encoded/matched), and a match is only accepted once one face frame shows both total motion
(`preprocess::motion_profile`'s `total` ≥ `liveness_motion_threshold`, against the oldest face
frame within `liveness_window_ms`, so a still face's slow drift counts) and motion a rigid shift
can't explain in the most-changed 8x8 block against the previous face frame (`local` ≥
`liveness_residual_motion_threshold`; the shift is refined to a quarter pixel). Both
are measured on a normalised patch of the face, cut from the pre-CLAHE frame with the earlier
frame's box, so background motion and exposure changes don't count. `total` defeats a static
photo; `local` a photo moved by hand (default 0 = off: a still face may not blink within one
scan). `examples/motion-profile.rs` prints all three for a clip (optional `GAP` mimics the window).

Every entry point calls `config.verify_pinned_camera()` first (no-op unless `pin-camera.sh` has
been run). The two authenticate paths also consult `lockout::check` before any camera work;
enrolment does not.

### Config layering (`config.rs`)

Two loaders, deliberately different:

- `FaceAuthConfig::load()` (vinoauthface enroll and the offline tools) merges struct defaults →
  `/etc/face-auth.toml` → `~/.config/face-auth.toml` → `FACE_AUTH_*` env vars.
- `FaceAuthConfig::load_for_auth(user)` (the PAM path) reads `/etc/face-auth.toml` only, ignores
  the environment, and applies the target user's `~/.config/face-auth.toml` through
  `apply_user_overlay`, a whitelist that may only *tighten* thresholds
  (`threshold`, `detector_threshold`, `liveness_*_threshold`), adjust timing within bounds,
  or pick a validated IR `device`. Anything else (model paths, `embeddings_dir`, camera pin,
  lockout, backend) is system policy. **A new setting is system-only unless you deliberately add
  it to the overlay**, and only if a user choosing it can never weaken authentication; add a
  test next to `user_overlay_cannot_redirect_lookups` when you do.

Every field is `Option<T>` with a `fn field_name(&self) -> T` accessor supplying the
default. Always add new settings this way (optional field + accessor with fallback), not by
making the raw field required, so old config files without the new key keep working.

### Recognition-model identity (`storage.rs`, `config.rs::model_tag()`)

Two interchangeable recognition models are supported (`r50` default for NPU builds, `mbf` for CPU
builds; override at deploy time via `FACE_AUTH_RECOGNITION_MODEL`, see `docs/configuration.md`'s model table). They produce
numerically incompatible 512-d embedding spaces, so mixing them silently would corrupt matching.
`EmbeddingStore` (v2 binary format) tags each saved embeddings file with `model_tag` (the
`model_path` basename, plus `+<detector basename>` for any detector but the legacy
`version-slim-320.onnx`, since aligned embeddings differ); `FaceAuth::check_model_tag` refuses to authenticate or `--improve`
against a store tagged for a different model. Legacy v1 files (no tag) and a fresh
`EmbeddingStore::default()` are treated as "unknown" and always pass: a mismatch can only be
raised once both sides are actually known (`model_tag_matches`). Keep this permissive-on-unknown
behavior if you touch this path; it's what keeps existing installs from breaking on upgrade.

### Lockout (`lockout.rs`)

Per-user exponential backoff state (`<user>/lockout/state.bin`, the one group-writable
directory in the store, so the set-group-ID `vinoauthface-auth` can update it from a lock screen running
as the user, while the user themselves cannot reach it to reset the count) tracked across
separate PAM invocations (each `vinoauthface-auth` run is a fresh process). Only throttles the *face*
factor (never PAM's password fallback) and caps the actual sleep at `max_tarpit_ms`
regardless of the computed cooldown, so a long lockout window still can't stall the password
prompt. `authenticate_scan` only counts a scan toward failure if a face was actually detected
during it (`face_seen`): an unattended `sudo` invocation with nobody in front of the camera
isn't a failed *attempt*.

### On-disk formats

Both `embeddings.bin` and `lockout/state.bin` are little-endian binary, versioned, written via
temp-file + fsync + `fs::rename` + directory fsync, with modes set explicitly rather than trusted
to umask (`storage.rs`: templates `0o640` in `0o2750` directories, lockout `0o660` in `0o2770`;
the set-group-ID bit on directories makes entries inherit the `face-auth` group). New state the
lock-screen path must write goes under `lockout/`; anything else stays group read-only. Paths
are built by `storage::user_store_dir` (which validates the username). Loads bound every length
read from disk before allocating (`MAX_EMBEDDINGS`, `MAX_MODEL_TAG_LEN`) and reject trailing bytes and
non-finite values. Follow this pattern for any new per-user state file; `cameras.rs` (the
text `<user>/cameras` list of enrolled USB IDs) is the small example.

`/var/lib/face-auth` itself is root:root `0700`. Never make it user-writable: whatever can write
a template chooses whose face unlocks the account (upstream's privesc fix, issue #25).

### Deploy/uninstall scripts

`deploy.sh` and `uninstall.sh` are the integration-test surface for anything touching config
defaults, model paths, or PAM: they encode where files land and how PAM stanzas are patched
(see `docs/pam.md` and `docs/install.md` for the specifics: insertion points per
service, `.face-auth.bak` backups, SHA-256 checksum verification, SELinux policy compile/load).
CI (`ci.yml`'s `deploy-script` job) actually runs both scripts on the runner, not just lints
them, so a real regression here fails the build. If you change a default path or add a new
required file, update both scripts and `config/face-auth.toml.example` together, not just the
Rust defaults. The deploy script is often the only thing that actually creates these files on a
target system.

`pin-camera.sh` is a separate, opt-in hardening step (frame-injection defense via USB bus-path
pinning). Read `docs/security.md`'s "Frame injection (a fake camera)" section before touching
`config.rs::verify_pinned_camera` or `capture.rs`'s `device_bus_path`/`device_capture_index`.

### Platform constraint

V4L2 ioctl numbers/struct layouts in `capture.rs` are hardcoded for x86_64. Porting to
aarch64 needs the `v4l` crate instead, not a quick tweak.

### Upstream

This is a fork of `pfalkingham/authFace` (git remote `upstream`), resynced by replaying fork
features onto `upstream/main` rather than merging (the histories had diverged too far). To keep
future syncs cheap, prefer extending upstream's structure over reshaping it, and keep fork-only
behaviour in clearly separate functions/files (`lockout.rs`, `pin-camera.sh`, the `npu` cfg
blocks). The GTK GUI and GNOME scan-indicator extension are intentionally dropped here (the tray replaces
them); skip them when pulling upstream changes.
