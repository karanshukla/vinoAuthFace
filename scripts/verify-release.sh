#!/bin/bash
set -euo pipefail

# Verify a release file's minisign signature against the vinoAuthFace release
# key, using only openssl and coreutils. minisign itself isn't on the immutable
# distros the download path exists for, but openssl is, and it can check both
# halves of a minisign signature: the Ed25519 signature over the file (BLAKE2b-512
# prehashed, or raw for legacy signatures) and the global signature that binds
# the trusted comment to it.
#
#   verify-release.sh FILE FILE.minisig
#
# Exits 0 only if both signatures verify under the release key. deploy.sh runs
# it from the checkout, and installs it as /usr/local/share/face-auth/verify-release.sh
# for vinoauthface-upgrade. Same check as `minisign -Vm FILE -P "$RELEASE_PUBKEY"`.

# The public half of the key release.yml signs SHA256SUMS with. Rotating it means
# every older install refuses newer releases until it is redeployed from a
# checkout, so only do it if the secret half leaks (docs/releasing.md).
RELEASE_PUBKEY="RWQtIibw1QKBz3HEjneCzxh0yBFzHKMcsjWB08i0+YSMc2TLZzxv9nYG"

# CI signs its fake release with a throwaway key. Only honoured together with
# the release URL override, which already means "not GitHub's releases"; both are
# environment, so only whoever runs deploy.sh as root can set them.
if [ -n "${FACE_AUTH_DEPLOY_RELEASE_BASE:-}" ] && [ -n "${FACE_AUTH_RELEASE_PUBKEY:-}" ]; then
    RELEASE_PUBKEY="$FACE_AUTH_RELEASE_PUBKEY"
fi

if [ "$#" -ne 2 ]; then
    echo "usage: verify-release.sh FILE FILE.minisig" >&2
    exit 2
fi
FILE="$1"
SIG="$2"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# Key: "Ed" | key ID (8) | Ed25519 public key (32).
if ! base64 -d <<<"$RELEASE_PUBKEY" > "$WORK/pub" 2>/dev/null \
   || [ "$(wc -c < "$WORK/pub")" -ne 42 ] || [ "$(head -c 2 "$WORK/pub")" != Ed ]; then
    echo "verify-release: the release public key is malformed" >&2
    exit 1
fi

if [ ! -f "$FILE" ] || [ ! -f "$SIG" ]; then
    echo "verify-release: $FILE or $SIG is missing" >&2
    exit 1
fi

# .minisig: an untrusted comment, the signature, "trusted comment: ...", and the
# global signature, one per line.
sed -n 2p "$SIG" | base64 -d > "$WORK/sig" 2>/dev/null || true
sed -n 4p "$SIG" | base64 -d > "$WORK/global" 2>/dev/null || true
TRUSTED_LINE="$(sed -n 3p "$SIG")"
if [ "$(wc -c < "$WORK/sig")" -ne 74 ] || [ "$(wc -c < "$WORK/global")" -ne 64 ] \
   || [ "${TRUSTED_LINE#trusted comment: }" = "$TRUSTED_LINE" ]; then
    echo "verify-release: $SIG is not a minisign signature" >&2
    exit 1
fi

# Signature: algorithm (2) | key ID (8) | Ed25519 signature (64).
ALG="$(head -c 2 "$WORK/sig")"
if ! cmp -s <(head -c 10 "$WORK/sig" | tail -c 8) <(head -c 10 "$WORK/pub" | tail -c 8); then
    echo "verify-release: $SIG was made with a different key" >&2
    exit 1
fi
tail -c 64 "$WORK/sig" > "$WORK/sig.raw"

# DER SubjectPublicKeyInfo for an Ed25519 key is a fixed 12-byte prefix.
{ printf '\x30\x2a\x30\x05\x06\x03\x2b\x65\x70\x03\x21\x00'; tail -c 32 "$WORK/pub"; } > "$WORK/pub.der"

ed25519_verify() { # MESSAGE_FILE SIGNATURE_FILE
    openssl pkeyutl -verify -pubin -keyform DER -inkey "$WORK/pub.der" \
        -rawin -in "$1" -sigfile "$2" >/dev/null 2>&1
}

case "$ALG" in
    ED) openssl dgst -blake2b512 -binary "$FILE" > "$WORK/msg" ;;
    Ed) cp "$FILE" "$WORK/msg" ;;
    *)  echo "verify-release: unsupported signature algorithm in $SIG" >&2; exit 1 ;;
esac
if ! ed25519_verify "$WORK/msg" "$WORK/sig.raw"; then
    echo "verify-release: bad signature on $FILE" >&2
    exit 1
fi

# The global signature covers the file signature plus the trusted comment.
{ cat "$WORK/sig.raw"; printf '%s' "${TRUSTED_LINE#trusted comment: }"; } > "$WORK/global.msg"
if ! ed25519_verify "$WORK/global.msg" "$WORK/global"; then
    echo "verify-release: bad signature on the trusted comment in $SIG" >&2
    exit 1
fi
