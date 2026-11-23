---
title: Packages (deb, rpm)
nav_order: 12
---

# Packages (deb, rpm)

`packaging/build.sh` builds `.deb` and `.rpm` packages with [nfpm](https://nfpm.goreleaser.com):

| Package | Contents |
|---|---|
| `vinoauthface` | the set-group-ID PAM binary, `vinoauthface`, `vinoauthface-setup`, `vinoauthface-fetch-models`, `/etc/face-auth.toml`, the template store |
| `vinoauthface-tray` | the tray, its pkexec helper, the polkit policy, desktop files, icons |
| `vinoauthface-settings` | the optional settings window |

Packages are the CPU (tract) build. The NPU build and its OpenVINO runtime stay with `deploy.sh` and `ovfetch`.

## Install prefix

`deploy.sh` installs under `/usr/local`; a package must use `/usr`. The binaries name each other, the
models and the polkit-pinned helper by absolute path, so the prefix is fixed at compile time:
`VINOAUTHFACE_PREFIX` (default `/usr/local`) and `VINOAUTHFACE_LIBEXECDIR` (default `$PREFIX/libexec`),
read by `crates/face-auth-core/build.rs` into `face_auth_core::paths`. `build.sh` builds with `/usr` and copies
the scripts, policy and desktop files with `/usr/local` rewritten to `/usr`. Add a new installed path to
`build.rs` rather than writing it as a literal.

## What installing does

Only adds files, and prints what is left. It does not edit `/etc/pam.d` or download anything: a maintainer
script runs as root with no user session, so it can neither show a GUI nor ask. To switch it on:
`sudo vinoauthface-fetch-models` (checked against `models.sha256`, like `deploy.sh`),
`sudo vinoauthface enroll --user $USER`, `sudo vinoauthface-setup`.

For headless installs and testing, `touch /etc/vinoauthface-auto-setup` (or `VINOAUTHFACE_AUTO_SETUP=1`)
before installing makes the post-install script fetch the models and run `vinoauthface-setup`; a failure
never fails the install. Removing the package strips the PAM lines it added; upgrading leaves them.

`vinoauthface-upgrade` in a package only says to use the package manager; `installed-from` reads `package`.
