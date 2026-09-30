#!/usr/bin/env bash
# Tag the next release. Versions are consecutive (v2, v3, ...), not semver:
# each release is simply main at that point (#98). Pushing the tag runs
# release.yml, which builds and publishes the binaries as a pre-release.
#
#   scripts/release.sh --dry-run   # show the next tag and what it contains
#   scripts/release.sh             # tag origin/main's head and push the tag
set -euo pipefail
cd "$(dirname "$0")/.."

die() { echo "release: $*" >&2; exit 1; }

DRY_RUN=0
case "${1:-}" in
    --dry-run) DRY_RUN=1 ;;
    "") ;;
    *) die "usage: scripts/release.sh [--dry-run]" ;;
esac

git fetch -q --tags origin main
head="$(git rev-parse origin/main)"
if [ "$DRY_RUN" = 0 ]; then
    [ "$(git rev-parse HEAD)" = "$head" ] || die "check out origin/main's head first (git switch main && git pull)"
    [ -z "$(git status --porcelain)" ] || die "working tree has changes"
fi

# The highest leading number of any v* tag; older tags (v1.0.0, v1.1) count by
# their first number.
last="$(git tag -l 'v*' | sed -nE 's/^v([0-9]+).*/\1/p' | sort -n | tail -1)"
next="v$(( ${last:-0} + 1 ))"
prev="$(git tag -l "v${last:-0}*" | sort -V | tail -1)"

git tag --points-at "$head" -l 'v*' | grep -q . && die "origin/main is already tagged: $(git tag --points-at "$head" -l 'v*' | tr '\n' ' ')"

echo "Next release: $next ($(git rev-parse --short "$head"))"
if [ -n "$prev" ]; then
    echo "Merged since $prev:"
    # A merge commit's subject has the PR number and its body the title.
    git log --merges --format='%s%x09%b' "$prev..$head" \
        | sed -nE 's/^Merge pull request (#[0-9]+) from [^\t]*\t(.*)/  \1 \2/p'
fi

[ "$DRY_RUN" = 1 ] && exit 0
read -r -p "Tag and push $next? [y/N] " answer
[ "$answer" = y ] || die "not tagged"
git tag -a "$next" -m "$next" "$head"
git push origin "$next"
echo "Pushed $next; release.yml publishes it: gh run watch \$(gh run list -w release.yml -L1 --json databaseId -q '.[0].databaseId')"
