---
title: Tray icon
nav_order: 7
---

# Tray icon

vinoAuthFace includes an optional session tray icon that shows live scan state and provides quick
access to enrolment, retraining, and test scans without memorizing CLI flags. It replaces upstream's
GTK settings GUI. The tray runs as a standalone per-user application and plays no part in
authentication; `vinoauthface-auth` does not interact with it.

The tray uses the StatusNotifierItem protocol, and menu items that make privileged changes invoke
`pkexec`, which requires a polkit authentication agent in your session:

| Desktop | Tray support | Polkit agent |
|---|---|---|
| KDE Plasma | Native | Built in |
| GNOME | Requires the AppIndicator extension (enabled by default on Ubuntu; on Fedora install `gnome-shell-extension-appindicator`; on Fedora Atomic run `rpm-ostree install gnome-shell-extension-appindicator` or install from extensions.gnome.org) | Built in |
| COSMIC | Panel status area applet | Built in |
| sway, Hyprland, and other wlroots compositors | Requires a bar with a tray module (such as Waybar's `tray`) | Not built in; start one such as `hyprpolkitagent`, `polkit-gnome`, or `lxqt-policykit` |

Without a desktop tray, the icon will not appear, though the **Enrol face** application launcher
still works. Without a polkit agent, `pkexec` fails with "No authentication agent found"; run the
command in a terminal instead (for example, `sudo vinoauthface enroll --user $USER`).

## Install

```bash
sudo ./deploy.sh
```

The tray is installed by default. Skip it with `sudo ./deploy.sh --no-tray` (or `FACE_AUTH_TRAY=0`).

It starts automatically at your next login, or you can launch `vinoauthface-tray` immediately (it
detaches from the terminal, so closing the terminal leaves it running). Only one instance runs per
session; launching a second `vinoauthface-tray` exits immediately. `uninstall.sh` removes all tray
files.

| Installed file | Purpose |
|---|---|
| `/usr/local/bin/vinoauthface-tray` | Tray application (runs as your user) |
| `/usr/local/libexec/vinoauthface-helper` | Root helper invoked by the tray through `pkexec` |
| `/usr/share/polkit-1/actions/io.github.karanshukla.vinoauthface.policy` | Polkit policy defining one action per helper verb (installed to `/usr/local/share/polkit-1/actions` on read-only `/usr`, requiring polkit 124+) |
| `/etc/xdg/autostart/vinoauthface-tray.desktop` | Autostart entry for login |
| `/usr/local/share/applications/vinoauthface-{tray,enrol}.desktop` | Desktop launchers (**Enrol face** works even when the tray is not running) |
| `/usr/local/share/face-auth/{login,setting}-mode.sh` | Scripts invoked by the Login screen and Settings menu entries |
| `/usr/local/share/face-auth/uninstall.sh` | Uninstaller installed by every deploy (symlinked as `vinoauthface-uninstall`) |
| `/usr/local/bin/vinoauthface-upgrade` | Release upgrader installed by every deploy (invoked by the Update menu entry) |

## Menu

| Entry | Action | Runs as |
|---|---|---|
| **Update to vN** | Appears at the top when a newer GitHub release is available (checked 1 minute after startup, then daily; set `update_check = false` to disable). Runs `vinoauthface-upgrade` ([install.md](install.md#updating)), restarts the tray, and notifies once per release. | root (`pkexec`) |
| **Status** | Submenu showing the detected camera, enrolment state, backend, recognition and detector models, and installed version. Updates its label when scanning, enrolling, or reporting an error (e.g., "Status: No IR camera found", "Status: Enrolling your face: Captured 3/30") with a warning icon on errors. Run `sudo vinoauthface doctor` for full diagnostics. | your user |
| **Enrol face** | Runs `vinoauthface enroll --user <you>`. Shown only before you are enrolled. To re-enrol later, use the [settings window](settings.md)'s Face page or run `sudo vinoauthface enroll --user $USER`. | root (`pkexec`) |
| **Retrain face** | Runs `vinoauthface improve --user <you>` to capture additional frames (such as new lighting or glasses) while keeping existing templates. | root (`pkexec`) |
| **Test scan** | Runs a live verification scan the same way a lock screen does and displays the result as a desktop notification. | your user |
| **Login screen** | Shown on Plasma Login or SDDM systems. Switches between password only, password + face, or face unlock via `login-mode.sh` (see [pam.md](pam.md#login-screen)). | root (`pkexec`) |
| **Settings** | Opens the [settings window](settings.md) if installed. Otherwise opens a submenu with Security preset, scan duration, two system toggles (applied to `/etc/face-auth.toml` via `setting-mode.sh`), and **Hide icon when idle** (saved to `~/.config/face-auth.toml`). Hand-edited values that do not match a menu option appear as "custom". | root (`pkexec`); your user for Hide icon when idle |

### Settings

The built-in Settings submenu is shown only when the standalone settings window is not installed.

Each system option maps to its own fixed polkit action (`set-<setting>-<choice>`), so no arbitrary
values are passed from the user session to root. When updating a dotted key, `setting-mode.sh` also
replaces any legacy flat key (such as `start_delay_ms`) because flat keys take precedence. Custom
hand-edited values continue to work normally and simply display as "custom" in the menu.

| Setting | Config key | Choices |
|---|---|---|
| **Security** | Four keys (see table below) | Convenient, Balanced (default), Strict |
| **Scan time** | `scan_duration_ms` | 3 s, 5 s (default), 8 s, 12 s |
| **Confirm sudo with Enter** | `guards.require_confirmation_elevation` | Toggle (off by default) |
| **Check for updates** | `update_check` | Toggle (on by default; restart tray after disabling) |

The **Security** preset updates four keys together:

| Preset | `liveness.preset` | `threshold` | `min_face_size_ratio` | `guards.start_delay_ms` |
|---|---|---|---|---|
| **Convenient** | `standard` | `0.5` | `0.0` (any size) | `0` |
| **Balanced** | `standard` | `0.6` | `0.0` (any size) | `2000` |
| **Strict** | `strict` | `0.7` | `0.1` (fairly close) | `5000` |

If the four keys do not match one of these presets (for instance after a manual edit), the menu
shows "custom". **Convenient** matches more easily (which also raises lookalike risk) and scans lock
screens immediately; **Strict** may fail if you sit far away or hold completely still. The desktop
notification notes these trade-offs when switching presets. Any explicit `liveness.*` threshold in
`/etc/face-auth.toml` still overrides `liveness.preset`.

Settings that can lock you out, break enrolment, or weaken system security are intentionally
excluded from the tray menu: disabling liveness, individual security keys, camera selection and
pinning, models and backend, template directory and TPM sealing, `bind_camera`, `lockout.*`, other
`guards.*` options, `liveness.window_ms`, `detector_threshold`, `capture_timeout_ms`, and
`scan_interval_ms`. Edit `/etc/face-auth.toml` directly (or use the settings window) for those
options (see [configuration.md](configuration.md)).

**Hide icon when idle** (10 minutes, 30 minutes, 1 hour, or Never) is a per-user preference saved as
`tray_idle_minutes` in `~/.config/face-auth.toml`. It requires no password and takes effect
immediately.

### Icon state and idle hiding

Enrolment progress (such as "Capturing frame 3/30" or "Face too small. Move closer.") is parsed from
`vinoauthface enroll`'s output and shown in both the tray tooltip and desktop notifications.

The icon depicts an IR camera lens and emitter dot inside a rounded frame, styled to match your
Plasma theme. Left-clicking opens the status and action menu:

- **Ready:** Emitter dot is green once you are enrolled and an IR camera is detected.
- **Not ready:** Lens is dimmed and the emitter is off until enrolled with a working camera.
- **Scanning:** Emitter turns amber during any `sudo`, polkit, or test scan. The tray detects active
  scans by polling `/proc` for `vinoauthface-auth` every 200 ms. It cannot appear on the KDE lock
  screen (which has no system tray), and if `/proc` is mounted with `hidepid=1` or `hidepid=2`,
  root-owned scans are hidden so the icon does not turn amber.

After 30 minutes of inactivity (including time spent suspended), the icon sets its StatusNotifier
status to `Passive`, which Plasma moves into the panel's hidden-icons drawer while keeping the tray
running. Any scan, status change (ready, not enrolled, no camera), new release notification, or menu
click restores it to active and resets the timer. Change the timeout via **Settings → Hide icon when
idle** or by setting `tray_idle_minutes` in `~/.config/face-auth.toml` or `/etc/face-auth.toml`
(`0` keeps the icon visible permanently; manual edits take effect after restarting the tray). Other
desktop trays may ignore the `Passive` hint and keep the icon visible.

## Privileges

- **No root-to-user status channel.** Scan state is observed by reading `/proc`. Unlike upstream's
  GNOME indicator (which had root write a status file inside `/run/user/<uid>`, risking symlink
  attacks), nothing running as root writes into your user session.
- **Fixed root helper instead of arbitrary `pkexec` arguments.** Polkit's `exec.path` restricts the
  binary path but not arbitrary flags, so allowing `pkexec vinoauthface enroll --embeddings-dir ...`
  would let an unprivileged caller pick where root writes. Instead, `vinoauthface-helper` accepts
  exactly one verb (`enrol`, `retrain`, `upgrade`, `login-off`, `login-both`, `login-face`, or
  `set-<setting>-<choice>`) with no flags. Each verb is pinned via `exec.argv1` in the polkit
  policy, and the target user is always taken from `PKEXEC_UID` set by `pkexec`. All 17 actions
  require `auth_admin`.
- **Isolated upgrades.** Running `upgrade` through the helper executes as plain root with no
  `SUDO_USER`, downloading and unpacking the release under root's home directory so an unprivileged
  process cannot tamper with files between checksum verification and running `deploy.sh`. Upgrades
  through the tray always install the latest release and do not accept a version argument.
- **Enrolment checks without store access.** Because `/var/lib/face-auth` is unreadable by normal
  users, the tray queries `vinoauthface-auth --enrolled`, which uses the `face-auth` group and only
  reports status for the caller's own UID.
- **Test scans count toward lockout.** Running **Test scan** increments the failed-attempt lockout
  counter just like a real authentication attempt, preventing it from being used as an unthrottled
  oracle.

### A face can approve enrol and retrain

Polkit prompts authenticate through the `polkit-1` PAM stack, which `deploy.sh` patches with
vinoAuthFace. As a result, a face match can approve the tray's enrol, retrain, and upgrade prompts,
just as it can approve `sudo` (polkit cannot route a single action through a separate PAM service).

Someone who manages to spoof your face (see
[security.md](security.md#presentation-attacks-something-held-up-to-the-real-camera)) could therefore
add their own frames using **Retrain face**, though they could already do the same via
`sudo vinoauthface enroll`. If you want to require a password for polkit actions, remove
vinoAuthFace from `/etc/pam.d/polkit-1` (see [pam.md](pam.md)).
