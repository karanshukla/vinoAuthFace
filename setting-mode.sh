#!/bin/bash
set -euo pipefail

# The settings the tray's Settings menu changes in /etc/face-auth.toml.
#
#   set KEY CHOICE   write one choice (root)
#   status           print "KEY CHOICE" per setting; CHOICE is "custom" when
#                    the file holds a value that is not one of the menu's
#
# Only the keys and values below can be set: the tray's root helper runs this
# with a fixed pair, never a value from the caller. Everything else in the
# file (camera, models, lockout, the camera pin, the backend, ...) is system
# policy, deliberately not here. The menu's labels are in
# crates/face-auth-tray/src/helper.rs; a test there checks both lists agree.

CONF="${FACE_AUTH_SETTINGS_CONF:-/etc/face-auth.toml}"

usage() {
    echo "Usage: sudo $0 set KEY CHOICE, or $0 status" >&2
    exit 2
}

KEYS="liveness scan match minface delay confirm updates"

# spec KEY: sets TOML (the key written), FLAT (its flat spelling, which wins
# over a dotted one, or empty), DEFAULT (the choice when the file has no
# value), QUOTED (1 for a string) and CHOICES ("id=value ...").
spec() {
    FLAT=""
    QUOTED=0
    case "$1" in
        liveness) TOML="liveness.preset"; DEFAULT=standard; QUOTED=1
                  CHOICES="off=off standard=standard strict=strict" ;;
        scan)     TOML="scan_duration_ms"; DEFAULT=5s
                  CHOICES="3s=3000 5s=5000 8s=8000 12s=12000" ;;
        match)    TOML="threshold"; DEFAULT=standard
                  CHOICES="relaxed=0.5 standard=0.6 strict=0.7" ;;
        minface)  TOML="min_face_size_ratio"; DEFAULT=off
                  CHOICES="off=0.0 near=0.1 close=0.2" ;;
        delay)    TOML="guards.start_delay_ms"; FLAT="start_delay_ms"; DEFAULT=2s
                  CHOICES="off=0 1s=1000 2s=2000 5s=5000" ;;
        confirm)  TOML="guards.require_confirmation_elevation"; FLAT="require_confirmation_elevation"; DEFAULT=off
                  CHOICES="off=false on=true" ;;
        updates)  TOML="update_check"; DEFAULT=on
                  CHOICES="on=true off=false" ;;
        *) return 1 ;;
    esac
}

# The value written for a key in the file, flat spelling first. Empty if none.
read_value() {
    local name value
    for name in $FLAT $TOML; do
        value="$(sed -nE "s/^[[:space:]]*${name//./[.]}[[:space:]]*=[[:space:]]*\"?([^\"#[:space:]]*)\"?.*/\1/p" "$CONF" 2>/dev/null | tail -n1)"
        if [ -n "$value" ]; then
            echo "$value"
            return
        fi
    done
}

same() {
    case "$1$2" in
        *[!0-9.]*) [ "$1" = "$2" ] ;;
        *) awk -v a="$1" -v b="$2" 'BEGIN { exit !(a + 0 == b + 0) }' ;;
    esac
}

current() {
    local value pair
    value="$(read_value)"
    [ -n "$value" ] || { echo "$DEFAULT"; return; }
    for pair in $CHOICES; do
        if same "$value" "${pair#*=}"; then
            echo "${pair%%=*}"
            return
        fi
    done
    echo custom
}

status() {
    local key
    for key in $KEYS; do
        spec "$key"
        echo "$key $(current)"
    done
}

[ $# -ge 1 ] || usage
case "$1" in
    status) [ $# -eq 1 ] || usage; status; exit 0 ;;
    set) [ $# -eq 3 ] || usage ;;
    *) usage ;;
esac
KEY="$2"
CHOICE="$3"
spec "$KEY" || { echo "Unknown setting: $KEY" >&2; exit 2; }
VALUE=""
for pair in $CHOICES; do
    [ "${pair%%=*}" = "$CHOICE" ] && VALUE="${pair#*=}"
done
[ -n "$VALUE" ] || { echo "Unknown choice for $KEY: $CHOICE" >&2; exit 2; }

if [ "$EUID" -ne 0 ] && [ -z "${FACE_AUTH_SETTINGS_CONF:-}" ]; then
    echo "Must run as root: sudo $0 set $KEY $CHOICE" >&2
    exit 1
fi

# Drop the key's active lines, then add ours at the end (plain keys, as
# deploy.sh does, so it never lands inside a table). Written beside the file and
# renamed over it, keeping its owner and mode.
TMP="$(mktemp "$CONF.XXXXXX")"
trap 'rm -f "$TMP"' EXIT
if [ -f "$CONF" ]; then
    DROP="$(for name in $FLAT $TOML; do printf '%s|' "${name//./[.]}"; done)"
    grep -vE "^[[:space:]]*(${DROP%|})[[:space:]]*=" "$CONF" > "$TMP" || true
    chmod --reference="$CONF" "$TMP"
    chown --reference="$CONF" "$TMP"
else
    chmod 0644 "$TMP"
fi
if [ "$QUOTED" = 1 ]; then
    echo "$TOML = \"$VALUE\"" >> "$TMP"
else
    echo "$TOML = $VALUE" >> "$TMP"
fi
mv "$TMP" "$CONF"
trap - EXIT

echo "$KEY: $CHOICE"
