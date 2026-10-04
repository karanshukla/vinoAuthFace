#!/bin/bash
set -euo pipefail

# Motion liveness preset in /etc/face-auth.toml. See docs/configuration.md.
#
#   off       no motion check: a photo or screen replay can unlock
#   standard  the default
#   strict    also rejects a photo moved by hand
#   status    print the current preset ("standard" when none is set)
#
# The tray's root helper runs this, one verb per preset, so the config edit
# lives in one place and the helper never takes a value from the caller.

CONF="${FACE_AUTH_LIVENESS_CONF:-/etc/face-auth.toml}"

usage() {
    echo "Usage: sudo $0 off|standard|strict, or $0 status" >&2
    exit 2
}

status() {
    local value
    value="$(sed -nE 's/^[[:space:]]*liveness\.preset[[:space:]]*=[[:space:]]*"([a-z]+)".*/\1/p' "$CONF" 2>/dev/null | tail -n1)"
    case "$value" in
        off|standard|strict) echo "$value" ;;
        *) echo standard ;;
    esac
}

[ $# -eq 1 ] || usage
MODE="$1"
case "$MODE" in
    status) status; exit 0 ;;
    off|standard|strict) ;;
    *) usage ;;
esac

if [ "$EUID" -ne 0 ] && [ -z "${FACE_AUTH_LIVENESS_CONF:-}" ]; then
    echo "Must run as root: sudo $0 $MODE" >&2
    exit 1
fi

# Drop any active preset line, then add ours at the end (plain dotted keys, as
# deploy.sh does, so it never lands inside a table). Written beside the file and
# renamed over it, keeping its owner and mode.
TMP="$(mktemp "$CONF.XXXXXX")"
trap 'rm -f "$TMP"' EXIT
if [ -f "$CONF" ]; then
    grep -vE '^[[:space:]]*liveness\.preset[[:space:]]*=' "$CONF" > "$TMP" || true
    chmod --reference="$CONF" "$TMP"
    chown --reference="$CONF" "$TMP"
else
    chmod 0644 "$TMP"
fi
echo "liveness.preset = \"$MODE\"" >> "$TMP"
mv "$TMP" "$CONF"
trap - EXIT

echo "Liveness: $MODE"
