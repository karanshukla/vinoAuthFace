# Security model and limitations

Face unlock here is a convenience over a password you still have, not a stronger factor. To
report a vulnerability, see [SECURITY.md](../SECURITY.md).

## Trust model

- **Face templates are root-owned.** `/var/lib/face-auth` is `root:face-auth` mode `2750`, with
  templates at `0640`. Whatever can write a template decides whose face unlocks that account, so
  enrolment goes through `sudo`.
- **`face-auth` is set-group-ID `face-auth`, not set-user-ID root.** Lock screens (KScreenLocker,
  swaylock) run PAM as the user, so the binary borrows a group that can read templates and write
  only `<user>/lockout/` (`2770`). A bug in it exposes templates and lockout counters, never root.
  Nothing else has the group and the store is closed to everyone else, so a user can't reach
  their own lockout state to reset it.
- **Borrowed privileges mean an untrusted caller.** When the real and effective user or group IDs
  differ, `face-auth` drops the caller's environment (keeping only what `pam_exec` sets) and only
  lets a non-root caller authenticate its own account.
- **The PAM path trusts only `/etc/face-auth.toml`.** A user's own config may make matching
  stricter, never looser, and may not redirect model or template paths, unpin the camera, or lift
  the lockout. `FACE_AUTH_*` environment variables are ignored during authentication. Details:
  [configuration.md](configuration.md).
- **Identity comes from `PAM_USER` only.** `face-auth` refuses to run if PAM didn't set it.
- **Remote sessions are refused.** If `PAM_RHOST` names a non-local host, face authentication is
  declined: the camera is at the console, so otherwise whoever sits at the desk would authenticate
  an SSH session.

## Presentation attacks (something held up to the real camera)

- **Screens are blocked by sensor physics.** OLED and most LCD panels emit essentially no near-IR
  and barely reflect the camera's illuminator, so a phone showing your photo produces no
  face-shaped IR signal. Measured: zero detections across 100 attempts against a phone screen.
- **Motion liveness** requires pixel-level motion between consecutive face frames before a match
  counts, which defeats a rigidly held static image.
- **Printed photos remain an open risk.** Paper does reflect some NIR, and a gently moved print
  could pass the motion check. There's no structured-light or depth check. High-quality IR-visible
  prints or 3D masks may bypass verification.

## Frame injection (a fake camera)

By default face-auth trusts frames from whatever `device` resolves to. A USB device claiming the
real camera's VID/PID (just a string; any device can) could feed replayed frames.
`pin-camera.sh` closes this by pinning the camera's physical USB port path and V4L2 index, read
from sysfs, which a spoofed device can't occupy at the same time as the real one. face-auth
re-verifies that identity on every authentication and enrolment, independent of the udev rule.
Opt-in ([install.md](install.md#pinning-the-camera-recommended)). This is separate from the
presentation defences above; you want both.

Auto-detect only ever considers IR-named nodes or physical greyscale sensors, and never virtual
(v4l2loopback) nodes, whose format any local user can set.

## Other limitations

- **Rate limiting covers the face factor only.** The lockout throttles repeated face attempts;
  PAM's password fallback is untouched, so wire `pam_faillock` for that separately.
- **`sufficient` bypasses the rest of the auth stack.** A successful match satisfies
  authentication outright, so the strength of the stack becomes the strength of the face match.
  The face-auth line currently sits above `pam_nologin` and `pam_faillock`
  ([#31](https://github.com/karanshukla/vinoAuthFace/issues/31)).
- **SELinux policy scope:** the lock-screen policy grants `xdm_t` mmap access to all V4L2
  devices. Narrowing it requires custom udev device types.
- **Model integrity:** both models are pinned by SHA-256 and the detector URL is pinned to a
  commit. `deploy.sh` aborts on mismatch. Release binaries are verified against `SHA256SUMS`.
