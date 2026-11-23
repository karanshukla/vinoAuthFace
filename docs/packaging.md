---
title: Packages (deb, rpm)
nav_order: 12
---

# Packages (deb, rpm)

`release.yml` builds `.deb` and `.rpm` packages on every tag and attaches them to the GitHub release, with
their checksums in `SHA256SUMS`. Install one with `sudo apt install ./vinoauthface_16.0.0_amd64.deb` or
`sudo dnf install ./vinoauthface-16.0.0-1.x86_64.rpm` (add the `-tray` and `-settings` files for the
tray and the settings window; the tray package needs the same version of the core one). There is no apt or
dnf repository yet, so updating means downloading the next release's files.

`packaging/build.sh` builds them with [nfpm](https://nfpm.goreleaser.com):

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

## Building and checking locally

`packaging/build.sh [VERSION]` needs `cargo`, the musl target, `musl-tools`, `cmake` and `nfpm` on `PATH`, and
writes `packaging/dist/`. `packaging/smoke-test.sh VERSION` is what the release job runs on the result; it
needs `dpkg-deb`, and `rpm` for the rpm checks. The release job installs nfpm pinned by version and SHA-256.
Dev builds (a run that is not a tag) are versioned `0~dev`.

The SELinux sources are copied with `/usr/local` rewritten to `/usr` like the scripts: the file-context line
labels the unseal helper by path, so a copy that still named `/usr/local/libexec` would never label it.
