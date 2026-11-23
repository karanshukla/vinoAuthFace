#!/bin/bash
# Builds the deb and rpm packages (docs/packaging.md).
#
#   packaging/build.sh [VERSION]        # VERSION defaults to the nearest tag, or dev
#
# Output: packaging/dist/*.deb *.rpm. Needs: cargo, the musl target, musl-tools,
# cmake, and nfpm (https://nfpm.goreleaser.com) on PATH.
#
# Everything installs under /usr (VINOAUTHFACE_PREFIX, read by
# face-auth-core/build.rs), which the binaries, the polkit policy and the PAM
# lines all name by absolute path. The scripts and data files are deploy.sh's,
# copied with /usr/local rewritten to /usr.
set -euo pipefail
cd "$(dirname "$(readlink -f "$0")")/.."

VERSION="${1:-$(git describe --tags --abbrev=0 2>/dev/null || echo dev)}"
export VINOAUTHFACE_VERSION="$VERSION"
export VINOAUTHFACE_PREFIX=/usr
export VINOAUTHFACE_LIBEXECDIR=/usr/libexec
# nfpm wants a semver-shaped version and rewrites v16 to 16.0.0 itself, which would
# leave the packages' "= 16" dependencies unsatisfiable. Say it in full here:
# v16 -> 16.0.0, v1.1 -> 1.1.0, dev -> 0~dev.
PKG_VERSION="${VERSION#v}"
case "$PKG_VERSION" in
    [0-9]*.[0-9]*.[0-9]*) ;;
    [0-9]*.[0-9]*) PKG_VERSION="$PKG_VERSION.0" ;;
    [0-9]*) PKG_VERSION="$PKG_VERSION.0.0" ;;
    *) PKG_VERSION="0~$PKG_VERSION" ;;
esac
export PKG_VERSION

TARGET="${CARGO_TARGET_DIR:-target}"
STAGE=packaging/stage
rm -rf "$STAGE" packaging/dist
mkdir -p "$STAGE"/{bin,libexec,share,data} packaging/dist

# Static musl: the PAM binary loads nothing at run time.
cargo build --release --locked --target x86_64-unknown-linux-musl -p face-auth -p face-enroll
M="$TARGET/x86_64-unknown-linux-musl/release"
# glibc: the tray, its root helper and the settings window.
cargo build --release --locked -p face-auth-tray -p face-auth-settings
G="$TARGET/release"

install -m755 "$M/vinoauthface-auth" "$M/vinoauthface" "$G/vinoauthface-tray" "$G/vinoauthface-settings" "$STAGE/bin/"
install -m755 "$M/vinoauthface-unseal" "$G/vinoauthface-helper" "$STAGE/libexec/"
install -m755 packaging/vinoauthface-upgrade packaging/vinoauthface-fetch-models "$STAGE/bin/"
# The release binaries must carry the package's paths, not deploy.sh's.
# Not `grep -q`: it exits early, and under pipefail the pipeline would report strings' SIGPIPE.
if strings "$STAGE/bin/vinoauthface-auth" | grep -E '/usr/local/(bin/vinoauthface|libexec/vinoauthface|share/face-auth|lib/face-auth)' >/dev/null; then
    echo "build: vinoauthface-auth still names the /usr/local install paths" >&2; exit 1
fi

# Scripts, policy and desktop files: deploy.sh's, repathed.
rewrite() { sed 's#/usr/local#/usr#g' "$1" > "$2"; }
rewrite setup.sh "$STAGE/share/setup.sh"
rewrite login-mode.sh "$STAGE/share/login-mode.sh"
rewrite setting-mode.sh "$STAGE/share/setting-mode.sh"
# The file-context line labels the unseal helper by path, so it has to name the packaged one.
rewrite selinux/face-auth.te "$STAGE/share/face-auth.te"
rewrite selinux/face-auth.fc "$STAGE/share/face-auth.fc"
cp config/models.sha256 "$STAGE/share/"
rewrite config/face-auth.toml.example "$STAGE/data/face-auth.toml"
for f in crates/face-auth-tray/data/*.policy crates/face-auth-tray/data/*.desktop crates/face-auth-settings/data/*.desktop; do
    rewrite "$f" "$STAGE/data/$(basename "$f")"
done
cp crates/face-auth-tray/data/*.svg "$STAGE/data/"
echo package > "$STAGE/share/installed-from"
"$STAGE/bin/vinoauthface" completions bash > "$STAGE/data/vinoauthface.bash"
"$STAGE/bin/vinoauthface" completions zsh  > "$STAGE/data/_vinoauthface"
"$STAGE/bin/vinoauthface" completions fish > "$STAGE/data/vinoauthface.fish"
chmod 644 "$STAGE"/data/* "$STAGE"/share/installed-from "$STAGE"/share/face-auth.te "$STAGE"/share/face-auth.fc "$STAGE"/share/models.sha256

# nfpm resolves src: and scripts: against its working directory.
cd packaging
for conf in nfpm-vinoauthface.yaml nfpm-vinoauthface-tray.yaml nfpm-vinoauthface-settings.yaml; do
    for fmt in deb rpm; do
        nfpm package --config "$conf" --packager "$fmt" --target dist/
    done
done
cd ..
ls -l packaging/dist
