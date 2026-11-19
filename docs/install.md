---
title: Installing
nav_order: 2
---

# Installing

You can install vinoAuthFace from a prebuilt GitHub release or directly from a source checkout. Both
paths run `deploy.sh` to install binaries, models, configuration, and system hooks:

```bash
# Install the latest release (checksum-verified binaries, no compilation required)
curl -fsSL https://github.com/karanshukla/vinoAuthFace/releases/latest/download/install.sh | sudo bash

# Or install from a local git checkout (builds from source if Rust is available)
sudo ./deploy.sh
```

This page covers how installation works, supported distributions, building from source, OpenVINO/NPU
setup, camera pinning, SELinux policy, updating, and uninstalling.

## The one-line installer

`install.sh` queries GitHub for the latest release, downloads `vinoauthface-source.tar.gz` and
`SHA256SUMS`, verifies that the archive is listed and matches its checksum, and runs that release's
`deploy.sh --prebuilt` from a private temporary directory.

Because `--prebuilt` is passed, `deploy.sh` installs the checksum-verified release binaries even if
Rust is installed on the machine. If [ovfetch](https://github.com/karanshukla/ovfetch) is on `PATH`
or OpenVINO is already installed, it selects the [NPU build](#openvino--npu-backend) and the
OpenVINO runtime picked by `ovfetch`; otherwise it installs the static CPU build.

Pass options after `bash -s --`:

| Option | Effect |
|---|---|
| `vN` | Install release `vN` instead of the latest release |
| `--with-settings` | Also install the optional [settings window](settings.md) (built against glibc) |
| `--no-tray` | Skip installing the [tray icon](tray.md) |
| `--login=off\|both\|face` | Configure face unlock at the KDE [login screen](pam.md#login-screen) |

Re-running the installer performs a clean reinstall while preserving your existing configuration,
face templates, and PAM settings. Because `SHA256SUMS` is published alongside the release archives,
checksum verification protects against corrupted or truncated downloads over GitHub TLS.

### Installed management commands

Every installation adds four helper commands to `/usr/local/bin` so you do not need to keep a git
checkout around afterwards:

| Command | Purpose |
|---|---|
| `vinoauthface-upgrade` | Upgrade or switch releases ([Updating](#updating)) |
| `vinoauthface-setup` | Re-apply PAM hooks, login-screen mode, Bitwarden policy, and SELinux rules |
| `vinoauthface-pin-camera` | Pin the IR camera by physical USB port ([Pinning the camera](#pinning-the-camera-recommended)) |
| `vinoauthface-uninstall` | Remove vinoAuthFace ([Uninstalling](#uninstalling)) |

## Requirements

- A Windows Hello-style IR camera supported by `uvcvideo` (see [hardware.md](hardware.md)).
- PAM with `pam_exec.so` (included by default on standard Linux distributions).
- `curl` and `unzip` for downloading and unpacking models. `deploy.sh` checks for both before making
  any changes and prints the install command for your distro if either is missing.
- On SELinux systems, `policycoreutils` and `checkpolicy` to compile the login-screen policy
  (installed by default on standard Fedora).

## Distros

| Family | Tested | PAM layout | MAC | Notes |
|---|---|---|---|---|
| Fedora, Fedora Atomic (Silverblue, Kinoite, Bazzite) | Fedora KDE, Kinoite | `/etc/pam.d`, authselect | SELinux (policy loaded) | Primary target. On Atomic variants, `/usr/local` maps to `/var/usrlocal`, so installs survive OS updates. |
| Debian, Ubuntu, Mint | Ubuntu 24.04 (VM, no IR camera) | `/etc/pam.d`, `@include common-auth` | AppArmor (no policy needed) | — |
| Arch, CachyOS, EndeavourOS | No | `/etc/pam.d`, `include system-auth` | None | — |
| openSUSE Tumbleweed, Leap 16, Aeon, MicroOS | No | Vendor files in `/usr/lib/pam.d`, `include common-auth` | SELinux on new installs | `deploy.sh` loads the Fedora-derived SELinux policy; if a type is missing, it warns and continues (only login screens lose camera access). |
| SteamOS | No (unsupported) | `/etc/pam.d` | None | Steam Deck has no IR camera. Set a password for `deck` (`passwd`) first. OS updates reset `/etc`, removing PAM lines and `/etc/face-auth.toml`; re-run `sudo ./deploy.sh` after updates (`vinoauthface-setup` restores PAM hooks only). |

"Tested" means someone has run `deploy.sh` on that distro and unlocked with it. Other distributions
should generally work because `deploy.sh` inspects what is on disk (PAM files, SELinux state, and
whether `/usr` is writable) instead of matching distro names. Reports of working or broken setups
are welcome.

A build toolchain is optional. `install.sh` always uses prebuilt release binaries, and when
`deploy.sh` finds neither Rust nor local binaries in `target/`, it downloads checksum-verified
binaries from [GitHub Releases](https://github.com/karanshukla/vinoAuthFace/releases) (choosing the
NPU build if OpenVINO is installed). You only need to build from source if you are testing
unreleased changes.

## What deploy.sh does

`deploy.sh` handles two phases: installing files, and then running `setup.sh` to wire them into the
system. The second phase is also installed as `vinoauthface-setup`. Run `sudo vinoauthface-setup` if
a distro or `authselect` update resets `/etc/pam.d`, or after installing a new lock screen. It
accepts `--login=off|both|face` and refuses to run if vinoAuthFace is not installed.

### How binaries are selected

During the build step, `deploy.sh` picks the first matching option in order:

1. **`--prebuilt` passed**: download the checksum-verified release binaries (NPU build if OpenVINO is
   installed, static musl build otherwise).
2. **OpenVINO + Cargo available**: build the NPU backend from source.
3. **OpenVINO available, no Cargo**: download the prebuilt NPU release binaries.
4. **Prebuilt binaries already in `target/`**: use them, unless Cargo is available and source files
   are newer. Set `FACE_AUTH_FORCE_BUILD=1` to force a rebuild (this also rebuilds the tray).
5. **Cargo available**: build the static musl CPU binaries from source.
6. **Fallback**: download and checksum-verify the release binaries.

### Installation steps

| Step | Action | Details |
|---|---|---|
| Binaries | Installs to `/usr/local/bin` | Installs `vinoauthface-auth` (set-group-ID `face-auth`; see [security.md](security.md#trust-model)), `vinoauthface`, and shell completions for bash, zsh, and fish under `/usr/local/share` (regenerate with `vinoauthface completions bash\|zsh\|fish`). |
| Models | Downloads and verifies SHA-256 | Installs the recognition model (`w600k_r50.onnx` for NPU builds, `w600k_mbf.onnx` for CPU builds) and the `det_500m.onnx` (SCRFD) detector into `/usr/local/share/face-auth/`. Local copies in `models/` are verified and reused if present. Upgrading from an older install without SCRFD requires re-enrolling (`deploy.sh` will notify you). |
| Config | Installs default config | Creates `/etc/face-auth.toml` if missing. On re-deploy, keeps your file and appends any new settings from `config/face-auth.toml.example` as commented defaults. |
| Tray | Installs unless `--no-tray` | Installs the tray binary, root helper, polkit actions, autostart entry, and desktop launchers (see [tray.md](tray.md)). Every deploy also installs the [management commands](#installed-management-commands). |
| NPU cache | Rebuilds `/var/cache/face-auth` | Clears the root-owned cache to discard stale entries from older models or drivers, then warms it with `vinoauthface-auth --warm-cache` (NPU builds only). |
| Storage | Secures `/var/lib/face-auth` | Sets ownership and permissions to `root:face-auth` `2750`, templates to `0640`, and per-user `lockout/` to `2770`. Existing stores are re-secured in place. |

### System integration (`setup.sh` / `vinoauthface-setup`)

| Step | Action | Details |
|---|---|---|
| PAM | Patches PAM service files | Adds the `pam_exec` line to supported services (see [pam.md](pam.md)). Backs up each edited file with a `.face-auth.bak` suffix for your reference (`uninstall.sh` removes only the added line instead of overwriting with the backup). |
| Login screen | Applies `--login=off\|both\|face` | Configures KDE login screen mode for Plasma Login or SDDM. Defaults to `off` on fresh installs and preserves the existing setting on re-deploy (see [pam.md](pam.md#login-screen)). |
| Bitwarden | Installs polkit action if present | Adds Bitwarden's polkit biometric unlock policy when a Bitwarden client is detected. |
| SELinux | Compiles and loads policy | Grants greeters (`xdm_t`) access to the camera, NPU, template store, and lockout state (see [SELinux](#selinux)). |

## Building from source

### Core (static musl, no runtime deps)

`tract-linalg`'s build script compiles its assembly kernels with `musl-gcc`, so install that first:

| Distro | Command |
|---|---|
| Fedora | `sudo dnf install gcc musl-gcc` |
| Debian, Ubuntu | `sudo apt install musl-tools` |
| Arch | `sudo pacman -S --needed musl` |
| Fedora Atomic, openSUSE | Build in a [distrobox](#immutable-distros-via-distrobox) or [container](#container-build-no-toolchain-installed) |

Install Rust via `rustup` rather than your distro's `rust` package, which often omits the musl
standard library:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup target add x86_64-unknown-linux-musl

git clone https://github.com/karanshukla/vinoAuthFace.git
cd vinoAuthFace
cargo build --release --locked --target x86_64-unknown-linux-musl -p face-auth -p face-enroll
sudo ./deploy.sh
```

Because `rustup` installs `cargo` into `~/.cargo/bin` (which is not on root's `PATH`), `deploy.sh`
locates Cargo for the invoking user and runs the build as that user so `target/` does not end up
root-owned.

### OpenVINO / NPU backend

The `npu` feature replaces pure-Rust `tract` inference with OpenVINO running on an Intel NPU, GPU,
or CPU. Because it loads OpenVINO's shared libraries dynamically at runtime, it is compiled against
glibc (instead of static musl). It can be built without OpenVINO installed on the build host, and a
single binary works with any OpenVINO release from 2025.1 onward. GitHub Releases ship this build as
`vinoauthface-auth-openvino-x86_64-unknown-linux-gnu` and
`vinoauthface-openvino-x86_64-unknown-linux-gnu` (built on Ubuntu 22.04).

When `deploy.sh` or `install.sh` detects OpenVINO, it installs the NPU build (building from source
if Rust is available in a checkout, or downloading the release binary otherwise) and sets
`backend = "openvino"` in `/etc/face-auth.toml`. Choose the inference device with
`npu_device = "NPU" | "GPU" | "CPU"`.

The installer looks for OpenVINO in this order:

| Source | How it is detected | Where `vinoauthface-auth` loads it from |
|---|---|---|
| [ovfetch](https://github.com/karanshukla/ovfetch) 0.6.0+ | Found on `PATH` or in `~/.cargo/bin` | `/usr/local/lib/face-auth/openvino` (dependencies resolved via rpath) |
| Previously installed `ovfetch` runtime | Used when `ovfetch` is absent (or during a root-only tray upgrade), provided the prefix still matches its `SHA256SUMS` | `/usr/local/lib/face-auth/openvino` (kept as-is) |
| System package | `libopenvino_c.so*` in a standard system library directory | That system library directory |
| Extracted OpenVINO archive | Found in `~/.local/opt` or `/opt/intel` with `setupvars.sh` | Copied to `/usr/local/lib/face-auth/openvino/intel64` and registered with `ldconfig` |

At runtime, `vinoauthface-auth` searches only these fixed paths and ignores environment variables.
If OpenVINO cannot be found in any of them, inference falls back to `tract` (using the same models
and templates, at lower speed) and `vinoauthface doctor` logs a warning. If OpenVINO is present but
fails to load, the scan fails and PAM falls back to your password. Run
`vinoauthface-auth --openvino-devices` to verify loading and list the devices OpenVINO sees.

#### Using ovfetch (recommended)

`ovfetch` automatically selects the OpenVINO release compatible with your NPU hardware and installed
driver, and rejects downloads whose hashes are not corroborated by independent sources. Install it
with `cargo install ovfetch --locked` or download an attested binary from its GitHub releases.

How `deploy.sh` works with `ovfetch`:

- `ovfetch` runs as your user into a staging directory using `ovfetch install --openvino-only`
  (without ONNX Runtime, which vinoAuthFace does not use). This installs the latest OpenVINO build
  supported by your NPU driver with no compilation required. The legacy `--build-runtime` flag is
  still accepted and ignored.
- After copying the staged runtime to `/usr/local/lib/face-auth/openvino`, `deploy.sh` verifies
  every file against `ovfetch`'s `SHA256SUMS`. If any file is missing, unlisted, or fails
  verification, the directory is removed (causing face unlock to fall back to the password).
- Subsequent deploys only re-download when a newer compatible build is resolved.
- If `ovfetch` installed the current runtime but is now missing or older than 0.6.0, `deploy.sh`
  keeps the existing runtime as long as its `SHA256SUMS` checks pass, and aborts if verification
  fails instead of silently switching to the CPU backend.

#### Missing NPU compiler library

If your installed NPU driver does not ship a compiler library (for example, Fedora's 1.32.0 RPM),
OpenVINO cannot compile models for the NPU on its own. In that case, `deploy.sh` runs
`ovfetch npu install` as your user (requires `ovfetch` 0.6.0+) to fetch Intel's matching compiler
from the `linux-npu-driver` release, verify it against `ovfetch`'s ledger, and place it in
`~/.local/lib/ovfetch-npu` (where `ovfetch detect` and other vino apps can also use it).

Because PAM callers cannot read your home directory, `deploy.sh` mirrors that prefix to
`/usr/local/lib/face-auth/npu` (root-owned and verified against its `SHA256SUMS`). Before opening
the NPU, `vinoauthface-auth` preloads the libraries listed in the prefix's `ovfetch-preload.txt` in
order, re-rooted to `/usr/local/lib/face-auth/npu`.

If you ran `ovfetch npu install` manually (including `--full` for a complete driver), whatever
prefix `ovfetch detect` reports is mirrored the same way, and the mirror is removed if that prefix
is later deleted. If NPU compiler setup fails completely, `deploy.sh` falls back to building the
`tract` backend so you do not end up with a broken NPU build. Run `ovfetch detect` to inspect your
machine's NPU driver status.

### Container build (no toolchain installed)

If `deploy.sh` finds no Rust toolchain and cannot download a release, it prints the following
container command instead of running it directly (running rootless Podman under `sudo -u` often
fails due to a missing `XDG_RUNTIME_DIR`):

```bash
podman run --rm -v "$PWD":/src:Z -w /src docker.io/library/rust:alpine \
  sh -c 'apk add --no-cache musl-dev && \
         cargo build --release --locked --target x86_64-unknown-linux-musl \
           -p face-auth -p face-enroll'
sudo ./deploy.sh
```

`rust:alpine` targets musl natively and produces the same static binary. Run `podman` as your normal
user so files created in `target/` remain owned by you.

### Immutable distros via distrobox

```bash
distrobox create --image registry.fedoraproject.org/fedora:latest --name authface-dev
distrobox enter authface-dev

# Fedora's distro rust package does not include the musl stdlib, so use rustup.
sudo dnf install -y gcc musl-gcc cmake
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y \
  --target x86_64-unknown-linux-musl
source "$HOME/.cargo/env"

cd ~/Projects/vinoAuthFace
cargo build --release --locked --target x86_64-unknown-linux-musl -p face-auth -p face-enroll

exit
sudo ./deploy.sh   # run on the host
```

## Pinning the camera (recommended)

```bash
sudo vinoauthface-pin-camera /dev/videoN   # or ./pin-camera.sh in a checkout
```

Run this once enrolment and a test unlock succeed (`vinoauthface enroll` prints the exact command
for the camera it used). It writes a udev rule that creates a stable `/dev/face-auth-ir` symlink and
records the camera's physical USB identity in `/etc/face-auth.toml`. Re-run the command if you
intentionally replace or move the camera hardware. See
[security.md](security.md#frame-injection-a-fake-camera) for what camera pinning protects against.

## SELinux

When SELinux is enforcing, display manager greeters (GDM, SDDM, Plasma Login) run vinoAuthFace in
the confined `xdm_t` domain. `sudo`, the KDE lock screen, and polkit agents run in your unconfined
user session and do not require custom policy.

`deploy.sh` compiles and installs `selinux/face-auth.te` along with the file labels in
`selinux/face-auth.fc`:

| Permission / label for `xdm_t` | Purpose |
|---|---|
| `map` on the camera (`v4l_device_t`) | Allows V4L2 frame capture |
| Labels `/dev/accel/*` as `dri_device_t` | Stock SELinux policy leaves Intel NPUs unlabelled (`device_t`), blocking OpenVINO |
| Read access to `/var/lib/face-auth` (`face_auth_var_lib_t`) | Allows reading face templates |
| Write and `lock` on `<user>/lockout` (`face_auth_lockout_t`) | Allows updating failed-attempt lockout state at the greeter |
| Read and `map` on `/var/cache/face-auth` (`face_auth_cache_t`) | Allows loading compiled NPU models |
| `getattr` on `/usr/lib64/games` | Avoids denials when a cold NPU cache recompiles and the driver compiler stats library directories |

After loading the module, `deploy.sh` relabels `/dev/accel`, the cache, the template store, and the
OpenVINO runtime directory.

### SELinux and TPM-sealed templates

Sealed templates (`seal_embeddings = true`) require access to the TPM and systemd's host credential
key, which can decrypt any systemd credential on the machine. To avoid exposing those to `xdm_t`,
the greeter executes `/usr/local/libexec/vinoauthface-unseal` (labelled `face_auth_unseal_exec_t`),
which transitions into its own `face_auth_unseal_t` domain. Only `face_auth_unseal_t` has access to
`/dev/tpmrm0` and `credential.secret`. The helper accepts a UID and one of face-auth's credential
names, and only invokes `systemd-creds decrypt` on them. `xdm_t` (and `local_login_t` for TTY
logins) only receives permission to execute this helper.

### Installing the SELinux module manually

If the installer reported missing SELinux tools, install them and re-run `sudo vinoauthface-setup`,
or compile and load the module manually:

```bash
sudo dnf install -y policycoreutils checkpolicy
sudo checkmodule -M -m -o face_auth.mod selinux/face-auth.te
sudo semodule_package -o face_auth.pp -m face_auth.mod -f selinux/face-auth.fc
sudo semodule -i face_auth.pp
sudo restorecon -R /dev/accel /var/lib/face-auth /var/cache/face-auth /usr/local/lib/face-auth
```

On Fedora Atomic (Silverblue, Kinoite, Bazzite), `policycoreutils` is already included in the base
image. Layer `checkpolicy`, reboot, and then run `sudo vinoauthface-setup` (or the `checkmodule`
commands above):

```bash
sudo rpm-ostree install checkpolicy
systemctl reboot
```

To remove the SELinux module later, run `sudo semodule -r face_auth`.

## Updating

vinoAuthFace does not auto-update. Instead, `vinoauthface doctor` prints an `update` status line,
and the tray icon adds an **Update to vN** menu entry (plus one notification per release) when a
newer release is published. Both query GitHub's public releases API via `curl` (the tray checks one
minute after startup and once a day thereafter). Set `update_check = false` in `/etc/face-auth.toml`
to disable update checks. Local dev builds never check for updates.

To upgrade without a git checkout:

```bash
sudo vinoauthface-upgrade          # upgrade to the latest release
sudo vinoauthface-upgrade v3       # switch to a specific release (including downgrades)
sudo vinoauthface-upgrade --force  # reinstall the currently installed release
```

`vinoauthface-update` is an alias for `vinoauthface-upgrade`.

`vinoauthface-upgrade` invokes the [one-line installer](#the-one-line-installer) for the target
release, downloading prebuilt CPU or NPU binaries without compiling. Because each upgrade runs the
target release's `deploy.sh`, updates to models, config defaults, PAM hooks, and SELinux policy are
applied automatically. Existing `--no-tray` and `--with-settings` choices are preserved.

When run via `sudo`, an NPU upgrade asks `ovfetch` (as your user) whether a newer OpenVINO runtime
is needed. When triggered from the tray's **Update to vN** menu entry via `pkexec` (see
[tray.md](tray.md)), it runs as root alone and keeps the currently installed OpenVINO runtime.

> Previous NPU upgrades built from source in `~/.cache/vinoauthface`; the first upgrade run under
> `sudo` removes that directory automatically.

Releases prior to the one-line installer do not include a source bundle; install those from a git
checkout instead. From a checkout, `git pull && sudo ./deploy.sh` continues to work, and checking
out a release tag installs (or builds, if Rust is installed) that tag.

In all cases, your config, face templates, and PAM configuration are preserved, and any newly added
config keys are appended to `/etc/face-auth.toml` as commented defaults.

## Updating after a code change

```bash
sudo ./update.sh            # rebuild and replace binaries only
sudo ./update.sh --no-tray  # rebuild without touching the tray
```

Use `update.sh` when iterating on source changes. It requires a Rust toolchain and an existing
`deploy.sh` installation. It detects the installed backend from `vinoauthface-auth --build`
(`openvino` for an NPU build, otherwise static musl), rebuilds, and replaces `vinoauthface-auth`
(preserving set-group-ID `face-auth`), `vinoauthface`, and the tray and its helper if installed.
Unchanged binaries are skipped.

`update.sh` does not modify models, config files, PAM hooks, SELinux policy, the OpenVINO runtime,
the NPU cache, or the template store. After pulling changes to any of those, run `sudo ./deploy.sh`
instead.

## Uninstalling

```bash
sudo vinoauthface-uninstall           # removes binaries, models, compiled cache, config, PAM hooks, and camera pin
sudo vinoauthface-uninstall --purge   # also removes face templates and the face-auth group
```

`./uninstall.sh` in a source checkout runs the same script.

The uninstaller removes PAM hooks first, before deleting any binaries. It strips the vinoAuthFace
line from each service file and deletes the `.face-auth.bak` backups instead of restoring them, so
any later edits made by you or your distribution are preserved. PAM override copies (`polkit-1`,
KDE, COSMIC) and the Bitwarden polkit action are removed only if `deploy.sh` created them. It also
cleans up files left over from older installs that included the GTK GUI.
