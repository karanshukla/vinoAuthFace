<p align="center"><img src="docs/assets/banner.png" alt="vinoAuthFace: IR camera face unlock for Linux" width="100%"></p>

# vinoAuthFace: IR Camera Face Unlock for Linux

**Windows Hello-style face unlock for Linux using PAM and an IR camera.** It works on immutable
distros (Bazzite, Bluefin, Fedora Silverblue, Fedora Kinoite) without layering system packages or
running background daemons.

- Unlocks **`sudo`, polkit prompts, and GNOME, KDE, COSMIC, Sway, and Hyprland lock screens**
- Authenticates in about 2 seconds from camera poll to unlock
- Ships as a static musl binary with no daemon, systemd units, or D-Bus service
- Keeps all installed files in `/usr/local`, `/etc`, and `/var/lib`
- Supports an optional OpenVINO backend for Intel NPUs

vinoAuthFace is a fork of [Peter Falkingham's authFace](https://github.com/pfalkingham/authFace),
kept in sync with upstream. It adds motion liveness, failed-match lockout, camera pinning, an NPU
backend, and more; see [what this fork adds](docs/architecture.md#relationship-to-upstream).

> Face unlock is intended as a convenient alternative to typing your password, with your password
> remaining available as a fallback. Active near-IR cameras reject phone screens (which is why RGB
> webcams are unsupported), though a printed IR photo moved by hand can still pass a default
> configuration. See [docs/security.md](docs/security.md) for the full threat model.

## Why this exists

vinoAuthFace is designed for immutable Linux desktops and laptops with an Intel NPU. Most other
Linux face-authentication tools require a background daemon over D-Bus or a Python runtime installed
through the system package manager, which forces package layering and reboots on Bazzite or
Silverblue. vinoAuthFace installs as a standalone binary in `/usr/local`.

The camera pipeline is intentionally IR-only, with no RGB fallback. For the reasoning behind this
design and alternatives if your machine lacks an IR camera, see
[docs/architecture.md](docs/architecture.md#why-it-is-built-this-way).

## Quick start

You need a Windows Hello-style IR camera supported by `uvcvideo`. If you are unsure whether your
camera is compatible, check [docs/hardware.md](docs/hardware.md) first.

```bash
# 1. Install the latest release (checksum-verified binaries, no compilation required)
curl -fsSL https://github.com/karanshukla/vinoAuthFace/releases/latest/download/install.sh | sudo bash

# 2. Enrol your face
sudo vinoauthface enroll --user $USER

# 3. Test authentication
sudo -k && sudo true

# 4. Recommended: pin the camera (vinoauthface enroll prints the exact command)
sudo vinoauthface-pin-camera /dev/videoN
```

Pass installer flags after `bash -s --`, for example `| sudo bash -s -- --with-settings` to include
the settings window. If [ovfetch](https://github.com/karanshukla/ovfetch) or OpenVINO is already
installed, the installer selects the prebuilt NPU (OpenVINO) binaries automatically; to build from
source instead, clone the repository and run `sudo ./deploy.sh`. See
[docs/install.md](docs/install.md) for all installation options.

When your face matches, authentication succeeds immediately. If there is no match, the camera is
unavailable, or the lockout is active, PAM falls through to your normal password prompt so a broken
camera cannot lock you out.

## What gets face unlock

| Where | PAM service |
|---|---|
| `sudo` | `sudo` |
| GNOME lock screen | `gdm-password` |
| KDE lock screen (runs in the background alongside the password field) | `kde-fingerprint` |
| KDE login screen (opt-in: [off, password + face, or face](docs/pam.md#login-screen)) | `plasmalogin-fingerprint`, `plasmalogin`, or `sddm` |
| COSMIC lock screen and greeter | `cosmic-greeter` |
| swaylock | `swaylock` |
| hyprlock (Hyprland) | `hyprlock` |
| polkit prompts (`pkexec`, system settings, Bitwarden unlock) | `polkit-1` |

See [docs/pam.md](docs/pam.md) for PAM insertion details and desktop-specific behaviour.

## Uninstall

```bash
sudo vinoauthface-uninstall           # removes everything except your face templates
sudo vinoauthface-uninstall --purge   # removes face templates and the face-auth group as well
```

## Documentation

| Page | Covers |
|---|---|
| [Installing](docs/install.md) | One-line installer, `deploy.sh`, building from source, the NPU backend, and SELinux |
| [Configuration and enrolment](docs/configuration.md) | Config files, user overrides, enrolment commands, and `mbf` vs `r50` models |
| [PAM integration](docs/pam.md) | PAM services, KDE lock/login screens, polkit, Bitwarden, and keyrings |
| [Hardware and diagnostics](docs/hardware.md) | Supported camera stacks and `vinoauthface-camera-diag` |
| [Troubleshooting](docs/troubleshooting.md) | Diagnostics, debug logs, and common errors |
| [Tray icon](docs/tray.md) | Status indicator, enrolment, retraining, and test scans (`--no-tray` skips it) |
| [Settings window](docs/settings.md) | GUI for system and user config keys, guided enrolment, and model cleanup |
| [Security model](docs/security.md) | Trust model, TPM sealing, spoofing, frame injection, and limitations |
| [Architecture](docs/architecture.md) | Pipeline overview, models and licensing, repository layout, and upstream differences |
| [Releasing](docs/releasing.md) | Version tags and `scripts/release.sh` |
| [Comment style](docs/comment-style.md) | Guidelines for writing code comments |

See [CHANGELOG.md](CHANGELOG.md) for release history and [SECURITY.md](SECURITY.md) for reporting
vulnerabilities.

## License

The source code in this repository is licensed under the MIT License.

The ONNX models downloaded during installation (`det_500m.onnx`, `w600k_mbf.onnx`, and
`w600k_r50.onnx`) are InsightFace weights licensed for **non-commercial research use only**. As
shipped, this software is therefore limited to personal, non-commercial research use. Any fork
intended for commercial use must replace these models. See
[docs/architecture.md](docs/architecture.md#models) for details.
