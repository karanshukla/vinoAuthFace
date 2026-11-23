#!/bin/sh
# On removal (not on upgrade), undo what the package didn't own but its use
# created, as uninstall.sh does: the downloaded models and the compiled cache,
# and the SELinux module vinoauthface-setup loaded. The face templates in
# /var/lib/face-auth and /etc/face-auth.toml stay: they are the user's, and
# uninstall.sh keeps them without --purge too.
# $1 is "remove" or "purge" (deb), "0" (rpm, last uninstall); an upgrade passes
# something else.
case "$1" in
    remove|purge|0)
        for model in w600k_mbf w600k_r50 det_500m version-slim-320; do
            rm -f "/usr/share/face-auth/$model.onnx"
        done
        # What vinoauthface-setup compiled beside the package's own SELinux sources.
        rm -f /usr/share/face-auth/selinux/face_auth.*
        rmdir /usr/share/face-auth/selinux /usr/share/face-auth 2>/dev/null || true
        rm -rf /var/cache/face-auth/* 2>/dev/null || true
        if command -v semodule >/dev/null 2>&1; then
            semodule -r face_auth 2>/dev/null || true
        fi ;;
esac
exit 0
