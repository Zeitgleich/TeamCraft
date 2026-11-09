#!/usr/bin/env bash
# Is every code change since the last release covered by a full, green CI run of its pull request?
# Used by nightly-release.yml (docs/release.md, "Nightly releases"): then the release needs no
# separate CI run on main and doesn't wait for one.
#
# Covered means: each commit on main since <last-tag> that touches code (not docs or Markdown) is
# either the nightly's own version bump or the squash merge of a pull request whose head commit has
# a green ci.yml run in which the full CI ran (the `test (x86_64)` job succeeded, not skipped: a
# pull request without the `ci` label passes `ci-gate` without it). A direct push, a merge without
# that run or a red run is not covered.
#
# Usage: scripts/nightly-ci-covered.sh <last-tag>   (needs GH_TOKEN and GH_REPO)
# Exit 0: covered. Exit 1: not covered; the reason goes to stdout.
set -euo pipefail

FULL_CI_JOB="test (x86_64)"

last_tag="${1:?usage: $0 <last-tag>}"
: "${GH_REPO:?GH_REPO must be set}"

for sha in $(git rev-list --first-parent "$last_tag..HEAD" -- . ':!docs' ':!*.md'); do
  author="$(git log -1 --format=%ae "$sha")"
  subject="$(git log -1 --format=%s "$sha")"
  if [[ "$author" == *"github-actions[bot]"* && "$subject" =~ ^Release\ [0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    continue
  fi
  pr="$(gh api "repos/$GH_REPO/commits/$sha/pulls" \
    --jq ".[] | select(.merge_commit_sha == \"$sha\") | \"\(.number) \(.head.sha)\"" | head -n 1)"
  if [ -z "$pr" ]; then
    echo "$sha ($subject) is no pull request merge"
    exit 1
  fi
  read -r number head <<< "$pr"
  covered=false
  for run in $(gh run list --workflow ci.yml --commit "$head" --status success --limit 10 \
    --json databaseId --jq '.[].databaseId'); do
    job="$(gh run view "$run" --json jobs \
      --jq ".jobs[] | select(.name == \"$FULL_CI_JOB\") | .conclusion" | head -n 1)"
    if [ "$job" = success ]; then
      covered=true
      break
    fi
  done
  if [ "$covered" != true ]; then
    echo "#$number ($head) has no green run of the full CI"
    exit 1
  fi
  echo "#$number: full CI green on $head"
done
