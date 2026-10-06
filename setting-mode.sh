#!/bin/bash
set -euo pipefail

# The settings the tray's Settings menu changes in /etc/face-auth.toml.
#
#   set KEY CHOICE   write one choice (root)
#   status           print "KEY CHOICE" per setting; CHOICE is "custom" when
#                    the file holds a value that is not one of the menu's
#
# "security" is a preset over four keys (liveness, match, minface, delay),
# which are not settable on their own; it reads as "custom" unless all four
# match one preset.
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

KEYS="security scan confirm updates"

# parts KEY CHOICE: the "part=choice" pairs a choice writes. Fails for an
# unknown key or choice.
parts() {
    case "$1" in
        security)
            case "$2" in
                convenient) echo "liveness=standard match=relaxed minface=off delay=off" ;;
                balanced)   echo "liveness=standard match=standard minface=off delay=2s" ;;
                strict)     echo "liveness=strict match=strict minface=near delay=5s" ;;
                *) return 1 ;;
            esac ;;
        scan|confirm|updates)
            spec "$1"
            case " $CHOICES " in
                *" $2="*) echo "$1=$2" ;;
                *) return 1 ;;
            esac ;;
        *) return 1 ;;
    esac
}

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

# The choice of a key from KEYS. A preset is the one whose parts all read
# back as written.
current_key() {
    local choice part have=""
    if [ "$1" != security ]; then
        spec "$1"
        current
        return
    fi
    for part in $(parts security balanced); do
        spec "${part%%=*}"
        have="$have ${part%%=*}=$(current)"
    done
    for choice in convenient balanced strict; do
        [ "$(parts security "$choice")" = "${have# }" ] && { echo "$choice"; return; }
    done
    echo custom
}

status() {
    local key
    for key in $KEYS; do
        echo "$key $(current_key "$key")"
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
PARTS="$(parts "$KEY" "$CHOICE")" || { echo "Unknown setting or choice: $KEY $CHOICE" >&2; exit 2; }

if [ "$EUID" -ne 0 ] && [ -z "${FACE_AUTH_SETTINGS_CONF:-}" ]; then
    echo "Must run as root: sudo $0 set $KEY $CHOICE" >&2
    exit 1
fi

# Drop the parts' active lines, then add ours at the end (plain keys, as
# deploy.sh does, so it never lands inside a table). Written beside the file and
# renamed over it, keeping its owner and mode, so a preset lands all at once.
DROP=""
LINES=""
for part in $PARTS; do
    spec "${part%%=*}"
    for pair in $CHOICES; do
        [ "${pair%%=*}" = "${part#*=}" ] && VALUE="${pair#*=}"
    done
    for name in $FLAT $TOML; do
        DROP="$DROP${name//./[.]}|"
    done
    if [ "$QUOTED" = 1 ]; then
        LINES="$LINES$TOML = \"$VALUE\""$'\n'
    else
        LINES="$LINES$TOML = $VALUE"$'\n'
    fi
done
TMP="$(mktemp "$CONF.XXXXXX")"
trap 'rm -f "$TMP"' EXIT
if [ -f "$CONF" ]; then
    grep -vE "^[[:space:]]*(${DROP%|})[[:space:]]*=" "$CONF" > "$TMP" || true
    chmod --reference="$CONF" "$TMP"
    chown --reference="$CONF" "$TMP"
else
    chmod 0644 "$TMP"
fi
printf '%s' "$LINES" >> "$TMP"
mv "$TMP" "$CONF"
trap - EXIT

echo "$KEY: $CHOICE"
