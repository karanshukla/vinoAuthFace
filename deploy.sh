#!/bin/bash
set -euo pipefail

# ---- Output ----
# One line per step: ok (done), skip (nothing to do), warn (needs a look).
# Colour only on a terminal, and never with NO_COLOR set.
if [ -t 1 ] && [ -z "${NO_COLOR:-}" ]; then
    BOLD=$'\e[1m' DIM=$'\e[2m' GREEN=$'\e[32m' YELLOW=$'\e[33m' RED=$'\e[31m' RESET=$'\e[0m'
else
    BOLD="" DIM="" GREEN="" YELLOW="" RED="" RESET=""
fi
section() { printf '\n%s%s%s\n' "$BOLD" "$1" "$RESET"; }
ok()      { printf '  %s✓%s %-10s %s\n' "$GREEN" "$RESET" "$1" "$2"; }
skip()    { printf '  %s· %-10s %s%s\n' "$DIM" "$1" "$2" "$RESET"; }
step()    { printf '  %s… %s%s\n' "$DIM" "$1" "$RESET"; }
# warn <label> <line> [continuation lines...]
warn() {
    printf '  %s!%s %-10s %s\n' "$YELLOW" "$RESET" "$1" "$2"
    shift 2
    local line
    for line in "$@"; do printf '    %-10s %s\n' "" "$line"; done
}
# fail <message> [detail lines...]: to stderr; the caller exits.
fail() {
    printf '%s✗ %s%s\n' "$RED" "$1" "$RESET" >&2
    shift
    local line
    for line in "$@"; do printf '  %s\n' "$line" >&2; done
}

# ---- Progress ----
# Bars only on a terminal; a log (CI, a pipe) keeps the quiet one-line steps.
if [ -t 1 ]; then INTERACTIVE=1; CURL_FLAGS=(-fL -#); else INTERACTIVE=0; CURL_FLAGS=(-fsSL); fi

# bar <done> <total> <label>: redraws one line in place, capped at 99% so a
# rough total never claims to be finished before the build is.
bar() {
    local n="$1" total="$2" label="$3" width=30 pct filled
    [ "$n" -ge "$total" ] && n=$((total - 1))
    [ "$n" -lt 0 ] && n=0
    pct=$((n * 100 / total)); filled=$((n * width / total))
    printf '\r\e[K  %s%s%s %s%s%s %3d%% %s(%d/%d)%s' "$DIM" "$label" "$RESET" \
        "$GREEN" "$(printf '%*s' "$filled" '' | tr ' ' '#')$(printf '%*s' $((width - filled)) '' | tr ' ' '-')" "$RESET" \
        "$pct" "$DIM" "$n" "$total" "$RESET"
}

# count_units <cargo> <args...>: the crates a build compiles, for the bar's
# total. A `cargo tree` of the same packages, so it is close, not exact.
count_units() {
    as_user "$@" -e normal,build --prefix none --format '{p}' 2>/dev/null | sort -u | wc -l
}

# cargo_build <label> <total> <command...>: run a cargo build. On a terminal
# it reads cargo's JSON artifact stream and draws a bar; warnings and errors
# still print (cargo renders them on stderr). Otherwise it is just --quiet.
cargo_build() {
    local label="$1" total="$2"
    shift 2
    if [ "$INTERACTIVE" != 1 ] || [ "$total" -lt 1 ]; then
        "$@" --quiet
        return
    fi
    # Cargo's output goes to a file that is polled while it runs, not down a
    # pipe: a pipe only ends when every holder of it exits, so anything cargo
    # left running (a compiler server, say) would hang the build here.
    local seen out pid rc=0
    out="$(mktemp)"
    bar 0 "$total" "$label"
    "$@" --quiet --message-format=json-render-diagnostics >"$out" &
    pid=$!
    while kill -0 "$pid" 2>/dev/null; do
        seen="$(grep -c '"reason":"compiler-artifact"' "$out" || true)"
        bar "$seen" "$total" "$label"
        sleep 0.2
    done
    wait "$pid" || rc=$?
    rm -f "$out"
    printf '\r\e[K'
    return "$rc"
}

# ---- Options ----
# The tray icon, its root helper and polkit actions (docs/tray.md) install by
# default. --no-tray (or FACE_AUTH_TRAY=0) skips them; --with-tray is kept as a
# no-op so existing invocations keep working.
WITH_TRAY="${FACE_AUTH_TRAY:-1}"
for arg in "$@"; do
    case "$arg" in
        --with-tray) WITH_TRAY=1 ;;
        --no-tray) WITH_TRAY=0 ;;
        *) fail "Unknown option '$arg'" "Usage: sudo ./deploy.sh [--no-tray]"; exit 1 ;;
    esac
done

# Recognition model. "mbf" (MobileFaceNet, buffalo_sc) is the default: ~14MB
# and fast. "r50" (ResNet50, buffalo_l) is ~175MB and more accurate, at a few
# ms more per frame. Select with:
#   sudo FACE_AUTH_RECOGNITION_MODEL=r50 ./deploy.sh
# Switching models means re-enrolling: the two produce incompatible embedding
# spaces, and face-auth refuses to compare across them (see storage.rs).
RECOGNITION_MODEL="${FACE_AUTH_RECOGNITION_MODEL:-mbf}"
case "$RECOGNITION_MODEL" in
    mbf)
        MODEL_URL="https://github.com/deepinsight/insightface/releases/download/v0.7/buffalo_sc.zip"
        MODEL_ZIP="buffalo_sc.zip"
        MODEL_NAME="w600k_mbf.onnx"
        MODEL_CHECKSUM="9cc6e4a75f0e2bf0b1aed94578f144d15175f357bdc05e815e5c4a02b319eb4f"
        ;;
    r50)
        MODEL_URL="https://github.com/deepinsight/insightface/releases/download/v0.7/buffalo_l.zip"
        MODEL_ZIP="buffalo_l.zip"
        MODEL_NAME="w600k_r50.onnx"
        MODEL_CHECKSUM="4c06341c33c2ca1f86781dab0e829f88ad5b64be9fba56e56bc9ebdefc619e43"
        ;;
    *)
        fail "Unknown FACE_AUTH_RECOGNITION_MODEL '$RECOGNITION_MODEL'" "Expected 'mbf' or 'r50'."
        exit 1
        ;;
esac

# Pinned to the commit that introduced the file, not to a moving branch: a
# `master` URL silently changes what gets installed. The checksum is the real
# gate; the pin keeps it from breaking on an unrelated upstream commit.
DETECTOR_URL="https://raw.githubusercontent.com/Linzaer/Ultra-Light-Fast-Generic-Face-Detector-1MB/0f9ca4a9fc80170fd505168fd1132b837141f7df/models/onnx/version-slim-320.onnx"
DETECTOR_CHECKSUM="e9adbd0f920ddcce9368434c4d34d72520dc0c19b526fd44b4ef49bde2c3b1a8"

BIN_DIR="/usr/local/bin"
SHARE_DIR="/usr/local/share/face-auth"
CONFIG_DIR="/etc"
PAM_DIR="/etc/pam.d"
VAR_DIR="/var/lib/face-auth"
NPU_CACHE_DIR="/var/cache/face-auth"
SELINUX_DIR="/usr/local/share/face-auth/selinux"
OPENVINO_INSTALL_DIR="/usr/local/lib/face-auth/openvino"

PAM_LINE="auth       sufficient  pam_exec.so quiet /usr/local/bin/face-auth"

if [ "$(id -u)" -ne 0 ]; then
    fail "Run this with sudo" "It installs into /usr/local, /etc and /var/lib."
    exit 1
fi

ACTUAL_USER="${SUDO_USER:-${USER:-$(id -un)}}"

printf '%sface-auth installer%s\n' "$BOLD" "$RESET"

# ---- Undo any previous partial setup ----

for service in sudo swaylock gdm-password polkit-1 kde-fingerprint; do
    if [ -f "$PAM_DIR/$service" ]; then
        sed -i '/pam_exec\.so.*face-auth/d' "$PAM_DIR/$service" 2>/dev/null || true
    fi
done

# ---- Build ----
MUSL_TARGET="x86_64-unknown-linux-musl"
ARTIFACT_DIR="target/$MUSL_TARGET/release"

# Locate cargo. This script runs under sudo, and root's PATH normally does not
# include the invoking user's rustup installation, so look there as well.
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

# Build as the invoking user, never as root: cargo fetches crates and runs
# build scripts, and root-owned files left in target/ break their next build.
as_user() {
    if [ -n "${SUDO_USER:-}" ] && [ "$(id -u)" -eq 0 ]; then
        sudo -u "$SUDO_USER" -H "$@"
    else
        "$@"
    fi
}

build_with_cargo() {
    local cargo="$1" tray=()
    [ "$WITH_TRAY" = 1 ] && tray=(-p face-auth-tray)
    step "building (a first build takes a few minutes)"
    local total=0
    [ "$INTERACTIVE" = 1 ] && total="$(count_units "$cargo" tree --locked --target "$MUSL_TARGET" \
        -p face-auth -p face-enroll "${tray[@]}")"
    if ! cargo_build "Compiling" "$total" as_user "$cargo" build --release --locked --target "$MUSL_TARGET" \
            -p face-auth -p face-enroll "${tray[@]}"; then
        fail "Build failed" "If the error mentions a missing target, add it with:" "  rustup target add $MUSL_TARGET"
        return 1
    fi
}

section "Build"

# ---- OpenVINO (NPU backend) detection ----
# The `npu` feature links OpenVINO's glibc .so files, so an NPU build uses the
# host (glibc) target instead of static musl. Three ways to get OpenVINO, in
# order of preference:
#
#   ovfetch https://github.com/karanshukla/ovfetch picks the OpenVINO build the
#           NPU and its installed driver need, and hash-verifies it. Installed
#           into $OPENVINO_INSTALL_DIR; face-auth finds it through an rpath.
#   system  RPM/DEB package: libopenvino_c lands in a standard lib dir and the
#           package ran ldconfig, so build and PAM-time loading both just work.
#   archive extracted tarball (~/.local/opt or /opt/intel) with setupvars.sh.
#           That covers the build, but PAM runs face-auth with no shell
#           profile, so the runtime libraries are copied system-wide below.
ACTUAL_HOME="$(getent passwd "$ACTUAL_USER" | cut -d: -f6)"
OPENVINO_MODE=""
# 0.2.2 installs every SONAME link. Before it, TBB loaded libtbbmalloc.so.2
# from wherever else the system had one.
OVFETCH_MIN="0.2.2"

# secure_path drops ~/.cargo/bin under sudo, the same as for cargo.
find_ovfetch() {
    local bin
    for bin in "$(command -v ovfetch 2>/dev/null)" "$ACTUAL_HOME/.cargo/bin/ovfetch"; do
        [ -n "$bin" ] && [ -x "$bin" ] || continue
        local have
        have="$("$bin" --version 2>/dev/null | awk '{print $2}')"
        if [ -n "$have" ] && [ "$(printf '%s\n' "$OVFETCH_MIN" "$have" | sort -V | head -1)" = "$OVFETCH_MIN" ]; then
            echo "$bin"
            return 0
        fi
        warn ovfetch "Ignoring $bin (${have:-unknown version}); needs $OVFETCH_MIN or newer." >&2
    done
    return 1
}

# No config yet on a first install: sed fails, and pipefail must not end the script.
CONF_NPU_DEVICE="$(sed -n 's/^npu_device *= *"\(.*\)"/\1/p' "$CONFIG_DIR/face-auth.toml" 2>/dev/null | head -1 || true)"
if OVFETCH_BIN="$(find_ovfetch)"; then
    # Run as the user: resolving and downloading need no privileges. The plan
    # is reused below to install exactly what was checked here.
    step "asking ovfetch which OpenVINO this machine needs (takes a few seconds)"
    if OVFETCH_PLAN="$(as_user "$OVFETCH_BIN" resolve --json)"; then
        # compiler_present is false on a machine with no NPU at all too, where
        # GPU or CPU can still use the OpenVINO build.
        if [ "${CONF_NPU_DEVICE:-NPU}" = "NPU" ] \
           && as_user "$OVFETCH_BIN" detect | grep -q '"kind": "npu"' \
           && grep -q '"compiler_present": false' <<<"$OVFETCH_PLAN"; then
            # Every compile_model would fail with ZE_RESULT_ERROR_UNSUPPORTED_FEATURE,
            # and every unlock would silently fall through to the password.
            warn OpenVINO "The NPU driver has no compiler library, so nothing can run on the NPU." \
                "Building the CPU (tract) backend instead. See: ovfetch detect"
            OPENVINO_MODE="none"
        else
            OPENVINO_MODE="ovfetch"
        fi
    else
        warn OpenVINO "ovfetch found no build for this machine (see above)."
    fi
fi
if [ -z "$OPENVINO_MODE" ]; then
    for dir in /usr/lib64 /usr/lib/x86_64-linux-gnu /lib/x86_64-linux-gnu /lib; do
        if compgen -G "$dir/libopenvino_c.so*" >/dev/null 2>&1; then
            OPENVINO_MODE="system"
            break
        fi
    done
fi
if [ -z "$OPENVINO_MODE" ]; then
    # find exits non-zero on a missing search dir; that is not an error here.
    OPENVINO_SRC="$(find "$ACTUAL_HOME/.local/opt" /opt/intel -maxdepth 1 -type d \
        \( -iname "openvino_toolkit_*" -o -iname "openvino" -o -iname "openvino_2022" \) \
        2>/dev/null | sort -V | tail -1 || true)"
    if [ -n "$OPENVINO_SRC" ] && [ -f "$OPENVINO_SRC/setupvars.sh" ]; then
        OPENVINO_MODE="archive"
    fi
fi

NPU_FEATURES="face-auth-core/npu,face-auth/npu,face-enroll/npu"
NPU_ACTIVE=0
BIN_SRC="$ARTIFACT_DIR"

# ovfetch's prefix. Downloaded only when the resolved build differs from the
# one already installed, or those files no longer match their SHA256SUMS.
# Otherwise ovfetch runs as the user into a staging directory: the build links
# against that, and the system prefix is only replaced once the new binaries
# are installed, so a failed build leaves the working install alone.
OV_STAGE=""
# The full version (2025.4.1); the plan's floor only has two parts.
ov_version() { grep -oE '"openvino": "[0-9]+\.[0-9]+\.[0-9]+"' <<<"$1" | head -1 | cut -d'"' -f4 || true; }
stage_ovfetch() {
    local want have
    want="$(grep -o '"sha256": "[0-9a-f]*"' <<<"$OVFETCH_PLAN" | head -1 || true)"
    have="$(grep -o '"sha256": "[0-9a-f]*"' "$OPENVINO_INSTALL_DIR/ovfetch.lock.json" 2>/dev/null | head -1 || true)"
    if [ -n "$want" ] && [ "$want" = "$have" ] \
       && (cd "$OPENVINO_INSTALL_DIR" && sha256sum -c --strict --quiet SHA256SUMS) 2>/dev/null; then
        ok OpenVINO "$(ov_version "$OVFETCH_PLAN") via ovfetch (installed, hashes verified)"
        OV_LIB_DIR="$OPENVINO_INSTALL_DIR"
        return 0
    fi
    OV_STAGE="$(as_user mktemp -d)"
    as_user "$OVFETCH_BIN" install --prefix "$OV_STAGE/ov" || return 1
    OV_LIB_DIR="$OV_STAGE/ov"
    ok OpenVINO "$(ov_version "$OVFETCH_PLAN") via ovfetch (downloaded, hashes verified)"
}

if [ -n "$OPENVINO_MODE" ] && [ "$OPENVINO_MODE" != "none" ] && CARGO_BIN="$(find_cargo)"; then
    [ "$OPENVINO_MODE" = "ovfetch" ] || ok OpenVINO "$OPENVINO_MODE install"
    NPU_STEP="building with the NPU backend (a first build takes a few minutes)"
    NPU_UNITS=0
    [ "$INTERACTIVE" = 1 ] && NPU_UNITS="$(count_units "$CARGO_BIN" tree --locked --features "$NPU_FEATURES" \
        -p face-auth -p face-enroll)"
    if [ "$OPENVINO_MODE" = "ovfetch" ]; then
        stage_ovfetch || { [ -n "$OV_STAGE" ] && rm -rf "$OV_STAGE"; exit 1; }
        # openvino-sys's build script records where it found OpenVINO and
        # never reruns on an environment change. A first install builds
        # against the staging prefix, deleted once installed, so the next
        # relink would search a directory that is gone. Rebuild it whenever
        # the recorded directory is not the one this build uses.
        if grep -hs '^cargo:rustc-link-search=native=' target/release/build/openvino-sys-*/output \
                | grep -qvxF "cargo:rustc-link-search=native=$OV_LIB_DIR"; then
            as_user "$CARGO_BIN" clean --quiet --release -p openvino-sys
        fi
        # LD_LIBRARY_PATH is where openvino-sys's build script looks for a flat
        # prefix. The rpath is how face-auth finds it at unlock time: pam_exec
        # gives it no environment. DT_RPATH (--disable-new-dtags) rather than
        # RUNPATH, because only DT_RPATH also covers the libraries OpenVINO
        # itself pulls in, and set-group-ID face-auth runs in glibc's secure
        # mode, where their own $ORIGIN rpaths are ignored.
        step "$NPU_STEP"
        # The staging directory is new every run, and openvino-sys bakes the
        # library path it found into its cached build output. Reused from an
        # earlier build, that is a -L to a directory that no longer exists.
        as_user "$CARGO_BIN" clean --quiet --release -p openvino-sys >/dev/null 2>&1 || true
        # Rust 1.98's default linker, rust-lld, segfaults linking openvino-sys's
        # build script (RUSTFLAGS reaches build scripts on a host-target build).
        # GNU ld is the fallback where it exists.
        NPU_RUSTFLAGS="-C link-arg=-Wl,--disable-new-dtags,-rpath,$OPENVINO_INSTALL_DIR"
        command -v ld.bfd >/dev/null 2>&1 && NPU_RUSTFLAGS="$NPU_RUSTFLAGS -C link-arg=-fuse-ld=bfd"
        cargo_build "Compiling" "$NPU_UNITS" as_user env LD_LIBRARY_PATH="$OV_LIB_DIR" \
            RUSTFLAGS="$NPU_RUSTFLAGS" \
            "$CARGO_BIN" build --release --locked --features "$NPU_FEATURES" \
            -p face-auth -p face-enroll || { [ -n "$OV_STAGE" ] && rm -rf "$OV_STAGE"; exit 1; }
    elif [ "$OPENVINO_MODE" = "system" ]; then
        step "$NPU_STEP"
        cargo_build "Compiling" "$NPU_UNITS" as_user "$CARGO_BIN" build --release --locked \
            --features "$NPU_FEATURES" -p face-auth -p face-enroll || exit 1
    else
        step "$NPU_STEP"
        cargo_build "Compiling" "$NPU_UNITS" as_user bash -c "
            set -eo pipefail
            source '$OPENVINO_SRC/setupvars.sh' >/dev/null
            set -u
            '$CARGO_BIN' build --release --locked --features '$NPU_FEATURES' -p face-auth -p face-enroll \"\$@\"
        " _ || exit 1
    fi
    BIN_SRC="target/release"
    NPU_ACTIVE=1
    ok Build "NPU backend (OpenVINO, glibc)"
elif [ -f "$ARTIFACT_DIR/face-auth" ] && [ -f "$ARTIFACT_DIR/face-enroll" ] \
   && [ -z "${FACE_AUTH_FORCE_BUILD:-}" ]; then
    skip Build "using $ARTIFACT_DIR/ (FACE_AUTH_FORCE_BUILD=1 to rebuild)"
elif CARGO_BIN="$(find_cargo)"; then
    build_with_cargo "$CARGO_BIN" || exit 1
    ok Build "CPU backend (tract, static musl)"
else
    # No toolchain and nothing built locally: fetch the release binaries CI
    # publishes, verified against the release's SHA256SUMS.
    RELEASE_REPO="karanshukla/vinoAuthFace"
    if [ -n "${FACE_AUTH_DEPLOY_RELEASE_BASE:-}" ]; then
        # Override for air-gapped mirrors, and for CI exercising this path
        # with a file:// URL.
        DOWNLOAD_BASE="$FACE_AUTH_DEPLOY_RELEASE_BASE"
    elif GIT_TAG="$(git describe --tags --exact-match 2>/dev/null)"; then
        # A tagged checkout installs its own release, so the binaries match
        # the deploy logic running them.
        DOWNLOAD_BASE="https://github.com/$RELEASE_REPO/releases/download/$GIT_TAG"
    else
        DOWNLOAD_BASE="https://github.com/$RELEASE_REPO/releases/latest/download"
    fi

    skip Build "no Rust toolchain; downloading release binaries"
    printf '    %-10s %s%s%s\n' "" "$DIM" "$DOWNLOAD_BASE" "$RESET"
    DL_DIR="$(mktemp -d)"
    DOWNLOAD_OK=1
    for asset in face-auth-$MUSL_TARGET face-enroll-$MUSL_TARGET SHA256SUMS; do
        curl "${CURL_FLAGS[@]}" --retry 5 --retry-delay 2 --retry-all-errors --connect-timeout 20 \
            -o "$DL_DIR/$asset" "$DOWNLOAD_BASE/$asset" || { DOWNLOAD_OK=0; break; }
    done
    if [ "$DOWNLOAD_OK" = 1 ]; then
        # Both binaries must be listed, not just match: --ignore-missing alone
        # would pass a file SHA256SUMS never mentions.
        if ! (cd "$DL_DIR" \
                && grep -E "[ *](face-auth|face-enroll)-$MUSL_TARGET\$" SHA256SUMS > want \
                && [ "$(wc -l < want)" -eq 2 ] \
                && sha256sum -c --strict --quiet want); then
            fail "Checksum verification failed for the downloaded binaries"
            rm -rf "$DL_DIR"
            exit 1
        fi
        mkdir -p "$DL_DIR/bin"
        mv "$DL_DIR/face-auth-$MUSL_TARGET" "$DL_DIR/bin/face-auth"
        mv "$DL_DIR/face-enroll-$MUSL_TARGET" "$DL_DIR/bin/face-enroll"
        BIN_SRC="$DL_DIR/bin"
        ok Download "release binaries, checksums verified"
        # Separately, so a release without the tray still installs the rest.
        if [ "$WITH_TRAY" = 1 ]; then
            TRAY_DL_OK=1
            for asset in face-auth-tray-$MUSL_TARGET face-auth-helper-$MUSL_TARGET; do
                curl "${CURL_FLAGS[@]}" --retry 5 --retry-delay 2 --retry-all-errors --connect-timeout 20 \
                    -o "$DL_DIR/$asset" "$DOWNLOAD_BASE/$asset" || { TRAY_DL_OK=0; break; }
            done
            if [ "$TRAY_DL_OK" = 1 ] && (cd "$DL_DIR" \
                    && grep -E "[ *](face-auth-tray|face-auth-helper)-$MUSL_TARGET\$" SHA256SUMS > want-tray \
                    && [ "$(wc -l < want-tray)" -eq 2 ] \
                    && sha256sum -c --strict --quiet want-tray); then
                mv "$DL_DIR/face-auth-tray-$MUSL_TARGET" "$DL_DIR/bin/face-auth-tray"
                mv "$DL_DIR/face-auth-helper-$MUSL_TARGET" "$DL_DIR/bin/face-auth-helper"
            fi
        fi
    else
        rm -rf "$DL_DIR"
        CONTAINER_ENGINE=""
        for engine in podman docker; do
            command -v "$engine" &>/dev/null && { CONTAINER_ENGINE="$engine"; break; }
        done

        fail "Nothing to install" "No Rust toolchain, no pre-built binaries in $ARTIFACT_DIR/, and the release download failed."
        echo ""

        if [ -n "$CONTAINER_ENGINE" ]; then
            # Deliberately not run from here: this script is under sudo, and
            # rootless $CONTAINER_ENGINE driven through `sudo -u` frequently
            # fails on a missing XDG_RUNTIME_DIR. Running it directly is
            # reliable, and keeps the build artifacts owned by you.
            echo "Option 1 — build in a container, no toolchain needed."
            echo "Run this as yourself (NOT with sudo), then re-run sudo ./deploy.sh:"
            echo ""
            echo "  $CONTAINER_ENGINE run --rm -v \"\$PWD\":/src:Z -w /src \\"
            echo "    docker.io/library/rust:alpine \\"
            echo "    sh -c 'apk add --no-cache musl-dev && \\"
            echo "           cargo build --release --locked --target $MUSL_TARGET \\"
            echo "             -p face-auth -p face-enroll'"
            echo ""
            echo "Option 2 — install a Rust toolchain:"
        else
            echo "Install a Rust toolchain:"
        fi

        echo "  Arch/CachyOS:  sudo pacman -S --needed rust"
        echo "  Fedora:        sudo dnf install rust cargo"
        echo "  Or rustup:     curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
        echo "  Then:          rustup target add $MUSL_TARGET"
        echo ""
        echo "Either way, re-run sudo ./deploy.sh afterwards."
        exit 1
    fi
fi

for bin in face-auth face-enroll; do
    if [ ! -f "$BIN_SRC/$bin" ]; then
        fail "Expected $BIN_SRC/$bin after the build, but it is missing"
        exit 1
    fi
done

# The tray is always static musl: it never runs inference, so an NPU build
# does not change it.
TRAY_SRC=""
if [ "$WITH_TRAY" = 1 ]; then
    if [ -n "${DL_DIR:-}" ]; then
        [ -f "$DL_DIR/bin/face-auth-tray" ] && TRAY_SRC="$DL_DIR/bin"
    elif [ "$BIN_SRC" = "$ARTIFACT_DIR" ] && [ -f "$ARTIFACT_DIR/face-auth-tray" ] \
         && [ -f "$ARTIFACT_DIR/face-auth-helper" ]; then
        TRAY_SRC="$ARTIFACT_DIR"
    elif CARGO_BIN="$(find_cargo)"; then
        step "building the tray"
        TRAY_UNITS=0
        [ "$INTERACTIVE" = 1 ] && TRAY_UNITS="$(count_units "$CARGO_BIN" tree --locked --target "$MUSL_TARGET" \
            -p face-auth-tray)"
        cargo_build "Compiling" "$TRAY_UNITS" as_user "$CARGO_BIN" build --release --locked \
            --target "$MUSL_TARGET" -p face-auth-tray && TRAY_SRC="$ARTIFACT_DIR"
    fi
fi

# A binary that does not match the backend written to the config fails at
# unlock time with a confusing error, so check the linkage now.
if [ "$NPU_ACTIVE" = 1 ] && ! ldd "$BIN_SRC/face-auth" | grep -q libopenvino; then
    fail "NPU build requested, but $BIN_SRC/face-auth has no OpenVINO linkage"
    exit 1
fi

# ---- Install binaries ----
section "Install"
# Set-group-ID face-auth (mode 2755): KScreenLocker and swaylock run PAM as
# the logged-in user, so face-auth borrows the face-auth group to read the
# store, which nothing else can open. face-auth scrubs its environment and only
# lets a non-root caller authenticate its own account (see
# crates/face-auth/src/main.rs).
getent group face-auth >/dev/null || groupadd --system face-auth
install -D -o root -g face-auth -m 2755 "$BIN_SRC/face-auth" "$BIN_DIR/face-auth"
if findmnt -no OPTIONS --target "$BIN_DIR" 2>/dev/null | tr ',' '\n' | grep -qx nosuid; then
    warn Binaries "$BIN_DIR is mounted nosuid, which ignores the set-group-ID bit." \
        "sudo, polkit and GDM still work; KDE's lock screen and swaylock fall back to the password."
fi
install -Dm755 "$BIN_SRC/face-enroll" "$BIN_DIR/face-enroll"
ok Binaries "face-auth, face-enroll in $BIN_DIR"
# The tray's uninstall entry runs this copy; the repo may be long gone.
install -D -o root -g root -m 0755 uninstall.sh "$SHARE_DIR/uninstall.sh"

# ---- Tray (default; skipped by --no-tray) ----
# The helper is what polkit authorises: root-owned, fixed path, one verb, no
# flags (crates/face-auth-tray/src/helper.rs). The policy goes in /usr/share
# where that is writable, since every polkit reads it; image-based distros
# get /usr/local/share, which polkit 124 and later also read.
TRAY_DATA="crates/face-auth-tray/data"
if [ -d /usr/share/polkit-1/actions ] && [ -w /usr/share/polkit-1/actions ] \
   && touch /usr/share/polkit-1/actions/.face-auth-write-test 2>/dev/null; then
    rm -f /usr/share/polkit-1/actions/.face-auth-write-test
    POLKIT_ACTIONS_DIR="/usr/share/polkit-1/actions"
else
    POLKIT_ACTIONS_DIR="/usr/local/share/polkit-1/actions"
fi
if [ -n "$TRAY_SRC" ]; then
    install -Dm755 "$TRAY_SRC/face-auth-tray" "$BIN_DIR/face-auth-tray"
    install -D -o root -g root -m 0755 "$TRAY_SRC/face-auth-helper" /usr/local/libexec/face-auth-helper
    install -Dm644 "$TRAY_DATA/io.github.karanshukla.vinoauthface.policy" \
        "$POLKIT_ACTIONS_DIR/io.github.karanshukla.vinoauthface.policy"
    install -Dm644 "$TRAY_DATA/vinoauthface-tray.desktop" /etc/xdg/autostart/vinoauthface-tray.desktop
    install -Dm644 "$TRAY_DATA/vinoauthface-tray.desktop" /usr/local/share/applications/vinoauthface-tray.desktop
    install -Dm644 "$TRAY_DATA/vinoauthface-enrol.desktop" /usr/local/share/applications/vinoauthface-enrol.desktop
    install -Dm644 "$TRAY_DATA/vinoauthface.svg" /usr/local/share/icons/hicolor/scalable/apps/vinoauthface.svg
    # The tray asks for these by name so Plasma recolours them to the panel.
    for icon in vinoauthface vinoauthface-scanning vinoauthface-attention; do
        install -Dm644 "$TRAY_DATA/$icon-symbolic.svg" \
            "/usr/local/share/icons/hicolor/symbolic/apps/$icon-symbolic.svg"
    done
    if command -v restorecon &>/dev/null; then
        restorecon "$BIN_DIR/face-auth-tray" /usr/local/libexec/face-auth-helper \
            "$POLKIT_ACTIONS_DIR/io.github.karanshukla.vinoauthface.policy" 2>/dev/null || true
    fi
    ok Tray "starts at your next login, or run face-auth-tray now"
elif [ "$WITH_TRAY" = 1 ]; then
    warn Tray "No tray binaries to install (no Rust toolchain, and the release has none)."
fi
[ -n "${DL_DIR:-}" ] && rm -rf "$DL_DIR"

# ---- OpenVINO runtime libraries (ovfetch and archive installs) ----
if [ -n "$OV_STAGE" ]; then
    step "installing the OpenVINO runtime"
    # Also clears a tarball copy an older deploy left here.
    rm -rf "$OPENVINO_INSTALL_DIR"
    install -d -o root -g root -m 0755 "$OPENVINO_INSTALL_DIR"
    cp -a "$OV_STAGE/ov/." "$OPENVINO_INSTALL_DIR/"
    rm -rf "$OV_STAGE"
    chown -hR root:root "$OPENVINO_INSTALL_DIR"
    chmod -R go-w "$OPENVINO_INSTALL_DIR"
    if ! (cd "$OPENVINO_INSTALL_DIR" && sha256sum -c --strict --quiet SHA256SUMS); then
        fail "$OPENVINO_INSTALL_DIR does not match its SHA256SUMS after copying"
        exit 1
    fi
    ok OpenVINO "runtime in $OPENVINO_INSTALL_DIR"
fi
# The archive install's library path entry would shadow the rpath, and
# expose its TBB to everything else on the system.
if [ "$OPENVINO_MODE" = "ovfetch" ] && [ -f /etc/ld.so.conf.d/face-auth-openvino.conf ]; then
    rm -f /etc/ld.so.conf.d/face-auth-openvino.conf
    ldconfig
fi
# OpenVINO finds its device plugins and ONNX frontend by scanning the
# directory libopenvino.so lives in, so intel64/ is copied as a unit.
if [ "$NPU_ACTIVE" = 1 ] && [ "$OPENVINO_MODE" = "archive" ]; then
    rm -rf "$OPENVINO_INSTALL_DIR"
    mkdir -p "$OPENVINO_INSTALL_DIR/intel64" "$OPENVINO_INSTALL_DIR/tbb"
    cp -a "$OPENVINO_SRC/runtime/lib/intel64/." "$OPENVINO_INSTALL_DIR/intel64/"
    cp -a "$OPENVINO_SRC/runtime/3rdparty/tbb/lib/." "$OPENVINO_INSTALL_DIR/tbb/"
    printf '%s\n' "$OPENVINO_INSTALL_DIR/intel64" "$OPENVINO_INSTALL_DIR/tbb" \
        > /etc/ld.so.conf.d/face-auth-openvino.conf
    ldconfig
    ok OpenVINO "runtime in $OPENVINO_INSTALL_DIR"
fi

# ---- Install models ----
# Staged in a private mktemp directory. A fixed /tmp path can be pre-created by
# another user, who then owns it and can swap the file between the checksum
# check and the install.
WORK_DIR="$(mktemp -d)"
trap 'rm -rf "$WORK_DIR"' EXIT

# GitHub release downloads redirect to a CDN that intermittently resets the
# connection mid-handshake ("TLS connect error: unexpected eof while reading").
# Retry rather than abandoning a half-finished install; --retry-all-errors so
# a reset connection counts, not just a retryable HTTP status.
# verify <file> <expected-sha256> — applied to every model, however it arrived.
# A file staged in models/ is no more trusted than one off the network.
verify() {
    local file="$1" want="$2"
    if ! echo "$want  $file" | sha256sum -c --status -; then
        fail "Checksum mismatch for $file" \
            "expected: $want" \
            "actual:   $(sha256sum "$file" | cut -d' ' -f1)" \
            "Refusing to install a model that is not the one this release pins."
        return 1
    fi
}

# fetch <url> <dest> <manual-recovery-hint>
fetch() {
    local url="$1" dest="$2" hint="$3"
    curl "${CURL_FLAGS[@]}" --retry 5 --retry-delay 2 --retry-all-errors \
         --connect-timeout 20 -o "$dest" "$url" && return 0

    fail "Download failed after retries: $url" \
        "This script prefers a local copy over downloading, so you can place the" \
        "file yourself and re-run:"
    echo "$hint" >&2
    return 1
}

# Models already in place are not re-checked: this pass only installs.
MODELS_NEW=()
if [ -f "$SHARE_DIR/$MODEL_NAME" ]; then
    :
elif [ -f "models/$MODEL_NAME" ]; then
    verify "models/$MODEL_NAME" "$MODEL_CHECKSUM" || exit 1
    install -Dm644 "models/$MODEL_NAME" "$SHARE_DIR/$MODEL_NAME"
    MODELS_NEW+=("$MODEL_NAME")
else
    fetch "$MODEL_URL" "$WORK_DIR/$MODEL_ZIP" \
        "  mkdir -p models
  curl -fL -o /tmp/$MODEL_ZIP '$MODEL_URL'
  unzip -j /tmp/$MODEL_ZIP $MODEL_NAME -d models/" || exit 1
    unzip -oq "$WORK_DIR/$MODEL_ZIP" -d "$WORK_DIR/"
    verify "$WORK_DIR/$MODEL_NAME" "$MODEL_CHECKSUM" || exit 1
    install -Dm644 "$WORK_DIR/$MODEL_NAME" "$SHARE_DIR/$MODEL_NAME"
    MODELS_NEW+=("$MODEL_NAME")
fi

DETECTOR_NAME="version-slim-320.onnx"
if [ -f "$SHARE_DIR/$DETECTOR_NAME" ]; then
    :
elif [ -f "models/$DETECTOR_NAME" ]; then
    verify "models/$DETECTOR_NAME" "$DETECTOR_CHECKSUM" || exit 1
    install -Dm644 "models/$DETECTOR_NAME" "$SHARE_DIR/$DETECTOR_NAME"
    MODELS_NEW+=("$DETECTOR_NAME")
else
    fetch "$DETECTOR_URL" "$WORK_DIR/$DETECTOR_NAME" \
        "  mkdir -p models
  curl -fL -o models/$DETECTOR_NAME '$DETECTOR_URL'" || exit 1
    verify "$WORK_DIR/$DETECTOR_NAME" "$DETECTOR_CHECKSUM" || exit 1
    install -Dm644 "$WORK_DIR/$DETECTOR_NAME" "$SHARE_DIR/$DETECTOR_NAME"
    MODELS_NEW+=("$DETECTOR_NAME")
fi
if [ "${#MODELS_NEW[@]}" -gt 0 ]; then
    ok Models "installed ${MODELS_NEW[*]} (checksums verified)"
else
    ok Models "$MODEL_NAME ($RECOGNITION_MODEL), $DETECTOR_NAME"
fi

if [ -f "$CONFIG_DIR/face-auth.toml" ]; then
    CONF_STATE="kept"
else
    install -Dm644 config/face-auth.toml.example "$CONFIG_DIR/face-auth.toml"
    CONF_STATE="installed"
fi

# An existing config keeps every value it has, so a setting a newer release
# adds would stay invisible. Append the ones it lacks as commented-out
# defaults, copied from the example: nothing changes until someone edits them.
if [ "$CONF_STATE" = kept ]; then
    NEW_KEYS=() NEW_LINES=()
    while read -r key; do
        grep -qE "^#? ?$key ?=" "$CONFIG_DIR/face-auth.toml" && continue
        NEW_KEYS+=("$key")
        NEW_LINES+=("# ${key} = $(grep -m1 -E "^#? ?$key ?= " config/face-auth.toml.example | sed -E 's/^#? ?[a-z0-9_]+ = //')")
    done < <(sed -nE 's/^#? ?([a-z][a-z0-9_]*) = .*/\1/p' config/face-auth.toml.example | sort -u)
    if [ "${#NEW_KEYS[@]}" -gt 0 ]; then
        CONF="$CONFIG_DIR/face-auth.toml"
        [ -s "$CONF" ] && [ "$(tail -c1 "$CONF" | wc -l)" -eq 0 ] && echo >> "$CONF"
        {
            echo
            echo "# Added by deploy.sh: newer settings, at their defaults. See"
            echo "# config/face-auth.toml.example for what each does."
            printf '%s\n' "${NEW_LINES[@]}"
        } >> "$CONF"
        ok Config "added ${#NEW_KEYS[@]} new settings to $CONF as commented defaults: ${NEW_KEYS[*]}"
    fi
fi

# Point model_path at the model installed this run. Only touched for a
# non-default model, so a plain deploy never rewrites an existing config.
if [ "$RECOGNITION_MODEL" != "mbf" ]; then
    CONF="$CONFIG_DIR/face-auth.toml"
    if grep -q '^model_path' "$CONF"; then
        sed -i "s@^model_path.*@model_path = \"$SHARE_DIR/$MODEL_NAME\"@" "$CONF"
    elif grep -q '^# model_path = ' "$CONF"; then
        # @ delimiter: the pattern itself contains a '#'.
        sed -i "s@^# model_path = .*@model_path = \"$SHARE_DIR/$MODEL_NAME\"@" "$CONF"
    else
        # Without a trailing newline the appended key would land inside a comment.
        [ -s "$CONF" ] && [ "$(tail -c1 "$CONF" | wc -l)" -eq 0 ] && echo >> "$CONF"
        echo "model_path = \"$SHARE_DIR/$MODEL_NAME\"" >> "$CONF"
    fi
    warn Config "model_path set to $MODEL_NAME. Switching models means re-enrolling."
fi

# Keep the backend line in sync with what was actually built. A mismatch
# (config says openvino, binary lacks it) fails at unlock time.
CONF="$CONFIG_DIR/face-auth.toml"
if [ "$NPU_ACTIVE" = 1 ]; then
    if grep -q '^backend' "$CONF"; then
        sed -i 's/^backend.*/backend = "openvino"/' "$CONF"
    elif grep -q '^# backend = ' "$CONF"; then
        sed -i 's/^# backend = .*/backend = "openvino"/' "$CONF"
    else
        [ -s "$CONF" ] && [ "$(tail -c1 "$CONF" | wc -l)" -eq 0 ] && echo >> "$CONF"
        echo 'backend = "openvino"' >> "$CONF"
    fi
    ok Config "$CONF $CONF_STATE, backend = openvino"
elif grep -q '^backend\s*=\s*"openvino"' "$CONF"; then
    sed -i 's/^backend.*/backend = "tract"/' "$CONF"
    warn Config "No NPU build this run, so backend reverted to tract in $CONF."
else
    ok Config "$CONF $CONF_STATE"
fi

# ---- PAM setup ----
PAM_DONE=() PAM_ABSENT=() PAM_COVERED=()

# polkit-1 (pkexec, GUI admin prompts, Bitwarden's system unlock) usually has
# no /etc/pam.d override: it falls back to the vendor file in /usr/lib/pam.d.
# Materialise that as an override so there is something to patch, and mark it
# so uninstall.sh deletes it rather than "restoring" a file that never was.
if [ ! -f "$PAM_DIR/polkit-1" ] && [ -f /usr/lib/pam.d/polkit-1 ]; then
    cp /usr/lib/pam.d/polkit-1 "$PAM_DIR/polkit-1"
    touch "$PAM_DIR/.face-auth-polkit-1-created"
fi

# A face match ends the auth stack, so the line goes just above the first
# module that actually authenticates. Gates above that (pam_nologin,
# pam_faillock preauth, pam_selinux_permit) stay in front of it.
PAM_AUTHENTICATOR='^[[:space:]]*-?auth[[:space:]]+(include|substack)[[:space:]]|^[[:space:]]*-?auth[[:space:]].*pam_(unix|sss|fprintd|u2f)|^[[:space:]]*@include[[:space:]]'

# Stacks this file pulls its auth lines from, one level deep.
pam_included_stacks() {
    awk '$1 ~ /^-?auth$/ && ($2 == "include" || $2 == "substack") { print $3 }
         $1 == "@include" { print $2 }' "$1"
}

for service in sudo swaylock gdm-password polkit-1 kde-fingerprint; do
    conf="$PAM_DIR/$service"
    if [ ! -f "$conf" ]; then
        PAM_ABSENT+=("$service")
        continue
    fi

    # A face-auth line in an included stack (added by hand, an authselect
    # profile, an older install) already covers this service. A second one
    # would scan the camera twice per attempt.
    covered_by=""
    for stack in $(pam_included_stacks "$conf"); do
        if [ -f "$PAM_DIR/$stack" ] && grep -q "pam_exec\.so.*face-auth" "$PAM_DIR/$stack"; then
            covered_by="$PAM_DIR/$stack"
            break
        fi
    done
    if [ -n "$covered_by" ]; then
        PAM_COVERED+=("$service (via $(basename "$covered_by"))")
        continue
    fi

    line_no=$(grep -nE "$PAM_AUTHENTICATOR" "$conf" | head -n1 | cut -d: -f1)
    if [ -z "$line_no" ]; then
        warn PAM "$conf has no authenticating auth line. Add this above the password module:" \
            "$PAM_LINE"
        continue
    fi

    cp "$conf" "$conf.face-auth.bak"
    sed -i "${line_no}i $PAM_LINE" "$conf"
    PAM_DONE+=("$service")
done
# Joined with ", " for the summary lines.
join() { local IFS=,; echo "$*" | sed 's/,/, /g'; }
[ "${#PAM_DONE[@]}" -gt 0 ] && ok PAM "$(join "${PAM_DONE[@]}") (backups: *.face-auth.bak)"
[ "${#PAM_COVERED[@]}" -gt 0 ] && ok PAM "$(join "${PAM_COVERED[@]}")"
[ "${#PAM_ABSENT[@]}" -gt 0 ] && skip PAM "not on this system: $(join "${PAM_ABSENT[@]}")"

# ---- Bitwarden polkit action (only if Bitwarden is installed) ----
# Bitwarden's "Unlock with system authentication" is a polkit action that
# re-authenticates through the polkit-1 stack patched above. Flatpak and Snap
# builds cannot install the action themselves (see
# https://bitwarden.com/help/biometrics/#tab-linux). The policy below is
# transcribed from Bitwarden's own source, the string its native builds write
# via pkexec: apps/desktop/src/key-management/biometrics/native-v2/
# os-biometrics-linux.service.ts in github.com/bitwarden/clients.
if command -v bitwarden &>/dev/null || command -v bitwarden-desktop &>/dev/null \
   || flatpak info com.bitwarden.desktop &>/dev/null \
   || snap list bitwarden-desktop &>/dev/null; then
    BW_POLICY="/usr/share/polkit-1/actions/com.bitwarden.Bitwarden.policy"
    if [ -f "$BW_POLICY" ]; then
        ok Bitwarden "polkit unlock action already present"
    elif ! touch "$BW_POLICY" 2>/dev/null; then
        # Read-only /usr on image-based distros.
        warn Bitwarden "Cannot write $BW_POLICY (read-only /usr?), so system unlock isn't wired."
    else
        cat > "$BW_POLICY" <<'EOF'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE policyconfig PUBLIC
 "-//freedesktop//DTD PolicyKit Policy Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/PolicyKit/1.0/policyconfig.dtd">

<policyconfig>
    <action id="com.bitwarden.Bitwarden.unlock">
      <description>Unlock Bitwarden</description>
      <message>Authenticate to unlock Bitwarden</message>
      <defaults>
        <allow_any>no</allow_any>
        <allow_inactive>no</allow_inactive>
        <allow_active>auth_self</allow_active>
      </defaults>
    </action>
</policyconfig>
EOF
        chown root:root "$BW_POLICY"
        chmod 644 "$BW_POLICY"
        touch "$SHARE_DIR/.bitwarden-policy-installed"
        command -v restorecon &>/dev/null && restorecon "$BW_POLICY" 2>/dev/null || true
        ok Bitwarden "polkit action installed. Enable: Settings > Unlock with system authentication"
    fi
fi

# ---- SELinux policy (for lock screen) ----
if command -v checkmodule &>/dev/null && command -v semodule_package &>/dev/null; then
    step "loading the SELinux policy (semodule can take 10-30 seconds)"
    mkdir -p "$SELINUX_DIR"
    cp selinux/face-auth.te "$SELINUX_DIR/face_auth.te"
    checkmodule -M -m -o "$SELINUX_DIR/face_auth.mod" "$SELINUX_DIR/face_auth.te"
    semodule_package -o "$SELINUX_DIR/face_auth.pp" -m "$SELINUX_DIR/face_auth.mod"
    semodule -i "$SELINUX_DIR/face_auth.pp"
    ok SELinux "lock-screen camera policy loaded"
else
    warn SELinux "Tools not found, so the lock screen can't reach the camera." \
        "Install policycoreutils (sudo dnf install policycoreutils), then re-run this script."
fi

# ---- Embeddings directory ----
#
# Face templates are authentication data. Anything that can write them can
# choose whose face unlocks an account, so templates are root-owned and
# enrolment goes through sudo/pkexec. The face-auth group (only the
# set-group-ID face-auth binary has it) may read templates and write only the
# per-user lockout/ directory:
#
#   $VAR_DIR/                        root:face-auth 2750
#   $VAR_DIR/<user>/                 root:face-auth 2750
#   $VAR_DIR/<user>/embeddings.bin   root:face-auth 0640
#   $VAR_DIR/<user>/lockout/         root:face-auth 2770
#
# The set-group-ID bit on the directories makes new entries inherit the group. Earlier versions made this 1777 with
# user-owned subdirectories, which let any local user create a template
# directory for an account that had not enrolled yet.
# ---- NPU compile cache ----
#
# face-auth points the Intel NPU driver's cache (normally $HOME/.cache) here.
# Root-owned so only root paths (sudo, GDM, polkit) write compiled blobs; the
# lock screen runs as the user and only reads. Emptied on every deploy, since
# new models or a new driver leave stale entries.
rm -rf "$NPU_CACHE_DIR"
install -d -o root -g root -m 0755 "$NPU_CACHE_DIR"
# Refill it now, as root. Left empty, every lock-screen unlock would compile
# both models on the CPU until the next sudo or polkit prompt.
if [ "$NPU_ACTIVE" = 1 ]; then
    step "compiling the models for the NPU"
    if WARM="$("$BIN_DIR/face-auth" --warm-cache 2>&1)"; then
        ok "NPU cache" "$WARM"
    else
        warn "NPU cache" "Could not compile the models, so face unlock falls through to the password." \
            "$WARM" "Check the NPU driver, and npu_device in $CONF."
    fi
fi

# Recorded before re-securing: 1777 is the old world-writable store.
VAR_MODE_BEFORE="$(stat -c %a "$VAR_DIR" 2>/dev/null || true)"
install -d -o root -g face-auth -m 2750 "$VAR_DIR"
STORE_STATE="created, $VAR_DIR"

if [ -n "$(find "$VAR_DIR" -mindepth 1 -maxdepth 1 -print -quit 2>/dev/null)" ]; then
    # Existing templates are kept; only their ownership and modes change, so
    # nobody has to enrol again after upgrading.
    #
    # This directory may have been world-writable before this release, so treat
    # what is in it as untrusted. Anything that is not a regular file or a
    # directory (symlinks especially) is removed first: `chown -R` dereferences
    # symlinks, so a planted link could otherwise redirect it at a file
    # elsewhere on the system.
    STORE_STATE="existing templates kept and re-secured"

    STRAY="$(find "$VAR_DIR" -mindepth 1 ! -type d ! -type f -print 2>/dev/null || true)"
    if [ -n "$STRAY" ]; then
        # shellcheck disable=SC2086 # one path per line
        warn Store "Removed entries that aren't files or directories:" $STRAY
        find "$VAR_DIR" -mindepth 1 ! -type d ! -type f -delete 2>/dev/null || true
    fi

    # Lockout state moved into <user>/lockout/; the old counters are dropped.
    find "$VAR_DIR" -mindepth 2 -maxdepth 2 -type f \( -name 'lockout.bin' -o -name 'lockout.bin.tmp' \) -delete

    # -h so the chown applies to entries themselves, never through a link.
    find "$VAR_DIR" -mindepth 1 \( -type d -o -type f \) -exec chown -h root:face-auth {} +
    find "$VAR_DIR" -mindepth 1 -type d -exec chmod 2750 {} +
    find "$VAR_DIR" -mindepth 1 -type f -exec chmod 0640 {} +
    for user_dir in "$VAR_DIR"/*/; do
        [ -d "$user_dir" ] || continue
        install -d -o root -g face-auth -m 2770 "${user_dir}lockout"
        find "${user_dir}lockout" -mindepth 1 -type f -exec chmod 0660 {} +
    done

    if [ "$VAR_MODE_BEFORE" = 1777 ] || [ -n "$STRAY" ]; then
        warn Store "This was the old world-writable store. To be sure no one planted a template:" \
            "sudo rm -rf $VAR_DIR && sudo ./deploy.sh, then re-enrol"
    fi
fi
ok Store "$STORE_STATE"

section "Done"
if [ -f "$VAR_DIR/$ACTUAL_USER/embeddings.bin" ]; then
    printf '  %-10s %s\n' "Test" "sudo -k && sudo true, or lock the screen (Super+L)"
    printf '  %-10s %s\n' "Re-enrol" "sudo face-enroll --user $ACTUAL_USER"
else
    printf '  %-10s %s\n' "Enrol" "sudo face-enroll --user $ACTUAL_USER"
    printf '  %-10s %s\n' "Then test" "sudo -k && sudo true, or lock the screen (Super+L)"
fi
PINNED="$(sed -n 's/^pinned_camera_path *= *"\(.*\)"/\1/p' "$CONF" 2>/dev/null | head -1)"
if [ -n "$PINNED" ]; then
    printf '  %-10s %s%s%s\n' "Camera" "pinned to USB port " "${PINNED##*/}" " (re-pin only if you move it)"
else
    printf '  %-10s %s\n' "Camera" "once unlock works: sudo ./pin-camera.sh /dev/videoN (face-enroll prints it)"
fi
printf '  %-10s %s\n' "Uninstall" "sudo ./uninstall.sh"
