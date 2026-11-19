---
title: Troubleshooting
nav_order: 6
---

# Troubleshooting

Start with these diagnostic commands to inspect your installation and test authentication directly:

```bash
# Check the entire installation: PAM hooks, model checksums, backend, ownership,
# templates, camera, camera pin, TPM sealing, lockout, sudo's targetpw, SELinux,
# and (on NPU builds) the NPU driver. Run with sudo to include enrolment, lockout,
# sudoers, and SELinux checks.
sudo vinoauthface doctor

# Generate a sanitized Markdown report for GitHub issues (strips usernames, paths, and hostnames)
sudo vinoauthface doctor --report

# List V4L2 devices and see which camera auto-detection chooses
vinoauthface-camera-diag list

# Test verification directly with debug logs (bypasses PAM)
sudo env RUST_LOG=face_auth_core=debug,vinoauthface_auth=debug vinoauthface-auth --verify $USER
echo $?   # 0 = match, 1 = no match, 2 = error

# View outcomes of recent PAM scans (match/no match/error, service name, sealed status)
journalctl -b -t vinoauthface-auth

# Check for SELinux denials
sudo ausearch -m avc -ts recent
```

`RUST_LOG` only takes effect when running `--verify` under `sudo`. When `vinoauthface-auth` runs
inside PAM with elevated privileges (under `sudo` or from a lock screen via set-group-ID), it strips
the caller's environment, including `RUST_LOG`. Every scan still records a single line to syslog
(`authpriv`) with the result, PAM service, and whether templates were sealed (for example:
`face match for 'you' (service kde, sealed templates)`).

## "PAM_USER is not set"

`vinoauthface-auth` never infers the target account from `USER` or `LOGNAME`. To test verification
manually from the command line, run `vinoauthface-auth --verify <username>` instead of setting
`PAM_USER` by hand.

## Lock-screen unlock is slow on the NPU backend

This usually means the OpenVINO compile cache in `/var/cache/face-auth` is empty or invalidated,
forcing every unlock to recompile both models on the CPU first. Lock screens run as your user and
can only read the cache; only root can write to it. `deploy.sh` populates the cache during
installation, but NPU driver updates invalidate it. Rebuild it with:

```bash
sudo vinoauthface-auth --warm-cache
```

## "reports pixel format ... expected GREY, YUYV or Y16"

The configured device is not an IR capture node—it is either a standard RGB webcam or the metadata
node paired with the real IR capture node. Unset `device` in `/etc/face-auth.toml` to let
auto-detection choose, or inspect your nodes with `vinoauthface-camera-diag list`.

## "no IR camera found"

`device` is unset, and auto-detection did not find any IR-named node or physical greyscale sensor
that opens as a capture device. Face auth declines and PAM falls through to your password (it never
falls back to `/dev/video0`, which is usually the RGB webcam). Verify the camera is connected and
listed by `vinoauthface-camera-diag list`, or configure `device` explicitly in
`/etc/face-auth.toml`.

## "camera identity mismatch"

Camera pinning is enabled and `device` now points to a different physical USB port or V4L2 node than
what was recorded. If you intentionally moved or replaced the camera, re-run
`sudo vinoauthface-pin-camera`.

## "no face detected" every time

Most Windows Hello IR modules strobe their IR illuminator, alternating between a lit frame and a
nearly black frame. You can inspect this with `cargo run --example frame-stats`. vinoAuthFace
captures frames in pairs and keeps the brighter frame to handle strobing automatically. If *every*
frame is dark, the IR illuminator is not firing at the hardware or driver level and software tuning
cannot fix it.

## Lock screen asks for the password, sudo works

Lock screens invoke `vinoauthface-auth` as your user and rely on its set-group-ID `face-auth` bit:

- Run `ls -l /usr/local/bin/vinoauthface-auth` and verify permissions show `-rwxr-sr-x root face-auth`.
  If the `s` bit is missing, reinstall via the one-line installer or `sudo ./deploy.sh`.
- If `deploy.sh` warned that `/usr/local/bin` is mounted `nosuid`, the kernel ignores the
  set-group-ID bit and lock screens cannot read `/var/lib/face-auth`.
- Confirm your lock screen's PAM service is configured (see [pam.md](pam.md)).

## The lock screen unlocks right after I lock it

If you are still facing the camera when locking the screen, vinoAuthFace may recognize you and
unlock immediately. This is a known issue:
[#21](https://github.com/karanshukla/vinoAuthFace/issues/21). (See also `guards.start_delay_ms`
below.)

## Face auth never runs over SSH, or with the lid closed

This is intentional:

- **SSH sessions:** Because nobody is in front of the local camera over SSH, vinoAuthFace skips
  scanning and lets PAM ask for your password (`guards.abort_if_ssh = true` by default; set to
  `false` in `/etc/face-auth.toml` to disable).
- **Closed laptop lid:** Setting `guards.abort_if_lid_closed = true` (off by default) skips scanning
  when the built-in lid is shut. External IR cameras on ports reported by firmware as removable are
  still used with the lid closed; if a USB hub hides that removable flag, leave the lid check
  disabled.

## The lock screen waits a couple of seconds before scanning

This delay is intentional. Screen lockers start PAM as soon as they lock, so scanning immediately
would unlock the screen while you are still sitting there. vinoAuthFace waits until the locker has
been running for `guards.start_delay_ms` (default `2000` ms) before starting the first scan. Because
the delay is measured from when the locker started, retries on the same lock screen do not wait
again. On Plasma 6.8 (where PAM runs in `kscreenlocker_worker` after resume), the timer is measured
from the greeter's start, so waking from sleep does not wait either.

`sudo`, polkit, and login prompts are not delayed by default. Set `guards.start_delay_ms = 0` in
`/etc/face-auth.toml` to remove the delay, or set `start_delay_scope = "all"` to apply it to every
prompt.

## sudo says the face matched and waits for Enter

When `require_confirmation_elevation = true` is enabled in `/etc/face-auth.toml` (off by default), a
face match for `sudo`, `su`, or polkit asks you to press Enter on the terminal before granting
elevation. This prevents a background process that invokes `sudo` while you happen to be at your
desk from obtaining root silently. Pressing any other key, or waiting 20 seconds without input, falls
through to the password prompt. Lock screens, login screens, and non-terminal callers (such as
`pkexec` from a GUI) are never prompted. Set `require_confirmation_elevation = false` to disable it.

## Face auth stopped being tried after a few failures

You have hit the failed-attempt lockout. After 5 failed attempts, vinoAuthFace skips camera scans
for an exponentially doubling cooldown (up to 5 minutes), and PAM falls straight through to your
password.

An attempt counts as failed as soon as a scan detects a face, so cancelling or erroring mid-scan
still increments the counter; scans where no face is seen do not. The next successful face match
resets the counter. Adjust these limits under `lockout.*` in `/etc/face-auth.toml`.

## My threshold change did nothing

Your user config (`~/.config/face-auth.toml`) can only make matching *stricter*. To loosen matching,
lower `threshold` in `/etc/face-auth.toml` as root. See [configuration.md](configuration.md).
