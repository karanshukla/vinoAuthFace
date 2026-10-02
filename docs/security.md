# Security model and limitations

Face unlock here is a convenience over a password you still have, not a stronger factor. To
report a vulnerability, see [SECURITY.md](../SECURITY.md).

## Trust model

- **Face templates are root-owned.** `/var/lib/face-auth` is `root:face-auth` mode `2750`, with
  templates at `0640`. Whatever can write a template decides whose face unlocks that account, so
  enrolment goes through `sudo`.
- **`vinoauthface-auth` is set-group-ID `face-auth`, not set-user-ID root.** Lock screens (KScreenLocker,
  swaylock) run PAM as the user, so the binary borrows a group that can read templates and write
  only `<user>/lockout/` (`2770`). A bug in it exposes templates and lockout counters, never root.
  Nothing else has the group and the store is closed to everyone else, so a user can't reach
  their own lockout state to reset it.
- **Borrowed privileges mean an untrusted caller.** When the real and effective user or group IDs
  differ, `vinoauthface-auth` drops the caller's environment (keeping only what `pam_exec` sets) and only
  lets a non-root caller authenticate its own account.
- **The PAM path trusts only `/etc/face-auth.toml`.** A user's own config may make matching
  stricter, never looser, and may not redirect model or template paths, unpin the camera, or lift
  the lockout. `FACE_AUTH_*` environment variables are ignored during authentication. Details:
  [configuration.md](configuration.md).
- **Identity comes from `PAM_USER` only.** `vinoauthface-auth` refuses to run if PAM didn't set it.
- **Remote sessions are refused.** If `PAM_RHOST` names a non-local host, face authentication is
  declined: the camera is at the console, so otherwise whoever sits at the desk would authenticate
  an SSH session. `sudo` over SSH doesn't set `PAM_RHOST`, so vinoAuthFace also walks its own
  process ancestry and skips the scan if an `sshd` is found (`abort_if_ssh`, on by default). A
  `tmux` or `screen` session started over SSH and reattached later isn't caught: its server's
  parent is init, not `sshd`.
- **The tray acts only through a fixed root helper.** The optional tray runs as the user. For
  enrol, retrain and uninstall it runs `vinoauthface-helper` through pkexec. The helper takes one verb,
  no flags, and acts only for `PKEXEC_UID`. A face match can approve those polkit prompts, the same
  as `sudo`. See [tray.md](tray.md#privileges).
- **Only the user at the seat.** vinoAuthFace reads logind's state in `/run/systemd` and declines
  unless the target account owns the active seat0 session, or seat0 is showing a login greeter.
  With fast user switching, a `sudo` in B's background session won't match A's face while A is at
  the desk. No active session, or state it can't read, declines too. Without `/run/systemd/seats`
  (no logind) the check is skipped. `guards.seat_check = false` turns it off.

## Templates at rest (TPM sealing)

Off by default; set `seal_embeddings = true` in `/etc/face-auth.toml`, then enrol again (or run
`vinoauthface improve`, which rewrites the existing templates sealed). The template
payload is encrypted by `systemd-creds` with a key sealed to this machine's TPM, so a stolen
laptop or a disk booted in another OS yields nothing usable. Nothing extra to install: it is part
of systemd, and the blobs live in `/var/lib/face-auth`, which survives image updates.

- **It defends the powered-off case only.** On the running machine root can unseal any user's
  templates, and each user their own (as `vinoauthface-auth` does). It is not protection against a
  local root attacker.
- **No PCR policy.** The key survives firmware, bootloader and `bootc`/`rpm-ostree` updates. The
  price is that it doesn't notice a modified boot chain.
- **Bound to the account and the model.** The user name and recognition-model tag are part of the
  sealed credential's name, which systemd authenticates, so a blob copied to another account or
  relabelled for another model won't unseal.
- **No TPM: enrolment warns and stores unsealed.** Hardware without one still works.
- **Sealed but unsealable is an error, not a fallback.** After a TPM clear or a board swap the
  key is gone. Face auth declines, PAM falls through to your password, it doesn't count toward
  lockout, and the message tells you to re-enrol.
- **Downgrade guard.** With the option on, a plaintext template file is refused. Someone with
  offline write access can also edit `/etc/face-auth.toml`, so this stops a swapped file, not a
  determined offline attacker; only a PCR policy (deliberately not used) would.
- **Sealed per user.** Credentials are user-scoped (`systemd-creds --uid`, systemd 256+). A lock
  screen runs as you and unseals your own templates with no polkit prompt; `sudo` and the polkit
  helper run as root, which can unseal anyone's. A greeter checking a *different* user (running as
  `gdm`/`sddm`) can't, and falls through to the password. The key also mixes in the user name, UID
  and `machine-id`: change any of them and you re-enrol. Unsealing adds to each scan's start-up;
  check `RUST_LOG=face_auth_core=debug` "store loaded" timing on your hardware.
- **Needs both the TPM and `/var/lib/systemd/credential.secret`.** Scoped credentials can't use
  the TPM alone (`host+tpm2`). The host secret is root-only, and a disk without this machine's
  TPM still can't unseal. Under SELinux only `vinoauthface-unseal`'s domain gets them, never the
  greeter's (see [install.md](install.md#selinux)).

## Presentation attacks (something held up to the real camera)

- **Screens are blocked by sensor physics.** OLED and most LCD panels emit essentially no near-IR
  and barely reflect the camera's illuminator, so a phone showing your photo produces no
  face-shaped IR signal. Measured: zero detections across 100 attempts against a phone screen.
- **Motion liveness** requires motion of the face against an earlier frame of the scan, up to
  `liveness.window_ms` (default 1 s) back: a face held still barely changes in the ~130 ms between
  consecutive frames, but drifts over a second, while a mounted photo doesn't change against any
  frame ([#96](https://github.com/karanshukla/vinoAuthFace/issues/96)). Only the face region counts, after removing brightness and contrast changes, so a hand moving beside a still
  photo or an auto-exposure step doesn't pass it. This defeats a rigidly held image, but not a
  photo moved by hand. Once a face has matched and only liveness is missing, the scan runs up to
  `liveness.grace_ms` (default 4 s) longer so a still face isn't failed; that also gives a
  matching photo the same extra time to produce motion.
- **Moved photos: opt-in.** `liveness.residual_motion_threshold` additionally requires motion,
  between consecutive frames, that a single rigid shift (found to a quarter pixel) can't explain, in the most-changed eye-sized block
  of the face: a blink, the mouth, parallax as the head turns. In software tests, synthetic clips
  of a moved, tilted or zoomed IR photo peaked at 0.20, blinks reached 0.3-0.6, and a moving face
  cleared 0.3 in most frame pairs. It stays off by default because a face held still can go the
  whole scan without a qualifying blink: on one real 15 s still clip, with glasses glare covering
  both eyes, the first came about 8.5 s in
  ([#17](https://github.com/karanshukla/vinoAuthFace/issues/17)).
- **Printed photos are an accepted risk.** A laser print of an IR photo of you, moved by hand,
  can pass on a default install: paper reflects NIR, and the moved-photo check above is off by
  default. This has beaten other IR face unlock too (Windows Hello, SySS-2017-027), and it needs
  an IR capture of your face, not an ordinary photo. The defenses above were measured in software
  against synthetic prints, not against real prints on a real sensor, and a print flexed as it
  moves isn't rigid. There's no structured-light or depth check, so 3D masks are out of scope too.
  Accepted because face unlock here is a convenience over a password you still have, not a
  stronger factor ([#17](https://github.com/karanshukla/vinoAuthFace/issues/17)).

## Frame injection (a fake camera)

**Camera binding (default on).** Each enrolment records the camera's USB vendor:product ID in the
user's store (`<user>/cameras`), and a scan from any other camera is refused before it opens:
another webcam, or a camera with no USB identity such as a v4l2loopback node, can't stand in for
the IR sensor. It's an error, not a failed attempt, so PAM falls through to the password and the
lockout isn't touched. Accounts enrolled before this existed, or on a camera with no USB ID (MIPI),
accept any camera until their next enrolment; `sudo vinoauthface doctor` says which. `bind_camera =
false` turns it off. Adapted from [facelock](https://github.com/tyvsmith/facelock)'s device
coupling.

That ID is just a string, though: a programmable USB device can claim the real camera's VID/PID and
feed replayed frames.
`pin-camera.sh` closes this by pinning the camera's physical USB port path and V4L2 index, read
from sysfs, which a spoofed device can't occupy at the same time as the real one. vinoAuthFace
re-verifies that identity on every authentication and enrolment, independent of the udev rule.
Opt-in ([install.md](install.md#pinning-the-camera-recommended)). This is separate from the
presentation defences above; you want both.

Auto-detect only ever considers physical IR-named or greyscale nodes, and never virtual
(v4l2loopback) nodes, whose frames any local user can write and whose name is just a module
parameter. A user's own `device` setting is held to the same rule. Only `device` in
`/etc/face-auth.toml` can name a virtual node, for testing.

## Other limitations

- **Rate limiting covers the face factor only.** The lockout throttles repeated face attempts;
  PAM's password fallback is untouched, so wire `pam_faillock` for that separately.
- **`sufficient` bypasses the rest of the auth stack.** A successful match satisfies
  authentication outright, so the strength of the stack becomes the strength of the face match.
  The vinoAuthFace line currently sits above `pam_nologin` and `pam_faillock`
  ([#31](https://github.com/karanshukla/vinoAuthFace/issues/31)).
- **SELinux policy scope:** the greeter policy grants `xdm_t` mmap access to all V4L2
  devices, and labels the NPU as a GPU (`dri_device_t`), so every domain allowed the GPU can
  open it. Narrowing either requires custom udev device types. Only `xdm_t` is covered: TTY
  `login` (`local_login_t`) isn't wired by `deploy.sh`, and confined users' `sudo_t` is untested.
- **Model integrity:** both models are pinned by SHA-256 and the detector URL is pinned to a
  commit. `deploy.sh` aborts on mismatch. Release binaries, and the source bundle `vinoauthface-upgrade` installs from, are verified
  against the release's `SHA256SUMS`. That catches a corrupted or truncated download, not a
  compromised release: `SHA256SUMS` is published alongside the files it covers, so the trust
  anchor is GitHub over TLS.
