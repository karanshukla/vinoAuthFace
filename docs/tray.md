# Tray icon

An optional tray icon for the session: an icon that shows scan state, and enrol, retrain, test and uninstall
without remembering CLI flags. It replaces upstream's GTK settings GUI. It is a separate
per-user app and is never part of authentication: `vinoauthface-auth` doesn't know it exists.

Plasma shows it natively (StatusNotifierItem). GNOME needs the AppIndicator extension.

## Install

```bash
sudo ./deploy.sh
```

The tray installs by default. Skip it with `sudo ./deploy.sh --no-tray` (or `FACE_AUTH_TRAY=0`).

It starts at the next login, or run `vinoauthface-tray` now (it detaches from the terminal, so closing it leaves the tray running). `uninstall.sh` removes it. Only one tray runs per session: a second `vinoauthface-tray` exits at once.

| File | What |
|---|---|
| `/usr/local/bin/vinoauthface-tray` | The tray. Runs as you |
| `/usr/local/libexec/vinoauthface-helper` | Root helper that the tray runs through pkexec |
| `/usr/share/polkit-1/actions/io.github.karanshukla.vinoauthface.policy` | One polkit action per helper verb. On a read-only `/usr` it goes in `/usr/local/share/polkit-1/actions` instead, which needs polkit 124 or later |
| `/etc/xdg/autostart/vinoauthface-tray.desktop` | Starts it at login |
| `/usr/local/share/applications/vinoauthface-{tray,enrol}.desktop` | Launchers. "Enrol face" works without the tray running |
| `/usr/local/share/face-auth/{login,setting}-mode.sh` | What the login-screen and Settings entries run |
| `/usr/local/share/face-auth/uninstall.sh` | Installed by every deploy, so the uninstall entry works after the repo is gone |
| `/usr/local/bin/vinoauthface-upgrade` | Installed by every deploy (not only with the tray). The upgrade entry runs it |

## Menu

| Entry | Does | Runs as |
|---|---|---|
| Update to vN | Shown at the top only when a newer release exists (checked a minute after start, then daily; `update_check = false` disables it). Runs `vinoauthface-upgrade` (see [install.md](install.md#updating)), then restarts the tray on the new version. On an OpenVINO install it only shows the command, since that build compiles as you. "What's new" next to it opens the release page. A notification appears once per new release | root, via pkexec |
| Status | A submenu: the camera it would use, whether you're enrolled, the backend and the installed version. When something is wrong its label says what ("Status: No IR camera found") with a warning icon. For the full picture, run `sudo vinoauthface doctor` | you |
| Enrol face | `vinoauthface enroll --user <you>`. Once you're enrolled it becomes "Enrol again from scratch" and takes a second click, since it replaces your templates | root, via pkexec |
| Retrain face | `vinoauthface improve --user <you>`: captures more frames (new lighting, glasses) and keeps the old ones | root, via pkexec |
| Test scan | One live scan, the same way the lock screen runs it. The result is a notification | you |
| Login screen | Only with Plasma Login. Password only, password then face, or face: see [pam.md](pam.md#login-screen). Runs `login-mode.sh` | root, via pkexec |
| Settings | One submenu: a Security preset and the scan time as radio lists, and the two on/off settings as checkmarks. Changes `/etc/face-auth.toml` through `setting-mode.sh` (see the table below). A value you set by hand that the menu doesn't offer shows as "custom" with nothing selected | root, via pkexec |
| Uninstall | Runs `uninstall.sh` after a second click. Your templates are kept, as with `sudo ./uninstall.sh`. The tray exits when it finishes | root, via pkexec |

### Settings

Each choice is one polkit action (`set-<setting>-<choice>`), so there is no value the caller can
pass. A flat key already in the file (`start_delay_ms`) is replaced too, since it would win over the
dotted one. A threshold written in the file that no choice matches still works; the menu just calls
it "custom".

| Setting | Key | Choices |
|---|---|---|
| Security | four keys, below | Convenient, Balanced (default), Strict |
| Scan time | `scan_duration_ms` | 3 s, 5 s (default), 8 s, 12 s |
| Confirm sudo with Enter | `guards.require_confirmation_elevation` | Checkmark, off by default |
| Check for updates | `update_check` | Checkmark, on by default. Restart the tray after turning it off |

Security sets four keys together:

| Preset | `liveness.preset` | `threshold` | `min_face_size_ratio` | `guards.start_delay_ms` |
|---|---|---|---|---|
| Convenient | standard | 0.5 | 0.0 (any size) | 0 |
| Balanced | standard | 0.6 | 0.0 (any size) | 2000 |
| Strict | strict | 0.7 | 0.1 (fairly close) | 5000 |

Security shows "custom" when the four keys don't all match one preset, for instance after
setting one of them by hand. Convenient matches more easily (so does a lookalike) and scans the lock
screen at once; Strict may fail if you sit very still or far back. The notification says so. An
explicit `liveness.*` threshold in the file still overrides the liveness preset.

Deliberately not in the menu, because a wrong value locks you out, breaks enrolment or
loosens the system: liveness off, the four security keys one at a time, the camera and its pin,
the models and backend, the template directory and sealing, `bind_camera`, `lockout.*`, the other
`guards.*`, `liveness.window_ms`, `detector_threshold`, and the capture timeout and scan interval.
Edit `/etc/face-auth.toml` for those ([configuration.md](configuration.md)). `tray_idle_minutes`
stays a file setting too: it is per user and read at startup.

Enrolment progress ("Capturing frame 3/30", "Face too small: move closer") shows in the tooltip
and a notification. The tray reads it from `vinoauthface enroll`'s normal output.

The icon is a monochrome viewfinder that follows your Plasma theme, with a green face when ready.
Left click opens the menu, which doubles as the status panel.

The icon turns amber while any face scan is running: `sudo`, a polkit prompt, or a test scan. It
checks for a running `vinoauthface-auth` process in `/proc` every 200 ms. It can't show on the KDE lock
screen, which has no tray. With `/proc` mounted `hidepid=1` or `2`, root's scans are invisible and
the icon doesn't change.

After 30 minutes with nothing happening, the icon reports itself Passive, which Plasma moves into
the hidden icons behind the panel's arrow. The tray keeps running. A scan, a change of status (ready,
not enrolled, no camera), a new release or a click on a menu entry brings it back and restarts the
count. The 30 minutes include time spent suspended. Set the delay with `tray_idle_minutes` in
`~/.config/face-auth.toml` or `/etc/face-auth.toml`; 0 keeps the icon in view. It is read at
startup, so restart the tray after changing it. Other trays may ignore Passive and keep showing the
icon.

## Privileges

- **Nothing root-owned writes to your session.** The scanning icon comes from `/proc`. Upstream's
  GNOME indicator had root write a status file in `/run/user/<uid>`, which could be pointed
  elsewhere with a symlink. That approach isn't used here.
- **pkexec runs a fixed helper, never `vinoauthface enroll`.** polkit's `exec.path` pins a binary but not
  its arguments, so `pkexec vinoauthface enroll --embeddings-dir …` would let the caller choose where
  root writes. `vinoauthface-helper` takes exactly one verb (`enrol`, `retrain`, `uninstall`,
  `upgrade`, `login-off`, `login-both`, `login-face`, or `set-<setting>-<choice>`, one per choice in the Settings table) and no flags. The policy pins each verb
  with `exec.argv1`, so the login-screen mode is one action per mode rather than an argument. The
  target user comes from `PKEXEC_UID`, which pkexec sets, so there's no way to name another
  account. All 18 actions are `auth_admin`.
- **Upgrade and uninstall never touch your home.** They run as plain root with no `SUDO_USER`, so
  the upgrade downloads and unpacks the release under root's home, where nothing you run can swap
  it between the checksum check and `deploy.sh` running it. Upgrade only installs the newest
  release: no version can be named.
- **Enrolment status without reading the store.** The store is closed to you, so the tray asks
  `vinoauthface-auth --enrolled`, which runs with the `face-auth` group and only answers for your own
  user ID.
- **Test scan counts toward the lockout**, like any other scan. Otherwise it would be an
  unthrottled way to try faces.

### A face can approve enrol and retrain

The polkit password prompt goes through the `polkit-1` PAM stack, and `deploy.sh` adds
vinoAuthFace to that stack. So a face match can approve the tray's enrol, retrain, uninstall and
upgrade actions, the same way it can approve `sudo`. polkit can't send one action through a different PAM
service. Someone who spoofs your face (see [security.md](security.md#presentation-attacks-something-held-up-to-the-real-camera))
could add their own frames with Retrain. They could do that anyway through
`sudo vinoauthface enroll`, since a face also satisfies `sudo`, so the tray grants nothing new. To require
a password for these actions, remove vinoAuthFace from `polkit-1` (see [pam.md](pam.md)).
