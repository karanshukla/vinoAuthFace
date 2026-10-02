#!/bin/bash
set -euo pipefail

PURGE=false

USAGE="Usage: sudo ./uninstall.sh [--purge]"
for arg in "$@"; do
    case "$arg" in
        --purge) PURGE=true ;;
        *) printf 'Unknown option %s\n%s\n' "'$arg'" "$USAGE" >&2; exit 1 ;;
    esac
done

if [ "$(id -u)" -ne 0 ]; then
    echo "Run this with sudo: it removes files from /usr/local, /etc and /var/lib." >&2
    exit 1
fi

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
ZSH_COMPLETION_DIR="/usr/local/share/zsh/site-functions"
FISH_COMPLETION_DIR="/usr/local/share/fish/vendor_completions.d"
NPU_CACHE_DIR="/var/cache/face-auth"
CONFIG_DIR="/etc"
PAM_DIR="/etc/pam.d"
PAM_SERVICES="sudo swaylock gdm-password polkit-1 kde-fingerprint plasmalogin-fingerprint plasmalogin cosmic-greeter"

if [ "$USR_WRITABLE" = true ]; then
    ICON_DIR="/usr/share/icons/hicolor/scalable/apps"
    APP_DIR="/usr/share/applications"
    GUI_DATA_DIR="/usr/local/share/face-auth-gtk"
else
    ICON_DIR="${XDG_DATA_HOME:-$ACTUAL_HOME/.local/share}/icons/hicolor/scalable/apps"
    APP_DIR="${XDG_DATA_HOME:-$ACTUAL_HOME/.local/share}/applications"
    GUI_DATA_DIR="${XDG_DATA_HOME:-$ACTUAL_HOME/.local/share}/face-auth-gtk"
fi

# PAM first, while the binaries are still there. A required line (login-mode
# both) pointing at a missing vinoauthface-auth fails every login, so if
# anything below stops the script, PAM must already be clean. No `|| true`
# on the edits for the same reason: better to stop here with the binaries
# in place than carry on and remove them.
echo "Removing vinoAuthFace from PAM configs..."
for service in $PAM_SERVICES; do
    conf="$PAM_DIR/$service"
    marker="$PAM_DIR/.face-auth-$service-created"
    if [ -f "$marker" ]; then
        # deploy.sh created this override from the vendor default. Remove it
        # only if it is still what deploy.sh wrote; if the user has edited
        # it since, strip our line and keep their changes. A marker with no
        # recorded hash (older deploy) falls back to comparing against the
        # vendor file once our line is gone.
        recorded=$(cat "$marker" 2>/dev/null)
        if [ -n "$recorded" ]; then
            unchanged=false
            [ "$(sha256sum "$conf" 2>/dev/null | cut -d' ' -f1)" = "$recorded" ] && unchanged=true
        else
            unchanged=false
            if [ -f "/usr/lib/pam.d/$service" ] && [ -f "$conf" ] \
               && sed '/pam_exec\.so.*face-auth/d' "$conf" | cmp -s - "/usr/lib/pam.d/$service"; then
                unchanged=true
            fi
        fi
        if [ "$unchanged" = true ] || [ ! -f "$conf" ]; then
            rm -f "$conf" "$conf.face-auth.bak" "$marker"
            echo "Removed $conf (created by vinoAuthFace)"
        else
            sed -i '/pam_exec\.so.*face-auth/d' "$conf"
            rm -f "$conf.face-auth.bak" "$marker"
            echo "Kept $conf (edited since deploy); removed only the vinoAuthFace line"
        fi
        continue
    fi
    # Strip our line rather than restoring the .face-auth.bak: the backup is
    # from before deploy.sh's last run, so putting it back would undo any
    # distro update or admin edit since. Anchored to our own stanza: a bare
    # /face-auth/d would delete any unrelated line that happens to mention it.
    if [ -f "$conf" ]; then
        sed -i '/pam_exec\.so.*face-auth/d' "$conf"
        echo "Cleaned $conf"
    fi
    rm -f "$conf.face-auth.bak"
done

echo "Removing binaries..."
rm -f "$BIN_DIR/vinoauthface-auth" "$BIN_DIR/face-auth"
rm -f /usr/local/libexec/vinoauthface-unseal
rm -f "$BIN_DIR/vinoauthface" "$BIN_DIR/vinoauthface-upgrade" "$BIN_DIR/vinoauthface-update" "$COMPLETION_DIR/vinoauthface" "$BIN_DIR/face-enroll" "$COMPLETION_DIR/face-enroll"
rm -f "$ZSH_COMPLETION_DIR/_vinoauthface" "$FISH_COMPLETION_DIR/vinoauthface.fish"

# The tray (installed by deploy.sh unless --no-tray). A running tray exits on its own when its
# uninstall entry finishes; one started some other way keeps running until
# logout, with nothing left for it to call.
rm -f "$BIN_DIR/vinoauthface-tray" /usr/local/libexec/vinoauthface-helper \
    "$BIN_DIR/face-auth-tray" /usr/local/libexec/face-auth-helper
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
    echo "Removing Bitwarden polkit action installed by vinoAuthFace..."
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

# Release sources vinoauthface-upgrade unpacked, and its build directory. As
# the user under sudo: the path is in their home, so root resolving it could
# be steered elsewhere by a symlinked ~/.cache.
if [ -n "${SUDO_USER:-}" ]; then
    sudo -u "$SUDO_USER" -H rm -rf -- "$ACTUAL_HOME/.cache/vinoauthface" \
        || echo "Could not remove $ACTUAL_HOME/.cache/vinoauthface; delete it by hand." >&2
else
    rm -rf -- "$ACTUAL_HOME/.cache/vinoauthface"
fi

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

echo "Removing SELinux policy module..."
semodule -r face_auth 2>/dev/null || true
# The kept store's types went with the module; back to the stock labels.
if command -v restorecon &>/dev/null; then
    [ -d /var/lib/face-auth ] && restorecon -R /var/lib/face-auth 2>/dev/null || true
    [ -d /dev/accel ] && restorecon -R /dev/accel 2>/dev/null || true
fi

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
