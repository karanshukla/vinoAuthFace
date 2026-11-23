#!/bin/sh
# The set-group-ID vinoauthface-auth is installed as root:face-auth, so the
# group has to exist before the files are unpacked.
set -e
getent group face-auth >/dev/null || groupadd --system face-auth
