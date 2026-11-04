#!/bin/bash
set -euo pipefail

# The system half of an install: hooks the installed binaries into PAM, the
# KDE login screen, Bitwarden's polkit action and the SELinux policy. deploy.sh
# runs it after putting the files in place, and installs a copy as
# vinoauthface-setup, which re-applies all of it on its own. That's for when
# something resets /etc/pam.d (a distro or authselect update), or after
# installing a lock screen that wasn't there before.
#
#   sudo vinoauthface-setup                  # keep the login screen's mode
#   sudo vinoauthface-setup --login=both     # or set it: off, both, face
#
# Reads login-mode.sh and selinux/ from beside itself: the checkout, or
# /usr/local/share/face-auth for the installed copy.
cd "$(dirname "$(readlink -f "$0")")"

if [ -t 1 ] && [ -z "${NO_COLOR:-}" ]; then
    BOLD=$'\e[1m' DIM=$'\e[2m' GREEN=$'\e[32m' YELLOW=$'\e[33m' RED=$'\e[31m' RESET=$'\e[0m'
else
    BOLD="" DIM="" GREEN="" YELLOW="" RED="" RESET=""
fi
ok()      { printf '  %s✓%s %-10s %s\n' "$GREEN" "$RESET" "$1" "$2"; }
skip()    { printf '  %s· %-10s %s%s\n' "$DIM" "$1" "$2" "$RESET"; }
step()    { printf '  %s… %s%s\n' "$DIM" "$1" "$RESET"; }
warn() {
    printf '  %s!%s %-10s %s\n' "$YELLOW" "$RESET" "$1" "$2"
    shift 2
    local line
    for line in "$@"; do printf '    %-10s %s\n' "" "$line"; done
}
fail() {
    printf '%s✗ %s%s\n' "$RED" "$1" "$RESET" >&2
    shift
    local line
    for line in "$@"; do printf '  %s\n' "$line" >&2; done
}

LOGIN_MODE="${FACE_AUTH_LOGIN:-}"
USAGE="Usage: sudo vinoauthface-setup [--login=off|both|face]"
for arg in "$@"; do
    case "$arg" in
        --login=off|--login=both|--login=face) LOGIN_MODE="${arg#--login=}" ;;
        *) fail "Unknown option '$arg'" "$USAGE"; exit 1 ;;
    esac
done
case "$LOGIN_MODE" in
    ""|off|both|face) ;;
    *) fail "FACE_AUTH_LOGIN must be off, both or face" "$USAGE"; exit 1 ;;
esac

# Keep in step with deploy.sh and uninstall.sh.
BIN_DIR="/usr/local/bin"
SHARE_DIR="/usr/local/share/face-auth"
PAM_DIR="/etc/pam.d"
# The KDE login screen's services are login-mode.sh's, not this list's.
PAM_SERVICES="sudo swaylock hyprlock gdm-password polkit-1 kde-fingerprint kde-smartcard cosmic-greeter"
VAR_DIR="/var/lib/face-auth"
NPU_CACHE_DIR="/var/cache/face-auth"
SELINUX_DIR="/usr/local/share/face-auth/selinux"

PAM_LINE="auth       sufficient  pam_exec.so quiet stdout /usr/local/bin/vinoauthface-auth"
# For a KDE lock screen slot with nothing after us that can authenticate. A
# miss falling through to the stub there (Fedora's authinfo_unavail
# fingerprint-auth or smartcard-auth) makes KScreenLocker drop the slot until
# the next lock; failing here instead lets a wrong password rescan.
PAM_LINE_KDE_ONLY="auth       [success=done new_authtok_reqd=done default=die]  pam_exec.so quiet stdout /usr/local/bin/vinoauthface-auth"

if [ "$(id -u)" -ne 0 ]; then
    fail "Run this with sudo" "It edits /etc/pam.d and loads the SELinux policy."
    exit 1
fi

# A line pointing at a missing binary is harmless where it's sufficient, but
# the login screen's "both" mode makes it required, and that would refuse
# every login.
if [ ! -x "$BIN_DIR/vinoauthface-auth" ] || ! getent group face-auth >/dev/null; then
    fail "vinoAuthFace isn't installed" "Install it first, then run this again."
    exit 1
fi

# Where the tray's policy went: /usr/share, or /usr/local/share on a read-only
# /usr (Fedora Atomic, Aeon), which polkit 124 and later read. Same test as
# deploy.sh.
if [ -d /usr/share/polkit-1/actions ] && [ -w /usr/share/polkit-1/actions ] \
   && touch /usr/share/polkit-1/actions/.face-auth-write-test 2>/dev/null; then
    rm -f /usr/share/polkit-1/actions/.face-auth-write-test
    POLKIT_ACTIONS_DIR="/usr/share/polkit-1/actions"
else
    POLKIT_ACTIONS_DIR="/usr/local/share/polkit-1/actions"
fi

# ---- Distro ----
# Only for the install hints printed when something is missing; nothing that
# gets installed depends on it. ID first, then ID_LIKE, so derivatives
# (Bazzite, CachyOS, Linux Mint, Aeon) land in their parent's family.
os_release_field() { sed -n "s/^$1=//p" /etc/os-release 2>/dev/null | tr -d '"' | head -1; }
DISTRO_FAMILY=""
for id in $(os_release_field ID) $(os_release_field ID_LIKE); do
    case "$id" in
        fedora|rhel|centos) DISTRO_FAMILY=fedora ;;
        debian|ubuntu) DISTRO_FAMILY=debian ;;
        arch) DISTRO_FAMILY=arch ;;
        opensuse*|suse|sles) DISTRO_FAMILY=suse ;;
        *) continue ;;
    esac
    break
done
# Image-based: Fedora Atomic (Silverblue, Kinoite, Bazzite) layers packages
# with rpm-ostree; openSUSE Aeon/MicroOS with transactional-update. Both need a
# reboot before the package is there.
TRANSACTIONAL=0
if [ "$DISTRO_FAMILY" = suse ] && command -v transactional-update &>/dev/null \
   && findmnt -no OPTIONS / 2>/dev/null | tr ',' '\n' | grep -qx ro; then
    TRANSACTIONAL=1
fi

# install_hint <fedora> <debian> <arch> <suse>: the command that installs
# those packages here. Fails (prints nothing) for an unknown distro, or an
# empty package list for this one.
install_hint() {
    local pkgs
    case "$DISTRO_FAMILY" in
        fedora) pkgs="$1" ;; debian) pkgs="$2" ;; arch) pkgs="$3" ;; suse) pkgs="$4" ;; *) return 1 ;;
    esac
    [ -n "$pkgs" ] || return 1
    case "$DISTRO_FAMILY" in
        fedora)
            if [ -e /run/ostree-booted ]; then echo "sudo rpm-ostree install $pkgs, then reboot"
            else echo "sudo dnf install $pkgs"; fi ;;
        debian) echo "sudo apt install $pkgs" ;;
        arch) echo "sudo pacman -S --needed $pkgs" ;;
        suse)
            if [ "$TRANSACTIONAL" = 1 ]; then echo "sudo transactional-update pkg install $pkgs, then reboot"
            else echo "sudo zypper install $pkgs"; fi ;;
    esac
}

printf '\n%sSetup%s\n' "$BOLD" "$RESET"

# Start from a clean stack: every line this script adds is added again below.
for service in $PAM_SERVICES; do
    if [ -f "$PAM_DIR/$service" ]; then
        sed -i '/pam_exec\.so.*face-auth/d' "$PAM_DIR/$service" 2>/dev/null || true
    fi
done

# ---- PAM setup ----
PAM_DONE=() PAM_ABSENT=() PAM_COVERED=()

# Stacks this file pulls its auth lines from, one level deep.
pam_included_stacks() {
    awk '$1 ~ /^-?auth$/ && ($2 == "include" || $2 == "substack") { print $3 }
         $1 == "@include" { print $2 }' "$1"
}

# A service's file: the override, else the vendor default.
pam_file() {
    if [ -f "$PAM_DIR/$1" ]; then echo "$PAM_DIR/$1"
    elif [ -f "/usr/lib/pam.d/$1" ]; then echo "/usr/lib/pam.d/$1"
    else return 1
    fi
}

# stack_uses SERVICE MODULE_REGEX: an auth line in the service, or in a stack
# it includes, runs a matching module.
stack_uses() {
    local file stack
    file=$(pam_file "$1") || return 1
    for stack in "$file" $(pam_included_stacks "$file" | while read -r s; do pam_file "$s"; done); do
        grep -qE "^[[:space:]]*-?auth[[:space:]].*$2" "$stack" && return 0
    done
    return 1
}

# Plasma 6.8 moved KScreenLocker's PAM calls into kscreenlocker_worker, and
# from then on only kde-fingerprint runs beside the password field: smartcard
# (and its own kde-face) run only when picked instead of the password.
kscreenlocker_has_worker() {
    local dir
    for dir in /usr/libexec /usr/lib /usr/lib64 /usr/lib/*/libexec /usr/lib64/libexec; do
        [ -x "$dir/kscreenlocker_worker" ] && return 0
    done
    return 1
}

# Before 6.8, KScreenLocker runs kde-fingerprint and kde-smartcard side by
# side, next to the password field. Face takes the fingerprint slot, unless
# fprintd owns it and the smartcard slot is free: then face and finger run at
# once instead of one after the other (#81). Both taken, or 6.8 and later:
# face goes first, then the finger.
KDE_SLOT=kde-fingerprint KDE_SLOT_LINE="$PAM_LINE_KDE_ONLY"
if stack_uses kde-fingerprint pam_fprintd; then
    if ! kscreenlocker_has_worker && pam_file kde-smartcard >/dev/null \
       && ! stack_uses kde-smartcard 'pam_(pkcs11|p11|sss)'; then
        KDE_SLOT=kde-smartcard
    else
        KDE_SLOT_LINE="$PAM_LINE"
    fi
fi
for service in kde-fingerprint kde-smartcard; do
    [ "$service" = "$KDE_SLOT" ] && continue
    # The slot a previous deploy used: drop its copy of the vendor file.
    marker="$PAM_DIR/.face-auth-$service-created"
    if [ -f "$marker" ] && cmp -s "$PAM_DIR/$service" "/usr/lib/pam.d/$service"; then
        rm -f "$PAM_DIR/$service" "$PAM_DIR/$service.face-auth.bak" "$marker"
    fi
done

# A service with no /etc/pam.d override falls back to the vendor file in
# /usr/lib/pam.d. That's usual for polkit-1 (pkexec, GUI admin prompts,
# Bitwarden's system unlock) and, on image-based distros, the KDE and COSMIC
# lock screens and greeters. openSUSE ships every service that way, sudo
# included. Materialise the vendor file as an override so there is something
# to patch, and mark it so uninstall.sh deletes it rather than "restoring" a
# file that never was.
for service in $PAM_SERVICES; do
    case "$service" in
        kde-fingerprint | kde-smartcard) [ "$service" = "$KDE_SLOT" ] || continue ;;
    esac
    if [ ! -f "$PAM_DIR/$service" ] && [ -f "/usr/lib/pam.d/$service" ]; then
        cp "/usr/lib/pam.d/$service" "$PAM_DIR/$service"
        touch "$PAM_DIR/.face-auth-$service-created"
    fi
done

# A face match ends the auth stack, so the line goes just above the first
# module that actually authenticates. Gates above that (pam_nologin,
# pam_faillock preauth, pam_selinux_permit) stay in front of it.
PAM_AUTHENTICATOR='^[[:space:]]*-?auth[[:space:]]+(include|substack)[[:space:]]|^[[:space:]]*-?auth[[:space:]].*pam_(unix|sss|fprintd|u2f)|^[[:space:]]*@include[[:space:]]'

for service in $PAM_SERVICES; do
    case "$service" in
        kde-fingerprint | kde-smartcard)
            [ "$service" = "$KDE_SLOT" ] || continue
            line="$KDE_SLOT_LINE" ;;
        *) line="$PAM_LINE" ;;
    esac
    conf="$PAM_DIR/$service"
    if [ ! -f "$conf" ]; then
        PAM_ABSENT+=("$service")
        continue
    fi

    # A face-auth line in an included stack (added by hand, an authselect
    # profile, an older install) already covers this service. A second one
    # would scan the camera twice per attempt.
    covered_by=""
    for stack in $(pam_included_stacks "$conf"); do
        if [ -f "$PAM_DIR/$stack" ] && grep -q "pam_exec\.so.*face-auth" "$PAM_DIR/$stack"; then
            covered_by="$PAM_DIR/$stack"
            break
        fi
    done
    if [ -n "$covered_by" ]; then
        PAM_COVERED+=("$service (via $(basename "$covered_by"))")
        continue
    fi

    line_no=$(grep -nE "$PAM_AUTHENTICATOR" "$conf" | head -n1 | cut -d: -f1)
    if [ -z "$line_no" ]; then
        warn PAM "$conf has no authenticating auth line. Add this above the password module:" \
            "$line"
        continue
    fi

    cp "$conf" "$conf.face-auth.bak"
    sed -i "${line_no}i $line" "$conf"
    PAM_DONE+=("$service")
    # Record what we wrote so uninstall.sh can tell whether the user has
    # edited the copy since. Legacy markers are empty.
    if [ -f "$PAM_DIR/.face-auth-$service-created" ]; then
        sha256sum "$conf" | cut -d' ' -f1 > "$PAM_DIR/.face-auth-$service-created"
    fi
done
# Joined with ", " for the summary lines.
join() { local IFS=,; echo "$*" | sed 's/,/, /g'; }
[ "${#PAM_DONE[@]}" -gt 0 ] && ok PAM "$(join "${PAM_DONE[@]}") (backups: *.face-auth.bak)"
[ "${#PAM_COVERED[@]}" -gt 0 ] && ok PAM "$(join "${PAM_COVERED[@]}")"
[ "${#PAM_ABSENT[@]}" -gt 0 ] && skip PAM "not on this system: $(join "${PAM_ABSENT[@]}")"

# The KDE login screen: off unless asked for, kept across re-deploys.
LOGIN_NOW="$(bash login-mode.sh status)"
if [ "$LOGIN_NOW" = none ]; then
    if [ "${LOGIN_MODE:-off}" != off ]; then
        warn PAM "--login=$LOGIN_MODE does nothing here: neither Plasma Login nor SDDM is installed."
    fi
else
    LOGIN_MODE="${LOGIN_MODE:-$LOGIN_NOW}"
    if LOGIN_OUT="$(bash login-mode.sh "$LOGIN_MODE" 2>&1)"; then
        case "$LOGIN_MODE" in
            off) ok PAM "KDE login screen: password only (--login=both or --login=face to change)" ;;
            both) ok PAM "KDE login screen: password, then face" ;;
            face) ok PAM "KDE login screen: face, or the password" ;;
        esac
    else
        warn PAM "Could not set the KDE login screen to $LOGIN_MODE." "$LOGIN_OUT"
    fi
fi

# ---- Bitwarden polkit action (only if Bitwarden is installed) ----
# Bitwarden's "Unlock with system authentication" is a polkit action that
# re-authenticates through the polkit-1 stack patched above. Flatpak and Snap
# builds cannot install the action themselves (see
# https://bitwarden.com/help/biometrics/#tab-linux). The policy below is
# transcribed from Bitwarden's own source, the string its native builds write
# via pkexec: apps/desktop/src/key-management/biometrics/native-v2/
# os-biometrics-linux.service.ts in github.com/bitwarden/clients.
if command -v bitwarden &>/dev/null || command -v bitwarden-desktop &>/dev/null \
   || flatpak info com.bitwarden.desktop &>/dev/null \
   || snap list bitwarden-desktop &>/dev/null; then
    # Where the tray's policy went: /usr/share, or /usr/local/share on a
    # read-only /usr (Fedora Atomic, Aeon), which polkit 124 and later read.
    BW_POLICY="$POLKIT_ACTIONS_DIR/com.bitwarden.Bitwarden.policy"
    if [ -f /usr/share/polkit-1/actions/com.bitwarden.Bitwarden.policy ] || [ -f "$BW_POLICY" ]; then
        ok Bitwarden "polkit unlock action already present"
    elif ! { mkdir -p "$POLKIT_ACTIONS_DIR" && touch "$BW_POLICY"; } 2>/dev/null; then
        warn Bitwarden "Cannot write $BW_POLICY, so system unlock isn't wired."
    else
        cat > "$BW_POLICY" <<'EOF'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE policyconfig PUBLIC
 "-//freedesktop//DTD PolicyKit Policy Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/PolicyKit/1.0/policyconfig.dtd">

<policyconfig>
    <action id="com.bitwarden.Bitwarden.unlock">
      <description>Unlock Bitwarden</description>
      <message>Authenticate to unlock Bitwarden</message>
      <defaults>
        <allow_any>no</allow_any>
        <allow_inactive>no</allow_inactive>
        <allow_active>auth_self</allow_active>
      </defaults>
    </action>
</policyconfig>
EOF
        chown root:root "$BW_POLICY"
        chmod 644 "$BW_POLICY"
        touch "$SHARE_DIR/.bitwarden-policy-installed"
        command -v restorecon &>/dev/null && restorecon "$BW_POLICY" 2>/dev/null || true
        ok Bitwarden "polkit action installed. Enable: Settings > Unlock with system authentication"
    fi
fi

# ---- SELinux policy (for lock screen) ----
# Decide on whether SELinux is enabled, not on whether the tools are installed:
# Ubuntu/Debian use AppArmor and need no policy, and loading a module into a
# kernel with SELinux off is pointless.
selinux_enabled=0
if command -v selinuxenabled &>/dev/null; then
    selinuxenabled && selinux_enabled=1
elif [[ -e /sys/fs/selinux/enforce ]]; then
    selinux_enabled=1
fi

if [[ $selinux_enabled -eq 0 ]]; then
    step "SELinux not enabled; lock-screen policy not needed"
elif command -v checkmodule &>/dev/null && command -v semodule_package &>/dev/null; then
    step "loading the SELinux policy (semodule can take 10-30 seconds)"
    mkdir -p "$SELINUX_DIR"
    cp selinux/face-auth.te "$SELINUX_DIR/face_auth.te"
    cp selinux/face-auth.fc "$SELINUX_DIR/face_auth.fc"
    # The policy is written against Fedora's types. Another distro's policy
    # (openSUSE's, say) may lack one, and a failed load must not stop the
    # deploy here, with PAM patched and the template store not yet set up:
    # only the greeters need it.
    if checkmodule -M -m -o "$SELINUX_DIR/face_auth.mod" "$SELINUX_DIR/face_auth.te" \
       && semodule_package -o "$SELINUX_DIR/face_auth.pp" -m "$SELINUX_DIR/face_auth.mod" -f "$SELINUX_DIR/face_auth.fc" \
       && semodule -i "$SELINUX_DIR/face_auth.pp"; then
        # udev labels the NPU node from here on; this covers the one already there.
        [ -d /dev/accel ] && restorecon -R /dev/accel 2>/dev/null || true
        [ -f /usr/local/libexec/vinoauthface-unseal ] && restorecon /usr/local/libexec/vinoauthface-unseal
        ok SELinux "greeter policy loaded"
    else
        warn SELinux "Could not load the greeter policy (errors above), so login screens can't reach the camera." \
            "sudo, polkit and the lock screens are unaffected. Please open an issue with the error and your distro."
    fi
else
    SELINUX_HINT="$(install_hint "policycoreutils checkpolicy" "policycoreutils checkpolicy" "" "policycoreutils checkpolicy")" \
        || SELINUX_HINT="install policycoreutils and checkpolicy"
    warn SELinux "Tools not found, so the login screens can't reach the camera." \
        "$SELINUX_HINT, then re-run this script."
fi

# The policy labels these; on a first install they were created before it
# was loaded.
if [[ $selinux_enabled -eq 1 ]] && command -v restorecon &>/dev/null; then
    restorecon -R "$VAR_DIR" "$NPU_CACHE_DIR" 2>/dev/null || true
fi
