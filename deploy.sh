#!/bin/bash
set -euo pipefail

# Recognition model registry. "mbf" (MobileFaceNet, buffalo_sc pack) is the default: small
# (~14MB) and fast, chosen for interactive PAM latency. "r50" (ResNet50, buffalo_l pack) is
# larger (~175MB) and noticeably more accurate (wider genuine-match similarity margin observed
# in local benchmarking) at a small, fixed extra cost per auth attempt (a few ms/frame on top of
# mbf's ~1ms, plus a one-time NPU compile-cache-miss the first time it's ever loaded) — not a
# scan-time regression, since the scan loop is paced by camera frame interval either way.
# Select with: FACE_AUTH_RECOGNITION_MODEL=r50 sudo -E ./deploy.sh
#
# Switching models requires re-enrolling (`face-enroll --user $USER`) — different recognition
# models produce numerically incompatible embedding spaces despite the same 512-d shape, so
# face-auth-core refuses to compare embeddings across a model change rather than silently
# producing meaningless similarity scores (see crates/face-auth-core/src/storage.rs model_tag).
RECOGNITION_MODEL="${FACE_AUTH_RECOGNITION_MODEL:-mbf}"
case "$RECOGNITION_MODEL" in
    mbf)
        MODEL_URL="https://github.com/deepinsight/insightface/releases/download/v0.7/buffalo_sc.zip"
        MODEL_NAME="w600k_mbf.onnx"
        MODEL_CHECKSUM="9cc6e4a75f0e2bf0b1aed94578f144d15175f357bdc05e815e5c4a02b319eb4f"
        ;;
    r50)
        MODEL_URL="https://github.com/deepinsight/insightface/releases/download/v0.7/buffalo_l.zip"
        MODEL_NAME="w600k_r50.onnx"
        MODEL_CHECKSUM="4c06341c33c2ca1f86781dab0e829f88ad5b64be9fba56e56bc9ebdefc619e43"
        ;;
    *)
        echo "Error: unknown FACE_AUTH_RECOGNITION_MODEL '$RECOGNITION_MODEL' (expected 'mbf' or 'r50')"
        exit 1
        ;;
esac

BIN_DIR="/usr/local/bin"
SHARE_DIR="/usr/local/share/face-auth"
CONFIG_DIR="/etc"
PAM_DIR="/etc/pam.d"
VAR_DIR="/var/lib/face-auth"
SELINUX_DIR="/usr/local/share/face-auth/selinux"
OPENVINO_INSTALL_DIR="/usr/local/lib/face-auth/openvino"

ACTUAL_USER="${SUDO_USER:-$USER}"
ACTUAL_HOME=$(getent passwd "$ACTUAL_USER" | cut -d: -f6)

# ---- Undo any previous partial setup ----
echo "Cleaning up any previous partial setup..."

for service in sudo swaylock gdm-password polkit-1; do
    if [ -f "$PAM_DIR/$service" ]; then
        sed -i '/^auth\s\s*sufficient\s\s*pam_exec\.so.*face-auth/d' "$PAM_DIR/$service" 2>/dev/null || true
    fi
done

# ---- Detect OpenVINO NPU toolchain ----
# The `npu` feature dynamically links against OpenVINO's own .so files, which are built
# for glibc — that's incompatible with the plain musl (fully static) build used otherwise,
# so an NPU build switches target and needs the OpenVINO runtime libraries installed
# system-wide (see below) since PAM invokes this binary with no shell profile sourced,
# so LD_LIBRARY_PATH / setupvars.sh being sourced interactively doesn't help it at runtime.
OPENVINO_SRC=$(find "$ACTUAL_HOME/.local/opt" -maxdepth 1 -iname "openvino_toolkit_*" -type d 2>/dev/null | sort -V | tail -1)
NPU_BUILD=0
if [ -n "$OPENVINO_SRC" ] && [ -f "$OPENVINO_SRC/setupvars.sh" ]; then
    NPU_BUILD=1
fi

# Locate cargo explicitly: under `sudo`, root's PATH does not include a rustup user
# install (~/.cargo/bin), so `command -v cargo` silently fails as root even though the
# actual user has it — that failure used to fall through to stale pre-built binaries
# without anyone noticing, so it's resolved up front instead of relying on PATH.
CARGO_BIN=$(command -v cargo || true)
if [ -z "$CARGO_BIN" ] && [ -x "$ACTUAL_HOME/.cargo/bin/cargo" ]; then
    CARGO_BIN="$ACTUAL_HOME/.cargo/bin/cargo"
fi

# ---- Build ----
# Building as root doesn't work here even with cargo's path resolved: rustup's shim
# needs a default toolchain configured under $HOME/.rustup, which only exists for the
# actual user, not root. So the build itself runs as $ACTUAL_USER (via sudo -u -H, which
# gives it that user's real $HOME); only the install steps below need root.
NPU_ACTIVE=0
if [ -n "$CARGO_BIN" ]; then
    if [ "$NPU_BUILD" = "1" ]; then
        echo "OpenVINO found at $OPENVINO_SRC — building with NPU backend (host/glibc target)..."
        sudo -u "$ACTUAL_USER" -H bash -c "
            set -eo pipefail
            source '$OPENVINO_SRC/setupvars.sh' >/dev/null
            set -u
            '$CARGO_BIN' build --release --features face-auth-core/npu,face-auth/npu,face-enroll/npu -p face-auth -p face-enroll
        "
        FACE_AUTH_BIN="target/release/face-auth"
        FACE_ENROLL_BIN="target/release/face-enroll"
        NPU_ACTIVE=1
    else
        echo "No OpenVINO toolkit found under $ACTUAL_HOME/.local/opt — building CPU-only (musl, static)..."
        sudo -u "$ACTUAL_USER" -H "$CARGO_BIN" build --release --target x86_64-unknown-linux-musl -p face-auth -p face-enroll
        FACE_AUTH_BIN="target/x86_64-unknown-linux-musl/release/face-auth"
        FACE_ENROLL_BIN="target/x86_64-unknown-linux-musl/release/face-enroll"
    fi
elif [ -f "target/x86_64-unknown-linux-musl/release/face-auth" ]; then
    echo "Using pre-built musl binaries from target/ (cargo not found — note these predate NPU support if built before it existed)"
    FACE_AUTH_BIN="target/x86_64-unknown-linux-musl/release/face-auth"
    FACE_ENROLL_BIN="target/x86_64-unknown-linux-musl/release/face-enroll"
else
    echo "cargo not found and no pre-built binaries in target/ — downloading prebuilt release binaries instead..."
    RELEASE_REPO="karanshukla/vinoAuthFace"
    DL_DIR="/tmp/face-auth-release-bin"
    rm -rf "$DL_DIR"
    mkdir -p "$DL_DIR"

    # Prefer the release matching this exact checkout (if it's a tagged clone) over whatever's
    # currently "latest" — this script's own PAM-patching/config logic can change between tags,
    # so the binaries it installs should match the deploy.sh version actually running it.
    GIT_TAG=$(git describe --tags --exact-match 2>/dev/null || true)
    if [ -n "$GIT_TAG" ]; then
        DOWNLOAD_BASE="https://github.com/$RELEASE_REPO/releases/download/$GIT_TAG"
    else
        DOWNLOAD_BASE="https://github.com/$RELEASE_REPO/releases/latest/download"
    fi

    for bin in face-auth face-enroll; do
        asset="${bin}-x86_64-unknown-linux-musl"
        curl -fL -o "$DL_DIR/$asset" "$DOWNLOAD_BASE/$asset" || {
            echo "Error: failed to download $asset from $DOWNLOAD_BASE"
            echo "Either install a Rust toolchain (rustup target add x86_64-unknown-linux-musl)"
            echo "and re-run, or check that a release with prebuilt binaries exists at"
            echo "https://github.com/$RELEASE_REPO/releases"
            rm -rf "$DL_DIR"
            exit 1
        }
    done
    curl -fL -o "$DL_DIR/SHA256SUMS" "$DOWNLOAD_BASE/SHA256SUMS" || {
        echo "Error: failed to download SHA256SUMS from $DOWNLOAD_BASE"
        rm -rf "$DL_DIR"
        exit 1
    }

    echo "Verifying checksums..."
    (cd "$DL_DIR" && sha256sum -c --ignore-missing SHA256SUMS) || {
        echo "Error: checksum mismatch! The downloaded binaries may be corrupted or tampered."
        rm -rf "$DL_DIR"
        exit 1
    }

    chmod +x "$DL_DIR/face-auth-x86_64-unknown-linux-musl" "$DL_DIR/face-enroll-x86_64-unknown-linux-musl"
    FACE_AUTH_BIN="$DL_DIR/face-auth-x86_64-unknown-linux-musl"
    FACE_ENROLL_BIN="$DL_DIR/face-enroll-x86_64-unknown-linux-musl"
fi

# Fail loudly rather than silently install a binary that doesn't match what we intended —
# this is exactly the class of mismatch (config says openvino, binary is CPU-only) that
# caused a confusing runtime error the first time this was wired up.
if [ "$NPU_ACTIVE" = "1" ] && ! ldd "$FACE_AUTH_BIN" | grep -q libopenvino; then
    echo "Error: NPU build was attempted but $FACE_AUTH_BIN has no OpenVINO linkage — aborting install."
    exit 1
fi

# ---- Install binaries ----
echo "Installing binaries..."
install -Dm755 "$FACE_AUTH_BIN" "$BIN_DIR/face-auth"
install -Dm755 "$FACE_ENROLL_BIN" "$BIN_DIR/face-enroll"

# ---- Install OpenVINO runtime libraries system-wide (NPU builds only) ----
if [ "$NPU_ACTIVE" = "1" ]; then
    echo "Installing OpenVINO runtime libraries to $OPENVINO_INSTALL_DIR..."
    # The whole intel64 directory is copied as a unit (not just libopenvino*.so):
    # OpenVINO auto-discovers its CPU/NPU/GPU plugins and the ONNX frontend by scanning
    # the directory libopenvino.so itself lives in, so they all have to stay co-located.
    mkdir -p "$OPENVINO_INSTALL_DIR/intel64" "$OPENVINO_INSTALL_DIR/tbb"
    cp -a "$OPENVINO_SRC/runtime/lib/intel64/." "$OPENVINO_INSTALL_DIR/intel64/"
    cp -a "$OPENVINO_SRC/runtime/3rdparty/tbb/lib/." "$OPENVINO_INSTALL_DIR/tbb/"
    cat > /etc/ld.so.conf.d/face-auth-openvino.conf <<EOF
$OPENVINO_INSTALL_DIR/intel64
$OPENVINO_INSTALL_DIR/tbb
EOF
    ldconfig
    echo "OpenVINO runtime libraries registered system-wide via ldconfig"
fi

# ---- Install recognition model (mbf or r50, selected above via RECOGNITION_MODEL) ----
echo "Installing recognition model ($RECOGNITION_MODEL: $MODEL_NAME)..."
if [ -f "$SHARE_DIR/$MODEL_NAME" ]; then
    echo "Model already installed at $SHARE_DIR/$MODEL_NAME"
elif [ -f "models/$MODEL_NAME" ]; then
    install -Dm644 "models/$MODEL_NAME" "$SHARE_DIR/$MODEL_NAME"
    echo "Installed model from models/$MODEL_NAME"
else
    echo "Downloading model from InsightFace..."
    mkdir -p /tmp/face-auth-model
    curl -L -o /tmp/face-auth-model/model.zip "$MODEL_URL"
    unzip -o /tmp/face-auth-model/model.zip -d /tmp/face-auth-model/
    echo "Verifying checksum..."
    echo "$MODEL_CHECKSUM  /tmp/face-auth-model/$MODEL_NAME" | sha256sum -c - || {
        echo "Error: Checksum mismatch! The model may be corrupted or tampered."
        rm -rf /tmp/face-auth-model
        exit 1
    }
    install -Dm644 "/tmp/face-auth-model/$MODEL_NAME" "$SHARE_DIR/$MODEL_NAME"
    rm -rf /tmp/face-auth-model
    echo "Model downloaded and installed"
fi

# ---- Install face detector model ----
echo "Installing face detector model..."
DETECTOR_NAME="version-slim-320.onnx"
if [ -f "$SHARE_DIR/$DETECTOR_NAME" ]; then
    echo "Detector model already installed at $SHARE_DIR/$DETECTOR_NAME"
elif [ -f "models/$DETECTOR_NAME" ]; then
    install -Dm644 "models/$DETECTOR_NAME" "$SHARE_DIR/$DETECTOR_NAME"
    echo "Installed detector model from models/$DETECTOR_NAME"
else
    echo "Error: $DETECTOR_NAME not found in models/"
    echo "Run inside the dev distrobox: python3 -m onnxsim version-slim-320.onnx version-slim-320.onnx"
    exit 1
fi

echo "Installing config..."
if [ -f "$CONFIG_DIR/face-auth.toml" ]; then
    echo "Config already exists at $CONFIG_DIR/face-auth.toml — leaving your settings (device, threshold, etc.) as-is."
else
    install -Dm644 config/face-auth.toml.example "$CONFIG_DIR/face-auth.toml"
fi

# The backend line is always kept in sync with what was actually built, regardless of
# whether the config is fresh or pre-existing — a mismatch here (config says openvino,
# binary doesn't have it, or vice versa) is exactly the bug that bit us the first time
# this was wired up, so it's not left to chance either way.
if [ "$NPU_ACTIVE" = "1" ]; then
    if grep -q '^backend' "$CONFIG_DIR/face-auth.toml"; then
        sed -i 's/^backend.*/backend = "openvino"/' "$CONFIG_DIR/face-auth.toml"
    elif grep -q '^# backend' "$CONFIG_DIR/face-auth.toml"; then
        sed -i 's/^# backend = "tract"/backend = "openvino"/' "$CONFIG_DIR/face-auth.toml"
    else
        # A `#`-prefixed line makes TOML treat everything after it — including whatever `>>`
        # concatenates onto it if the file doesn't already end in a newline — as part of that
        # same comment, silently dropping the line being appended instead of erroring.
        [ -s "$CONFIG_DIR/face-auth.toml" ] && [ "$(tail -c1 "$CONFIG_DIR/face-auth.toml" | wc -l)" -eq 0 ] && echo >> "$CONFIG_DIR/face-auth.toml"
        echo 'backend = "openvino"' >> "$CONFIG_DIR/face-auth.toml"
    fi
    echo "Set backend = \"openvino\" in $CONFIG_DIR/face-auth.toml (binary was built with NPU support)"
elif grep -q '^backend\s*=\s*"openvino"' "$CONFIG_DIR/face-auth.toml" 2>/dev/null; then
    sed -i 's/^backend.*/backend = "tract"/' "$CONFIG_DIR/face-auth.toml"
    echo "Warning: NPU build not active this run — reverted backend to \"tract\" in $CONFIG_DIR/face-auth.toml"
fi

# Keep model_path in face-auth.toml pointed at whatever RECOGNITION_MODEL was actually
# installed this run — only touched when a non-default model is explicitly requested, so a
# plain (default) `sudo ./deploy.sh` never rewrites a user's existing config, matching how
# the rest of this script treats an already-present /etc/face-auth.toml as hands-off.
if [ "$RECOGNITION_MODEL" != "mbf" ]; then
    if grep -q '^model_path' "$CONFIG_DIR/face-auth.toml"; then
        sed -i "s@^model_path.*@model_path = \"$SHARE_DIR/$MODEL_NAME\"@" "$CONFIG_DIR/face-auth.toml"
    elif grep -q '^# model_path' "$CONFIG_DIR/face-auth.toml"; then
        # Delimiter is @, not # — the pattern itself contains a literal '#' (matching the
        # commented-out example line), which would otherwise terminate the sed pattern early
        # and get misparsed as trailing flags ("unknown option to `s'").
        sed -i "s@^# model_path = .*@model_path = \"$SHARE_DIR/$MODEL_NAME\"@" "$CONFIG_DIR/face-auth.toml"
    else
        [ -s "$CONFIG_DIR/face-auth.toml" ] && [ "$(tail -c1 "$CONFIG_DIR/face-auth.toml" | wc -l)" -eq 0 ] && echo >> "$CONFIG_DIR/face-auth.toml"
        echo "model_path = \"$SHARE_DIR/$MODEL_NAME\"" >> "$CONFIG_DIR/face-auth.toml"
    fi
    echo "Set model_path = \"$SHARE_DIR/$MODEL_NAME\" in $CONFIG_DIR/face-auth.toml"
    echo "NOTE: switching recognition models requires re-enrolling: face-enroll --user $ACTUAL_USER"
fi

# ---- PAM setup ----
echo "Installing PAM configs..."

# polkit-1 usually has no /etc/pam.d override out of the box — it falls back to the vendor
# default (/usr/lib/pam.d/polkit-1, typically just `include system-auth`). Materialize that
# default as a real file first, so the loop below has something to back up and patch, and so
# uninstall.sh can tell "we created this from scratch" apart from "we edited an existing admin
# override" (the former gets deleted outright on uninstall, the latter gets restored).
if [ ! -f "$PAM_DIR/polkit-1" ]; then
    if [ -f "/usr/lib/pam.d/polkit-1" ]; then
        cp "/usr/lib/pam.d/polkit-1" "$PAM_DIR/polkit-1"
    else
        cat > "$PAM_DIR/polkit-1" <<'EOF'
#%PAM-1.0
auth       include      system-auth
account    include      system-auth
password   include      system-auth
session    include      system-auth
EOF
    fi
    touch "$PAM_DIR/.face-auth-polkit-1-created"
fi

for service in sudo swaylock gdm-password polkit-1; do
    conf="$PAM_DIR/$service"
    if [ ! -f "$conf" ]; then
        echo "Warning: $conf not found, skipping"
        continue
    fi
    cp "$conf" "$conf.face-auth.bak"
    sed -i '/pam_exec\.so.*face-auth/d' "$conf"

    if [ "$service" = "gdm-password" ]; then
        # Insert after pam_selinux_permit.so line (lock screen)
        sed -i '/^auth.*pam_selinux_permit\.so$/a auth       sufficient  pam_exec.so /usr/local/bin/face-auth' "$conf"
    else
        # Insert after #%PAM-1.0 (must remain first line)
        sed -i '/^#%PAM-1\.0/a auth       sufficient  pam_exec.so /usr/local/bin/face-auth' "$conf"
    fi
    echo "Updated $conf (backup at $conf.face-auth.bak)"
done

# ---- Bitwarden polkit policy (only if Bitwarden is actually installed) ----
# Bitwarden's Linux biometric unlock is a polkit action (com.bitwarden.Bitwarden.unlock,
# allow_active=auth_self) that re-authenticates the active user through the polkit-1 PAM
# service wired above. The action definition itself normally needs installing manually for
# Flatpak/Snap builds (sandboxed, can't self-install) — see
# https://bitwarden.com/help/biometrics/#tab-linux. Content below is transcribed verbatim
# from Bitwarden's own official source (the same string their native, non-sandboxed builds
# write via pkexec at runtime), not downloaded from a URL, so there's no separate integrity
# check needed: apps/desktop/src/key-management/biometrics/native-v2/os-biometrics-linux.service.ts
# in https://github.com/bitwarden/clients.
BITWARDEN_DETECTED=false
if command -v bitwarden &>/dev/null || command -v bitwarden-desktop &>/dev/null; then
    BITWARDEN_DETECTED=true
elif flatpak info com.bitwarden.desktop &>/dev/null 2>&1; then
    BITWARDEN_DETECTED=true
elif snap list bitwarden-desktop &>/dev/null 2>&1; then
    BITWARDEN_DETECTED=true
fi

if [ "$BITWARDEN_DETECTED" = true ]; then
    BW_POLICY="/usr/share/polkit-1/actions/com.bitwarden.Bitwarden.policy"
    if [ -f "$BW_POLICY" ]; then
        echo "Bitwarden polkit policy already installed at $BW_POLICY"
    else
        echo "Installing Bitwarden polkit action (enables Bitwarden's 'Unlock with system authentication', including via face-auth)..."
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
        command -v restorecon &>/dev/null && restorecon "$BW_POLICY" 2>/dev/null
        echo "Installed $BW_POLICY — polkitd picks up new actions automatically, no restart needed."
        echo "In Bitwarden: File > Settings > check 'Unlock with system authentication'."
    fi
else
    echo "Bitwarden not detected, skipping its polkit policy (polkit-1 PAM hook above still covers pkexec/other polkit prompts)."
fi

# ---- SELinux policy (for lock screen) ----
if command -v checkmodule &>/dev/null && command -v semodule_package &>/dev/null; then
    echo "Installing SELinux policy module for lock-screen camera access..."
    mkdir -p "$SELINUX_DIR"
    cp selinux/face-auth.te "$SELINUX_DIR/face_auth.te"
    checkmodule -M -m -o "$SELINUX_DIR/face_auth.mod" "$SELINUX_DIR/face_auth.te"
    semodule_package -o "$SELINUX_DIR/face_auth.pp" -m "$SELINUX_DIR/face_auth.mod"
    semodule -i "$SELINUX_DIR/face_auth.pp"
    echo "SELinux policy installed"
else
    echo "Warning: SELinux tools not found. To enable lock-screen support, install:"
    echo "  sudo dnf install policycoreutils"
    echo "Then compile and install the policy from selinux/face-auth.te"
fi

# ---- Embeddings directory ----
# $VAR_DIR itself stays 1777 (sticky, like /tmp) so any user can create their own
# subdirectory on first enroll without re-running this script as root. Each user's own
# subdirectory holds raw biometric embeddings and is locked to 0700/owner-only — face-auth
# also enforces this on every save (crates/face-auth-core/src/storage.rs), this is just
# belt-and-suspenders for the one this script creates directly.
echo "Creating embeddings directory..."
mkdir -p "$VAR_DIR/$ACTUAL_USER"
chmod 1777 "$VAR_DIR"
chown -R "$ACTUAL_USER:$ACTUAL_USER" "$VAR_DIR/$ACTUAL_USER"
chmod 700 "$VAR_DIR/$ACTUAL_USER"

echo ""
echo "=== Install complete! ==="
echo ""
echo "Run this command to enroll your face:"
echo ""
echo "  face-enroll --user $ACTUAL_USER"
echo ""
echo "Then test:"
echo "  sudo true           # should authenticate via face"
echo "  (lock screen: Super+L, then press a key to unlock)"
echo ""
echo "Once enrollment and a test unlock both work, run 'sudo ./pin-camera.sh' to lock"
echo "face-auth to this exact camera — without it, a spoofed USB device claiming the"
echo "same VID/PID could be used to inject frames. See README.md's Security & Limitations."
echo ""
echo "To uninstall: sudo ./uninstall.sh"
