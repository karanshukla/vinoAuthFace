#!/bin/bash
set -euo pipefail

# Face unlock at the KDE login screen: Plasma Login, or SDDM where there's no
# Plasma Login (KDE on Arch, openSUSE, Debian and Kubuntu, Fedora before 45).
# See docs/pam.md's "Login screen".
#
#   off    password only (a fresh install's default)
#   both   password, then face: both must pass
#   face   face instead of the password
#   status print the current mode, or "none" without Plasma Login or SDDM
#
# deploy.sh and the tray's root helper both run this, so the login screen's
# PAM edits live in one place. Scoped to Plasma Login and SDDM on purpose: GDM
# and COSMIC share one service between their login and lock screens.

PAM_DIR="/etc/pam.d"
VENDOR_DIR="/usr/lib/pam.d"
FACE_AUTH="/usr/local/bin/vinoauthface-auth"
# Every service a mode can wire, so switching (or a changed display manager)
# never leaves a line behind.
SERVICES="plasmalogin-fingerprint plasmalogin sddm"
# The same as deploy.sh's: the first auth line that actually authenticates.
AUTHENTICATOR='^[[:space:]]*-?auth[[:space:]]+(include|substack)[[:space:]]|^[[:space:]]*-?auth[[:space:]].*pam_(unix|sss|fprintd|u2f)|^[[:space:]]*@include[[:space:]]'

usage() {
    echo "Usage: sudo $0 off|both|face, or $0 status" >&2
    exit 2
}

has_service() {
    [ -f "$PAM_DIR/$1" ] || [ -f "$VENDOR_DIR/$1" ]
}

# The login screen's service: the active display manager's when it's one of
# the two, else Plasma Login, else SDDM, else none.
greeter() {
    local dm service
    dm="$(readlink /etc/systemd/system/display-manager.service 2>/dev/null || true)"
    dm="${dm##*/}"
    dm="${dm%.service}"
    case "$dm" in
        plasmalogin|sddm) has_service "$dm" && { echo "$dm"; return; } ;;
    esac
    for service in plasmalogin sddm; do
        has_service "$service" && { echo "$service"; return; }
    done
    echo none
}

status() {
    local files=()
    for service in $SERVICES; do files+=("$PAM_DIR/$service"); done
    if [ "$(greeter)" = none ]; then
        echo none
    elif grep -qsE '^[[:space:]]*auth[[:space:]]+required[[:space:]]+pam_exec\.so.*face-auth' "${files[@]}"; then
        echo both
    elif grep -qs 'pam_exec\.so.*face-auth' "${files[@]}"; then
        echo face
    else
        echo off
    fi
}

# Strip our line, and delete an override deploy.sh or this script created
# once it's back to the vendor file.
unwire() {
    local conf="$PAM_DIR/$1" marker="$PAM_DIR/.face-auth-$1-created"
    [ -f "$conf" ] || return 0
    sed -i '/pam_exec\.so.*face-auth/d' "$conf"
    if [ -f "$marker" ] && cmp -s "$conf" "$VENDOR_DIR/$1"; then
        rm -f "$conf" "$marker"
    fi
    # Stale once our line is gone; uninstall.sh would restore it over later edits.
    rm -f "$conf.face-auth.bak"
}

# wire SERVICE CONTROL before|after: our line next to the first authenticating one.
wire() {
    local conf="$PAM_DIR/$1" marker="$PAM_DIR/.face-auth-$1-created" line_no
    if [ ! -f "$conf" ]; then
        cp "$VENDOR_DIR/$1" "$conf"
        touch "$marker"
    fi
    line_no=$(grep -nE "$AUTHENTICATOR" "$conf" | head -n1 | cut -d: -f1)
    if [ -z "$line_no" ]; then
        echo "$conf has no authenticating auth line; left it alone." >&2
        exit 1
    fi
    cp "$conf" "$conf.face-auth.bak"
    local line
    line="$(printf 'auth       %-11s pam_exec.so quiet stdout %s' "$2" "$FACE_AUTH")"
    if [ "$3" = before ]; then
        sed -i "${line_no}i $line" "$conf"
    else
        sed -i "${line_no}a $line" "$conf"
    fi
    if [ -f "$marker" ]; then
        sha256sum "$conf" | cut -d' ' -f1 > "$marker"
    fi
}

[ $# -eq 1 ] || usage
MODE="$1"
case "$MODE" in
    status) status; exit 0 ;;
    off|both|face) ;;
    *) usage ;;
esac

if [ "$EUID" -ne 0 ]; then
    echo "Must run as root: sudo $0 $MODE" >&2
    exit 1
fi
GREETER="$(greeter)"
if [ "$GREETER" = none ]; then
    [ "$MODE" = off ] && exit 0
    echo "Neither Plasma Login nor SDDM is installed (no plasmalogin or sddm PAM service)." >&2
    exit 1
fi

# The greeter unseals through vinoauthface-unseal's SELinux domain (#122). An
# install without it (a release from before it) can't, so a required scan
# would lock the login screen.
if [ "$(cat /sys/fs/selinux/enforce 2>/dev/null)" = 1 ] \
   && [ ! -x /usr/local/libexec/vinoauthface-unseal ] \
   && grep -qE '^[[:space:]]*seal_embeddings[[:space:]]*=[[:space:]]*true' /etc/face-auth.toml 2>/dev/null; then
    if [ "$MODE" = both ]; then
        echo "Refusing both: sealed templates need /usr/local/libexec/vinoauthface-unseal at the login screen under SELinux, and it isn't installed, so no one could log in. Re-run deploy.sh." >&2
        exit 1
    fi
    if [ "$MODE" = face ]; then
        echo "Note: without /usr/local/libexec/vinoauthface-unseal, sealed templates can't be unsealed at the login screen under SELinux; it will fall back to the password." >&2
    fi
fi

for service in $SERVICES; do
    unwire "$service"
done

case "$MODE" in
    # sufficient, above the password: a match logs in; anything else falls
    # through to the password. -fingerprint where there is one, which the
    # greeter runs hands-free beside the password field. SDDM has none.
    face)
        if [ "$GREETER" = plasmalogin ] && has_service plasmalogin-fingerprint; then
            wire plasmalogin-fingerprint sufficient before
        else
            wire "$GREETER" sufficient before
        fi
        ;;
    # required, below the password: both must pass, and the password still
    # reaches pam_kwallet5 to unlock the wallet.
    both)
        wire "$GREETER" required after
        ;;
esac

echo "Login screen ($GREETER): $MODE"
