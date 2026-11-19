---
title: Home
layout: home
nav_order: 1
permalink: /
---

# vinoAuthFace

**Windows Hello-style face unlock for Linux using PAM and an IR camera.** It works on immutable
distros (Bazzite, Bluefin, Fedora Silverblue, Fedora Kinoite) without layering system packages or
running background daemons.

- Unlocks **`sudo`, polkit prompts, and GNOME, KDE, COSMIC, Sway, and Hyprland lock screens**
- Authenticates in about 2 seconds from camera poll to unlock
- Ships as a static musl binary with no daemon, systemd units, or D-Bus service
- Keeps all installed files in `/usr/local`, `/etc`, and `/var/lib`
- Supports an optional OpenVINO backend for Intel NPUs

> Face unlock is intended as a convenient alternative to typing your password, with your password
> remaining available as a fallback. Active near-IR cameras reject phone screens, though a printed
> IR photo moved by hand can still pass a default configuration. See
> [the security model](security.md) for details.

## Start here

You need a Windows Hello-style IR camera supported by `uvcvideo`; [check your hardware](hardware.md)
first. Then install the latest release:

```bash
curl -fsSL https://github.com/karanshukla/vinoAuthFace/releases/latest/download/install.sh | sudo bash
```

To build from a source checkout instead, clone the repository and run `sudo ./deploy.sh`:

```bash
git clone https://github.com/karanshukla/vinoAuthFace && cd vinoAuthFace
sudo ./deploy.sh
```

See [Installing](install.md) for what `deploy.sh` does, NPU setup, building from source, and
uninstalling. The [GitHub README](https://github.com/karanshukla/vinoAuthFace#readme) has the full
quick-start walkthrough.

vinoAuthFace is a fork of [Peter Falkingham's authFace](https://github.com/pfalkingham/authFace),
kept in sync with upstream. See [what this fork adds](architecture.md#relationship-to-upstream).
