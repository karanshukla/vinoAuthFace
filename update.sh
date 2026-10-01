#!/bin/bash
set -euo pipefail

# Rebuild and reinstall the binaries over an existing deploy, and nothing else:
# no models, config, PAM, SELinux policy, OpenVINO runtime or template store.
# For iterating on the code. deploy.sh is still the install, and the thing to
# re-run after pulling changes to any of those.
#
#   sudo ./update.sh            # same backend (CPU or NPU) as the installed build
#   sudo ./update.sh --no-tray  # leave the tray alone

if [ -t 1 ] && [ -z "${NO_COLOR:-}" ]; then
    BOLD=$'\e[1m' DIM=$'\e[2m' GREEN=$'\e[32m' RED=$'\e[31m' RESET=$'\e[0m'
else
    BOLD="" DIM="" GREEN="" RED="" RESET=""
fi
ok()   { printf '  %s✓%s %-10s %s\n' "$GREEN" "$RESET" "$1" "$2"; }
skip() { printf '  %s· %-10s %s%s\n' "$DIM" "$1" "$2" "$RESET"; }
step() { printf '  %s… %s%s\n' "$DIM" "$1" "$RESET"; }
fail() {
    printf '%s✗ %s%s\n' "$RED" "$1" "$RESET" >&2
    shift
    local line
    for line in "$@"; do printf '  %s\n' "$line" >&2; done
}

WITH_TRAY=1
for arg in "$@"; do
    case "$arg" in
        --no-tray) WITH_TRAY=0 ;;
        *) fail "Unknown option '$arg'" "Usage: sudo ./update.sh [--no-tray]"; exit 1 ;;
    esac
done

# Keep in step with deploy.sh.
BIN_DIR="/usr/local/bin"
HELPER="/usr/local/libexec/vinoauthface-helper"
OPENVINO_INSTALL_DIR="/usr/local/lib/face-auth/openvino"
MUSL_TARGET="x86_64-unknown-linux-musl"
NPU_FEATURES="face-auth-core/npu,face-auth/npu,face-enroll/npu"

if [ "$(id -u)" -ne 0 ]; then
    fail "Run this with sudo" "It replaces binaries in $BIN_DIR."
    exit 1
fi
cd "$(dirname "$0")"

if [ ! -f "$BIN_DIR/vinoauthface-auth" ] || ! getent group face-auth >/dev/null; then
    fail "No existing install to update" "Run sudo ./deploy.sh first."
    exit 1
fi

# Same lookup as deploy.sh: root's PATH under sudo lacks the user's rustup.
find_cargo() {
    if command -v cargo &>/dev/null; then
        command -v cargo
        return 0
    fi
    if [ -n "${SUDO_USER:-}" ]; then
        local user_home
        user_home="$(getent passwd "$SUDO_USER" | cut -d: -f6)"
        if [ -n "$user_home" ] && [ -x "$user_home/.cargo/bin/cargo" ]; then
            echo "$user_home/.cargo/bin/cargo"
            return 0
        fi
    fi
    return 1
}

# Build as the invoking user: root-owned files in target/ break their next build.
# Stamped the way deploy.sh stamps its builds.
RELEASE_TAG="$(git describe --tags --exact-match 2>/dev/null || cat VERSION 2>/dev/null || true)"
STAMP=(env)
[ -n "$RELEASE_TAG" ] && STAMP+=(VINOAUTHFACE_VERSION="$RELEASE_TAG")
# sudo drops the environment, so a build directory set by the caller
# (vinoauthface-upgrade sets one) is passed on to cargo explicitly.
TARGET_DIR="${CARGO_TARGET_DIR:-target}"
[ -n "${CARGO_TARGET_DIR:-}" ] && STAMP+=(CARGO_TARGET_DIR="$CARGO_TARGET_DIR")
as_user() {
    if [ -n "${SUDO_USER:-}" ]; then
        sudo -u "$SUDO_USER" -H "${STAMP[@]}" "$@"
    else
        "${STAMP[@]}" "$@"
    fi
}

if ! CARGO="$(find_cargo)"; then
    fail "No Rust toolchain" "update.sh only builds from source. sudo ./deploy.sh can install release binaries."
    exit 1
fi

printf '%svinoAuthFace update%s\n\n' "$BOLD" "$RESET"

# The tray is updated only where deploy.sh installed it. Always static musl.
TRAY=0
[ "$WITH_TRAY" = 1 ] && [ -f "$BIN_DIR/vinoauthface-tray" ] && TRAY=1

# ---- Build, matching the installed backend ----
# An NPU build links OpenVINO dynamically; the CPU build is static musl, which
# ldd rejects. Building the other one would swap backends under a config
# written for this one.
OV_LIB="$(ldd "$BIN_DIR/vinoauthface-auth" 2>/dev/null | awk '/libopenvino_c/ {print $3; exit}' || true)"
if [ -n "$OV_LIB" ]; then
    OV_LIB_DIR="$(dirname "$OV_LIB")"
    BIN_SRC="$TARGET_DIR/release"
    # An ovfetch deploy finds OpenVINO through an rpath (see deploy.sh for why
    # DT_RPATH and GNU ld); system and archive installs through the loader's
    # normal search.
    RUSTFLAGS_NPU=""
    if [ "$OV_LIB_DIR" = "$OPENVINO_INSTALL_DIR" ]; then
        RUSTFLAGS_NPU="-C link-arg=-Wl,--disable-new-dtags,-rpath,$OPENVINO_INSTALL_DIR"
        command -v ld.bfd >/dev/null 2>&1 && RUSTFLAGS_NPU="$RUSTFLAGS_NPU -C link-arg=-fuse-ld=bfd"
    fi
    # openvino-sys caches the library directory it found and never rechecks.
    if grep -hs '^cargo:rustc-link-search=native=' "$TARGET_DIR"/release/build/openvino-sys-*/output \
            | grep -qvxF "cargo:rustc-link-search=native=$OV_LIB_DIR"; then
        as_user "$CARGO" clean --quiet --release -p openvino-sys
    fi
    step "building with the NPU backend against $OV_LIB_DIR"
    as_user env LD_LIBRARY_PATH="$OV_LIB_DIR" RUSTFLAGS="$RUSTFLAGS_NPU" \
        "$CARGO" build --release --locked --features "$NPU_FEATURES" -p face-auth -p face-enroll \
        || { fail "Build failed"; exit 1; }
    if ! ldd "$BIN_SRC/vinoauthface-auth" | grep -q libopenvino; then
        fail "NPU build has no OpenVINO linkage" "Nothing was installed."
        exit 1
    fi
    ok Build "NPU backend (OpenVINO, glibc)"
    if [ "$TRAY" = 1 ]; then
        step "building the tray"
        as_user "$CARGO" build --release --locked --target "$MUSL_TARGET" -p face-auth-tray \
            || { fail "Tray build failed" "Nothing was installed."; exit 1; }
    fi
else
    BIN_SRC="$TARGET_DIR/$MUSL_TARGET/release"
    # One build with the tray, as deploy.sh does, so cargo resolves the same
    # features and an unchanged tree relinks nothing.
    tray=()
    [ "$TRAY" = 1 ] && tray=(-p face-auth-tray)
    step "building the CPU backend"
    as_user "$CARGO" build --release --locked --target "$MUSL_TARGET" -p face-auth -p face-enroll "${tray[@]}" \
        || { fail "Build failed" "If the error mentions a missing target: rustup target add $MUSL_TARGET"; exit 1; }
    ok Build "CPU backend (tract, static musl)"
fi

# ---- Install ----
# install <src> <dest> <owner:group> <mode>, reporting whether it changed.
CHANGED=0
put() {
    local src="$1" dest="$2" owner="$3" mode="$4" name
    name="$(basename "$dest")"
    if [ -f "$dest" ] && cmp -s "$src" "$dest"; then
        skip "$name" "unchanged"
        return 0
    fi
    install -D -o "${owner%:*}" -g "${owner#*:}" -m "$mode" "$src" "$dest"
    command -v restorecon &>/dev/null && restorecon "$dest" 2>/dev/null || true
    ok "$name" "updated"
    CHANGED=1
}

printf '\n'
# Set-group-ID face-auth, as deploy.sh installs it (see docs/security.md).
put "$BIN_SRC/vinoauthface-auth" "$BIN_DIR/vinoauthface-auth" root:face-auth 2755
put "$BIN_SRC/vinoauthface" "$BIN_DIR/vinoauthface" root:root 0755
put "$BIN_SRC/vinoauthface-unseal" /usr/local/libexec/vinoauthface-unseal root:root 0755
if [ "$TRAY" = 1 ]; then
    put "$TARGET_DIR/$MUSL_TARGET/release/vinoauthface-tray" "$BIN_DIR/vinoauthface-tray" root:root 0755
    put "$TARGET_DIR/$MUSL_TARGET/release/vinoauthface-helper" "$HELPER" root:root 0755
fi

printf '\n'
if [ "$CHANGED" = 0 ]; then
    echo "Nothing changed."
else
    echo "Done. The next sudo, polkit prompt or unlock uses the new build."
    [ "$TRAY" = 1 ] && echo "A running tray keeps the old binary until you log out or restart it."
fi
echo "Changed models, config, PAM or the SELinux policy still need sudo ./deploy.sh."
