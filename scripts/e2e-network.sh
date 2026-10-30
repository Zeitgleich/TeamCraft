#!/usr/bin/env bash
# Runs the end-to-end tests of the app on bad networks (tests/e2e/tests/suite/network_conditions.rs,
# and long_absence.rs) inside a rootless network namespace, where the tests may make the link to the
# server lose, delay or swallow packets (tc netem, nft) without root and without touching the host.
# `pasta` forwards the dev stack's ports into the namespace. Needs pasta, tc and nft; the dev stack
# and the release build of the server as scripts/e2e.sh prepares them.
#
#   scripts/e2e-network.sh                 # every network scenario
#   scripts/e2e-network.sh -- no_udp       # arguments after -- go to the test binary
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

filter=(network_conditions:: long_absence::)
if [ "${1:-}" = "--" ]; then shift; filter=("$@"); fi

export SQLX_OFFLINE=true
cargo build --release -p teamcraft-server
binary="$(cargo test --release -p teamcraft-e2e --features e2e --no-run --message-format=json 2>/dev/null \
  | python3 -c 'import json,sys
for line in sys.stdin:
    m = json.loads(line)
    if m.get("reason") == "compiler-artifact" and m["target"]["name"] == "suite" and m.get("executable"):
        print(m["executable"])')"

# Postgres, Keycloak, Garage (S3 and admin), Mailpit (HTTP and SMTP). pasta gets a PATH of system
# directories and the command by its full path: under SELinux it may not search the home directory,
# and each lookup there is reported as a denial. The tests get the caller's PATH back.
exec env PATH=/usr/bin:/bin pasta --config-net -T 5432,8080,3900,3903,8025,1025 -- \
  /usr/bin/env PATH="$PATH" E2E_NETNS=1 E2E_REQUIRED=1 \
  "$binary" --ignored --test-threads=1 --nocapture "${filter[@]}"
