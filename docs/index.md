---
title: Home
layout: home
nav_order: 1
permalink: /
---

# vinoAuthFace

**Windows Hello-style face unlock for Linux, through PAM and an IR camera.** Works on immutable
distros (Bazzite, Bluefin, Fedora Silverblue, Fedora Kinoite) with no system packages, daemons or
layering.

- Face unlock for **sudo, polkit prompts, and the GNOME, KDE and Sway lock screens**
- About 2 seconds from camera poll to authenticated
- A static musl binary: no daemon, no systemd units, no D-Bus
- Everything lives in `/usr/local`, `/etc` and `/var/lib`
- Optional OpenVINO backend for Intel NPUs

> Face unlock here is a convenience over a password you still have, not a stronger factor. Phone
> screens don't fool the IR sensor, but a printed IR photo moved by hand can, and that's an
> accepted risk. See [the security model](security.md).

## Start here

You need an IR camera on `uvcvideo`; [check yours](hardware.md) first. Then:

```bash
git clone https://github.com/karanshukla/vinoAuthFace && cd vinoAuthFace
sudo ./deploy.sh
```

[Installing](install.md) covers what `deploy.sh` does, building from source and removal.
The [GitHub README](https://github.com/karanshukla/vinoAuthFace#readme) has the full quick start.

A fork of [Peter Falkingham's authFace](https://github.com/pfalkingham/authFace), kept in sync with
upstream. See [what this fork adds](architecture.md#relationship-to-upstream).
