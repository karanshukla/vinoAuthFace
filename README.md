<p align="center"><img src="docs/assets/banner.png" alt="vinoAuthFace: IR camera face unlock for Linux" width="100%"></p>

# vinoAuthFace: IR Camera Face Unlock for Linux

**Windows Hello-style face unlock for Linux, through PAM and an IR camera.** Works on immutable
distros (Bazzite, Bluefin, Fedora Silverblue, Fedora Kinoite) with no system packages, daemons or
layering.

- Face unlock for **sudo, polkit prompts, and the GNOME, KDE and Sway lock screens**
- About 2 seconds from camera poll to authenticated
- A static musl binary: no daemon, no systemd units, no D-Bus
- Everything lives in `/usr/local`, `/etc` and `/var/lib`
- Optional OpenVINO backend for Intel NPUs

A fork of [Peter Falkingham's authFace](https://github.com/pfalkingham/authFace), kept in sync with
upstream. It adds motion liveness, a failed-match lockout, camera pinning, an NPU backend and more:
see [what this fork adds](docs/architecture.md#relationship-to-upstream).

> Face unlock here is a convenience over a password you still have, not a stronger factor. Phone
> screens don't fool the IR sensor (which is why there is no RGB path), but a printed IR photo
> moved by hand can, and that's an accepted risk. See [docs/security.md](docs/security.md).

## Why this exists

Built for immutable distros and laptops with an Intel NPU. The other Linux face-auth projects are
a daemon plus D-Bus or a Python runtime, installed through the distro package manager, which on
Bazzite or Silverblue means layering and a reboot. This is one static binary in `/usr/local`.

IR only, on purpose: there is no RGB path and no fallback to a normal webcam. The longer version,
and what to use if you have no IR camera, is in
[docs/architecture.md](docs/architecture.md#why-it-is-built-this-way).

## Quick start

You need an IR camera on `uvcvideo`. Not sure yours works? See
[docs/hardware.md](docs/hardware.md).

```bash
# 1. Install the latest release: checksum-verified binaries, no build.
curl -fsSL https://github.com/karanshukla/vinoAuthFace/releases/latest/download/install.sh | sudo bash

# 2. Enrol your face
sudo vinoauthface enroll --user $USER

# 3. Test
sudo -k && sudo true

# 4. Recommended: pin the camera (vinoauthface enroll prints the exact command)
sudo vinoauthface-pin-camera /dev/videoN
```

Options go after `bash -s --`, e.g. `| sudo bash -s -- --with-settings` for the settings window.
The NPU (OpenVINO) backend still builds from source: clone the repo and run `sudo ./deploy.sh`.
See [docs/install.md](docs/install.md).

A face match unlocks immediately. Anything else (no match, no camera, lockout) falls through to
your normal password prompt, so a broken camera can't lock you out.

## What gets face unlock

| Where | PAM service |
|---|---|
| `sudo` | `sudo` |
| GNOME lock screen | `gdm-password` |
| KDE lock screen (background, next to the password field) | `kde-fingerprint` |
| KDE login screen (opt-in: [off, password + face, or face](docs/pam.md#login-screen)) | `plasmalogin-fingerprint`, or `plasmalogin` without it, or `sddm` |
| COSMIC lock screen and greeter | `cosmic-greeter` |
| swaylock | `swaylock` |
| hyprlock (Hyprland) | `hyprlock` |
| polkit prompts (`pkexec`, settings, Bitwarden unlock) | `polkit-1` |

Details and insertion points: [docs/pam.md](docs/pam.md).

## Uninstall

```bash
sudo vinoauthface-uninstall           # everything except your face templates
sudo vinoauthface-uninstall --purge   # templates too
```

## Documentation

| Page | Covers |
|---|---|
| [Installing](docs/install.md) | `deploy.sh`, building from source, the NPU backend, SELinux |
| [Configuration and enrolment](docs/configuration.md) | Config files, user overrides, enrolment, mbf vs r50 |
| [PAM integration](docs/pam.md) | Services, KDE, polkit, Bitwarden, keyrings |
| [Hardware and diagnostics](docs/hardware.md) | Supported cameras, `vinoauthface-camera-diag` |
| [Troubleshooting](docs/troubleshooting.md) | Debug output, common errors |
| [Tray icon](docs/tray.md) | Status, enrol, retrain, test scan (`--no-tray` skips it) |
| [Settings window](docs/settings.md) | Every config key in one window, model clean-up |
| [Security model](docs/security.md) | Trust model, spoofing, frame injection, limits |
| [Architecture](docs/architecture.md) | Pipeline, models and licensing, layout, upstream |
| [Releasing](docs/releasing.md) | Version tags and `scripts/release.sh` |

Changes: [CHANGELOG.md](CHANGELOG.md). Reporting a vulnerability: [SECURITY.md](SECURITY.md).

## License

MIT, for this code. The models it downloads (`det_500m.onnx`, `w600k_mbf.onnx`, `w600k_r50.onnx`)
are InsightFace weights, licensed for **non-commercial research use only**, so personal research
use is all this software is good for as shipped. A fork meant for commercial use MUST replace the
models. See [docs/architecture.md](docs/architecture.md#models).
