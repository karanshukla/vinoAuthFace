# Troubleshooting

```bash
# Which camera will it use?
face-camera-diag list

# Test a stored face directly, with debug output (skips PAM)
sudo env RUST_LOG=face_auth_core=debug,face_auth=debug face-auth --verify $USER
echo $?   # 0 = match, 1 = no match, 2 = error

# PAM logs
journalctl -b | grep -i -e pam_exec -e face-auth

# SELinux denials
sudo ausearch -m avc -ts recent
```

`RUST_LOG` only works through `--verify` under `sudo`. When `face-auth` runs from a PAM stack
with borrowed privileges (sudo's own process, or a lock screen via the set-group-ID bit), it drops
the caller's environment, `RUST_LOG` included.

## "PAM_USER is not set"

`face-auth` doesn't guess the account from `USER`/`LOGNAME`. To run it by hand, use `--verify`
rather than setting `PAM_USER` yourself.

## "reports pixel format ... expected GREY, YUYV or Y16"

The selected device isn't an IR sensor: it's an ordinary RGB webcam, or the metadata node next to
the real capture node. Let auto-detection pick one, or check `face-camera-diag list`.

## "camera identity mismatch"

The camera is pinned and `device` now resolves to a different physical port or node. If you
replaced or moved the hardware on purpose, re-run `sudo ./pin-camera.sh`.

## "no face detected" every time

Most Windows Hello IR modules **strobe their illuminator**, alternating a lit frame and a
near-black one. `cargo run --example frame-stats` shows what yours does. face-auth captures frames
in pairs and keeps the brighter, so strobing is handled. If *every* frame is dark, the illuminator
isn't firing and no software will help.

## Lock screen asks for the password, sudo works

The lock screen runs `face-auth` as you, through its set-group-ID bit.

- `ls -l /usr/local/bin/face-auth` should show `-rwxr-sr-x root face-auth`. If the `s` is missing,
  re-run `sudo ./deploy.sh`.
- If `deploy.sh` warned that `/usr/local/bin` is mounted `nosuid`, the kernel ignores the bit and
  the lock screen can't read templates.
- Check that your lock screen's PAM service is wired up: [pam.md](pam.md).

## The lock screen unlocks right after I lock it

If you're still in front of the camera, it sees you and unlocks. Known issue:
[#21](https://github.com/karanshukla/vinoAuthFace/issues/21).

## Face auth stopped being tried after a few failures

That's the lockout. After 5 failed matches (a face was seen and rejected) face-auth skips the
camera with a doubling cooldown, up to 5 minutes, and PAM goes straight to the password. The next
successful face match resets it. Tune it with `lockout_*` in `/etc/face-auth.toml`.

## My threshold change did nothing

A user config may only make matching *stricter*. To loosen it, lower `threshold` in
`/etc/face-auth.toml` as root. See [configuration.md](configuration.md).
