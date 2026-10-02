#!/bin/bash
set -euo pipefail

# Upgrade an install to a release without a git checkout. deploy.sh installs
# this as vinoauthface-upgrade, and vinoauthface-update links to it. It downloads the release's source bundle,
# verifies it against the release's SHA256SUMS (itself signed with the release
# key, see scripts/verify-release.sh), and runs that release's own
# deploy.sh: every upgrade is a full reinstall, done by the deploy logic the
# release shipped with, so config, templates and PAM are kept the same way a
# re-run of deploy.sh keeps them.
#
#   sudo vinoauthface-upgrade          # the newest release
#   sudo vinoauthface-upgrade v3       # a specific one (also to go back)
#   sudo vinoauthface-upgrade --force  # reinstall even if already on it

if [ -t 1 ] && [ -z "${NO_COLOR:-}" ]; then
    BOLD=$'\e[1m' DIM=$'\e[2m' GREEN=$'\e[32m' RED=$'\e[31m' RESET=$'\e[0m'
else
    BOLD="" DIM="" GREEN="" RED="" RESET=""
fi
ok()   { printf '  %s✓%s %-10s %s\n' "$GREEN" "$RESET" "$1" "$2"; }
step() { printf '  %s… %s%s\n' "$DIM" "$1" "$RESET"; }
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
BUNDLE="vinoauthface-source.tar.gz"
# Installed by deploy.sh from scripts/verify-release.sh.
VERIFY_RELEASE="/usr/local/share/face-auth/verify-release.sh"

# The release is downloaded, verified and unpacked by root, into a root-owned
# directory, and deploy.sh runs from there: nothing the user can write sits
# between the signature check and root running what was checked. deploy.sh
# still compiles as the user who ran sudo, never as root; the sources are
# world-readable and the build writes only to the user's TARGET_DIR.
ACTUAL_USER="${SUDO_USER:-root}"
ACTUAL_HOME="$(getent passwd "$ACTUAL_USER" | cut -d: -f6)"
as_user() {
    if [ "$ACTUAL_USER" != root ]; then sudo -u "$ACTUAL_USER" -H "$@"; else "$@"; fi
}
# Not under /tmp or /var/tmp: cargo and rustup look for .cargo/config.toml and
# rust-toolchain.toml in every parent of the sources, and anyone can create
# those in a world-writable directory. Every parent here is root's. On disk,
# not /tmp, which is RAM on many systems.
STAGE_PARENT="/var/cache/vinoauthface-upgrade"
# One build directory for every release, so an upgrade recompiles only what
# changed rather than every dependency. The user's: the build runs as them.
TARGET_DIR="$ACTUAL_HOME/.cache/vinoauthface/target"

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

# An OpenVINO build compiles as the user, with their Rust and ovfetch. Run as
# plain root (the tray's helper), deploy.sh would find neither and quietly
# install the CPU build instead.
if [ "$ACTUAL_USER" = root ] \
   && ldd /usr/local/bin/vinoauthface-auth 2>/dev/null | grep -q libopenvino; then
    fail "This is an OpenVINO build, which compiles as your user" \
        "Run sudo vinoauthface-upgrade in a terminal."
    exit 1
fi

BASE="${FACE_AUTH_DEPLOY_RELEASE_BASE:-https://github.com/$RELEASE_REPO/releases/download/$TAG}"
as_user mkdir -p "$TARGET_DIR"
install -d -o root -g root -m 0755 "$STAGE_PARENT"
STAGE="$(mktemp -d "$STAGE_PARENT/XXXXXXXX")"
trap 'rm -rf "$STAGE"; rmdir "$STAGE_PARENT" 2>/dev/null || true' EXIT
DOWNLOAD="$STAGE/download"
DEST="$STAGE/src"
mkdir "$DOWNLOAD"

step "downloading $BASE/$BUNDLE"
for asset in "$BUNDLE" SHA256SUMS SHA256SUMS.minisig; do
    if ! curl -fsSL --retry 5 --retry-delay 2 --retry-all-errors --connect-timeout 20 \
            -o "$DOWNLOAD/$asset" "$BASE/$asset"; then
        fail "Download failed: $BASE/$asset" \
            "Releases before v6 aren't signed, and those before the upgrade command have no" \
            "source bundle: use a git checkout for those."
        exit 1
    fi
done
# A checksum file from the same place as the bundle only catches a bad
# download; the signature is what ties it to a release CI built.
if ! "$VERIFY_RELEASE" "$DOWNLOAD/SHA256SUMS" "$DOWNLOAD/SHA256SUMS.minisig"; then
    fail "Signature verification failed for the release's SHA256SUMS" \
        "If $VERIFY_RELEASE is missing, re-run deploy.sh from a checkout."
    exit 1
fi
# The bundle must be listed, not just match: --ignore-missing alone would pass
# a file SHA256SUMS never mentions.
if ! (cd "$DOWNLOAD" \
        && grep -E "[ *]$BUNDLE\$" SHA256SUMS > want \
        && [ "$(wc -l < want)" -eq 1 ] \
        && sha256sum -c --strict --quiet want); then
    fail "Checksum verification failed for $BUNDLE"
    exit 1
fi
ok Download "source bundle, signature and checksum verified"

# Readable by the user's build, writable by root only, whatever the archive's
# modes or the caller's umask.
mkdir "$DEST"
tar -xzf "$DOWNLOAD/$BUNDLE" -C "$DEST" --strip-components=1 --no-same-owner
chmod -R u+rwX,go+rX,go-w "$DEST"
chmod 0755 "$STAGE"
if [ "$(cat "$DEST/VERSION" 2>/dev/null)" != "$TAG" ] || [ ! -f "$DEST/deploy.sh" ]; then
    fail "The bundle isn't the $TAG release"
    exit 1
fi

# Sources earlier versions of this script unpacked into the user's cache. As
# the user: root deleting inside a directory they own could be pointed elsewhere.
as_user rm -rf -- "$ACTUAL_HOME/.cache/vinoauthface/src" 2>/dev/null || true

# A --no-tray install stays one.
DEPLOY_ARGS=()
[ -x /usr/local/bin/vinoauthface-tray ] || DEPLOY_ARGS+=(--no-tray)

printf '\n'
cd "$DEST"
# Forced: the shared build directory still holds the previous release's
# binaries, which deploy.sh would otherwise install as they are. The sources
# are removed on exit; the build directory is kept for the next upgrade.
CARGO_TARGET_DIR="$TARGET_DIR" FACE_AUTH_FORCE_BUILD=1 ./deploy.sh "${DEPLOY_ARGS[@]}"
