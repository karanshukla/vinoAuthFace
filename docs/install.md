# Installing

`sudo ./deploy.sh` (or `sudo ./install.sh`, which just runs it) is the only supported install path. This page covers what it does, how to
build from source instead of using the release binaries, camera pinning, and removal.

## Requirements

- An IR camera on `uvcvideo` (Windows Hello style). See [hardware.md](hardware.md).
- PAM with `pam_exec.so`, standard everywhere.
- On SELinux systems, `policycoreutils` to compile the lock-screen policy. Installed by default on
  Fedora.

No build toolchain is required. If `deploy.sh` finds no Rust toolchain and no local build, it
downloads the static musl binaries from
[GitHub Releases](https://github.com/karanshukla/vinoAuthFace/releases) and verifies them against
the release's `SHA256SUMS`. Build from source for an unreleased change or the NPU backend.

## What deploy.sh does

| Step | What | Details |
|------|------|---------|
| Build | Picks the first that applies | OpenVINO + cargo: NPU build. Prebuilt binaries in `target/`: use them (`FACE_AUTH_FORCE_BUILD=1` to rebuild). Cargo: static musl build. Otherwise: download and checksum-verify the release binaries |
| Binaries | Installs to `/usr/local/bin` | `vinoauthface-auth` (set-group-ID `face-auth`, see [security.md](security.md#trust-model)) and `vinoauthface`, plus its bash, zsh and fish completions under `/usr/local/share` (regenerate with `vinoauthface completions bash`, `zsh` or `fish`) |
| Models | Downloads and SHA-256 verifies | Recognition model (`w600k_r50.onnx` for an NPU build, `w600k_mbf.onnx` otherwise) and the `det_500m.onnx` (SCRFD) detector, to `/usr/local/share/face-auth/`. Installing the detector over an older install means re-enrolling; `deploy.sh` says so. A copy in `models/` is used first, and verified too |
| Config | Installs default config | `/etc/face-auth.toml`, kept if it already exists; the deploy appends any settings from `config/face-auth.toml.example` it lacks, commented out at their defaults |
| PAM | Patches PAM service files | See [pam.md](pam.md). Each file is backed up with a `.face-auth.bak` suffix |
| Bitwarden | Only if installed | Adds Bitwarden's polkit unlock action |
| SELinux | Compiles and loads policy | Lets the greeters (`xdm_t`) use the camera, NPU, template store and lockout state. See [SELinux](#selinux) |
| NPU cache | Empties and refills it | `/var/cache/face-auth`, root-owned. Emptied because a new model or driver leaves stale entries, then refilled with `vinoauthface-auth --warm-cache` (NPU builds only) |
| Login screen | `--login=off\|both\|face` | The Plasma login screen's mode, off on a fresh install and kept on re-deploy. See [pam.md](pam.md#login-screen) |
| Tray | Unless `--no-tray` | Tray binary, root helper, polkit actions, autostart and launchers. See [tray.md](tray.md). Every deploy also installs `uninstall.sh` to `/usr/local/share/face-auth/` |
| Storage | Secures the template store | `/var/lib/face-auth`, `root:face-auth` `2750`, templates `0640`, per-user `lockout/` `2770`. An existing store is re-secured in place |

## Building from source

### Core (static musl, no runtime deps)

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
links OpenVINO's glibc libraries, so it can't be a static musl build.

`deploy.sh` handles it. If it finds OpenVINO and a Rust toolchain, it builds with `--features
npu` and sets `backend = "openvino"` in `/etc/face-auth.toml`. Pick the device with
`npu_device = "NPU" | "GPU" | "CPU"`. It looks for OpenVINO in this order:

| Source | How it's found | Runtime lookup |
|--------|----------------|----------------|
| [ovfetch](https://github.com/karanshukla/ovfetch) 0.2.2+ | On `PATH` or in `~/.cargo/bin` | rpath baked into `vinoauthface-auth` |
| System package | `libopenvino_c.so*` in a standard lib dir | The package's own `ldconfig` entry |
| Extracted archive | `~/.local/opt` or `/opt/intel`, with `setupvars.sh` | Copied, then registered with `ldconfig` |

ovfetch is the recommended one. It picks the OpenVINO build your NPU and its installed driver
need, and refuses anything whose hash independent sources don't agree on. It installs to
`/usr/local/lib/face-auth/openvino`, and later deploys only download again when a different build
resolves. Install it with `cargo install ovfetch --locked`, or grab the attested binary from its
releases.

If the NPU driver has no compiler library (Fedora's 1.32.0 rpm ships none), OpenVINO can't compile
anything for the NPU. `deploy.sh` then builds the `tract` backend instead of shipping a build that
would silently fall through to the password on every unlock. Run `ovfetch detect` to check.

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
sudo ./pin-camera.sh /dev/videoN
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
| Write on `<user>/lockout` (`face_auth_lockout_t`) | Without it the lockout never advances at the greeter |
| Read and `map` on `/var/cache/face-auth` (`face_auth_cache_t`) | The compiled NPU models |
| `getattr` on `/usr/lib64/games` | A cold NPU cache recompiles, and the driver's compiler stats every directory there |

`deploy.sh` relabels the NPU node, the cache and the store after loading it, and the OpenVINO
runtime after copying it in.

Not covered yet: unsealing TPM-sealed templates from `xdm_t`
([#122](https://github.com/karanshukla/vinoAuthFace/issues/122)). See
[pam.md](pam.md#login-screen).

If `deploy.sh` reported missing SELinux tools, install the module by hand:

```bash
sudo dnf install -y policycoreutils checkpolicy
sudo checkmodule -M -m -o face_auth.mod selinux/face-auth.te
sudo semodule_package -o face_auth.pp -m face_auth.mod -f selinux/face-auth.fc
sudo semodule -i face_auth.pp
sudo restorecon -R /dev/accel /var/lib/face-auth /var/cache/face-auth /usr/local/lib/face-auth
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

It downloads the release's source bundle (`vinoauthface-source.tar.gz`), checks it against the
release's `SHA256SUMS`, unpacks it into `~/.cache/vinoauthface/src/<tag>` and runs that release's
own `deploy.sh`. Every upgrade is a full reinstall by the release's deploy logic, so anything a
release changes beyond the binaries (models, config, PAM, SELinux) comes with it. A `--no-tray`
install stays one. Older releases' unpacked sources are deleted once it succeeds. Releases before
the command existed have no bundle; use a checkout for those.

An OpenVINO install builds from the bundle as you, with your Rust and ovfetch, the same as a
checkout. That's why the tray can't upgrade one: its helper runs as root only. The tray's
"Update to vN" entry is for CPU installs, and runs the same command through pkexec (see
[tray.md](tray.md)).

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
`vinoauthface-auth` (an NPU build if it links OpenVINO, static musl if not), then replaces
`vinoauthface-auth` (keeping it set-group-ID `face-auth`), `vinoauthface`, and the tray and its
helper if they're installed. Unchanged binaries are left alone. Models, config, PAM, SELinux
policy, the OpenVINO runtime, the NPU cache and the template store aren't touched, and it
needs a Rust toolchain and an existing `deploy.sh` install. After pulling changes to any of
those, run `sudo ./deploy.sh` instead.

## Uninstalling

```bash
sudo ./uninstall.sh           # binaries, models, config, PAM changes, camera pin
sudo ./uninstall.sh --purge   # ...plus face templates and the face-auth group
```

This restores the PAM backups, removes a PAM override (`polkit-1`, KDE, COSMIC) only if `deploy.sh` created it, and
removes the Bitwarden action only if `deploy.sh` installed it. It also cleans up leftovers from
older installs that had the GTK GUI.
