#!/usr/bin/env python3
"""Names the wasm frames of a web or desktop page crash report (docs/crash-reports.md, "Reading a
page report"). The shipped wasm has no names (download size), so a browser names a frame by its
function index, and the app's split files each have their own index space (#2228). The build's
names map (teamcraft-web-bundle `--names-out`, kept privately per release) has every file's
functions with their names and where their bodies are; this resolves the frames with it.

    scripts/symbolicate-web.py --names MAP_OR_DIR EVENT.json
    scripts/symbolicate-web.py --names MAP_OR_DIR ISSUE_ID

EVENT.json: an event as GlitchTip's API returns it (`/api/0/issues/<id>/events/latest/`) or as the
app sends it. ISSUE_ID: the latest event of that issue, fetched with GLITCHTIP_URL and
GLITCHTIP_TOKEN (an API token with event:read). MAP_OR_DIR: a map
(`teamcraft-ui-names-<build>.json`, also gzipped), or a directory with maps, from which the one of
the report's build is taken (the build ID every file name of a build carries, read from the frames).
Get it with `scripts/debug-files.sh fetch-web-names <build>`.

How a frame is matched, best first:
- Chrome and Firefox name the file and the byte offset (`…_bg.wasm:wasm-function[1185]:0x1234`):
  the function of that file whose body holds the offset.
- A file compiled from the pack's bytes is `wasm://wasm/<hash>` in Chrome and its loader script in
  Firefox: every file of the build that has a function with that index whose body holds the offset
  (usually one; else the candidates are printed).
- WebKit (Safari, the Linux desktop app's WebKitGTK) gives the index only (`<?>.wasm-function[17228]`):
  every file with a function of that index. A large index is the main file's alone; a small one may
  be several files' and is printed with its candidates.
"""
import argparse
import gzip
import json
import os
import pathlib
import re
import sys
import urllib.request

MAP_FORMAT = 1
# The build ID in the app's file names: teamcraft-ui-<id>_bg.wasm, teamcraft-ui-split-<id>.js, …
BUILD_ID = re.compile(r"-([0-9a-f]{16})(?:_bg)?\.(?:wasm|js|pack)\b")
WASM_FUNCTION = re.compile(r"(?:^|\.)wasm-function\[(\d+)\]$")
WASM_LOCATION = re.compile(r":wasm-function\[(\d+)\]:0x([0-9a-fA-F]+)$")
# Candidates printed for a frame that matches more than one file.
MAX_CANDIDATES = 4


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--names", required=True, help="names map, or a directory of them")
    parser.add_argument("event", help="event JSON file or GlitchTip issue ID")
    args = parser.parse_args()
    event = load_event(args.event)
    frames = stack_frames(event)
    if not frames:
        sys.exit("error: the event has no stack frames")
    names = load_map(pathlib.Path(args.names), build_of(frames))
    for number, frame in enumerate(reversed(frames)):
        print(f"#{number:<3} {describe(frame, names)}")


def load_event(source):
    if os.path.isfile(source):
        with open(source, encoding="utf-8") as handle:
            return json.load(handle)
    if not source.isdigit():
        sys.exit(f"error: '{source}' is neither an event file nor an issue ID")
    url, token = os.environ.get("GLITCHTIP_URL"), os.environ.get("GLITCHTIP_TOKEN")
    if not url or not token:
        sys.exit(f"error: set GLITCHTIP_URL and GLITCHTIP_TOKEN to fetch issue {source}, or pass an event file")
    request = urllib.request.Request(
        f"{url.rstrip('/')}/api/0/issues/{source}/events/latest/",
        headers={"Authorization": f"Bearer {token}"},
    )
    with urllib.request.urlopen(request, timeout=30) as response:
        return json.load(response)


def stack_frames(event):
    """The first exception's frames, oldest first, in the sent and in GlitchTip's API shape."""
    values = (event.get("exception") or {}).get("values") or []
    for entry in event.get("entries") or []:
        if entry.get("type") == "exception":
            values = values or (entry.get("data") or {}).get("values") or []
    for value in values:
        frames = (value.get("stacktrace") or {}).get("frames") or []
        if frames:
            return frames
    return []


def build_of(frames):
    """The build ID the frames' files name (one build per page), or None."""
    for frame in frames:
        match = BUILD_ID.search(frame.get("filename") or frame.get("absPath") or "")
        if match:
            return match.group(1)
    return None


def load_map(path, maybe_build):
    if path.is_dir():
        if not maybe_build:
            sys.exit("error: no frame names the build's files; pass the map itself")
        for name in (f"teamcraft-ui-names-{maybe_build}.json", f"teamcraft-ui-names-{maybe_build}.json.gz"):
            if (path / name).is_file():
                path = path / name
                break
        else:
            sys.exit(f"error: no names map of build {maybe_build} in {path}")
    opener = gzip.open if path.suffix == ".gz" else open
    with opener(path, "rt", encoding="utf-8") as handle:
        names = json.load(handle)
    if names.get("format") != MAP_FORMAT:
        sys.exit(f"error: {path} has map format {names.get('format')}, this script reads {MAP_FORMAT}")
    if maybe_build and names.get("build") != maybe_build:
        print(f"warning: the map is of build {names.get('build')}, the report of {maybe_build}", file=sys.stderr)
    return names


def wasm_position(frame):
    """(index, offset or None, file or None) of a wasm frame, else None. Reads the frame's own
    fields (function_id, instruction_addr) and, for older reports and backends that drop them, the
    function name and the location the browser wrote."""
    function = frame.get("function") or ""
    filename = frame.get("filename") or frame.get("absPath") or ""
    index = offset = None
    location = WASM_LOCATION.search(filename)
    if location:
        index, offset = int(location.group(1)), int(location.group(2), 16)
        filename = filename[: location.start()]
    if frame.get("function_id") is not None:
        index = parse_int(frame["function_id"])
    if frame.get("instruction_addr") is not None:
        offset = parse_int(frame["instruction_addr"])
    if index is None:
        match = WASM_FUNCTION.search(function)
        index = int(match.group(1)) if match else None
    if index is None:
        return None
    return index, offset, filename or None


def parse_int(value):
    return int(value, 16) if isinstance(value, str) and value.startswith("0x") else int(value)


def resolve(names, index, offset, maybe_file):
    """The (file, function) pairs the position can be: one when it is clear."""
    modules = names["modules"]
    if maybe_file and maybe_file.endswith(".wasm"):
        base = maybe_file.rsplit("/", 1)[-1]
        modules = [module for module in modules if module["file"] == base] or modules
    found = []
    for module in modules:
        position = index - module["imported_functions"]
        if not 0 <= position < len(module["functions"]):
            continue
        function = module["functions"][position]
        if offset is not None and not function["start"] <= offset < function["end"]:
            continue
        found.append((module["file"], function))
    return found


def describe(frame, names):
    position = wasm_position(frame)
    if position is None:
        where = frame.get("filename") or frame.get("absPath") or "?"
        if frame.get("lineno"):
            where += f":{frame['lineno']}"
            if frame.get("colno"):
                where += f":{frame['colno']}"
        return f"{frame.get('function') or '?'}  ({where})"
    index, offset, maybe_file = position
    place = f"wasm-function[{index}]" + (f" +{offset:#x}" if offset is not None else "")
    found = resolve(names, index, offset, maybe_file)
    if not found:
        return f"{frame.get('function') or place}  ({maybe_file or '?'}, {place}: not in the map, an import or another build)"
    if len(found) == 1:
        file, function = found[0]
        return f"{function.get('name') or '(unnamed)'}  ({file}, {place})"
    candidates = " | ".join(
        f"{function.get('name') or '(unnamed)'} [{file}]" for file, function in found[:MAX_CANDIDATES]
    )
    more = f" | … {len(found) - MAX_CANDIDATES} more" if len(found) > MAX_CANDIDATES else ""
    return f"one of {len(found)}: {candidates}{more}  ({place})"


if __name__ == "__main__":
    main()
