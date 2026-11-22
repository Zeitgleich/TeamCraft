#!/usr/bin/env bash
# The direct path's NAT matrix (#127, docs/performance.md, "Direct media through NATs"): two native
# clients, each behind a router of one NAT type, the SFU on the "internet" between them, every link
# shaped with jitter, loss and a rate that never stands still. Each combination runs a few calls; the
# summary says in how many the voice went direct and how long after the join.
#
# Rootless: everything runs in a user namespace of its own (network namespaces, veths, a bridge,
# nftables NAT, tc). Needs the kernel's netem, tbf, bridge, veth and nft NAT modules loaded.
#
#   cargo test -p teamcraft-sfu --test suite --no-run       # prints the suite binary
#   scripts/nat-matrix/nat-matrix.sh <suite binary> <out dir> [runs] [seconds]
#
# NAT_MATRIX_COMBOS picks combinations ("a:b" profiles, space separated). Profiles:
#   full_cone           endpoint-independent mapping and filtering
#   address_restricted  endpoint-independent mapping, address-dependent filtering
#   port_restricted     endpoint-independent mapping, address- and port-dependent filtering
#   port_restricted_nofw  the same without the router's own firewall: unsolicited packets to it
#                       leave connection-tracking entries that a Linux NAT then works around
#   symmetric           endpoint-dependent mapping (a new random port per destination)
#   symmetric_pmp       symmetric, with a router that maps ports on request (NAT-PMP)
#   cgnat               a port-restricted home router behind a carrier NAT (100.64.0.0/10)
#   udp_blocked         a firewall that drops all UDP (the call takes the WebSocket fallback)
set -euo pipefail

if [[ "${NAT_MATRIX_INSIDE:-}" != 1 ]]; then
  NAT_MATRIX_INSIDE=1 exec unshare --user --map-root-user --net --mount --propagation private "$0" "$@"
fi

HERE=$(dirname "$(readlink -f "$0")")
BIN=$(readlink -f "$1")
OUT=$(readlink -f "$2")
RUNS=${3:-5}
SECS=${4:-25}
DEFAULT_COMBOS="full_cone:full_cone address_restricted:address_restricted port_restricted:port_restricted \
symmetric:port_restricted symmetric:full_cone symmetric:symmetric cgnat:port_restricted \
symmetric_pmp:symmetric udp_blocked:port_restricted symmetric:port_restricted_nofw \
port_restricted_nofw:port_restricted_nofw"
read -r -a COMBOS <<<"${NAT_MATRIX_COMBOS:-$DEFAULT_COMBOS}"
SFU_IP=198.18.0.10
SFU_DELAY_MS=${SFU_DELAY_MS:-25}
PEER_DELAY_MS=${PEER_DELAY_MS:-4}
RATE_MBIT=${RATE_MBIT:-10}

mount -t tmpfs nat-matrix /run
mkdir -p /run/netns "$OUT"
# The background loops (rate walks) run while this file exists; it goes when the script ends, however.
RUNNING="$OUT/.running"
touch "$RUNNING"
trap 'rm -f "$RUNNING"' EXIT

ns() { ip netns exec "$1" "${@:2}"; }

reset_topology() {
  local name
  for name in $(ip netns list | awk '{print $1}'); do
    ip netns del "$name"
  done
}

new_ns() {
  ip netns add "$1"
  ns "$1" ip link set lo up
  ns "$1" sysctl -qw net.ipv4.ip_forward=1
}

# The internet: a core router with the SFU's address on it; each side's ISP router hangs off it,
# so a packet between the two sides passes three routers after the first NAT (as on the internet,
# where many more lie between, and where a punch with a small TTL dies on the way).
setup_inet() {
  new_ns inet
  ns inet ip addr add "$SFU_IP/32" dev lo
}

# link <left ns> <left dev> <right ns> <right dev>
link() {
  ns "$1" ip link add "$2" type veth peer name "$4" netns "$3"
  ns "$1" ip link set "$2" up
  ns "$3" ip link set "$4" up
}

# A router of `profile` for side `side` (a or b, number 1 or 2): client 10.N.0.2 behind it.
add_side() {
  local side=$1 number=$2 profile=$3
  local wan_ip="198.18.$number.2" client_ip="10.$number.0.2" lan_ip="10.$number.0.1"
  new_ns "isp$side"
  new_ns "rtr$side"
  new_ns "cli$side"
  link inet "isp$side" "isp$side" core
  ns inet ip addr add "198.19.$number.1/24" dev "isp$side"
  ns "isp$side" ip addr add "198.19.$number.2/24" dev core
  ns "isp$side" ip route add default via "198.19.$number.1"
  ns inet ip route add "198.18.$number.0/24" via "198.19.$number.2"
  link "isp$side" cust "rtr$side" wan
  ns "isp$side" ip addr add "198.18.$number.1/24" dev cust
  link "rtr$side" lan "cli$side" eth0
  ns "cli$side" ip addr add "$client_ip/24" dev eth0
  ns "cli$side" ip route add default via "$lan_ip"
  ns "rtr$side" ip addr add "$lan_ip/24" dev lan
  if [[ $profile == cgnat ]]; then
    # The home router's WAN is inside the carrier's network; the carrier NAT faces the internet.
    new_ns "cgn$side"
    ns "rtr$side" ip link set wan netns "cgn$side"
    ns "cgn$side" ip link set wan up
    ns "cgn$side" ip addr add "$wan_ip/24" dev wan
    ns "cgn$side" ip route add default via "198.18.$number.1"
    link "cgn$side" inside "rtr$side" wan
    ns "cgn$side" ip addr add "100.64.$number.1/24" dev inside
    ns "rtr$side" ip addr add "100.64.$number.2/24" dev wan
    ns "rtr$side" ip route add default via "100.64.$number.1"
    ns "cgn$side" ip route add "10.$number.0.0/24" via "100.64.$number.2"
    nat_rules "rtr$side" port_restricted "$client_ip"
    nat_rules "cgn$side" port_restricted "100.64.$number.2" wan
  else
    ns "rtr$side" ip addr add "$wan_ip/24" dev wan
    ns "rtr$side" ip route add default via "198.18.$number.1"
    nat_rules "rtr$side" "$profile" "$client_ip"
  fi
  if [[ $profile == symmetric_pmp ]]; then
    ns "rtr$side" python3 "$HERE/natpmp-router.py" "$lan_ip" "$wan_ip" "$client_ip" "$((SECS + 20))" \
      >"$RUN_DIR/natpmp-$side.log" 2>&1 &
  fi
}

# nat_rules <ns> <profile> <inside host> [wan dev]
nat_rules() {
  local ns_name=$1 profile=$2 inside=$3 wan=${4:-wan}
  local masquerade="masquerade"
  [[ $profile == symmetric* ]] && masquerade="masquerade fully-random"
  local rules="table ip nat {
    chain post {
      type nat hook postrouting priority 100;
      oifname \"$wan\" $masquerade;
    }"
  case $profile in
    full_cone)
      # The port is kept; anything arriving on it is the inside host's.
      rules+="
    chain pre {
      type nat hook prerouting priority -100;
      iifname \"$wan\" udp dport 1024-65535 dnat to $inside;
    }"
      ;;
    address_restricted)
      rules+="
    set contacted {
      type ipv4_addr; flags timeout; timeout 120s;
    }
    chain out {
      type filter hook forward priority 0;
      oifname \"$wan\" update @contacted { ip daddr };
    }
    chain pre {
      type nat hook prerouting priority -100;
      iifname \"$wan\" ip saddr @contacted udp dport 1024-65535 dnat to $inside;
    }"
      ;;
  esac
  rules+="
  }"
  if [[ $profile != *_nofw ]]; then
    # A home router's own firewall: from the WAN, only what belongs to a connection.
    rules+="
  table inet router {
    chain input {
      type filter hook input priority 0;
      iifname \"$wan\" ct state established,related accept;
      iifname \"$wan\" drop;
    }
  }"
  fi
  if [[ $profile == udp_blocked ]]; then
    rules+="
  table inet firewall {
    chain forward {
      type filter hook forward priority 0;
      meta l4proto udp drop;
    }
  }"
  fi
  ns "$ns_name" nft -f - <<<"$rules"
}

# shape <ns> <dev> <match: dst|src>: the SFU's traffic as a data centre further away, the rest (the
# other device) as the same city; jitter, 0.1 % loss, and a rate on a random walk.
shape() {
  local ns_name=$1 dev=$2 match=$3
  ns "$ns_name" tc qdisc add dev "$dev" root handle 1: prio bands 3 priomap 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1
  ns "$ns_name" tc qdisc add dev "$dev" parent 1:1 handle 10: netem limit 100000 \
    delay "${SFU_DELAY_MS}ms" 3ms distribution normal loss 0.1% rate 10gbit
  ns "$ns_name" tc qdisc add dev "$dev" parent 10:1 handle 11: tbf rate "${RATE_MBIT}mbit" burst 32k latency 150ms
  ns "$ns_name" tc qdisc add dev "$dev" parent 1:2 handle 20: netem limit 100000 \
    delay "${PEER_DELAY_MS}ms" 1ms distribution normal loss 0.1% rate 10gbit
  ns "$ns_name" tc qdisc add dev "$dev" parent 20:1 handle 21: tbf rate "${RATE_MBIT}mbit" burst 32k latency 150ms
  ns "$ns_name" tc filter add dev "$dev" parent 1: pref 1 protocol ip u32 match ip "$match" "$SFU_IP/32" flowid 1:1
  (
    rate=$RATE_MBIT
    while sleep 1 && [[ -e $RUNNING ]]; do
      rate=$((rate + (RANDOM % 5) - 2))
      ((rate < RATE_MBIT / 2)) && rate=$((RATE_MBIT / 2))
      ((rate > RATE_MBIT * 2)) && rate=$((RATE_MBIT * 2))
      for handle in "10:1 handle 11:" "20:1 handle 21:"; do
        # shellcheck disable=SC2086 # the handle pair is two words on purpose
        ns "$ns_name" tc qdisc change dev "$dev" parent $handle tbf rate "${rate}mbit" burst 32k latency 150ms \
          2>/dev/null || exit 0
      done
    done
  ) &
}

wait_for() {
  local file=$1 tries=0
  until [[ -s $file ]]; do
    sleep 0.2
    tries=$((tries + 1))
    if ((tries > 300)); then
      return 1
    fi
  done
  return 0
}

# The first second both clients said "direct", or "-".
first_direct() {
  local a=$1 b=$2
  awk 'FNR == NR { if ($2 == "direct" && !a) a = $1; next }
       { if ($2 == "direct" && !b) b = $1 }
       END { if (a && b) print (a > b ? a : b); else print "-" }' "$a" "$b"
}

run_one() {
  local a=$1 b=$2 run=$3
  RUN_DIR="$OUT/$a--$b/$run"
  mkdir -p "$RUN_DIR"
  reset_topology
  setup_inet
  add_side a 1 "$a"
  add_side b 2 "$b"
  local side
  for side in a b; do
    shape "rtr$side" lan src
    shape "rtr$side" wan dst
  done
  if [[ ${NAT_MATRIX_CAPTURE:-} == 1 ]]; then
    # Debugging: the routers' WAN side, small packets only (no media), for the length of the call.
    for side in a b; do
      ns "rtr$side" timeout "$((SECS + 5))" tcpdump -Z root -U -i wan -nn -w "$RUN_DIR/wan-$side.pcap" "udp and less 100" \
        >/dev/null 2>&1 &
    done
  fi
  local common=(env MEASURE_DIR="$RUN_DIR" MEASURE_SFU_HOST="$SFU_IP" MEASURE_SECS="$SECS" MEASURE_DIRECT=1
    RUST_LOG="${RUST_LOG:-info,teamcraft_media::call_native::direct=debug}")
  ns inet "${common[@]}" MEASURE_ROLE=sfu MEASURE_REFLECT=1 MEASURE_FALLBACK=1 MEASURE_SFU_EXTRA_SECS=3 \
    "$BIN" --ignored --exact direct_measure::run --nocapture >"$RUN_DIR/sfu.out" 2>&1 &
  local sfu=$!
  if ! wait_for "$RUN_DIR/sfu.info"; then
    echo "$a $b $run no-sfu" | tee -a "$OUT/runs.txt"
    wait "$sfu" || true
    return
  fi
  ns clia "${common[@]}" MEASURE_ROLE=1 "$BIN" --ignored --exact direct_measure::run --nocapture \
    >"$RUN_DIR/client1.out" 2>&1 &
  local client_a=$!
  ns clib "${common[@]}" MEASURE_ROLE=2 "$BIN" --ignored --exact direct_measure::run --nocapture \
    >"$RUN_DIR/client2.out" 2>&1 &
  local client_b=$!
  wait "$client_a" "$client_b" "$sfu" || true
  local direct
  direct=$(first_direct "$RUN_DIR/client1.log" "$RUN_DIR/client2.log" 2>/dev/null || echo "-")
  echo "$a $b $run $direct" | tee -a "$OUT/runs.txt"
}

for combo in "${COMBOS[@]}"; do
  for run in $(seq 1 "$RUNS"); do
    run_one "${combo%%:*}" "${combo##*:}" "$run"
  done
done
reset_topology

# Per combination: calls that went direct, and the median second it happened.
awk '{ key = $1 " × " $2; runs[key]++; if ($4 != "-") { ok[key]++; t[key] = t[key] " " $4 } }
     END { for (key in runs) {
       n = split(t[key], times, " "); asort(times); median = n ? times[int((n + 1) / 2)] " s" : "-"
       printf "%-40s %d/%d direct, median %s\n", key, ok[key], runs[key], median } }' \
  "$OUT/runs.txt" | sort | tee "$OUT/summary.txt"
