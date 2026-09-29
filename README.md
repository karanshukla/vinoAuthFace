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
> screens don't fool the IR sensor (which is why there is no RGB path), but printed photos are an
> open risk. See [docs/security.md](docs/security.md).

## Why this exists

**Built for immutable distros.** One static musl binary, no daemon, no systemd unit, no D-Bus, no
system packages. The other Linux face-auth projects are a daemon plus D-Bus
([gaze](https://github.com/gundulabs/gaze), Visage, Biopass) or a Python runtime (Howdy), and all
of them install through the distro package manager. On Bazzite, Bluefin, Silverblue or Kinoite
that means layering packages and rebooting, or not installing at all. gaze's installer
[refuses to run](https://github.com/gundulabs/gaze/blob/main/docs/public/install.sh) on rpm-ostree
systems.

**IR only, on purpose.** There is no RGB path, no hybrid mode, no fallback to a normal webcam.
The screen-spoof defense is a property of active near-infrared sensing, and it stops being true
the moment an RGB path exists. Narrower scope, stronger guarantee.

If you don't have an IR camera, or you want a GUI, an enrolment wizard or GNOME lock screen
integration, use gaze instead.

## Quick start

You need an IR camera on `uvcvideo`. Not sure yours works? See
[docs/hardware.md](docs/hardware.md).

```bash
# 1. Install. Builds from source if it finds cargo, otherwise downloads
#    checksum-verified release binaries.
sudo ./deploy.sh        # ./install.sh does the same

# 2. Enrol your face
sudo vinoauthface enroll --user $USER

# 3. Test
sudo -k && sudo true

# 4. Recommended: pin the camera (vinoauthface enroll prints the exact command)
sudo ./pin-camera.sh /dev/videoN
```

A face match unlocks immediately. Anything else (no match, no camera, lockout) falls through to
your normal password prompt, so a broken camera can't lock you out.

## What gets face unlock

| Where | PAM service |
|---|---|
| `sudo` | `sudo` |
| GNOME lock screen | `gdm-password` |
| KDE lock screen (hands-free, next to the password field) | `kde-fingerprint` |
| Plasma login screen | `plasmalogin-fingerprint` |
| COSMIC lock screen and greeter | `cosmic-greeter` |
| swaylock | `swaylock` |
| polkit prompts (`pkexec`, settings, Bitwarden unlock) | `polkit-1` |

Details and insertion points: [docs/pam.md](docs/pam.md).

## Uninstall

```bash
sudo ./uninstall.sh           # everything except your face templates
sudo ./uninstall.sh --purge   # templates too
```

## Documentation

| Page | Covers |
|---|---|
| [Installing](docs/install.md) | What `deploy.sh` does, building from source, NPU backend, distrobox, camera pinning, SELinux |
| [Configuration and enrolment](docs/configuration.md) | Config files and what users may override, enrolment, mbf vs r50 models |
| [PAM integration](docs/pam.md) | Services, KDE, polkit, Bitwarden, keyrings |
| [Hardware and diagnostics](docs/hardware.md) | Supported cameras, `vinoauthface-camera-diag`, hardware reports |
| [Troubleshooting](docs/troubleshooting.md) | Debug output, common errors |
| [Tray icon](docs/tray.md) | Optional tray: status, enrol, retrain, test scan, uninstall (installed by default, `--no-tray` to skip) |
| [Security model](docs/security.md) | Trust model, spoofing, frame injection, limitations |
| [Architecture](docs/architecture.md) | Pipeline, models and licensing, project layout, upstream relationship |

Changes: [CHANGELOG.md](CHANGELOG.md). Reporting a vulnerability: [SECURITY.md](SECURITY.md).

## License

MIT, for this code. The face detector `version-slim-320.onnx` is MIT. The recognition models
(`w600k_mbf.onnx`, `w600k_r50.onnx`) are InsightFace model-zoo weights, licensed for
non-commercial research use only; see [docs/architecture.md](docs/architecture.md#models).
