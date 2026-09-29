# Troubleshooting

```bash
# Which camera will it use?
vinoauthface-camera-diag list

# Test a stored face directly, with debug output (skips PAM)
sudo env RUST_LOG=face_auth_core=debug,vinoauthface_auth=debug vinoauthface-auth --verify $USER
echo $?   # 0 = match, 1 = no match, 2 = error

# What each PAM scan did (match / no match / error, service, sealed or not)
journalctl -b -t vinoauthface-auth

# SELinux denials
sudo ausearch -m avc -ts recent
```

`RUST_LOG` only works through `--verify` under `sudo`. When `vinoauthface-auth` runs from a PAM stack
with borrowed privileges (sudo's own process, or a lock screen via the set-group-ID bit), it drops
the caller's environment, `RUST_LOG` included. Every scan still leaves one line in syslog
(`authpriv`) with its outcome, the PAM service and whether the templates were sealed, e.g.
`face match for 'you' (service kde, sealed templates)`.

## "PAM_USER is not set"

`vinoauthface-auth` doesn't guess the account from `USER`/`LOGNAME`. To run it by hand, use `--verify`
rather than setting `PAM_USER` yourself.

## Lock-screen unlock is slow on the NPU backend

The NPU compile cache in `/var/cache/face-auth` is empty, so every unlock compiles both models on
the CPU first. The lock screen runs as you and can only read that cache; only root writes it.
`deploy.sh` refills it after emptying it, but a driver update invalidates it too. Refill it with:

```bash
sudo vinoauthface-auth --warm-cache
```

## "reports pixel format ... expected GREY, YUYV or Y16"

The selected device isn't an IR sensor: it's an ordinary RGB webcam, or the metadata node next to
the real capture node. Let auto-detection pick one, or check `vinoauthface-camera-diag list`.

## "camera identity mismatch"

The camera is pinned and `device` now resolves to a different physical port or node. If you
replaced or moved the hardware on purpose, re-run `sudo ./pin-camera.sh`.

## "no face detected" every time

Most Windows Hello IR modules **strobe their illuminator**, alternating a lit frame and a
near-black one. `cargo run --example frame-stats` shows what yours does. vinoAuthFace captures frames
in pairs and keeps the brighter, so strobing is handled. If *every* frame is dark, the illuminator
isn't firing and no software will help.

## Lock screen asks for the password, sudo works

The lock screen runs `vinoauthface-auth` as you, through its set-group-ID bit.

- `ls -l /usr/local/bin/vinoauthface-auth` should show `-rwxr-sr-x root face-auth`. If the `s` is missing,
  re-run `sudo ./deploy.sh`.
- If `deploy.sh` warned that `/usr/local/bin` is mounted `nosuid`, the kernel ignores the bit and
  the lock screen can't read templates.
- Check that your lock screen's PAM service is wired up: [pam.md](pam.md).

## The lock screen unlocks right after I lock it

If you're still in front of the camera, it sees you and unlocks. Known issue:
[#21](https://github.com/karanshukla/vinoAuthFace/issues/21).

## Face auth never runs over SSH, or with the lid closed

On purpose. Under SSH nobody is at the camera, and a built-in camera can't see through a closed
lid, so vinoAuthFace goes straight to the password. An IR camera on a port the firmware reports as
removable is still used with the lid shut. If yours isn't (a hub can hide it), set
`abort_if_lid_closed = false` in `/etc/face-auth.toml`. `abort_if_ssh = false` turns the SSH check
off.

## The lock screen waits a couple of seconds before scanning

On purpose. A screen locker runs vinoAuthFace the moment it starts, so if you lock the screen while
still looking at the camera it would unlock straight away. vinoAuthFace waits until the locker has
been running `start_delay_ms` (default 2000) before it scans, measured from when the locker
started, so a retry on the same lock screen doesn't wait again. `sudo`, polkit and login prompts
aren't delayed. Set `start_delay_ms = 0` in `/etc/face-auth.toml` to disable it, or
`start_delay_scope = "all"` to delay every prompt.

## sudo says the face matched and waits for Enter

With `require_confirmation_elevation = true` in `/etc/face-auth.toml` (off by default), a match for
`sudo`, `su` or polkit is followed by a request for Enter on the terminal, so a process
that runs `sudo` while you happen to sit at the camera can't get root unnoticed. Any other key, or
20 seconds of silence, falls through to the password. Lock screens and login are never asked, nor
is a caller with no terminal (`pkexec` from a GUI). Set it back to `false` to turn it off.

## Face auth stopped being tried after a few failures

That's the lockout. After 5 failed matches (a face was seen and rejected) vinoAuthFace skips the
camera with a doubling cooldown, up to 5 minutes, and PAM goes straight to the password. The next
successful face match resets it. Tune it with `lockout_*` in `/etc/face-auth.toml`.

## My threshold change did nothing

A user config may only make matching *stricter*. To loosen it, lower `threshold` in
`/etc/face-auth.toml` as root. See [configuration.md](configuration.md).
