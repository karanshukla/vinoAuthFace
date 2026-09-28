# Installing

`sudo ./deploy.sh` is the only supported install path. This page covers what it does, how to
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
| Binaries | Installs to `/usr/local/bin` | `face-auth` (set-group-ID `face-auth`, see [security.md](security.md#trust-model)) and `face-enroll` |
| Models | Downloads and SHA-256 verifies | Recognition model (`w600k_mbf.onnx` by default) and the `version-slim-320.onnx` detector, to `/usr/local/share/face-auth/`. A copy in `models/` is used first, and verified too |
| Config | Installs default config | `/etc/face-auth.toml`, kept if it already exists; the deploy lists any settings from `config/face-auth.toml.example` it lacks |
| PAM | Patches PAM service files | See [pam.md](pam.md). Each file is backed up with a `.face-auth.bak` suffix |
| Bitwarden | Only if installed | Adds Bitwarden's polkit unlock action |
| SELinux | Compiles and loads policy | Allows `xdm_t` to mmap the camera for lock-screen auth |
| NPU cache | Empties and refills it | `/var/cache/face-auth`, root-owned. Emptied because a new model or driver leaves stale entries, then refilled with `face-auth --warm-cache` (NPU builds only) |
| Tray | Only with `--with-tray` | Tray binary, root helper, polkit actions, autostart and launchers. See [tray.md](tray.md). Every deploy also installs `uninstall.sh` to `/usr/local/share/face-auth/` |
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
| [ovfetch](https://github.com/karanshukla/ovfetch) 0.2.2+ | On `PATH` or in `~/.cargo/bin` | rpath baked into `face-auth` |
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
`face-enroll` prints the exact command with the camera it used. It writes a udev rule for a stable
`/dev/face-auth-ir` symlink and records the camera's physical identity in `/etc/face-auth.toml`.
Re-run it if you replace the hardware on purpose. What this defends against:
[security.md](security.md#frame-injection-a-fake-camera).

## SELinux

With SELinux enforcing, the GNOME lock screen runs in the `xdm_t` domain, which can't `mmap`
video devices by default. `deploy.sh` installs a minimal module:

```
allow xdm_t v4l_device_t:chr_file map;
```

If `deploy.sh` reported missing SELinux tools, install the module by hand:

```bash
sudo dnf install -y policycoreutils
sudo checkmodule -M -m -o face_auth.mod selinux/face-auth.te
sudo semodule_package -o face_auth.pp -m face_auth.mod
sudo semodule -i face_auth.pp
```

Remove it with `sudo semodule -r face_auth`.

## Uninstalling

```bash
sudo ./uninstall.sh           # binaries, models, config, PAM changes, camera pin
sudo ./uninstall.sh --purge   # ...plus face templates and the face-auth group
```

This restores the PAM backups, removes the `polkit-1` override only if `deploy.sh` created it, and
removes the Bitwarden action only if `deploy.sh` installed it. It also cleans up leftovers from
older installs that had the GTK GUI.
