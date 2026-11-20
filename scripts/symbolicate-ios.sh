#!/usr/bin/env bash
# Symbolicates the frames of an iOS crash or app hang report on Linux (docs/crash-reports.md,
# "Reading an iOS report"). GlitchTip up to 6.x keeps neither `debug_meta` nor `threads` of an event
# and symbolicates no Apple frames, so a report shows addresses only; this resolves them with the
# release's dSYMs and llvm-symbolizer.
#
#   scripts/symbolicate-ios.sh [--dsyms DIR] EVENT.json
#   scripts/symbolicate-ios.sh [--dsyms DIR] ISSUE_ID
#       EVENT.json: an event as GlitchTip's API returns it (`/api/0/issues/<id>/events/latest/`) or
#       as sentry-cocoa sends it. ISSUE_ID: the latest event of that issue, fetched with
#       GLITCHTIP_URL and GLITCHTIP_TOKEN (an API token with event:read; GLITCHTIP_ORG defaults to
#       teamcraft).
#       DIR (default ~/.cache/teamcraft-debug/dsym): the release's .dSYM bundles, as the Xcode
#       archive's dSYMs folder holds them, or unpacked from the private bucket
#       (`scripts/debug-files.sh fetch-dsym UUID`). Searched recursively.
#
# Each frame of an image found in DIR (TeamCraft, its extensions, the Rust core linked into them) is
# resolved to function, file and line, inlined calls included. The image is matched by its debug ID
# when the event carries `debug_meta`, else by name: then check that the dSYM is the report's
# release, a dSYM of another build gives wrong names. The address in the dSYM is
# instruction_addr - image_addr + the image's __TEXT vmaddr; a frame other than the innermost is a
# return address, so the call before it is looked up.
set -euo pipefail

fail() { echo "error: $*" >&2; exit 1; }

main() {
  local dsym_dir="$HOME/.cache/teamcraft-debug/dsym" input event
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --dsyms) [ "$#" -ge 2 ] || fail "--dsyms needs a directory"; dsym_dir="$2"; shift 2 ;;
      -h | --help) sed -n '2,22p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
      *) break ;;
    esac
  done
  [ "$#" -eq 1 ] || fail "usage: $0 [--dsyms DIR] EVENT.json | ISSUE_ID"
  input="$1"
  for tool in llvm-symbolizer llvm-objdump llvm-dwarfdump python3; do
    command -v "$tool" >/dev/null || fail "$tool is missing (llvm package, python3)"
  done
  [ -d "$dsym_dir" ] || fail "no dSYM directory at $dsym_dir (--dsyms DIR)"
  if [ -f "$input" ]; then
    event="$input"
  elif [[ "$input" =~ ^[0-9]+$ ]]; then
    event="$(mktemp)"
    FETCHED_EVENT="$event"
    trap 'rm -f "$FETCHED_EVENT"' EXIT
    fetch_latest_event "$input" "$event"
  else
    fail "'$input' is neither an event file nor an issue ID"
  fi
  python3 -I -c "$RESOLVER" "$event" "$dsym_dir"
}

fetch_latest_event() {
  local issue="$1" out="$2"
  [ -n "${GLITCHTIP_URL:-}" ] && [ -n "${GLITCHTIP_TOKEN:-}" ] \
    || fail "set GLITCHTIP_URL and GLITCHTIP_TOKEN to fetch issue $issue, or pass an event file"
  # The token goes in a header read from stdin, never on the command line.
  printf 'header = "Authorization: Bearer %s"\n' "$GLITCHTIP_TOKEN" \
    | curl -fsS --config - -o "$out" "${GLITCHTIP_URL%/}/api/0/issues/$issue/events/latest/" \
    || fail "could not fetch the latest event of issue $issue"
}

# Reads the event, finds each image's DWARF file, resolves the frames and prints them innermost
# first, as a crash log reads.
read -r -d '' RESOLVER <<'PY' || true
import json, os, re, subprocess, sys

event_path, dsym_dir = sys.argv[1], sys.argv[2]
with open(event_path) as handle:
    event = json.load(handle)

def hex_int(value):
    if value is None:
        return None
    return int(value, 16) if isinstance(value, str) else int(value)

def stacks(event):
    """(label, frames) of the exception stacktraces and threads, in both API shapes."""
    found = []
    def exceptions(values):
        for index, value in enumerate(values or []):
            frames = (value.get("stacktrace") or {}).get("frames") or []
            if frames:
                found.append((f"exception {index}: {value.get('type', '')}", frames))
    def threads(values):
        for value in values or []:
            frames = (value.get("stacktrace") or {}).get("frames") or []
            if frames:
                name = "crashed" if value.get("crashed") else "current" if value.get("current") else ""
                found.append((f"thread {value.get('id', '?')} {name}".rstrip(), frames))
    exceptions((event.get("exception") or {}).get("values"))
    threads((event.get("threads") or {}).get("values"))
    for entry in event.get("entries") or []:
        if entry.get("type") == "exception":
            exceptions((entry.get("data") or {}).get("values"))
        elif entry.get("type") == "threads":
            threads((entry.get("data") or {}).get("values"))
    return found

def debug_images(event):
    images = (event.get("debug_meta") or {}).get("images") or []
    for entry in event.get("entries") or []:
        if entry.get("type") == "debugmeta":
            images += (entry.get("data") or {}).get("images") or []
    by_address = {}
    for image in images:
        address = hex_int(image.get("image_addr"))
        uuid = image.get("debug_id") or image.get("uuid")
        if address is not None and uuid:
            by_address[address] = uuid.lower()
    return by_address

def dwarf_files(root):
    """Every DWARF file under root: {uuid: path} and {name: [paths]}."""
    by_uuid, by_name = {}, {}
    for directory, _, files in os.walk(root):
        if not directory.endswith(os.path.join("Contents", "Resources", "DWARF")):
            continue
        for name in files:
            path = os.path.join(directory, name)
            output = subprocess.run(
                ["llvm-dwarfdump", "--uuid", path], capture_output=True, text=True
            ).stdout
            for match in re.finditer(r"UUID: ([0-9A-Fa-f-]{36}) \(arm64e?\)", output):
                by_uuid[match.group(1).lower()] = path
            by_name.setdefault(name, []).append(path)
    return by_uuid, by_name

def text_vmaddr(path, cache={}):
    if path not in cache:
        output = subprocess.run(
            ["llvm-objdump", "--macho", "--private-headers", "--arch=arm64", path],
            capture_output=True, text=True,
        ).stdout
        match = re.search(r"segname __TEXT\s+vmaddr (0x[0-9a-f]+)", output)
        cache[path] = int(match.group(1), 16) if match else 0x100000000
    return cache[path]

def symbolize(path, address):
    """(function, file:line) pairs, innermost inlined call first. Rust names come demangled from
    the linkage name; llvm-symbolizer doesn't demangle Swift, so a Swift frame takes the DWARF
    name (`core(createsKey:)`) instead."""
    def run(kind):
        output = subprocess.run(
            ["llvm-symbolizer", "--obj", path, "--inlining", f"--functions={kind}", "--demangle",
             hex(address)],
            capture_output=True, text=True,
        ).stdout.strip().splitlines()
        return [(output[i], output[i + 1] if i + 1 < len(output) else "") for i in range(0, len(output), 2)]
    linkage, short = run("linkage"), run("short")
    pairs = [
        (short_name if name.lstrip("_").startswith("$s") else name, location)
        for (name, location), (short_name, _) in zip(linkage, short)
    ]
    return [(function, location) for function, location in pairs if function and function != "??"]

by_uuid, by_name = dwarf_files(dsym_dir)
image_uuids = debug_images(event)
all_stacks = stacks(event)
if not all_stacks:
    sys.exit("error: the event has no stack trace")
if not by_uuid:
    print(f"warning: no dSYM found under {dsym_dir}", file=sys.stderr)
if not image_uuids:
    print("note: the event has no debug_meta (GlitchTip drops it): images are matched by name,"
          " so the dSYMs must be the report's release", file=sys.stderr)

for label, frames in all_stacks:
    print(f"== {label}")
    innermost = len(frames) - 1
    for depth, index in enumerate(range(innermost, -1, -1)):
        frame = frames[index]
        instruction = hex_int(frame.get("instruction_addr") or frame.get("instructionAddr"))
        image = hex_int(frame.get("image_addr") or frame.get("imageAddr"))
        package = frame.get("package") or ""
        name = os.path.basename(package) or "?"
        known = frame.get("function")
        prefix = f"{depth:3d} {name[:28]:28s}"
        if instruction is None or image is None:
            print(f"{prefix} {known or '(no address)'}")
            continue
        path = by_uuid.get(image_uuids.get(image, ""))
        if path is None and not image_uuids:
            candidates = by_name.get(name.removesuffix(".debug.dylib"), [])
            path = candidates[0] if len(candidates) == 1 else None
        offset = instruction - image
        if path is None:
            print(f"{prefix} {hex(instruction)} +{hex(offset)}" + (f" {known}" if known else ""))
            continue
        # A return address points after the call; the call is what the frame is in.
        lookup = offset if index == innermost else offset - 1
        address = text_vmaddr(path) + lookup
        resolved = symbolize(path, address)
        if not resolved:
            print(f"{prefix} {hex(instruction)} +{hex(offset)} (not in {os.path.basename(path)})")
            continue
        for depth_inlined, (function, location) in enumerate(resolved):
            marker = "" if depth_inlined == len(resolved) - 1 else " (inlined)"
            print(f"{prefix if depth_inlined == 0 else ' ' * len(prefix)} {function}{marker}  {location}")
PY

main "$@"
