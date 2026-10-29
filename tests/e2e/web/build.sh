#!/usr/bin/env bash
# Builds the web client in a container and writes it to target/e2e-web (served by the server under
# test). Cargo's registry and the build output live in named volumes so reruns are incremental; remove
# them with `docker volume rm teamcraft-e2e-web-cargo teamcraft-e2e-web-target-<id>`. The build output
# gets one volume per checkout: every checkout is mounted at /src, so a shared one mixed up the
# sources of different worktrees.
set -euo pipefail

root="$(cd "$(dirname "$0")/../../.." && pwd)"
engine="${CONTAINER_ENGINE:-docker}"
image="teamcraft-e2e-web:local"
out="${E2E_WEB_DIR:-$root/target/e2e-web}"
checkout_id="$(printf '%s' "$root" | sha256sum | cut -c1-12)"

"$engine" build -q -t "$image" -f "$root/tests/e2e/web/Containerfile" "$root/tests/e2e/web"
mkdir -p "$out"
# Trunk's pre_build hook writes the joined stylesheet to crates/teamcraft-ui/gen (#614); the
# checkout is mounted read-only, so gen/ gets a tmpfs. Its mount point has to exist in the checkout.
mkdir -p "$root/crates/teamcraft-ui/gen"
"$engine" run --rm \
  -v "$root:/src:ro" \
  --mount type=tmpfs,destination=/src/crates/teamcraft-ui/gen,tmpfs-mode=1777 \
  -v "$out:/out" \
  -v teamcraft-e2e-web-cargo:/usr/local/cargo/registry \
  -v "teamcraft-e2e-web-target-$checkout_id:/target" \
  -e CARGO_TARGET_DIR=/target \
  -e CARGO_INCREMENTAL=0 \
  "$image" \
  sh -c 'trunk build --release --dist /out && chown -R "$0:$1" /out' "$(id -u)" "$(id -g)"
echo "web client built: $out"
