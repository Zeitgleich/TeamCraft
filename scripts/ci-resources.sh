#!/usr/bin/env bash
# Prints the machine's memory, swap, memory pressure and free disk, with the processes that hold the
# most memory, every few seconds, until it is killed (issue #2344). CI's test job runs it beside the
# build and the tests, so a run that loses its runner (exit 137, or no log at all) still shows in its
# last lines what ran out. Reads /proc only: the CI image has no procps.
#
#   scripts/ci-resources.sh [interval-seconds] [path-for-disk]   # defaults: 30, the current dir

set -u

interval="${1:-30}"
disk_path="${2:-.}"
started=$SECONDS

while true; do
  # MemAvailable is what can still be had without swapping; the kernel kills from about zero.
  read -r available swap_total swap_free < <(
    awk '/^MemAvailable:/ {a=$2} /^SwapTotal:/ {t=$2} /^SwapFree:/ {f=$2} END {print a, t, f}' /proc/meminfo
  )
  # The share of the last ten seconds in which some task waited for memory: what systemd-oomd acts on.
  pressure=$(awk '/^some/ {sub("avg10=", "", $2); print $2}' /proc/pressure/memory 2>/dev/null || echo "-")
  disk_free=$(df -Pm "$disk_path" | awk 'NR == 2 {print $4}')
  # The five largest resident sets, as `name:MiB`.
  top=$(
    for status in /proc/[0-9]*/status; do
      awk '/^Name:/ {n=$2} /^VmRSS:/ {r=$2} END {if (r > 0) print r, n}' "$status" 2>/dev/null
    done | sort -rn | head -5 | awk '{printf "%s:%d ", $2, $1 / 1024}'
  )
  elapsed=$((SECONDS - started))
  printf '[resources] +%dm%02ds mem_available=%dMiB swap_used=%d/%dMiB mem_pressure_avg10=%s disk_free=%dMiB top=%s\n' \
    $((elapsed / 60)) $((elapsed % 60)) $((available / 1024)) $(((swap_total - swap_free) / 1024)) $((swap_total / 1024)) \
    "$pressure" "$disk_free" "$top"
  sleep "$interval"
done
