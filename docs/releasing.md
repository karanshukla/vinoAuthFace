---
title: Releasing
nav_order: 11
---

# Releasing

vinoAuthFace uses sequential integer version tags (`v2`, `v3`, `v4`, ...) instead of semantic
versioning. Each tag simply marks a snapshot of `main`, where higher numbers are newer. Legacy tags
(`v1.0.0`, `v1.1`) are ordered by their leading number, making `v2` the first release under this
scheme.

```bash
scripts/release.sh --dry-run   # show the next tag and PRs merged since the last release
scripts/release.sh             # from an up-to-date main: tag origin/main HEAD and push the tag
```

Pushing a tag triggers `.github/workflows/release.yml`, which compiles the static musl binaries,
OpenVINO binaries, and the glibc settings window with `VINOAUTHFACE_VERSION` set to the tag (printed
by `vinoauthface --version` and `vinoauthface doctor`; local builds report `dev`). The workflow
publishes a GitHub release with auto-generated release notes alongside `vinoauthface-source.tar.gz`,
`install.sh`, and `SHA256SUMS`.

The one-line installer in the README downloads `releases/latest/download/install.sh`, so cutting a
release updates what new users install. From `v2` onward, all tags are published as full releases
(earlier tags were pre-releases) so GitHub's "latest" endpoint and repository sidebar always point
to the newest version. `deploy.sh` queries the GitHub API for the most recent release regardless of
pre-release status.

There is no fixed release schedule; cut a new release whenever `main` has user-facing changes worth
shipping. Because running `deploy.sh` in a tagged checkout installs that tag's binaries, cutting a
release is also how changes reach users who install without a Rust toolchain.
