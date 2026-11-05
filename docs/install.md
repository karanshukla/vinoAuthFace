---
title: Installing
nav_order: 2
---

# Installing

Two ways in, both ending in the same `deploy.sh`:

```bash
# A release: checksum-verified binaries, nothing to build
curl -fsSL https://github.com/karanshukla/vinoAuthFace/releases/latest/download/install.sh | sudo bash

# A checkout: builds from source
sudo ./deploy.sh
```

This page covers what they do, building from source, camera pinning, and removal.

## The one-line installer

`install.sh` asks GitHub for the newest release, downloads its source bundle
(`vinoauthface-source.tar.gz`) and `SHA256SUMS`, refuses the bundle unless it's listed there and
matches, and runs that release's own `deploy.sh --prebuilt` from a private temporary directory.
`--prebuilt` installs the release's binaries (checked against the same `SHA256SUMS`) even if Rust
is installed, so nothing compiles. With [ovfetch](https://github.com/karanshukla/ovfetch) on the
`PATH`, or OpenVINO installed another way, that's the [NPU build](#openvino--npu-backend) and the
OpenVINO runtime ovfetch picks for this machine; otherwise the static CPU build. Options go after
`bash -s --`:

| Option | |
|---|---|
| `vN` | That release instead of the newest |
| `--with-settings` | The [settings window](settings.md), the release's one glibc binary |
| `--no-tray` | No [tray](tray.md) |
| `--login=off\|both\|face` | Face unlock at the KDE [login screen](pam.md#login-screen) |

Running it again reinstalls, keeping your config, templates and PAM setup. The checksums come from
the same release as the files, so they catch a corrupted or truncated download, not a compromised
GitHub account.

Every install also leaves these commands, so the repo isn't needed afterwards:

| Command | |
|---|---|
| `vinoauthface-upgrade` | [Updating](#updating) |
| `vinoauthface-setup` | Re-applies PAM, the login screen, Bitwarden and SELinux (see below) |
| `vinoauthface-pin-camera` | [Pinning the camera](#pinning-the-camera-recommended) |
| `vinoauthface-uninstall` | [Uninstalling](#uninstalling) |

## Requirements

- An IR camera on `uvcvideo` (Windows Hello style). See [hardware.md](hardware.md).
- PAM with `pam_exec.so`, standard everywhere.
- `curl`, and `unzip` for the model download. `deploy.sh` checks for both before it changes
  anything, and prints the install command for your distro if one is missing.
- On SELinux systems, `policycoreutils` and `checkpolicy` to compile the login-screen policy.
  Installed by default on Fedora.

## Distros

| Family | Tested | PAM | MAC | Notes |
|---|---|---|---|---|
| Fedora, Fedora Atomic (Silverblue, Kinoite, Bazzite) | Fedora KDE, Kinoite | `/etc/pam.d`, authselect | SELinux, policy loaded | The primary target. `/usr/local` is `/var/usrlocal` on Atomic, so the install survives updates |
| Debian, Ubuntu, Mint | Ubuntu 24.04 (VM, no IR camera) | `/etc/pam.d`, `@include common-auth` | AppArmor, nothing needed | |
| Arch, CachyOS, EndeavourOS | No | `/etc/pam.d`, `include system-auth` | None | |
| openSUSE Tumbleweed, Leap 16, Aeon, MicroOS | No | Vendor files in `/usr/lib/pam.d`, `include common-auth` | SELinux on new installs | `deploy.sh` loads the Fedora-derived policy; if a type is missing it warns and carries on, and only the login screens lose the camera |

"Tested" means someone has run `deploy.sh` there and unlocked with it. The rest should work, since
`deploy.sh` decides on what's on disk (PAM files, SELinux state, a writable `/usr`) rather than on
the distro name; reports either way are welcome.

No build toolchain is required. `install.sh` never builds, and if `deploy.sh` finds no Rust
toolchain and no local build, it downloads the static musl binaries from
[GitHub Releases](https://github.com/karanshukla/vinoAuthFace/releases) too, verified against the
release's `SHA256SUMS`, the NPU build if OpenVINO is installed. Build from source for an unreleased
change.

## What deploy.sh does

It installs the files, then runs `setup.sh` for everything that hooks them into the system. That
second half is also installed as `vinoauthface-setup`: run `sudo vinoauthface-setup` after a
distro or authselect update resets `/etc/pam.d`, or after installing a lock screen that wasn't
there before. It takes `--login=off|both|face` too, and refuses to run with nothing installed.

| Step | What | Details |
|------|------|---------|
| Build | Picks the first that applies | `--prebuilt`: download the release binaries (the NPU build if OpenVINO is installed). OpenVINO + cargo: NPU build. OpenVINO, no cargo: download the NPU build. Prebuilt binaries in `target/`: use them, unless cargo is available and the sources are newer than they are (`FACE_AUTH_FORCE_BUILD=1` rebuilds regardless; it covers the tray too). Cargo: static musl build. Otherwise: download and checksum-verify the release binaries |
| Binaries | Installs to `/usr/local/bin` | `vinoauthface-auth` (set-group-ID `face-auth`, see [security.md](security.md#trust-model)) and `vinoauthface`, plus its bash, zsh and fish completions under `/usr/local/share` (regenerate with `vinoauthface completions bash`, `zsh` or `fish`) |
| Models | Downloads and SHA-256 verifies | Recognition model (`w600k_r50.onnx` for an NPU build, `w600k_mbf.onnx` otherwise) and the `det_500m.onnx` (SCRFD) detector, to `/usr/local/share/face-auth/`. Installing the detector over an older install means re-enrolling; `deploy.sh` says so. A copy in `models/` is used first, and verified too |
| Config | Installs default config | `/etc/face-auth.toml`, kept if it already exists; the deploy appends any settings from `config/face-auth.toml.example` it lacks, commented out at their defaults |
| Tray | Unless `--no-tray` | Tray binary, root helper, polkit actions, autostart and launchers. See [tray.md](tray.md). Every deploy also installs the commands listed [above](#the-one-line-installer) |
| NPU cache | Empties and refills it | `/var/cache/face-auth`, root-owned. Emptied because a new model or driver leaves stale entries, then refilled with `vinoauthface-auth --warm-cache` (NPU builds only) |
| Storage | Secures the template store | `/var/lib/face-auth`, `root:face-auth` `2750`, templates `0640`, per-user `lockout/` `2770`. An existing store is re-secured in place |
| **Setup** | `setup.sh` / `vinoauthface-setup` | |
| PAM | Patches PAM service files | See [pam.md](pam.md). Each file is backed up with a `.face-auth.bak` suffix (for you; `uninstall.sh` strips the line instead of restoring it) |
| Login screen | `--login=off\|both\|face` | The KDE login screen's mode (Plasma Login or SDDM), off on a fresh install and kept on re-deploy. See [pam.md](pam.md#login-screen) |
| Bitwarden | Only if installed | Adds Bitwarden's polkit unlock action |
| SELinux | Compiles and loads policy | Lets the greeters (`xdm_t`) use the camera, NPU, template store and lockout state. See [SELinux](#selinux) |

## Building from source

### Core (static musl, no runtime deps)

`tract-linalg`'s build script assembles its kernels with `musl-gcc`, so install that first:

| Distro | Command |
|---|---|
| Fedora | `sudo dnf install gcc musl-gcc` |
| Debian, Ubuntu | `sudo apt install musl-tools` |
| Arch | `sudo pacman -S --needed musl` |
| Fedora Atomic, openSUSE | Build in a [distrobox](#immutable-distros-via-distrobox) or [container](#container-build-no-toolchain-installed) |

Use rustup rather than the distro's `rust` package, which may not ship the musl standard library:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup target add x86_64-unknown-linux-musl

git clone https://github.com/karanshukla/vinoAuthFace.git
cd vinoAuthFace
cargo build --release --locked --target x86_64-unknown-linux-musl -p face-auth -p face-enroll
sudo ./deploy.sh
```

If you installed Rust with rustup, `cargo` lives in `~/.cargo/bin`, which is not on root's
`PATH`. `deploy.sh` looks there for the invoking user and runs the build as that user, so it does
not leave root-owned files in `target/`.

### OpenVINO / NPU backend

The `npu` feature swaps pure-Rust `tract` inference for OpenVINO on an Intel NPU, GPU or CPU. It
loads OpenVINO's glibc libraries when the backend starts rather than linking them, so it's a glibc
build (not static musl), it builds with no OpenVINO present, and one binary runs against whichever
OpenVINO release is installed (2025.1 or newer). Releases ship it as
`vinoauthface-auth-openvino-x86_64-unknown-linux-gnu` and `vinoauthface-openvino-…`, built on
Ubuntu 22.04.

`deploy.sh` and the installer handle it. If they find OpenVINO, they install the NPU build (built
from source in a checkout with Rust, downloaded otherwise) and set `backend = "openvino"` in
`/etc/face-auth.toml`. Pick the device with `npu_device = "NPU" | "GPU" | "CPU"`. They look for
OpenVINO in this order:

| Source | How it's found | Where `vinoauthface-auth` loads it from |
|--------|----------------|----------------|
| [ovfetch](https://github.com/karanshukla/ovfetch) 0.6.0+ | On `PATH` or in `~/.cargo/bin` | `/usr/local/lib/face-auth/openvino`, its dependencies through the binary's rpath |
| The runtime ovfetch installed before | ovfetch missing (or the tray's upgrade, which runs as root alone), prefix still matches its `SHA256SUMS` | The same, kept as it is |
| System package | `libopenvino_c.so*` in a standard lib dir | That lib dir |
| Extracted archive | `~/.local/opt` or `/opt/intel`, with `setupvars.sh` | Copied to `/usr/local/lib/face-auth/openvino/intel64`, registered with `ldconfig` |

`vinoauthface-auth` only looks in those fixed directories, never the environment. If OpenVINO
isn't in any of them, both models run on `tract` instead (same models, same templates, slower) and
`vinoauthface doctor` warns; an OpenVINO that is there but won't load fails the scan, and unlock
falls back to the password. `vinoauthface-auth --openvino-devices` loads it the way a scan does
and lists the devices it sees.

ovfetch is the recommended one. It picks the OpenVINO build your NPU and its installed driver
need, and refuses anything whose hash independent sources don't agree on. It installs to
`/usr/local/lib/face-auth/openvino`, and later deploys only download again when a different build
resolves. ovfetch runs as you, into a staging directory; once root has copied that into place,
`deploy.sh` checks every file against ovfetch's `SHA256SUMS` and removes the directory if anything
doesn't match or isn't listed (face unlock then falls back to the password). It installs OpenVINO alone (`ovfetch install --openvino-only`), without ONNX Runtime, which face-auth doesn't use: that is the newest OpenVINO the NPU driver supports, the same release the machine's other NPU programs run, with nothing to compile. (`--build-runtime`, which compiled ONNX Runtime to get there, is no longer needed and is ignored.) If ovfetch installed the current runtime and is now missing or older than 0.6.0, deploy keeps that runtime as long as it still matches its `SHA256SUMS`, and stops rather than switch to the CPU backend if it doesn't. Install it with `cargo install ovfetch --locked`, or grab the attested binary from its
releases.

If the NPU driver has no compiler library (Fedora's 1.32.0 rpm ships none), OpenVINO can't compile
anything for the NPU. `deploy.sh` then runs `ovfetch npu install` as you (ovfetch 0.6.0): Intel's
compiler for that driver, from the linux-npu-driver release, checked against ovfetch's ledger, into
`~/.local/lib/ovfetch-npu`, where `ovfetch detect` and the other vino apps also find it. PAM callers
can't read your home, so deploy copies that prefix to `/usr/local/lib/face-auth/npu`, root-owned and
checked against its `SHA256SUMS` like the OpenVINO runtime, and `vinoauthface-auth` loads its
libraries (the prefix's `ovfetch-preload.txt`, in order, with the paths re-rooted to the copy)
before it first opens the NPU. The same happens if you ran `ovfetch npu install` yourself, including
`--full` for a whole driver: whatever prefix `detect` reports is mirrored, and the copy is removed
again when there is none. Only if that fails does `deploy.sh` build the `tract` backend instead of
shipping a build that would silently fall through to the password on every unlock. Run
`ovfetch detect` to check.

### Container build (no toolchain installed)

If `deploy.sh` finds no toolchain and the release download fails, it prints this command rather
than running it: `deploy.sh` runs under `sudo`, and rootless podman driven through `sudo -u` often
fails on a missing `XDG_RUNTIME_DIR`.

```bash
podman run --rm -v "$PWD":/src:Z -w /src docker.io/library/rust:alpine \
  sh -c 'apk add --no-cache musl-dev && \
         cargo build --release --locked --target x86_64-unknown-linux-musl \
           -p face-auth -p face-enroll'
sudo ./deploy.sh
```

`rust:alpine` targets musl natively, so the result is the same static binary. Run the container
as your own user so the files in `target/` stay yours.

### Immutable distros via distrobox

```bash
distrobox create --image registry.fedoraproject.org/fedora:latest --name authface-dev
distrobox enter authface-dev

# Fedora does not package a musl std for Rust, so use rustup, not the distro `rust` package.
sudo dnf install -y gcc musl-gcc cmake
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y \
  --target x86_64-unknown-linux-musl
source "$HOME/.cargo/env"

cd ~/Projects/vinoAuthFace
cargo build --release --locked --target x86_64-unknown-linux-musl -p face-auth -p face-enroll

exit
sudo ./deploy.sh   # on the host
```

## Pinning the camera (recommended)

```bash
sudo vinoauthface-pin-camera /dev/videoN   # ./pin-camera.sh in a checkout
```

Run it after enrolment and a test unlock both work: it pins whatever device you give it, and
`vinoauthface enroll` prints the exact command with the camera it used. It writes a udev rule for a stable
`/dev/face-auth-ir` symlink and records the camera's physical identity in `/etc/face-auth.toml`.
Re-run it if you replace the hardware on purpose. What this defends against:
[security.md](security.md#frame-injection-a-fake-camera).

## SELinux

With SELinux enforcing, the greeters (GDM, SDDM, Plasma Login) run face-auth in the confined
`xdm_t` domain. sudo, the KDE lock screen and polkit agents run in your unconfined session and
don't need it. `deploy.sh` installs a module, `selinux/face-auth.te` plus the labels in
`selinux/face-auth.fc`, that:

| Grants `xdm_t` | Why |
|---|---|
| `map` on the camera (`v4l_device_t`) | Frame capture |
| `/dev/accel/*` labelled `dri_device_t`, the GPU's type | Stock policy leaves the NPU unlabelled (`device_t`), so OpenVINO can't open it |
| Read on `/var/lib/face-auth` (`face_auth_var_lib_t`) | Templates |
| Write and `lock` on `<user>/lockout` (`face_auth_lockout_t`) | Without it the lockout never advances at the greeter |
| Read and `map` on `/var/cache/face-auth` (`face_auth_cache_t`) | The compiled NPU models |
| `getattr` on `/usr/lib64/games` | A cold NPU cache recompiles, and the driver's compiler stats every directory there |

`deploy.sh` relabels the NPU node, the cache and the store after loading it, and the OpenVINO
runtime after copying it in.

Sealed templates (`seal_embeddings = true`) need the TPM and systemd's host credential key, which
decrypts every systemd credential on the machine, so `xdm_t` gets neither. The greeter instead
execs `/usr/local/libexec/vinoauthface-unseal`, labelled `face_auth_unseal_exec_t`, which moves
into its own domain, `face_auth_unseal_t`, the only one with `/dev/tpmrm0` and
`credential.secret`. It takes a UID and one of face-auth's credential names, and only ever runs
`systemd-creds decrypt` on them. `xdm_t` (and `local_login_t`, for a TTY login) gains nothing but
the right to exec it.

If the install reported missing SELinux tools, install them and run `sudo vinoauthface-setup`
again, or install the module by hand:

```bash
sudo dnf install -y policycoreutils checkpolicy
sudo checkmodule -M -m -o face_auth.mod selinux/face-auth.te
sudo semodule_package -o face_auth.pp -m face_auth.mod -f selinux/face-auth.fc
sudo semodule -i face_auth.pp
sudo restorecon -R /dev/accel /var/lib/face-auth /var/cache/face-auth /usr/local/lib/face-auth
```

On Fedora Atomic (Silverblue, Kinoite, Bazzite) there's no `dnf install`, and `policycoreutils` is
already in the image. Layer `checkpolicy` and reboot, then run `sudo vinoauthface-setup` (or the
commands above, without the first line):

```bash
sudo rpm-ostree install checkpolicy
systemctl reboot
```

Remove it with `sudo semodule -r face_auth`.

## Updating

Nothing updates itself. `vinoauthface doctor` prints an `update` line, and the tray adds an
"Update to vN" entry plus one notification per new release, when a newer release exists. Both
ask GitHub's public releases API through `curl` (tray: after a minute, then daily). Turn it off
with `update_check = false` in `/etc/face-auth.toml`. Dev builds never check.

To update, no checkout needed:

```bash
sudo vinoauthface-upgrade          # the newest release
sudo vinoauthface-upgrade v3       # a specific one, also to go back
sudo vinoauthface-upgrade --force  # reinstall the one you're on
```

`vinoauthface-update` is the same command under another name.

It runs the [one-line installer](#the-one-line-installer) for that release: the release's
binaries, CPU or NPU, nothing built. Every upgrade is a full reinstall by the release's own deploy
logic, so anything a release changes beyond the binaries (models, config, PAM, SELinux) comes with
it. A `--no-tray` install stays one, and so does one with the settings window. Run with `sudo`, an
NPU install asks ovfetch (as you) whether this machine now wants a different OpenVINO. The tray's
"Update to vN" entry runs the same command through pkexec (see [tray.md](tray.md)), as root alone,
so it keeps the OpenVINO runtime that's installed.

NPU upgrades used to build from source into `~/.cache/vinoauthface`; the first upgrade under
`sudo` deletes it.

Releases before the one-line installer have no bundle; use a checkout for those.

From a checkout, `git pull` then `sudo ./deploy.sh` still works. A checkout on a release tag
installs that release's binaries (or builds them, with Rust installed).

Either way, your config, templates and PAM setup are kept. New settings are appended to
`/etc/face-auth.toml` as commented defaults.

## Updating after a code change

```bash
sudo ./update.sh            # rebuild and replace the binaries only
sudo ./update.sh --no-tray  # leave the tray alone
```

For iterating on the code. It rebuilds from source with the same backend as the installed
`vinoauthface-auth` (an NPU build if `vinoauthface-auth --build` says `openvino`, static musl if not), then replaces
`vinoauthface-auth` (keeping it set-group-ID `face-auth`), `vinoauthface`, and the tray and its
helper if they're installed. Unchanged binaries are left alone. Models, config, PAM, SELinux
policy, the OpenVINO runtime, the NPU cache and the template store aren't touched, and it
needs a Rust toolchain and an existing `deploy.sh` install. After pulling changes to any of
those, run `sudo ./deploy.sh` instead.

## Uninstalling

```bash
sudo vinoauthface-uninstall           # binaries, models, config, PAM changes, camera pin
sudo vinoauthface-uninstall --purge   # ...plus face templates and the face-auth group
```

`./uninstall.sh` in a checkout is the same script.

PAM goes first, before any binary is removed: it strips the vinoAuthFace line from each service
and deletes the `.face-auth.bak` backups rather than restoring them, so edits made since the
install (a distro update, your own) are kept. It removes a PAM override (`polkit-1`, KDE, COSMIC)
only if `deploy.sh` created it, and removes the Bitwarden action only if `deploy.sh` installed
it. It also cleans up leftovers from older installs that had the GTK GUI.
