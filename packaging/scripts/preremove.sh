#!/bin/sh
# On removal (not on upgrade), take the PAM lines out again: left behind they
# would call a binary that is gone. They are `sufficient`, so a missing binary
# falls through to the password, but there is no reason to leave them.
# $1 is "remove" or "purge" (deb), "0" (rpm, last uninstall).
case "$1" in
    remove|purge|0)
        for f in /etc/pam.d/*; do
            [ -f "$f" ] || continue
            grep -q 'pam_exec\.so.*vinoauthface-auth' "$f" 2>/dev/null || continue
            sed -i '/pam_exec\.so.*vinoauthface-auth/d' "$f" || true
        done ;;
esac
exit 0
