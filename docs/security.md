---
title: Security model
nav_order: 9
---

# Security model and limitations

vinoAuthFace is designed as a convenient alternative to typing your password, with your password
remaining available as the fallback. To report a vulnerability, see
[SECURITY.md](https://github.com/karanshukla/vinoAuthFace/blob/main/SECURITY.md).

## Trust model

- **Root-owned template store:** `/var/lib/face-auth` and per-user directories are `root:face-auth`
  with mode `2750`, and template files are `0640`. Because anyone who can write a face template
  controls which face unlocks an account, enrolment requires `sudo`.
- **Set-group-ID `face-auth` instead of set-user-ID root:** Lock screens (such as KScreenLocker and
  `swaylock`) run PAM as your user, so `vinoauthface-auth` uses the `face-auth` group to read
  templates and write only `<user>/lockout/` (`2770`). A bug in `vinoauthface-auth` can expose
  templates or lockout counters without granting root access. Because no other process holds the
  `face-auth` group and the directory is closed to others, users cannot clear their own lockout
  state.
- **Environment scrubbing under elevated privileges:** Whenever real and effective user or group IDs
  differ, `vinoauthface-auth` drops the caller's environment (retaining only variables set by
  `pam_exec`) and restricts non-root callers to authenticating their own account.
- **System-authoritative PAM config:** During PAM authentication, `/etc/face-auth.toml` is
  authoritative and `FACE_AUTH_*` environment variables are ignored. A user's
  `~/.config/face-auth.toml` may make matching stricter, but it cannot loosen thresholds, redirect
  model or template paths, unpin the camera, or change lockout rules. Root enrolment ignores both
  the user config and `FACE_AUTH_*` as well (so `sudo -E` cannot redirect enrolment), and any
  `--device` must be an IR sensor or the system's configured `device`. See
  [configuration.md](configuration.md) for the complete list.
- **Identity strictly from `PAM_USER`:** `vinoauthface-auth` refuses to run unless PAM sets
  `PAM_USER`.
- **Remote sessions are rejected:** If `PAM_RHOST` is set to anything other than empty, `localhost`,
  or a loopback IP (`127.0.0.0/8`, `::1`), face authentication is declined so someone sitting at the
  physical console cannot authorize an incoming SSH session. Hostnames are not resolved, so
  `127.example.com` is treated as remote. Because `sudo` over SSH does not set `PAM_RHOST`,
  vinoAuthFace also inspects its process ancestry and aborts if `sshd` is an ancestor
  (`guards.abort_if_ssh`, enabled by default). This check does not detect a `tmux` or `screen`
  session started over SSH and reattached later (since its server's parent is `init`), nor remote
  desktop sessions; `guards.seat_check` helps address remote desktop sessions for non-seat accounts.
- **Constrained tray helper:** The optional tray runs unprivileged and delegates `enrol`, `retrain`,
  `upgrade`, and settings changes to `vinoauthface-helper` via `pkexec`. The helper accepts a single
  fixed verb with no flags and acts only on `PKEXEC_UID`. Note that a face match can approve these
  polkit prompts just as it can approve `sudo` (see [tray.md](tray.md#privileges)).
- **Seat check (opt-in):** Enabling `guards.seat_check = true` checks `systemd-logind` state under
  `/run/systemd` and declines unless the target user owns the active `seat0` session or `seat0` is
  displaying a login greeter. On multi-user machines with fast user switching, this prevents user A
  at the desk from satisfying a `sudo` prompt in user B's background session. Missing or unreadable
  session state also declines, while systems without `/run/systemd/seats` (no `logind`) skip the
  check. It is off by default because it also declines `root` as the target account, which breaks
  `sudo` configured with `targetpw`/`rootpw` or polkit rules requesting the root password.

## Templates at rest (TPM sealing)

TPM sealing is off by default. To enable it, set `seal_embeddings = true` in `/etc/face-auth.toml`
and re-enrol (or run `vinoauthface improve`, which rewrites existing templates sealed). Template
payloads are encrypted with `systemd-creds` using a key sealed to the machine's TPM, preventing
offline extraction from a stolen laptop or a disk booted on another system. Because `systemd-creds`
is built into systemd and the encrypted files stay in `/var/lib/face-auth`, no extra packages are
required and sealed templates survive immutable OS updates.

Key properties and trade-offs:

- **Why sealing matters (template inversion):** Face embeddings are not one-way cryptographic
  hashes. Running gradient descent on an input image until the recognition model produces an
  embedding close to stored templates reconstructs a recognizable likeness of the user. Testing
  against `w600k_mbf` with 30 templates—using no face prior and no GPU, in a few minutes on 2
  vCPUs—yielded an unmistakable human face recognized by its owner that scored ~0.8 cosine
  similarity against the stored templates (well above the 0.6 default threshold). Although noisy
  without a face prior, pairing this with a pretrained face generator produces much cleaner results.
  Because you cannot revoke your face, treat leaked templates like leaked photos. (We did not test
  whether a reconstructed image passes the IR sensor, detector, or liveness checks; the primary risk
  is privacy.)
- **Powered-off protection only:** While the system is booted, root can unseal any user's templates
  and each user can unseal their own (just as `vinoauthface-auth` does). Sealing does not protect
  against a local root attacker.
- **No PCR binding:** Omitting PCR policy lets the key survive firmware, bootloader, and
  `bootc`/`rpm-ostree` updates without re-enrolling, at the cost of not detecting a modified boot
  chain.
- **Bound to account and model:** The username and recognition-model tag are embedded in the
  authenticated systemd credential name, so copying a sealed file to another user or relabelling it
  for another model fails decryption.
- **Machines without a TPM:** Enrolment logs a warning and stores templates unsealed so hardware
  without a TPM continues to work.
- **Unseal failures fail closed:** After a TPM clear or motherboard replacement, existing sealed
  templates can no longer be decrypted. Face auth declines with a message asking you to re-enrol,
  PAM falls back to your password, and the lockout counter is not incremented.
- **Downgrade guard:** With `seal_embeddings = true`, plaintext template files are refused. (An
  attacker with offline disk write access could also edit `/etc/face-auth.toml`, so this catches
  accidental or partial file swaps; stopping a full offline tampering attack would require PCR
  binding.)
- **Per-user scoping:** Credentials use `systemd-creds --uid` (requires systemd 256+). Lock screens
  running as your user can unseal your templates without a polkit prompt, and `sudo`/polkit running
  as root can unseal any user's templates. A display manager greeter checking a different user (as
  `gdm` or `sddm`) cannot unseal them directly and falls back to the password. The key also binds to
  the username, UID, and `machine-id`, so changing any of those requires re-enrolling. Unsealing
  adds minor startup overhead per scan; check the "store loaded" line with
  `RUST_LOG=face_auth_core=debug` to measure it on your hardware.
- **Requires both TPM and host secret:** User-scoped credentials use `host+tpm2` and require both
  the TPM and the root-only `/var/lib/systemd/credential.secret`. A disk moved to another machine
  without the original TPM still cannot unseal. Under SELinux, only `vinoauthface-unseal`'s
  dedicated domain can access `/dev/tpmrm0` and `credential.secret`, keeping them isolated from the
  greeter (see [install.md](install.md#selinux)).

## Presentation attacks (something held up to the real camera)

- **Phone and monitor screens are blocked by IR physics:** OLED and most LCD displays emit almost no
  near-IR light and reflect little from the camera's IR illuminator, so a photo on a phone screen
  produces no usable face signal. In testing, 100 attempts with a phone screen yielded zero face
  detections.
- **2D colour photos of the enrolled user score below threshold:** A colour photo of the legitimate
  user—phone selfie or cropped group shot—does not reach the 0.6 match threshold even when edited to
  mimic IR tones. In a November 2026 spot check against one enrolment (`r50`), a physical colour
  inkjet print held to the real IR camera scored 0.18–0.29; the same images as digital files,
  converted to IR-like grayscale (red-channel weighting plus lifted skin midtones), peaked at 0.58
  for a clean straight-on selfie and 0.55 for a casual group-photo crop—still short of 0.6, and
  before any liveness check runs. Colour inkjet ink is largely transparent at 850 nm, so a print
  discards most of the face signal the digital file keeps; the digital scores are a ceiling a
  physical colour print cannot exceed. This is a small single-subject check against match score
  alone, not a FAR benchmark, and unlike the liveness tests below it is one of the few numbers taken
  from a real sensor rather than synthetic clips. A black-only (laser/toner) print of a genuine IR
  image of the face stays the stronger, untested vector described under printed IR photos below.
- **Motion liveness:** By default, a match requires facial motion compared against an earlier frame
  within `liveness.window_ms` (default 1 second). Only frames that match the enrolled face count, so
  motion from someone else's live face can't carry over to a photo held up after it. Even a still
  live face drifts naturally over a second, whereas a rigidly mounted photo stays static
  ([#96](https://github.com/karanshukla/vinoAuthFace/issues/96)). Motion is computed only within the
  face region after normalizing brightness and contrast, preventing auto-exposure shifts or a hand
  waving beside a photo from passing. Once a face matches and only liveness is pending, the scan
  extends by up to `liveness.grace_ms` (default 4 seconds) so still users are not penalized (which
  also gives a matching photo that extra window to produce motion).
- **Hand-moved photo detection (opt-in):** Setting `liveness.residual_motion_threshold` requires
  frame-to-frame non-rigid motion—movement inside the most-changed eye-sized region that cannot be
  explained by a single rigid shift (estimated to quarter-pixel precision), such as blinking, mouth
  movement, or 3D parallax during a head turn. In software tests on synthetic clips, moved, tilted,
  or zoomed IR photos peaked at 0.20, while blinks scored 0.3–0.6 and moving faces exceeded 0.3 on
  most frame pairs. This check is off by default because a still person can easily go through an
  entire scan without a qualifying blink (in one 15-second real clip with glasses glare covering
  both eyes, the first qualifying blink occurred at 8.5 seconds; see
  [#17](https://github.com/karanshukla/vinoAuthFace/issues/17)).
- **Printed IR photos and 3D masks are an accepted risk:** On a default install, a laser-printed IR
  photo moved by hand can unlock the system because paper reflects near-IR and residual motion
  checking is off by default. This limitation also affects other IR systems (such as Windows Hello,
  SySS-2017-027) and requires obtaining an IR image of your face rather than an ordinary colour
  photo. Note that the liveness metrics above were evaluated in software against synthetic prints
  rather than physical prints on a real sensor, and a flexed paper print introduces non-rigid
  motion. Because there is no depth sensing or structured light, 3D masks are also out of scope
  ([#17](https://github.com/karanshukla/vinoAuthFace/issues/17)).

## Frame injection (a fake camera)

### Camera binding (enabled by default)

During enrolment, vinoAuthFace records the camera's USB `VID:PID` in `<user>/cameras`. Before
opening a camera for authentication, it verifies that the device matches an enrolled USB ID. This
prevents another webcam or a virtual `v4l2loopback` device (which has no USB identity) from
substituting for your IR sensor. A mismatch is treated as an error—PAM falls back to your password
without incrementing the lockout counter. Accounts enrolled before camera binding was added, or on
cameras lacking a USB ID (such as MIPI), accept any camera until re-enrolled (`sudo vinoauthface
doctor` reports this status). Set `bind_camera = false` to disable it. This design is adapted from
[facelock](https://github.com/tyvsmith/facelock)'s device coupling.

### Physical USB port pinning (opt-in)

Because a programmable USB device can spoof a camera's `VID:PID` string and replay recorded frames,
`pin-camera.sh` locks authentication to the camera's physical USB bus port path and V4L2 index as
reported by sysfs. A rogue device plugged into another port cannot claim the same physical path
while the built-in camera is present. `vinoauthface-auth` re-verifies this sysfs identity on every
scan and enrolment independently of the udev symlink. See
[install.md](install.md#pinning-the-camera-recommended).

### Virtual device filtering

Auto-detection only considers physical IR-named or greyscale capture nodes and ignores virtual
`v4l2loopback` devices (whose frames any local user can write and whose card names are arbitrary
module parameters). A user's `~/.config/face-auth.toml` `device` setting enforces the same rule.
Only `/etc/face-auth.toml` can explicitly point `device` at a virtual node for testing.

## Other limitations

- **Lockout applies to face auth only:** Exponential backoff throttles face scans, while password
  fallback remains governed by PAM (configure `pam_faillock` separately if you want password rate
  limiting). A scan counts as failed the moment a face is detected and resets only on a successful
  match, so cancelling (`Ctrl+C` on `sudo`) or running concurrent scans cannot bypass the counter.
- **`sufficient` ends the PAM auth stack:** A successful face match completes authentication
  immediately. In current PAM files, the vinoAuthFace line can sit above `pam_nologin` and
  `pam_faillock` depending on the distro's stack layout
  ([#31](https://github.com/karanshukla/vinoAuthFace/issues/31)).
- **SELinux policy scope:** The greeter policy grants `xdm_t` `map` access to all V4L2 devices and
  labels `/dev/accel/*` as `dri_device_t` (allowing any domain with GPU access to open the NPU).
  Tightening either would require custom udev device types. Only `xdm_t` is covered; TTY `login`
  (`local_login_t`) is not wired by `deploy.sh`, and confined users' `sudo_t` is untested.
- **Model and release integrity:** Models are pinned by SHA-256 (with the detector URL pinned to a
  commit) and checked by `deploy.sh` during installation and by `vinoauthface doctor`. They are not
  re-hashed on every scan, so a root attacker replacing a model in place would not be caught until
  `doctor` is run. Release binaries and source bundles downloaded by `install.sh` and
  `vinoauthface-upgrade` are verified against the release's `SHA256SUMS`, protecting against
  corrupted or truncated downloads with GitHub over TLS as the trust anchor.

## Match accuracy against strangers

Both shipped models were benchmarked in November 2026 against the
[CBSR NIR Face](https://huggingface.co/datasets/EricJaebeom/CBSR-NIR-Face) dataset (197 subjects,
3,940 near-IR images, MIT license). Every image ran through the live scan pipeline (CLAHE 3.0 with
8x8 tiles, SCRFD `det_500m`, five-point alignment, and encoding) and was compared pairwise across
37,430 same-person pairs and 7,722,000 different-person pairs.

At the default `threshold = 0.6`:

| Metric | `mbf` | `r50` | Windows Hello requirement |
|---|---|---|---|
| Strangers accepted (FAR) | 4 pairs (1 in 1.9 million) | 0 of 7.7 million | < 1 in 100,000 |
| Same person accepted (TAR) | 95.8% | 99.1% | > 95% |

**Why the default is 0.6.** The threshold is an operating point in the gap between two measured
distributions: genuine matches of the enrolled user, and everything else. Genuine-match similarity
floors sit well above it (`r50` min 0.83, `mbf` min 0.76 on the enrollment benchmark in
[configuration.md](configuration.md#recognition-model-mbf-vs-r50)), so legitimate users clear 0.6
with margin, while strangers and 2D photo-replays of the enrolled user fall below it (see
[Presentation attacks](#presentation-attacks-something-held-up-to-the-real-camera)). Lowering it
toward the 1-in-100,000 operating points below (0.51 for `r50`, 0.56 for `mbf`) trades that margin
for a higher genuine-accept rate; 0.6 keeps a conservative buffer on the stranger side for a small
TAR cost, and is the floor a user config can raise but not lower.

When tuned to exactly 1 in 100,000 strangers accepted, `r50` maintains a higher genuine-accept rate:

| Metric | `mbf` | `r50` |
|---|---|---|
| Threshold required for 1 in 100,000 FAR | `0.56` | `0.51` |
| Same person accepted (TAR) | 97.5% | 99.8% |

These results depend on selecting the face closest to the camera
([#193](https://github.com/karanshukla/vinoAuthFace/pull/193)). Earlier versions picked the highest
detector confidence score, which selected a background bystander on 11 CBSR images and caused `r50`
to accept 124 stranger pairs (and `mbf` 39) at threshold 0.6.

Important caveats when interpreting these numbers:

- **Dataset-specific sensor:** CBSR was captured on different hardware than your laptop, and the
  1-in-100,000 threshold rests on the top ~77 stranger pairs. Treat these numbers as a controlled
  model comparison rather than your exact real-world FAR.
- **Single-image comparisons:** A live unlock evaluates multiple frames per scan and permits 5
  attempts before lockout, multiplying a stranger's per-unlock odds relative to single-pair FAR.
- **Duplicate subject in CBSR:** Subject IDs 19 and 101 appear to be the same person photographed in
  two sessions and were excluded from impostor pairs. Counting them as strangers alone would push
  `r50`'s 1-in-100,000 threshold to 0.86.
- **Windows Hello baseline:** The Windows Hello column reflects Microsoft's published minimum
  [biometric requirements](https://learn.microsoft.com/en-us/windows-hardware/design/device-experiences/windows-hello-biometric-requirements),
  not a benchmark of Windows Hello itself.

## What has been tested, and what hasn't

A security review in October 2026 audited a deployed installation on an Ubuntu 24.04 VM using
`v4l2loopback` in place of a physical IR camera and found no exploitable vulnerabilities.

### Covered in the review

- **Code audit:** Inspected the set-group-ID entrypoint (environment scrubbing, `PATH`/`HOME`
  pinning, self-only check for non-root callers), username validation and NSS lookup, user-config
  overlay (`O_NOFOLLOW`, ownership and size checks, `fold_sections` applied before the allow-list),
  and V4L2 capture bounds (`bytesused` and buffer index clamped to mapped memory). Verified
  `cargo deny` passes.
- **PAM fail-closed testing (`pamtester`):** Confirmed that a missing camera feed, stalled feed,
  feed dying mid-scan, and non-IR pixel format all fall through to the password within the scan
  timeout. Also confirmed that truncated, empty, random, trailing-byte, `NaN`, symlinked, and
  missing templates all fail closed, and that an active lockout skips the camera after a bounded
  tarpit even when a matching face is presented.
- **Parser mutation testing:** Tested ~424,000 mutated template files, lockout files, camera lists,
  and config files with zero panics, no slow loads, and no format-limit violations. The template
  suite is now a unit test (`corrupted_templates_never_panic_or_break_the_format_limits`). Because
  this is unguided mutation without coverage feedback, it does not replace `cargo-fuzz` and does not
  exercise V4L2 ioctls, ONNX inference, or sealed (v3) templates.

### Known operational notes

These behaviours were noted during review and are not vulnerabilities:

- **Corrupt or missing `lockout/state.bin` resets backoff** (documented in `lockout.rs`). Modifying
  it requires `face-auth` group membership, which already grants read access to all templates.
- **Lockout file ownership:** When a lock screen runs `vinoauthface-auth` as your user, created
  lockout files are owned by that user. This remains safe as long as parent directories keep
  `root:face-auth` `2750` permissions preventing users from traversing into them; do not loosen
  those permissions.
- **Legacy 1777 store upgrade race:** When upgrading from an old world-writable store, `deploy.sh`
  removes stray files and runs `chmod` via `find -exec` (which follows symlinks). A local attacker
  racing that one-time upgrade could redirect `chmod`; `deploy.sh` already advises legacy users to
  wipe the old store and re-enrol.
- **Commented PAM lines:** A commented-out `pam_exec ... face-auth` line is treated by `deploy.sh`
  as already present. This fails safe by skipping face unlock instead of granting access.

### Not covered by VM testing

The following areas require real hardware and were outside the scope of the VM review:

- Live IR liveness and physical presentation attacks (replaying a recording into `v4l2loopback`
  cannot validate real sensor spoof resistance).
- `uvcvideo` hardware specifics such as USB port pinning and IR illuminator strobing.
- Intel NPU and OpenVINO runtime behaviour, latency, and power usage.
- Real GNOME and KDE lock screens and portal-based desktop flows.
