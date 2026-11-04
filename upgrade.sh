#!/bin/bash
set -euo pipefail

# Upgrade an install to a release without a git checkout. deploy.sh installs
# this as vinoauthface-upgrade, and vinoauthface-update links to it. Every
# upgrade is a full reinstall, done by the deploy logic the release shipped
# with, so config, templates and PAM are kept the same way a re-run of
# deploy.sh keeps them.
#
# It hands over to install.sh, which installs the release's binaries, CPU or
# NPU (OpenVINO) alike: nothing builds. Run as root alone (the tray's helper),
# an NPU install keeps the OpenVINO runtime it has, since there is no user to
# run ovfetch as.
#
#   sudo vinoauthface-upgrade          # the newest release
#   sudo vinoauthface-upgrade v3       # a specific one (also to go back)
#   sudo vinoauthface-upgrade --force  # reinstall even if already on it

if [ -t 1 ] && [ -z "${NO_COLOR:-}" ]; then
    BOLD=$'\e[1m' GREEN=$'\e[32m' RED=$'\e[31m' RESET=$'\e[0m'
else
    BOLD="" GREEN="" RED="" RESET=""
fi
ok()   { printf '  %s✓%s %-10s %s\n' "$GREEN" "$RESET" "$1" "$2"; }
fail() {
    printf '%s✗ %s%s\n' "$RED" "$1" "$RESET" >&2
    shift
    local line
    for line in "$@"; do printf '  %s\n' "$line" >&2; done
}

USAGE="Usage: sudo vinoauthface-upgrade [vN] [--force]"
TAG=""
FORCE=0
for arg in "$@"; do
    case "$arg" in
        --force) FORCE=1 ;;
        v[0-9]*) TAG="$arg" ;;
        *) fail "Unknown option '$arg'" "$USAGE"; exit 1 ;;
    esac
done
if [ -n "$TAG" ] && ! [[ "$TAG" =~ ^v[0-9]+(\.[0-9]+)*$ ]]; then
    fail "Not a release tag: '$TAG'" "$USAGE"
    exit 1
fi

if [ "$(id -u)" -ne 0 ]; then
    fail "Run this with sudo" "It runs deploy.sh, which installs into /usr/local, /etc and /var/lib."
    exit 1
fi

# Keep in step with deploy.sh.
RELEASE_REPO="karanshukla/vinoAuthFace"

printf '%svinoAuthFace upgrade%s\n' "$BOLD" "$RESET"

INSTALLED="$(/usr/local/bin/vinoauthface --version 2>/dev/null | awk '{ print $2 }' || true)"

if [ -z "$TAG" ]; then
    if [ -n "${FACE_AUTH_DEPLOY_RELEASE_BASE:-}" ]; then
        fail "Name the release to install" "FACE_AUTH_DEPLOY_RELEASE_BASE is set, so the latest can't be looked up."
        exit 1
    fi
    # Not releases/latest, for the same reason as deploy.sh: it skips
    # pre-releases.
    TAG="$(curl -fsSL "https://api.github.com/repos/$RELEASE_REPO/releases?per_page=1" \
        | grep -o '"tag_name": *"[^"]*"' | head -1 | cut -d'"' -f4 || true)"
    if [ -z "$TAG" ]; then
        fail "Could not find the latest release of $RELEASE_REPO"
        exit 1
    fi
fi

if [ "$INSTALLED" = "$TAG" ] && [ "$FORCE" = 0 ]; then
    ok Version "$TAG is already installed (--force to reinstall it)"
    exit 0
fi
ok Version "${INSTALLED:-nothing} installed, installing $TAG"

# A --no-tray install stays one.
DEPLOY_ARGS=()
[ -x /usr/local/bin/vinoauthface-tray ] || DEPLOY_ARGS+=(--no-tray)

printf '\n'
bash /usr/local/share/face-auth/install.sh "$TAG" "${DEPLOY_ARGS[@]}"

# Upgrades of NPU installs used to build here, into the user's cache. Nothing
# reads it any more. As the user: it's in their home, so root resolving the
# path could be steered elsewhere by a symlinked ~/.cache.
if [ -n "${SUDO_USER:-}" ]; then
    user_home="$(getent passwd "$SUDO_USER" | cut -d: -f6)"
    if [ -n "$user_home" ] && [ -d "$user_home/.cache/vinoauthface" ]; then
        sudo -u "$SUDO_USER" rm -rf "$user_home/.cache/vinoauthface"
    fi
fi
