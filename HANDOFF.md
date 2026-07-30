# Handoff: authFace on Fedora 44 / KDE Plasma (maki-wildcat)

Context for a future Claude session picking this up. Written 2026-07-29.

## Why this repo exists here

Was running `biopass` (github.com/TickLabVN/biopass) as a resident face-auth
daemon. Filed issues upstream (camera-exclusivity conflict, IR/RGB USB
bandwidth contention, OpenVINO NPU crashes) — maintainer wasn't going to fix
them and was rude about it. Fully uninstalled biopass from this system
(PAM/authselect, daemon, binaries, enrolled biometric DB — all gone). Source
kept at `~/Development/biopass` for reference only, not installed.

Plan: use `authFace` as the interim daily-driver face-unlock while eventually
building something that combines biopass's architecture ideas with the best
parts of a few other options. Candidates evaluated, from
https://github.com/stars/karanshukla/lists/face-auth-linux :

- **authFace** (this repo, Rust) — chosen for interim use. Explicit Fedora/
  immutable-distro support (Silverblue/Kinoite), installs to `/usr/local` +
  `~/.local` with no `/usr` layering. Early-stage (3 stars, single
  maintainer, ~20 commits). **No liveness/anti-spoof detection.**
- **LinuxCamPAM** (C++, most stars/forks, IR support) — ruled out as a base
  for *this* machine: Ubuntu/Debian-only (`.deb`, apt, PAM paths verified
  against Debian's layout). Would hit the same distro-path pain biopass did.
  Still worth mining for IR/camera-arbitration ideas for the combined build.
- **linux-hello** (Python, TPM, GUI) — runtime/language mismatch for a
  resident daemon; not pursued.
- **FaceAuth** (Shell) — too thin to build on.
- **ble-lock-session** — not face auth (Bluetooth proximity lock).
  Complementary multi-factor idea, not a base.

## This machine

- Fedora 44 (`fc44`), KDE Plasma (plasmalogin, ksmserver, kscreenlocker_greet
  6.7.3), hostname `maki-wildcat`, user `karanshukla`.
- Not an immutable variant — distrobox is **not** needed here; Rust was
  installed directly via rustup.
- Camera: single USB device `Integrated_Webcam_FHD` exposing 4 video nodes:
  - `/dev/video0` — RGB, MJPG/YUYV up to 1920x1080
  - `/dev/video1` — capture-flagged but no formats (control/metadata node)
  - `/dev/video2` — **the actual IR sensor**: `GREY` format, 360x360@15fps
  - `/dev/video3` — no formats (control/metadata node, not a capture device)
  - authFace's README/example config default to `/dev/video3` for IR — wrong
    on this hardware. Config now correctly set to `/dev/video2` (see below).

## Build (already done once, for reference)

Non-immutable Fedora path, no distrobox:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"
rustup target add x86_64-unknown-linux-musl
sudo dnf install -y musl-gcc gtk4-devel libadwaita-devel
cd ~/Development/authFace
cargo build --release --target x86_64-unknown-linux-musl -p face-auth -p face-enroll
cargo build --release -p face-auth-gtk
```

## Known upstream bugs / gaps found while deploying

1. **`deploy.sh` detector-model download is missing.** It auto-downloads and
   checksums the recognition model (`w600k_mbf.onnx`, from InsightFace's
   `buffalo_sc.zip` release) but has **no download path or checksum** for the
   detector model (`version-slim-320.onnx`) — it just errors if the file
   isn't already in `models/`, telling you to run `onnxsim` on a file you're
   never told how to get. README claims both models are auto-downloaded;
   that's only true for one.
   - **Workaround used:** pulled the pre-simplified detector straight from
     upstream (MIT-licensed, no checksum available anywhere — flagged as a
     real trust gap, accepted as low-risk since it's a well-known model):
     ```bash
     curl -L -o models/version-slim-320.onnx \
       https://raw.githubusercontent.com/Linzaer/Ultra-Light-Fast-Generic-Face-Detector-1MB/master/models/onnx/version-slim-320_simplified.onnx
     ```
     Drop that in `models/` *before* running `deploy.sh` and it picks it up.
   - Worth a PR upstream to fix `deploy.sh`'s detector-model handling.

2. **GUI camera picker (`face-auth-gtk`) misdetects the IR camera on this
   hardware.** `enumerate_cameras()` in
   `crates/face-auth-gtk/src/main.rs` (~line 421) filters
   `/sys/class/video4linux/*/name` for the substring `"ir"`/`"infrared"`.
   This webcam reports the same generic name (`Integrated_Webcam_FHD`) on
   every node, so the heuristic finds nothing and hardcodes a fallback to
   `/dev/video0` (line 439-441) — which is the RGB camera, not IR. This is
   **cosmetic only**: the actual `face-auth`/`face-enroll` binaries read
   `device` from `/etc/face-auth.toml`, not from this GUI heuristic, so
   real auth is unaffected as long as the config is set correctly (it is —
   see below). GUI dropdown/preview will just show the wrong camera. Also a
   candidate for an upstream PR (e.g. fall back to probing `GREY` format
   support via V4L2 ioctl instead of name-string matching).

## Config changes made on this machine

`/etc/face-auth.toml`:
```
device = "/dev/video2"
```
(uncommented and corrected from the `/dev/video3` example default.)

## PAM integration status

- **`sudo`** — patched automatically by `deploy.sh`:
  ```
  auth       sufficient  pam_exec.so /usr/local/bin/face-auth
  auth       include     system-auth
  ```
  `sufficient` + fallthrough to `system-auth` confirmed safe: any face-auth
  failure (no match/no camera/timeout) just falls through to password.

- **`swaylock` / `gdm-password`** — don't exist on this system (not
  GNOME/sway), `deploy.sh` silently skipped them. Irrelevant here.

- **KDE lock screen** — `deploy.sh`/`deploy-gui.sh` do **not** touch this at
  all; it was added manually this session and is **not tracked by the
  repo's install/uninstall scripts**:
  ```
  # /etc/pam.d/kde-fingerprint, line 1 (manually added, first line):
  auth        sufficient    pam_exec.so /usr/local/bin/face-auth
  auth        substack      fingerprint-auth   # pre-existing, unchanged
  ...
  ```
  Reasoning: Fedora's `kscreenlocker_greet` is patched to run
  `kde-fingerprint` as a *non-interactive* authenticator in parallel with the
  interactive `kde` (password) authenticator — confirmed via the PAM service
  files (`fingerprint-auth`'s own auth stack has no password fallback, ends
  in `pam_deny.so`; safety comes structurally from the parallel interactive
  slot, same as existing fingerprint unlock already relied on). Prepending
  face-auth to `kde-fingerprint` follows the same pattern `deploy.sh` uses
  for `sudo`. **User tested this live and confirmed it works, no lockout.**

  **Important for later:** if `uninstall.sh` is ever run, or if `authFace`
  is redeployed/reinstalled, this line in `/etc/pam.d/kde-fingerprint` will
  need to be **re-added manually** — it's outside the repo's automation.
  Same goes if `deploy.sh` is ever extended to handle KDE — check whether
  this line already exists before adding it again (avoid duplicate lines).

## Open threads / next steps

- No liveness/anti-spoof on authFace — acceptable for interim use, but a
  hard requirement for the eventual combined build (biopass had this).
- Consider upstreaming both bugs found above (`deploy.sh` detector-model
  gap, GUI camera-detection heuristic) to `pfalkingham/authFace`.
- Longer-term: scope the "combine biopass + authFace + LinuxCamPAM ideas"
  project — biopass's Rust daemon/PAM architecture, authFace's
  immutable-distro-friendly deploy model, LinuxCamPAM's IR/camera-arbitration
  handling and larger user base for reference. Not started yet.
