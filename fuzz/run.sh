#!/usr/bin/env bash
# Runs fuzz targets in the nightly container (rootless podman; docker works with CONTAINER_ENGINE).
#
#   fuzz/run.sh [all [seconds]]    # every target, 180 seconds each by default
#   fuzz/run.sh imip 600           # one target for ten minutes
#   fuzz/run.sh list               # the targets
#
# New inputs the fuzzer finds are written to fuzz/corpus/<target>/ (committed seeds are the
# starting point); crashes land in fuzz/artifacts/<target>/. A non-zero exit means a crash.
set -euo pipefail

cd "$(dirname "$0")/.."
engine=${CONTAINER_ENGINE:-podman}
seconds=${2:-180}
targets=(proto_requests proto_gateway bridge_ingest bridge_multipart imip workspace_log crypto_decoders)

if [ "${1:-}" = list ]; then printf '%s\n' "${targets[@]}"; exit 0; fi
if [ -n "${1:-}" ] && [ "$1" != all ]; then targets=("$1"); fi

"$engine" image exists teamcraft-fuzz || "$engine" build -f fuzz/Containerfile -t teamcraft-fuzz fuzz/

status=0
for target in "${targets[@]}"; do
  echo "== fuzzing $target for ${seconds}s"
  "$engine" run --rm \
    -v "$PWD:/src:Z" -v teamcraft-fuzz-target:/target -v teamcraft-fuzz-cargo:/usr/local/cargo/registry \
    teamcraft-fuzz cargo fuzz run "$target" -- -max_total_time="$seconds" -rss_limit_mb=4096 -timeout=10 \
    || status=1
done
exit $status
