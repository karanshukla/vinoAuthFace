# Releasing

Versions are consecutive numbers (`v2`, `v3`, ...), not semver: a release is main at that point,
and the number only says which one is newer. Older tags (`v1.0.0`, `v1.1`) count by their first
number, so the first release under this scheme is `v2`.

```bash
scripts/release.sh --dry-run   # next tag and the PRs merged since the last one
scripts/release.sh             # from an up-to-date main: tag origin/main's head, push the tag
```

Pushing the tag runs `release.yml`, which builds the static musl binaries with
`VINOAUTHFACE_VERSION` set to the tag (shown by `vinoauthface --version` and `doctor`; local
builds say `dev`) and publishes them as a GitHub release with generated notes. v2 onwards are full
releases, so GitHub's "latest" and the repo sidebar point at the newest one (earlier tags were
pre-releases). `deploy.sh` asks the API for the newest release of any kind, so it works either way.

When to cut one: whenever main has a user-facing change worth installing. There's no schedule.
A tagged checkout's `deploy.sh` installs that tag's binaries, so a release is also what makes a
change reach users who install without a Rust toolchain.
