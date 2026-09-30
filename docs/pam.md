# PAM integration

`deploy.sh` adds `auth sufficient pam_exec.so quiet stdout /usr/local/bin/vinoauthface-auth` to:

| Service | File | Covers |
|---------|------|--------|
| `sudo` | `/etc/pam.d/sudo` | sudo |
| `gdm-password` | `/etc/pam.d/gdm-password` | GNOME lock screen |
| `swaylock` | `/etc/pam.d/swaylock` | swaylock |
| `polkit-1` | `/etc/pam.d/polkit-1` | polkit prompts |
| `kde-fingerprint` | `/etc/pam.d/kde-fingerprint` | KDE lock screen |
| `plasmalogin-fingerprint` | `/etc/pam.d/plasmalogin-fingerprint` | Plasma login screen |
| `cosmic-greeter` | `/etc/pam.d/cosmic-greeter` | COSMIC lock screen and greeter |

The line goes just above the first `auth` line that actually authenticates: `pam_unix`,
`pam_sss`, `pam_fprintd`, `pam_u2f`, or an `include`/`substack`/`@include` of another stack.
Gates above that point, like `pam_nologin`, `pam_faillock preauth` and Fedora's
`pam_selinux_permit`, still run before a face match can end the stack. Gates inside an included
stack (Fedora's `system-auth`, say) don't.

If a stack the service includes already runs vinoAuthFace (added by hand, an authselect profile,
an older install), `deploy.sh` skips the service rather than scan the camera twice per attempt.

`sufficient` means a face match (exit 0) authenticates immediately. Anything else (no match, no
camera, timeout, lockout) falls through to the password prompt. `quiet` keeps `pam_exec` chatter
out of the unlock UI.

While scanning, `vinoauthface-auth` shows `Looking for your face...`:

- **With a terminal** (`sudo` in a terminal or on a VT), it's written to the controlling terminal
  and erased when the scan ends.
- **Without one** (lock screens, login screens, polkit agents), it's written once to stdout, which
  `stdout` has `pam_exec` relay to the application as a PAM info message, as it arrives. The
  application decides how to show it and replaces it with its own prompt if the scan falls
  through. `pam_exec` relays stderr the same way, so in this mode `vinoauthface-auth` sends its
  stderr to `/dev/null`: errors are in `journalctl -t vinoauthface-auth` either way.

Installs from before this change have the line without `stdout` and stay silent on lock screens
until `deploy.sh` is re-run (`update.sh` doesn't touch PAM).

## KDE

KScreenLocker starts the `kde-fingerprint` service up front, alongside the password field, so the
KDE lock screen unlocks hands-free: look at the camera and it opens, or type your password as
usual. `plasmalogin-fingerprint` is the same pattern for the Plasma login manager.

Plasma before 6.7 has a bug where a biometric unlock counts against `pam_faillock`
(kscreenlocker 29d01bf7), so repeated face unlocks can lock the password out until it expires.
Update Plasma or drop `pam_faillock` from the stack if you hit it.

## COSMIC

`cosmic-greeter` is one service for both the lock screen and the greeter. It shows a single PAM
message at a time, so the password field is hidden while vinoAuthFace scans, and there is no text
to say so. At login it runs as the `greeter` user rather than the person unlocking.

## Vendor-only services

On image-based distros these services often exist only as a vendor default in `/usr/lib/pam.d`.
`deploy.sh` copies that file to `/etc/pam.d` first to have something to patch (as it does for
`polkit-1`), and `uninstall.sh` deletes the copy rather than "restoring" a file that never
existed.

## polkit

`polkit-1` covers every polkit `auth_self` prompt: `pkexec`, package-manager GUIs, settings
changes, and apps like Bitwarden that ask polkit to re-authenticate you. Most distros ship only
the vendor default in `/usr/lib/pam.d/polkit-1`, so `deploy.sh` copies it to `/etc/pam.d/polkit-1`
first to have something to patch. `uninstall.sh` deletes that copy rather than "restoring" a file
that never existed.

vinoAuthFace only authenticates the account running the prompt. When an admin authenticates *on
behalf of* another logged-in user, it falls through to the password.

## Bitwarden biometric unlock

If a Bitwarden desktop client is installed (native, Flatpak or Snap), `deploy.sh` also installs
Bitwarden's polkit action (`/usr/share/polkit-1/actions/com.bitwarden.Bitwarden.policy`). Flatpak
and Snap builds are sandboxed and can't write it themselves. The content is transcribed from
Bitwarden's own source (`os-biometrics-linux.service.ts` in
[`bitwarden/clients`](https://github.com/bitwarden/clients)), not downloaded. Then enable
**Settings → Unlock with system authentication** in Bitwarden.

## Keyrings

A face match means PAM never sees a password. Any module that unlocks a secret store from the
login password has nothing to unlock it with, so after a face *login*:

| Store | Module | After face login |
|---|---|---|
| KWallet | `pam_kwallet5` | Locked |
| oo7 (Fedora 45's default Secret Service) | `pam_oo7` | Locked |
| GNOME Keyring | `pam_gnome_keyring` | Locked |

The first app that needs a secret asks for the password once, then the store stays open for the
session.

- **Lock screen unlock is fine.** The store was opened at login and stays open across screen
  locks, so there's nothing for vinoAuthFace to gate there.
- **Plasma Login Manager quirk.** If you typed a password and your face matched first, PLM still
  hands the typed text to `pam_kwallet5`, so the wallet unlocks. A mistyped password gives a face
  login with a locked wallet.
- **No TPM-sealed password.** Some projects seal a copy of the login password to the TPM and feed
  it back as `PAM_AUTHTOK` after a face match. `pam_exec` can't set `PAM_AUTHTOK`, so that needs a
  real PAM `.so` module, which this project doesn't ship. Not planned.
