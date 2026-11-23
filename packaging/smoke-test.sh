#!/bin/bash
# Looks inside the built packages before anything ships them (release.yml runs it).
#
#   packaging/smoke-test.sh TAG [DIR]     # DIR defaults to packaging/dist
#
# The PAM binary is set-group-ID in group face-auth, no script, policy or program
# names /usr/local, the binaries run and carry the tag, and the tray and settings
# packages depend on exactly the core package's version. The rpm checks need rpm; in CI
# (CI set) its absence is a failure, elsewhere they are skipped with a note.
set -euo pipefail
TAG="${1:?usage: smoke-test.sh TAG [DIR]}"
cd "${2:-$(dirname "$(readlink -f "$0")")/dist}"

test "$(find . -maxdepth 1 -type f | wc -l)" = 6
core_deb=(vinoauthface_*_amd64.deb)
tray_deb=(vinoauthface-tray_*_amd64.deb)
core_rpm=(vinoauthface-[0-9]*.x86_64.rpm)
tray_rpm=(vinoauthface-tray-[0-9]*.x86_64.rpm)

dpkg-deb -c "${core_deb[0]}" | grep -E '^-rwxr-sr-x root/face-auth .* \./usr/bin/vinoauthface-auth$' >/dev/null

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
for deb in *.deb; do
    dir="$work/${deb%.deb}"
    mkdir -p "$dir"
    dpkg-deb -x "$deb" "$dir"
    if grep -rIl '/usr/local' "$dir"; then echo "$deb: a file names /usr/local" >&2; exit 1; fi
done
# The binaries themselves: only the paths this project installs to matter, not strings from a
# dependency. The PAM binary and helper pin the install paths, so those must be /usr ones.
for bin in "$work"/vinoauthface_*/usr/bin/vinoauthface-auth "$work"/vinoauthface-tray_*/usr/libexec/vinoauthface-helper; do
    if strings "$bin" | grep -E '/usr/local/(bin/vinoauthface|libexec/vinoauthface|share/face-auth|lib/face-auth)'; then
        echo "$bin: names the /usr/local install paths" >&2; exit 1
    fi
done

core="$work/${core_deb[0]%.deb}"
"$core/usr/bin/vinoauthface" --version | tee /dev/stderr | grep -qF " $TAG"
rc=0
env -i "$core/usr/bin/vinoauthface-auth" || rc=$?
test "$rc" = 1

version="$(dpkg-deb -f "${core_deb[0]}" Version)"
dpkg-deb -f "${tray_deb[0]}" Depends | grep -qF "vinoauthface (= $version)"

if command -v rpm >/dev/null; then
    # --qf, not -v: the verbose listing cuts the group name to eight characters.
    rpm -qp --qf '[%{FILEMODES:perms} %{FILEGROUPNAME} %{FILENAMES}\n]' "${core_rpm[0]}" \
        | grep -Fx -- '-rwxr-sr-x face-auth /usr/bin/vinoauthface-auth' >/dev/null
    rpm -qp --requires "${tray_rpm[0]}" | grep -qF "vinoauthface = $version"
elif [ -n "${CI:-}" ]; then
    echo "rpm is not installed, so the rpm checks cannot run" >&2; exit 1
else
    echo "note: rpm not installed, skipped the rpm checks" >&2
fi
echo "packages ok ($version)"
