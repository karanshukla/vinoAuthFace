# Security

vinoAuthFace runs inside PAM, where a successful match is `sufficient`: it bypasses the password
prompt and grants access to whatever the PAM stack protects (`sudo`, lock screens, or polkit).
Because a flaw in the authentication path can lead to local privilege escalation, security issues
here carry high impact.

## Reporting

Report vulnerabilities through
[GitHub private vulnerability reporting](https://github.com/karanshukla/vinoAuthFace/security/advisories/new).
Please do not open a public issue for exploitable bugs.

If the issue also affects [upstream authFace](https://github.com/pfalkingham/authFace), mention that
in your report so disclosure can be coordinated with upstream.

## Threat model

| Threat | Defence |
|---|---|
| Local user plants or swaps a face template | `/var/lib/face-auth` and per-user directories are `root:face-auth` (`2750`), templates are `0640`, and enrolment requires root. Only `<user>/lockout/` (`2770`, files `0660`) is group-writable. Only the set-group-ID `vinoauthface-auth` binary holds the `face-auth` group, so a bug in it can expose templates or lockout state without granting root. `deploy.sh` also re-secures legacy world-writable stores in place and removes planted symlinks. |
| Local user weakens matching via config or environment | During PAM authentication, `vinoauthface-auth` treats `/etc/face-auth.toml` as authoritative and ignores `FACE_AUTH_*` environment variables. A user's `~/.config/face-auth.toml` can only tighten thresholds or pick a validated IR device; paths, camera pin, lockout, guards, and backend remain system policy. |
| Wrong account's template decides authentication | Target identity comes exclusively from `PAM_USER`, resolved through NSS and validated before being used as a path component. |
| Remote session triggers the local camera | Any `PAM_RHOST` value other than empty, `localhost`, or a loopback IP address is rejected. Because `sudo` does not set `PAM_RHOST`, scans are also skipped whenever `sshd` appears in the process ancestry (`abort_if_ssh`, enabled by default). Reattached `tmux`/`screen` sessions originally started over SSH and remote desktop sessions are not detected by this check; enabling `guards.seat_check` restricts remote desktop sessions to the account active at the seat. |
| Photo or phone screen held up to the camera | Active near-IR cameras reject OLED and LCD screens because screens emit almost no usable IR. Motion liveness rejects rigidly held images. |
| Spoofed USB device injects replayed frames | `pin-camera.sh` pins the camera's physical USB port and V4L2 index. `vinoauthface-auth` re-verifies this identity from sysfs on every attempt and fails closed on a mismatch. Auto-detection also ignores virtual (`v4l2loopback`) nodes. |
| Scripted brute-force retry loop | Five consecutive failed matches trigger an exponential lockout stored in the root-owned template store, where an unprivileged user cannot reset it. |
| Malformed template file or V4L2 driver data | All lengths read from disk or from the V4L2 driver are bounds-checked before use, and non-finite floats or trailing bytes are rejected. |
| Tampered model or release binary | Models are pinned by SHA-256 (with the detector URL pinned to a commit) and verified during installation (`deploy.sh`) and by `vinoauthface doctor` (though not re-hashed on every load). Release binaries are verified against `SHA256SUMS` before installation. |
| Pull request weakens authentication or build security | The required `guard` workflow fails any PR from a non-owner that modifies anything outside documentation and prose files (Markdown and images under `docs/`, `README.md`, `CHANGELOG.md`, `LICENSE`, issue templates, and `FUNDING.yml`). Code, scripts, the docs site's build files (`docs/Gemfile`, `docs/_config.yml`, `docs/CNAME`), SELinux policy, config, CI workflows, build files, this file, and newly added paths are all protected. The check runs from `main`'s copy via `pull_request_target` without executing PR code, fails if the complete PR file list cannot be read, and can only be overridden by the repository owner's ruleset bypass. |
| Compromised dependency or GitHub Action | `cargo-deny` restricts crates to crates.io, `Cargo.lock` is enforced, all CI actions are pinned to commit SHAs, Dependabot uses a 7-day cooldown, and nothing auto-merges. |

## What it does not defend against

- **Printed photos moved by hand, IR-visible prints, and 3D masks.** Paper reflects some near-IR
  light, and a gently moved print can satisfy the default motion check. There is no depth sensing.
  Treat face unlock as a convenient alternative to typing your password, keeping your password as
  the primary security boundary.
- **A local root attacker.** Anyone with root access can modify the template store, configuration,
  or installed binaries.
- **Someone standing at your unlocked desk.** A face match proves presence in front of the camera,
  not explicit user intent (see #29).
- **A compromised maintainer account.** Repository rulesets and review slow down unauthorized
  changes, but they cannot prevent someone who controls the repository keys from publishing
  malicious releases.
