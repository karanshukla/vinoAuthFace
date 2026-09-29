#!/bin/bash
set -euo pipefail

PURGE=false

for arg in "$@"; do
    case "$arg" in
        --purge) PURGE=true ;;
    esac
done

# ---- Detect actual user (handles sudo) ----
if [ -n "${SUDO_USER:-}" ]; then
    ACTUAL_USER="$SUDO_USER"
    ACTUAL_HOME=$(getent passwd "$SUDO_USER" | cut -d: -f6)
else
    ACTUAL_USER="${USER:-$(id -un)}"
    ACTUAL_HOME="${HOME:-$(getent passwd "$ACTUAL_USER" | cut -d: -f6)}"
fi

# ---- Check if /usr is writable (immutable FS detection) ----
USR_WRITABLE=false
if touch /usr/share/.face-auth-write-test 2>/dev/null; then
    rm -f /usr/share/.face-auth-write-test
    USR_WRITABLE=true
fi

BIN_DIR="/usr/local/bin"
SHARE_DIR="/usr/local/share/face-auth"
COMPLETION_DIR="/usr/local/share/bash-completion/completions"
NPU_CACHE_DIR="/var/cache/face-auth"
CONFIG_DIR="/etc"
PAM_DIR="/etc/pam.d"

if [ "$USR_WRITABLE" = true ]; then
    ICON_DIR="/usr/share/icons/hicolor/scalable/apps"
    APP_DIR="/usr/share/applications"
    GUI_DATA_DIR="/usr/local/share/face-auth-gtk"
else
    ICON_DIR="${XDG_DATA_HOME:-$ACTUAL_HOME/.local/share}/icons/hicolor/scalable/apps"
    APP_DIR="${XDG_DATA_HOME:-$ACTUAL_HOME/.local/share}/applications"
    GUI_DATA_DIR="${XDG_DATA_HOME:-$ACTUAL_HOME/.local/share}/face-auth-gtk"
fi

echo "Removing binaries..."
rm -f "$BIN_DIR/face-auth"
rm -f "$BIN_DIR/face-enroll" "$COMPLETION_DIR/face-enroll"

# The tray (installed by deploy.sh unless --no-tray). A running tray exits on its own when its
# uninstall entry finishes; one started some other way keeps running until
# logout, with nothing left for it to call.
rm -f "$BIN_DIR/face-auth-tray" /usr/local/libexec/face-auth-helper
rm -f /usr/share/polkit-1/actions/io.github.karanshukla.vinoauthface.policy
rm -f /usr/local/share/polkit-1/actions/io.github.karanshukla.vinoauthface.policy
rm -f /etc/xdg/autostart/vinoauthface-tray.desktop
rm -f /usr/local/share/applications/vinoauthface-tray.desktop
rm -f /usr/local/share/applications/vinoauthface-enrol.desktop
rm -f /usr/local/share/icons/hicolor/scalable/apps/vinoauthface.svg
rm -f /usr/local/share/icons/hicolor/symbolic/apps/vinoauthface{,-scanning,-attention}-symbolic.svg
rm -rf "$NPU_CACHE_DIR"

# Only remove the Bitwarden action if deploy.sh wrote it, not one Bitwarden
# or the admin installed. The marker lives in $SHARE_DIR, so check first.
if [ -f "$SHARE_DIR/.bitwarden-policy-installed" ]; then
    echo "Removing Bitwarden polkit action installed by face-auth..."
    rm -f /usr/share/polkit-1/actions/com.bitwarden.Bitwarden.policy
fi

# ovfetch installs leave no ld.so.conf entry (face-auth carries an rpath),
# so check the directory itself.
if [ -d /usr/local/lib/face-auth ] || [ -f /etc/ld.so.conf.d/face-auth-openvino.conf ]; then
    echo "Removing bundled OpenVINO runtime..."
    rm -rf /usr/local/lib/face-auth
    if [ -f /etc/ld.so.conf.d/face-auth-openvino.conf ]; then
        rm -f /etc/ld.so.conf.d/face-auth-openvino.conf
        ldconfig 2>/dev/null || true
    fi
fi

echo "Removing model and SELinux policy..."
rm -rf "$SHARE_DIR"

echo "Removing config..."
rm -f "$CONFIG_DIR/face-auth.toml"

if [ -f /etc/udev/rules.d/99-face-auth-camera.rules ]; then
    echo "Removing pinned-camera udev rule..."
    rm -f /etc/udev/rules.d/99-face-auth-camera.rules
    udevadm control --reload-rules 2>/dev/null || true
fi

# The GTK settings GUI and GNOME scan indicator were removed from this repo;
# clean up anything an older install left behind.
echo "Removing any leftover GUI files from an older install..."
rm -f "$BIN_DIR/face-auth-gtk"
rm -f "${XDG_BIN_HOME:-$ACTUAL_HOME/.local/bin}/face-auth-gtk"
rm -f "$APP_DIR/com.github.pfalkingham.face-auth-gtk.desktop"
rm -f "${XDG_DATA_HOME:-$ACTUAL_HOME/.local/share}/applications/com.github.pfalkingham.face-auth-gtk.desktop"
rm -f "$ICON_DIR/com.github.pfalkingham.face-auth-gtk.svg"
rm -f "${XDG_DATA_HOME:-$ACTUAL_HOME/.local/share}/icons/hicolor/scalable/apps/com.github.pfalkingham.face-auth-gtk.svg"
rm -rf "$GUI_DATA_DIR"
rm -rf "${XDG_DATA_HOME:-$ACTUAL_HOME/.local/share}/face-auth-gtk"
rm -rf "${XDG_DATA_HOME:-$ACTUAL_HOME/.local/share}/gnome-shell/extensions/authface-scan-indicator@samvivan.local"

echo "Restoring PAM configs..."
for service in sudo swaylock gdm-password polkit-1 kde-fingerprint; do
    conf="$PAM_DIR/$service"
    if [ "$service" = polkit-1 ] && [ -f "$PAM_DIR/.face-auth-polkit-1-created" ]; then
        # deploy.sh created this override from the vendor default; removing
        # it restores exactly what polkit used before.
        rm -f "$conf" "$conf.face-auth.bak" "$PAM_DIR/.face-auth-polkit-1-created"
        echo "Removed $conf (created by face-auth)"
        continue
    fi
    [ -f "$conf" ] || continue
    if [ -f "$conf.face-auth.bak" ]; then
        mv "$conf.face-auth.bak" "$conf"
        echo "Restored $conf from backup"
    else
        # Anchored to our own stanza: a bare /face-auth/d would delete any
        # unrelated line that happens to mention it.
        sed -i '/pam_exec\.so.*face-auth/d' "$conf" 2>/dev/null || true
        echo "Cleaned $conf"
    fi
done

echo "Removing SELinux policy module..."
semodule -r face_auth 2>/dev/null || true

echo ""
if [ "$PURGE" = true ]; then
    echo "Removing user embeddings..."
    rm -rf /var/lib/face-auth/
    # The group only exists for the store; with the store gone nothing owns it.
    getent group face-auth >/dev/null && groupdel face-auth
    echo "Uninstall complete (including user embeddings)."
else
    echo "Uninstall complete!"
    echo "Note: User embeddings in /var/lib/face-auth/ were preserved."
    echo "Remove them manually with: sudo rm -rf /var/lib/face-auth/"
    echo "Or run again with --purge to remove them automatically."
fi
