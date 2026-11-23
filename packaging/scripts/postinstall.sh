#!/bin/sh
# Runs on install and on upgrade (deb: configure; rpm: 1 or 2).
#
# By default this only installs files and says what is left: a maintainer
# script runs as root with no user session, so it can't show a GUI, and it
# shouldn't edit the login stack unasked. Finishing is the user's step (the
# three commands below; the tray is meant to offer the same).
#
# For headless installs, and for testing, opt in to what deploy.sh does:
#   touch /etc/vinoauthface-auto-setup      (or export VINOAUTHFACE_AUTO_SETUP=1)
# which downloads the models and runs vinoauthface-setup (PAM, KDE login
# screen, SELinux; idempotent, backs up what it edits). A failure never fails
# the install: face unlock stays off and the password keeps working.
set -u
SHARE=/usr/share/face-auth

if [ -e /etc/vinoauthface-auto-setup ] || [ -n "${VINOAUTHFACE_AUTO_SETUP:-}" ]; then
    if [ ! -f "$SHARE/w600k_mbf.onnx" ] || [ ! -f "$SHARE/det_500m.onnx" ]; then
        /usr/bin/vinoauthface-fetch-models \
            || echo "vinoauthface: could not download the models; run 'sudo vinoauthface-fetch-models' when online." >&2
    fi
    # PAM only once the models exist: with none, every sudo would try the camera for nothing.
    if [ -f "$SHARE/w600k_mbf.onnx" ] && [ -f "$SHARE/det_500m.onnx" ]; then
        /usr/bin/vinoauthface-setup \
            || echo "vinoauthface: PAM setup failed; run 'sudo vinoauthface-setup' to see why." >&2
    fi
    exit 0
fi

# Already set up (an upgrade): nothing to say.
if [ -f "$SHARE/w600k_mbf.onnx" ] && grep -qs 'vinoauthface-auth' /etc/pam.d/sudo; then
    exit 0
fi
cat <<'MSG'

vinoauthface is installed but not switched on. To finish, with the IR camera plugged in:

  sudo vinoauthface-fetch-models           download the face models (checksum-verified)
  sudo vinoauthface enroll --user $USER
  sudo vinoauthface-setup                  hook into PAM (sudo, polkit, lock screens)

MSG
exit 0
