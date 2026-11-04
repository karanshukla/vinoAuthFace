#!/bin/bash
# Install vinoAuthFace from a GitHub release, with no checkout and no build:
#
#   curl -fsSL https://github.com/karanshukla/vinoAuthFace/releases/latest/download/install.sh | sudo bash
#
# Options go after `bash -s --`, e.g. `| sudo bash -s -- --with-settings`:
#
#   vN                     that release instead of the newest
#   --with-settings        also the settings window (docs/settings.md)
#   --no-tray              no tray icon
#   --login=off|both|face  face unlock at the KDE login screen
#
# It downloads the release's source bundle and SHA256SUMS, checks the one
# against the other, and runs that release's own deploy.sh with --prebuilt:
# the release's binaries (also checksum-verified), its models, config, PAM and
# SELinux setup. With ovfetch on the PATH (or OpenVINO installed some other
# way), that's the NPU (OpenVINO) build and the runtime ovfetch picks for this
# machine; otherwise the static CPU build. Re-running it reinstalls, keeping config, templates
# and PAM setup the way a re-run of deploy.sh does.
#
# From a checkout, sudo ./deploy.sh builds from source instead.

# Everything is in main, called on the last line: piped into bash, a download
# cut short must not run half a script.
set -euo pipefail

RELEASE_REPO="karanshukla/vinoAuthFace"
BUNDLE="vinoauthface-source.tar.gz"

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

main() {
    local tag="" arg
    local deploy_args=(--prebuilt)
    for arg in "$@"; do
        case "$arg" in
            v[0-9]*)
                if ! [[ "$arg" =~ ^v[0-9]+(\.[0-9]+)*$ ]]; then
                    fail "Not a release tag: '$arg'"
                    exit 1
                fi
                tag="$arg" ;;
            # Checked by deploy.sh, which knows its own options.
            *) deploy_args+=("$arg") ;;
        esac
    done

    if [ "$(id -u)" -ne 0 ]; then
        fail "Run this with sudo" "It installs into /usr/local, /etc and /var/lib, and edits /etc/pam.d."
        exit 1
    fi
    if [ "$(uname -m)" != x86_64 ]; then
        fail "vinoAuthFace runs on x86_64 only" "This machine is $(uname -m)."
        exit 1
    fi
    local tool
    for tool in curl tar sha256sum; do
        if ! command -v "$tool" &>/dev/null; then
            fail "Missing $tool" "Install it with your package manager, then run this again."
            exit 1
        fi
    done

    printf '%svinoAuthFace install%s\n' "$BOLD" "$RESET"

    if [ -z "$tag" ]; then
        if [ -n "${FACE_AUTH_DEPLOY_RELEASE_BASE:-}" ]; then
            fail "Name the release to install" "FACE_AUTH_DEPLOY_RELEASE_BASE is set, so the latest can't be looked up."
            exit 1
        fi
        # Not releases/latest, the same as deploy.sh: it skips pre-releases.
        tag="$(curl -fsSL "https://api.github.com/repos/$RELEASE_REPO/releases?per_page=1" \
            | grep -o '"tag_name": *"[^"]*"' | head -1 | cut -d'"' -f4 || true)"
        if [ -z "$tag" ]; then
            fail "Could not find the latest release of $RELEASE_REPO"
            exit 1
        fi
    fi

    # FACE_AUTH_DEPLOY_RELEASE_BASE: a mirror, or CI's file:// fake release.
    # deploy.sh reads it too, so the binaries come from the same place.
    local base="${FACE_AUTH_DEPLOY_RELEASE_BASE:-https://github.com/$RELEASE_REPO/releases/download/$tag}"
    # Root's own directory, not a fixed path: nobody else may swap the bundle
    # between the checksum and the install.
    local work
    work="$(mktemp -d)"
    # shellcheck disable=SC2064 # expand now: $work is local
    trap "rm -rf '$work'" EXIT

    step "downloading $tag from $base"
    local asset
    for asset in "$BUNDLE" SHA256SUMS; do
        if ! curl -fsSL --retry 5 --retry-delay 2 --retry-all-errors --connect-timeout 20 \
                -o "$work/$asset" "$base/$asset"; then
            fail "Download failed: $base/$asset" \
                "Releases before the one-line installer have no source bundle: use a git checkout for those."
            exit 1
        fi
    done
    # Listed, not just matching: --ignore-missing alone would pass a file
    # SHA256SUMS never mentions.
    if ! (cd "$work" \
            && grep -E "[ *]$BUNDLE\$" SHA256SUMS > want \
            && [ "$(wc -l < want)" -eq 1 ] \
            && sha256sum -c --strict --quiet want); then
        fail "Checksum verification failed for $BUNDLE" "Nothing was installed."
        exit 1
    fi
    ok Download "$tag, checksum verified"

    mkdir "$work/src"
    tar -xzf "$work/$BUNDLE" -C "$work/src" --strip-components=1 --no-same-owner
    if [ "$(cat "$work/src/VERSION" 2>/dev/null)" != "$tag" ] || [ ! -f "$work/src/deploy.sh" ]; then
        fail "The bundle isn't the $tag release" "Nothing was installed."
        exit 1
    fi
    printf '\n'

    # deploy.sh takes the tag from VERSION, so it installs this release's
    # binaries. Its stdin is not this script's.
    bash "$work/src/deploy.sh" "${deploy_args[@]}" </dev/null
}

main "$@"
