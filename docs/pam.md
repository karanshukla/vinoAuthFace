---
title: PAM integration
nav_order: 4
---

# PAM integration

`deploy.sh` (via `setup.sh`) adds the following line to supported PAM services:

```text
auth sufficient pam_exec.so quiet stdout /usr/local/bin/vinoauthface-auth
```

| Service | File | Covers |
|---|---|---|
| `sudo` | `/etc/pam.d/sudo` | `sudo` (including `sudo -i` and `sudo su -`) |
| `gdm-password` | `/etc/pam.d/gdm-password` | GNOME lock screen |
| `swaylock` | `/etc/pam.d/swaylock` | swaylock |
| `hyprlock` | `/etc/pam.d/hyprlock` | hyprlock (Hyprland's screen locker) |
| `polkit-1` | `/etc/pam.d/polkit-1` | polkit authentication prompts |
| `kde-fingerprint` or `kde-smartcard` | `/etc/pam.d/kde-fingerprint` or `/etc/pam.d/kde-smartcard` | KDE lock screen (one of the two; see [KDE](#kde)) |
| `cosmic-greeter` | `/etc/pam.d/cosmic-greeter` | COSMIC lock screen and greeter |

Face unlock at the KDE login screen (Plasma Login or SDDM) is opt-in and configured separately; see
[Login screen](#login-screen).

### Insertion point and fallback behaviour

The `pam_exec` line is inserted immediately above the first `auth` rule that performs
authentication: `pam_unix`, `pam_sss`, `pam_fprintd`, `pam_u2f`, or an `include`/`substack`/`@include`
directive. Account gates above that point (such as `pam_nologin`, `pam_faillock preauth`, and
Fedora's `pam_selinux_permit`) still run before a face match can finish the stack. Gates placed
inside an included stack (such as Fedora's `system-auth`) are bypassed on a match.

If a stack included by a service already runs vinoAuthFace (for example via a manual edit, an
`authselect` profile, or an older install), `deploy.sh` skips patching that service so the camera is
not scanned twice per attempt.

Because the control flag is `sufficient`, a face match (exit code `0`) authenticates immediately.
Any other outcome (no match, missing camera, timeout, or active lockout) falls through to the normal
password prompt. The `quiet` option suppresses default `pam_exec` status messages in the unlock UI.

### Scan status message

While scanning, `vinoauthface-auth` displays `Looking for your face...`:

- **In a terminal** (`sudo` in a terminal emulator or VT): the message is written directly to the
  controlling terminal and cleared when the scan finishes.
- **Without a terminal** (lock screens, login screens, and polkit agents): the message is written
  once to stdout, which `pam_exec`'s `stdout` option forwards to the application as a PAM info
  message. The application decides how to render it and replaces it with a password prompt if face
  auth falls through. Because `pam_exec` forwards stderr the same way, `vinoauthface-auth` redirects
  stderr to `/dev/null` in this mode; errors are still logged to `journalctl -t vinoauthface-auth`.
  The KDE lock screen does not display this message (see [KDE](#kde)).

On text consoles, only `sudo` (and commands that authenticate through it, such as `sudo -i` and
`sudo su -`) is configured. `su` is excluded because it authenticates the *target* account rather
than the caller, and console `login` is not wired up.

> Older installs whose PAM lines lack `stdout` will stay silent on lock screens until you re-run
> `sudo ./deploy.sh` or `sudo vinoauthface-setup` (`update.sh` does not modify PAM files).

## KDE

Before Plasma 6.8, KScreenLocker ran two background PAM services alongside the password field:
`kde-fingerprint` and `kde-smartcard`. Starting with Plasma 6.8, only `kde-fingerprint` runs in the
background; `kde-smartcard` (and Plasma's own `kde-face`) run only when explicitly chosen in place
of the password.

`deploy.sh` places vinoAuthFace in a background service so you do not have to select a method on the
lock screen. Wake the screen with any key, click, or touch and look at the camera, or simply type
your password. Plasma does not start any PAM service until the lock screen wakes, so scans never run
while the display is asleep.

| `kde-fingerprint` state | `kde-smartcard` state | Target service | Control flag |
|---|---|---|---|
| Does not run `pam_fprintd` | Any | `kde-fingerprint` | `[success=done new_authtok_reqd=done default=die]` |
| Runs `pam_fprintd` | No smartcard module, Plasma < 6.8 | `kde-smartcard` | `[success=done new_authtok_reqd=done default=die]` |
| Runs `pam_fprintd` | Runs `pam_pkcs11`, `pam_p11`, or `pam_sss`, is missing, or Plasma >= 6.8 | `kde-fingerprint` | `sufficient` |

Detection checks both the service file itself and any stack it includes (such as Fedora's
`fingerprint-auth` and `smartcard-auth`). On Fedora, `fingerprint-auth` invokes `pam_fprintd` only
when `authselect`'s `with-fingerprint` feature is enabled (even without a fingerprint reader);
otherwise it is a stub. `deploy.sh` detects Plasma 6.8+ by checking for `kscreenlocker_worker`.

When placed in `kde-smartcard`, face unlock and fingerprint unlock run in parallel and Plasma shows
"(or scan your smartcard)" as its hint. When sharing `kde-fingerprint` with `pam_fprintd`, face
unlock runs first and the fingerprint reader starts if face unlock fails. Re-run `sudo ./deploy.sh`
or `sudo vinoauthface-setup` after enabling or disabling fingerprint login so the slot updates
accordingly.

A few KDE lock screen behaviours to keep in mind:

- **No status text:** Plasma's lock screen only renders PAM info messages from the main `kde`
  password service and drops messages from background services (just as it does for `pam_fprintd`).
  By contrast, the Plasma Login screen runs face unlock in its main stack and does display
  `Looking for your face...`.
- **Why `default=die` is used in dedicated slots:** When vinoAuthFace is the only real module in a
  background slot, a miss ends the stack with an error instead of falling through. On Fedora,
  falling through reaches a stub that returns "unavailable", which causes KScreenLocker to disable
  that service until the next lock.
- **Retrying a missed scan:** KScreenLocker launches background services once when the lock screen
  wakes, and moving the mouse again will not trigger a retry. To scan again, press Enter on an empty
  password field (or submit a wrong password); the failed password attempt restarts all services
  after 3 seconds.

## Login screen

Face unlock at the KDE login screen supports three modes and is disabled (`off`) by default. It
works with both Plasma Login (Fedora 45+) and SDDM (Arch, openSUSE, Debian, Kubuntu, and older
Fedora). If both are installed, the one referenced by `display-manager.service` is used (falling
back to Plasma Login).

| Mode | PAM configuration | Login screen behaviour |
|---|---|---|
| `off` | None | Password only |
| `both` | `required` line placed below the password stack (`plasmalogin` or `sddm`) | Enter your password, then look at the camera. Both must succeed. Your password still unlocks KWallet. |
| `face` | `sufficient` line above the password in `plasmalogin-fingerprint` (if present), otherwise `plasmalogin` or `sddm` | A face match logs you in; any other outcome falls back to the password. KWallet remains locked until prompted (see [Keyrings](#keyrings)). |

Configure the mode using any of these methods:

- `sudo vinoauthface-setup --login=off|both|face` (also accepted by `install.sh`, `deploy.sh`, or
  via `FACE_AUTH_LOGIN`)
- The tray icon's **Login screen** submenu
- `sudo /usr/local/share/face-auth/login-mode.sh off|both|face` (run `login-mode.sh status` to
  inspect the current mode)

Running `deploy.sh` or `vinoauthface-upgrade` without `--login` preserves your existing mode.

### Greeter behaviour and caveats

- **Hands-free vs. pressing Enter:** `plasmalogin-fingerprint` scans hands-free alongside the
  password field. Fedora 45 ships only `plasmalogin`, and SDDM has no hands-free service, so in
  `face` mode on those systems you select your user and press Enter with an empty password field.
  Only one service is ever patched so the greeter never scans twice.
- **Recovery in `both` mode:** Because `both` makes face unlock `required`, a covered camera, closed
  lid, active lockout, or broken NPU driver will block graphical login. Switch to a text console
  (`Ctrl+Alt+F3`), log in, and run `sudo /usr/local/share/face-auth/login-mode.sh off`. Note also
  that entering a wrong password still runs the camera scan before the login fails, because PAM
  evaluates the rest of the stack.
- **GDM and COSMIC:** These are not managed by `--login` because `gdm-password` and `cosmic-greeter`
  also serve their lock screens, where they keep the standard face-or-password configuration.
- **SELinux and sealed templates:** Greeters run inside SELinux's `xdm_t` domain and require the
  policy installed by `deploy.sh` (see [install.md](install.md#selinux)). When
  `seal_embeddings = true`, `vinoauthface-unseal` handles decryption in its own SELinux domain so
  the greeter never accesses the TPM or systemd's host key. Older installs lacking the unseal helper
  cannot unseal at the greeter (`face` falls back to password, and `login-mode.sh` refuses `both`);
  reinstalling fixes this.
- **Plasma < 6.7 `pam_faillock` bug:** Earlier Plasma versions counted biometric unlocks as password
  failures against `pam_faillock` (`kscreenlocker` commit `29d01bf7`), which could temporarily lock
  out password login. Upgrade Plasma or remove `pam_faillock` from the stack if you encounter this.

## hyprlock

`hyprlock` starts PAM as soon as the screen locks, so the face scan runs immediately and the
password prompt waits until it finishes. Like `swaylock`, `hyprlock` runs as your user and relies on
`vinoauthface-auth`'s set-group-ID `face-auth` bit (see [security.md](security.md#trust-model)). If
you configured `hyprlock` to use a custom PAM service via `auth:pam:module`, add the PAM line to
that service manually.

## COSMIC

`cosmic-greeter` handles both the lock screen and the login greeter. It displays only one PAM prompt
at a time, so the password field stays hidden while vinoAuthFace is scanning and no status text is
shown. At the login screen, it runs as the `greeter` user rather than the target user.

## Vendor-only services

On image-based distributions, KDE and COSMIC PAM files often exist only as vendor defaults under
`/usr/lib/pam.d`. On openSUSE (Tumbleweed, Leap 16, Aeon, MicroOS), all PAM services—including
`sudo`—ship in `/usr/lib/pam.d`.

For any supported service that only exists in `/usr/lib/pam.d`, `deploy.sh` copies the vendor file
to `/etc/pam.d` before patching it (the same mechanism used for `polkit-1`). `uninstall.sh` removes
any copy created by `deploy.sh`. Note that while a copy exists in `/etc/pam.d`, it shadows the
vendor file in `/usr/lib/pam.d`, so distro updates to that vendor file will not take effect until
you uninstall or merge them.

## NixOS

Because NixOS generates `/etc/pam.d` from `configuration.nix` and `nixos-rebuild` would overwrite
manual edits, `vinoauthface-setup` (and `deploy.sh`) skips PAM modifications on NixOS and logs a
notice. Configure the `pam_exec` line through `security.pam.services.<name>` in your NixOS
configuration instead. NixOS is not a tested target.

## polkit

Patching `polkit-1` enables face unlock for all polkit `auth_self` prompts, including `pkexec`,
GUI package managers, system settings changes, and applications like Bitwarden that request
re-authentication through polkit. Most distributions keep the default `polkit-1` file in
`/usr/lib/pam.d/polkit-1`; `deploy.sh` copies it to `/etc/pam.d/polkit-1` to patch it, and
`uninstall.sh` deletes that copy on removal.

vinoAuthFace only authenticates the account initiating the prompt. When an administrator
authenticates on behalf of another logged-in user, PAM falls through to the password prompt.

## Bitwarden biometric unlock

If a Bitwarden desktop client is detected (native, Flatpak, or Snap), `deploy.sh` installs
Bitwarden's polkit action to `/usr/share/polkit-1/actions/com.bitwarden.Bitwarden.policy` (or
`/usr/local/share/polkit-1/actions/` on read-only `/usr` systems like Fedora Atomic, which requires
polkit 124 or newer). Sandboxed Flatpak and Snap builds cannot install this policy file themselves.

The policy content is transcribed directly from Bitwarden's source
(`os-biometrics-linux.service.ts` in [`bitwarden/clients`](https://github.com/bitwarden/clients))
rather than fetched over the network. After installation, enable
**Settings → Unlock with system authentication** in Bitwarden.

## Keyrings

When you log in with your face, PAM never receives your login password. Any PAM module that relies
on that password to unlock a secret store cannot open it automatically:

| Secret store | PAM module | State after face login |
|---|---|---|
| KWallet | `pam_kwallet5` | Locked |
| oo7 (Fedora 45's default Secret Service) | `pam_oo7` | Locked |
| GNOME Keyring | `pam_gnome_keyring` | Locked |

The first application that requests a stored secret will prompt for your password once, after which
the store remains unlocked for the rest of the session.

- **Lock screen unlock is unaffected.** The keyring was unlocked at login and stays open across
  screen locks.
- **Plasma Login Manager quirk:** If you start typing your password and your face matches first,
  Plasma Login Manager still passes the typed text to `pam_kwallet5`, unlocking KWallet. If you
  mistyped the password, face login still succeeds while KWallet stays locked.
- **No TPM-sealed password injection:** Some projects seal a copy of the user's login password in
  the TPM and inject it into `PAM_AUTHTOK` after a face match. `pam_exec` cannot set `PAM_AUTHTOK`,
  so doing this requires a native C/Rust PAM `.so` module, which vinoAuthFace does not ship or plan
  to add.
