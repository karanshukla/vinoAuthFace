# Security

vinoAuthFace runs as root inside PAM. A successful match is `sufficient`: it skips the password and grants whatever the stack was guarding (sudo, the lock screen, polkit). A bug here is a local root, so the bar is higher than for a normal CLI.

## Reporting

Use [private vulnerability reporting](https://github.com/karanshukla/vinoAuthFace/security/advisories/new). Do not open a public issue for anything exploitable.

If the bug is also in [upstream authFace](https://github.com/pfalkingham/authFace), say so in the report; it gets coordinated with upstream before anything is published.

## Threat model

| Threat | Defence |
|---|---|
| Local user plants or swaps a face template | Store is root-owned `0700`, templates `0600`, enrolment needs root. `deploy.sh` re-secures an old world-writable store in place and removes planted symlinks. |
| Local user loosens matching via their own config or env | The PAM path reads `/etc/face-auth.toml` only and ignores `FACE_AUTH_*`. A user's config can only tighten thresholds or pick a validated IR device; paths, camera pin, lockout and backend are system policy. |
| Wrong account's template decides the result | Identity comes from `PAM_USER` only, resolved through NSS and validated before becoming a path component. |
| Remote session triggers the local camera | Non-local `PAM_RHOST` is refused. |
| Photo or screen held up to the camera | Active-NIR camera: screens emit no usable IR. Motion liveness rejects a rigidly held image. |
| Spoofed USB device injecting frames | `pin-camera.sh` pins the physical port and V4L2 index; vinoauthface-auth re-checks it from sysfs on every attempt and fails closed. Auto-detect ignores virtual (v4l2loopback) nodes. |
| Scripted retry loop | Exponential lockout after 5 failed matches, stored in the root-owned store so a user cannot reset it. |
| Crafted template file or driver data | Every length read from disk or the driver is bounded before use; non-finite values and trailing bytes are rejected. |
| Tampered model or release binary | Models are pinned by SHA-256 (detector URL pinned to a commit). Release binaries are verified against `SHA256SUMS` before install. |
| PR that weakens any of the above | The `guard` check fails any PR from someone other than the owner that touches `crates/`, the scripts, `selinux/`, `pam/`, `config/`, `.github/`, dependencies or this file. It runs from main's copy (`pull_request_target`) and never executes PR code. Only the owner's ruleset bypass can merge a flagged PR. |
| Compromised dependency or action | Crates from crates.io only (cargo-deny), `Cargo.lock` enforced, every CI action pinned to a commit, Dependabot waits 7 days and nothing auto-merges. |

## What it does not defend against

- **Printed photos moved by hand, IR-visible prints, 3D masks.** Paper reflects some NIR and a gently moved print can pass the motion check. There is no depth sensing. Treat face unlock as convenience over a password you still have, not a stronger factor.
- **Anyone with root.** Root can rewrite the store, the config, or the binary.
- **Someone at your unlocked desk.** A face match is a presence check, not an intent check (see #29).
- **A compromised maintainer account.** Rulesets and review slow this down; they do not stop someone holding the keys.
