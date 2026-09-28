# Tray icon

An optional tray icon for the session: status at a glance, and enrol, retrain, test and uninstall
without remembering CLI flags. It replaces upstream's GTK settings GUI. It is a separate
per-user app and is never part of authentication: `face-auth` doesn't know it exists.

Plasma shows it natively (StatusNotifierItem). GNOME needs the AppIndicator extension.

## Install

```bash
sudo ./deploy.sh --with-tray        # or: sudo FACE_AUTH_TRAY=1 ./deploy.sh
```

It starts at the next login, or run `face-auth-tray` now. `uninstall.sh` removes it.

| File | What |
|---|---|
| `/usr/local/bin/face-auth-tray` | The tray. Runs as you |
| `/usr/local/libexec/face-auth-helper` | Root helper that the tray runs through pkexec |
| `/usr/share/polkit-1/actions/io.github.karanshukla.vinoauthface.policy` | One polkit action per helper verb. On a read-only `/usr` it goes in `/usr/local/share/polkit-1/actions` instead, which needs polkit 124 or later |
| `/etc/xdg/autostart/vinoauthface-tray.desktop` | Starts it at login |
| `/usr/local/share/applications/vinoauthface-{tray,enrol}.desktop` | Launchers. "Enrol face" works without the tray running |
| `/usr/local/share/face-auth/uninstall.sh` | Installed by every deploy, so the uninstall entry works after the repo is gone |

## Menu

| Entry | Does | Runs as |
|---|---|---|
| Status | Camera, enrolled or not, backend | you |
| Enrol face | `face-enroll --user <you>`. Once you're enrolled it becomes "Enrol again from scratch" and takes a second click, since it replaces your templates | root, via pkexec |
| Retrain face | `face-enroll --user <you> --improve`: captures more frames (new lighting, glasses) and keeps the old ones | root, via pkexec |
| Test scan | One live scan, the same way the lock screen runs it. The result is a notification | you |
| Uninstall | Runs `uninstall.sh` after a second click. Your templates are kept, as with `sudo ./uninstall.sh`. The tray exits when it finishes | root, via pkexec |

Enrolment progress ("Capturing frame 3/30", "Face too small: move closer") shows in the tooltip
and a notification. The tray reads it from `face-enroll`'s normal output.

The icon is a monochrome viewfinder that follows your Plasma theme, with a green face when ready.
Left click opens the menu, which doubles as the status panel.

The icon turns amber while any face scan is running: `sudo`, a polkit prompt, or a test scan. It
checks for a running `face-auth` process in `/proc` every 200 ms. It can't show on the KDE lock
screen, which has no tray. With `/proc` mounted `hidepid=1` or `2`, root's scans are invisible and
the icon doesn't change.

## Privileges

- **Nothing root-owned writes to your session.** The scanning icon comes from `/proc`. Upstream's
  GNOME indicator had root write a status file in `/run/user/<uid>`, which could be pointed
  elsewhere with a symlink. That approach isn't used here.
- **pkexec runs a fixed helper, never `face-enroll`.** polkit's `exec.path` pins a binary but not
  its arguments, so `pkexec face-enroll --embeddings-dir …` would let the caller choose where
  root writes. `face-auth-helper` takes exactly one verb (`enrol`, `retrain` or `uninstall`) and
  no flags. The policy pins each verb with `exec.argv1`. The target user comes from
  `PKEXEC_UID`, which pkexec sets, so there's no way to name another account. All three actions
  are `auth_admin`.
- **Enrolment status without reading the store.** The store is closed to you, so the tray asks
  `face-auth --enrolled`, which runs with the `face-auth` group and only answers for your own
  user ID.
- **Test scan counts toward the lockout**, like any other scan. Otherwise it would be an
  unthrottled way to try faces.

### A face can approve enrol and retrain

The polkit password prompt goes through the `polkit-1` PAM stack, and `deploy.sh` adds
face-auth to that stack. So a face match can approve the tray's enrol, retrain and uninstall
actions, the same way it can approve `sudo`. polkit can't send one action through a different PAM
service. Someone who spoofs your face (see [security.md](security.md#presentation-attacks-something-held-up-to-the-real-camera))
could add their own frames with Retrain. They could do that anyway through
`sudo face-enroll`, since a face also satisfies `sudo`, so the tray grants nothing new. To require
a password for these actions, remove face-auth from `polkit-1` (see [pam.md](pam.md)).
