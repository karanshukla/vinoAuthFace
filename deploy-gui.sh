#!/bin/bash
set -euo pipefail

SHARE_DIR="/usr/local/share/face-auth"

# ---- Determine actual user (handles sudo) ----
if [ -n "${SUDO_USER:-}" ]; then
    ACTUAL_USER="$SUDO_USER"
    ACTUAL_HOME=$(getent passwd "$SUDO_USER" | cut -d: -f6)
else
    ACTUAL_USER="$USER"
    ACTUAL_HOME="$HOME"
fi

# ---- Check if /usr is writable (immutable FS detection) ----
USR_WRITABLE=false
if touch /usr/share/.face-auth-write-test 2>/dev/null; then
    rm -f /usr/share/.face-auth-write-test
    USR_WRITABLE=true
fi

if [ "$USR_WRITABLE" = true ]; then
    BIN_DIR="/usr/local/bin"
    APP_DIR="/usr/share/applications"
    ICON_DIR="/usr/share/icons/hicolor/scalable/apps"
    DATA_DIR="/usr/local/share/face-auth-gtk"
else
    echo "Detected read-only /usr, using per-user paths for $ACTUAL_USER..."
    BIN_DIR="${XDG_BIN_HOME:-$ACTUAL_HOME/.local/bin}"
    APP_DIR="${XDG_DATA_HOME:-$ACTUAL_HOME/.local/share}/applications"
    ICON_DIR="${XDG_DATA_HOME:-$ACTUAL_HOME/.local/share}/icons/hicolor/scalable/apps"
    DATA_DIR="${XDG_DATA_HOME:-$ACTUAL_HOME/.local/share}/face-auth-gtk"
fi

# ---- Ensure PATH includes our bin dir ----
if ! echo "$PATH" | tr ':' '\n' | grep -qx "$BIN_DIR"; then
    echo "Note: $BIN_DIR is not in PATH."
    echo "Add this to ~/.bashrc or ~/.zshrc for $ACTUAL_USER:"
    echo "  export PATH=\"\$PATH:$BIN_DIR\""
fi

# ---- Build (non-musl, dynamic GTK) ----
if command -v cargo &>/dev/null; then
    echo "Building face-auth-gtk..."
    cargo build --release -p face-auth-gtk
elif [ -f "target/release/face-auth-gtk" ]; then
    echo "Using pre-built binary from target/release/"
else
    echo "Error: cargo not found and no pre-built binary in target/release/"
    echo "Build first: cargo build --release -p face-auth-gtk"
    exit 1
fi

# ---- Install binary ----
echo "Installing binary to $BIN_DIR/face-auth-gtk..."
install -Dm755 target/release/face-auth-gtk "$BIN_DIR/face-auth-gtk"

# ---- Install .desktop file ----
echo "Installing desktop file to $APP_DIR/..."
install -Dm644 data/com.github.pfalkingham.face-auth-gtk.desktop "$APP_DIR/com.github.pfalkingham.face-auth-gtk.desktop"

# ---- Install icon ----
echo "Installing icon to $ICON_DIR/..."
install -Dm644 data/com.github.pfalkingham.face-auth-gtk.svg "$ICON_DIR/com.github.pfalkingham.face-auth-gtk.svg"

# ---- Ensure model directory exists ----
echo "Ensuring model directory exists..."
mkdir -p "$SHARE_DIR"

echo ""
echo "=== GUI install complete! ==="
echo ""
echo "Launch from the application menu: Face Authentication Settings"
echo "Or run: face-auth-gtk (if $BIN_DIR is in PATH)"
