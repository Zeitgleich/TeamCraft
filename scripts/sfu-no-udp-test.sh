#!/usr/bin/env bash
# Runs the SFU's call tests that need UDP really gone (crates/teamcraft-sfu/tests/suite/fallback.rs)
# in a rootless network namespace with one nft rule: every UDP packet is dropped, TCP is untouched.
# Two native clients and the SFU run inside, on the namespace's loopback. Needs unshare (user
# namespaces), ip and nft. No root and no change to the host's network.
#
#   scripts/sfu-no-udp-test.sh               # the UDP-blocked tests
#   scripts/sfu-no-udp-test.sh -- quality    # arguments after -- go to the test binary
#
# With NETEM set to `tc netem` arguments ("delay 40ms 10ms loss 1%"), the loopback gets them too:
# where the measured quality in docs/operations.md comes from. KEEP_UDP=1 leaves UDP alone, for the
# same measurement over QUIC.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

filter=(udp_really_blocked::)
if [ "${1:-}" = "--" ]; then shift; filter=("$@"); fi

binary="$(cargo test -p teamcraft-sfu --test suite --no-run --message-format=json 2>/dev/null \
  | python3 -c 'import json,sys
for line in sys.stdin:
    m = json.loads(line)
    if m.get("reason") == "compiler-artifact" and m["target"]["name"] == "suite" and m.get("executable"):
        print(m["executable"])')"

exec unshare --user --map-root-user --net -- bash -euc '
  ip link set lo up
  if [ -z "${KEEP_UDP:-}" ]; then
    nft add table inet no_udp
    nft "add chain inet no_udp out { type filter hook output priority 0; policy accept; }"
    nft add rule inet no_udp out meta l4proto udp drop
    echo "UDP blocked:"; nft list table inet no_udp
  fi
  if [ -n "${NETEM:-}" ]; then
    # shellcheck disable=SC2086
    tc qdisc add dev lo root netem $NETEM
  fi
  exec "$@"
' bash "$binary" --ignored --test-threads=1 --nocapture "${filter[@]}"
