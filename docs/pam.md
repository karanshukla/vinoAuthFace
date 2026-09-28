# PAM integration

`deploy.sh` adds `auth sufficient pam_exec.so quiet /usr/local/bin/face-auth` to:

| Service | File | Insertion point | Covers |
|---------|------|-----------------|--------|
| `sudo` | `/etc/pam.d/sudo` | After `#%PAM-1.0` | sudo |
| `gdm-password` | `/etc/pam.d/gdm-password` | After `pam_selinux_permit.so` (Fedora), else after `#%PAM-1.0` (Ubuntu/Debian) | GNOME lock screen |
| `swaylock` | `/etc/pam.d/swaylock` | After `#%PAM-1.0` | swaylock |
| `polkit-1` | `/etc/pam.d/polkit-1` | After `#%PAM-1.0` | polkit prompts |
| `kde-fingerprint` | `/etc/pam.d/kde-fingerprint` | Above the first `auth` line | KDE lock screen |

`sufficient` means a face match (exit 0) authenticates immediately. Anything else (no match, no
camera, timeout, lockout) falls through to the password prompt. `quiet` keeps `pam_exec` chatter
out of the unlock UI.

There's no visual cue while scanning. Auth either succeeds within the scan window or falls
through to the password prompt.

## KDE

KScreenLocker starts the `kde-fingerprint` service up front, alongside the password field, so the
KDE lock screen unlocks hands-free: look at the camera and it opens, or type your password as
usual. The Plasma login greeter isn't wired up yet
([#32](https://github.com/karanshukla/vinoAuthFace/issues/32)).

## polkit

`polkit-1` covers every polkit `auth_self` prompt: `pkexec`, package-manager GUIs, settings
changes, and apps like Bitwarden that ask polkit to re-authenticate you. Most distros ship only
the vendor default in `/usr/lib/pam.d/polkit-1`, so `deploy.sh` copies it to `/etc/pam.d/polkit-1`
first to have something to patch. `uninstall.sh` deletes that copy rather than "restoring" a file
that never existed.

face-auth only authenticates the account running the prompt. When an admin authenticates *on
behalf of* another logged-in user, it falls through to the password.

## Bitwarden biometric unlock

If a Bitwarden desktop client is installed (native, Flatpak or Snap), `deploy.sh` also installs
Bitwarden's polkit action (`/usr/share/polkit-1/actions/com.bitwarden.Bitwarden.policy`). Flatpak
and Snap builds are sandboxed and can't write it themselves. The content is transcribed from
Bitwarden's own source (`os-biometrics-linux.service.ts` in
[`bitwarden/clients`](https://github.com/bitwarden/clients)), not downloaded. Then enable
**Settings → Unlock with system authentication** in Bitwarden.

## Keyrings

KWallet unlocks once at login from the typed password and stays unlocked across screen locks, so
there's nothing for face-auth to gate at the lock screen. See
[#36](https://github.com/karanshukla/vinoAuthFace/issues/36) for what a face *login* does to
KWallet, oo7 and GNOME Keyring.
