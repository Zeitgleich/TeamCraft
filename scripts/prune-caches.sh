#!/usr/bin/env bash
# Keeps the repository's Actions cache inside its 10 GB budget (docs/operations.md, "Actions cache").
#
#   scripts/prune-caches.sh [--dry-run]
#
# Rust caches are keyed by the hash of Cargo.lock, so every dependency change leaves a ~1 GB entry
# next to the new one until GitHub evicts the least recently used cache, which is as likely to be
# the rarely read one that matters. This deletes:
#   - every cache of a pull request ref (pull requests only restore main's caches), and
#   - on every other ref, all but the newest cache of a family (the key without its trailing hashes).
# Needs `gh` with `actions: write`.
set -euo pipefail

dry_run=false
[ "${1:-}" = "--dry-run" ] && dry_run=true

caches="$(gh cache list --limit 1000 --json id,key,ref,sizeInBytes,createdAt)"

summary() {
  jq -r '"\(length) caches, \(([.[].sizeInBytes] | add // 0) / 1073741824 * 100 | round / 100) GiB"' <<< "$1"
}

echo "Before: $(summary "$caches")"

# Newest first per (ref, family); everything after the first of a family, and all pull request refs.
doomed="$(jq -r '
  def family: .key | sub("(-[0-9a-f]{8,64})+$"; "");
  (map(select(.ref | startswith("refs/pull/")))
   + (map(select(.ref | startswith("refs/pull/") | not))
      | group_by([.ref, family])
      | map(sort_by(.createdAt) | reverse | .[1:]) | add // []))
  | .[] | "\(.id)\t\(.sizeInBytes / 1048576 | floor) MiB\t\(.ref)\t\(.key)"
' <<< "$caches")"

if [ -z "$doomed" ]; then
  echo "Nothing to delete."
  exit 0
fi

while IFS=$'\t' read -r id size ref key; do
  echo "Delete $size $ref $key"
  $dry_run || gh cache delete "$id" || echo "  (already gone)"
done <<< "$doomed"

if ! $dry_run; then
  echo "After: $(summary "$(gh cache list --limit 1000 --json sizeInBytes)")"
fi
