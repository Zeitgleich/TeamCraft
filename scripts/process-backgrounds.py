#!/usr/bin/env python3
"""Rebuilds the photo background presets from their originals (assets/backgrounds/README.md).

For every entry of assets/backgrounds/manifest.json the script
  1. downloads the original (a Wikimedia Commons file) into a work directory and checks its
     SHA-256 against the manifest, so the result never silently changes,
  2. crops it to 16:9 around the entry's focus point, scales it to 1920x1080 and writes a JPEG
     (quality 85, lowered until it is at most 400 KiB; no EXIF/ICC/XMP: the pixels are copied
     into a fresh image),
  3. writes a 320x180 thumbnail for the picker,
  4. regenerates assets/backgrounds/SOURCES.md from the manifest.

Usage:  scripts/process-backgrounds.py [--work-dir DIR] [--check]

  --work-dir  where originals are cached (default: .background-originals/ in the repository;
              it is git-ignored and may be deleted afterwards). Keep it off /tmp (RAM).
  --check     only verify that the committed files match what the script produces; exits 1 if not.

Originals are untrusted input: they are only opened as images by Pillow (with a pixel limit),
never executed. Requires Python 3.9+ and Pillow. Output is deterministic for one Pillow version.
"""

import argparse
import hashlib
import io
import json
import sys
import time
import urllib.request
from pathlib import Path

from PIL import Image, ImageOps

ROOT = Path(__file__).resolve().parent.parent
ASSETS = ROOT / "assets" / "backgrounds"
MANIFEST = ASSETS / "manifest.json"
FULL_SIZE = (1920, 1080)
THUMB_SIZE = (320, 180)
FULL_QUALITY = 85
THUMB_QUALITY = 80
MIN_QUALITY = 60
MAX_FULL_BYTES = 400 * 1024
MAX_THUMB_BYTES = 30 * 1024
MAX_ORIGINAL_BYTES = 80 * 1024 * 1024
MAX_ORIGINAL_PIXELS = 120_000_000
USER_AGENT = "TeamCraft-background-build/1.0 (https://github.com/Zeitgleich/TeamCraft)"

Image.MAX_IMAGE_PIXELS = MAX_ORIGINAL_PIXELS


def main() -> int:
    args = parse_args()
    manifest = json.loads(MANIFEST.read_text(encoding="utf-8"))
    work = Path(args.work_dir)
    work.mkdir(parents=True, exist_ok=True)
    (ASSETS / "thumbs").mkdir(parents=True, exist_ok=True)
    mismatches = []
    for entry in manifest["images"]:
        original = fetch_original(entry, work)
        full, thumb = render(original, entry)
        for path, data in (
            (ASSETS / f"{entry['slug']}.jpg", full),
            (ASSETS / "thumbs" / f"{entry['slug']}.jpg", thumb),
        ):
            if args.check:
                if not path.exists() or path.read_bytes() != data:
                    mismatches.append(str(path.relative_to(ROOT)))
            else:
                path.write_bytes(data)
        print(f"{entry['slug']}: {len(full) // 1024} KiB + {len(thumb) // 1024} KiB")
    sources = render_sources(manifest)
    sources_path = ASSETS / "SOURCES.md"
    if args.check:
        if not sources_path.exists() or sources_path.read_text(encoding="utf-8") != sources:
            mismatches.append(str(sources_path.relative_to(ROOT)))
        if mismatches:
            print("out of date: " + ", ".join(mismatches), file=sys.stderr)
            return 1
    else:
        sources_path.write_text(sources, encoding="utf-8")
    return 0


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--work-dir", default=str(ROOT / ".background-originals"))
    parser.add_argument("--check", action="store_true")
    return parser.parse_args()


def fetch_original(entry: dict, work: Path) -> Path:
    """Downloads the original once and verifies its checksum."""
    path = work / entry["original_file"]
    if not path.exists():
        url = entry["download_url"]
        request = urllib.request.Request(url, headers={"User-Agent": USER_AGENT, "Referer": "https://www.flickr.com/"})
        for attempt in range(5):
            try:
                with urllib.request.urlopen(request, timeout=120) as response:
                    data = response.read(MAX_ORIGINAL_BYTES + 1)
                break
            except OSError as error:
                if attempt == 4:
                    raise SystemExit(f"{entry['slug']}: download failed: {error}")
                time.sleep(15 * (attempt + 1))
        if len(data) > MAX_ORIGINAL_BYTES:
            raise SystemExit(f"{entry['slug']}: original larger than {MAX_ORIGINAL_BYTES} bytes")
        path.write_bytes(data)
        time.sleep(3)
    digest = hashlib.sha256(path.read_bytes()).hexdigest()
    if digest != entry["sha256"]:
        raise SystemExit(f"{entry['slug']}: checksum {digest} differs from the manifest")
    return path


def render(original: Path, entry: dict) -> tuple[bytes, bytes]:
    """The 16:9 crop around the focus point, as the full-size and the thumbnail JPEG."""
    with Image.open(original) as opened:
        image = ImageOps.exif_transpose(opened).convert("RGB")
    focus_x, focus_y = entry.get("focus", [0.5, 0.5])
    box = crop_box(image.size, focus_x, focus_y, entry.get("zoom", 1.0))
    cropped = image.crop(box)
    full = cropped.resize(FULL_SIZE, Image.LANCZOS)
    thumb = cropped.resize(THUMB_SIZE, Image.LANCZOS)
    return encode(full, FULL_QUALITY, MAX_FULL_BYTES), encode(thumb, THUMB_QUALITY, MAX_THUMB_BYTES)


def crop_box(size: tuple[int, int], focus_x: float, focus_y: float, zoom: float) -> tuple[int, int, int, int]:
    """Largest 16:9 box (shrunk by `zoom` >= 1) centred on the focus point, kept inside the image."""
    width, height = size
    box_width = min(width, height * 16 / 9) / zoom
    box_height = box_width * 9 / 16
    left = min(max(focus_x * width - box_width / 2, 0), width - box_width)
    top = min(max(focus_y * height - box_height / 2, 0), height - box_height)
    return round(left), round(top), round(left + box_width), round(top + box_height)


def encode(image: Image.Image, quality: int, max_bytes: int) -> bytes:
    """A baseline JPEG without any metadata (fresh image, so no EXIF/ICC/XMP); the quality drops
    below the target until the file fits `max_bytes`."""
    clean = Image.frombytes("RGB", image.size, image.tobytes())
    while True:
        buffer = io.BytesIO()
        clean.save(buffer, "JPEG", quality=quality, optimize=True, progressive=False, subsampling=2)
        if buffer.tell() <= max_bytes or quality <= MIN_QUALITY:
            return buffer.getvalue()
        quality -= 1


def render_sources(manifest: dict) -> str:
    header = ["Preset", "Title", "Author", "Licence", "Source", "Retrieved"]
    rows = [
        [e["slug"], e["title"].replace("|", "/"), e["author"], e["license"], "<" + e["page_url"] + ">", e["retrieved"]]
        for e in manifest["images"]
    ]
    widths = [max(len(row[i]) for row in [header] + rows) for i in range(len(header))]

    def line(cells: list[str]) -> str:
        return "| " + " | ".join(cell.ljust(width) for cell, width in zip(cells, widths)) + " |"

    lines = [
        "# Photo background sources",
        "",
        "Generated by `scripts/process-backgrounds.py` from `manifest.json`; do not edit by hand.",
        "",
        "Every picture is cropped to 16:9 and scaled to 1920x1080 (plus a 320x180 thumbnail); all",
        "metadata is removed. The licence was checked on the source page on the retrieval date.",
        "",
        "- CC0 1.0: dedicated to the public domain by the author.",
        "- Public Domain Mark 1.0: the author (Flickr account \"Foto Miki Digital\", 3D renderings)",
        "  marks the pictures as public domain. The mark is a statement, not a legal dedication.",
        "",
        line(header),
        "|" + "|".join("-" * (width + 2) for width in widths) + "|",
    ]
    lines += [line(row) for row in rows]
    lines.append("")
    return "\n".join(lines)


if __name__ == "__main__":
    sys.exit(main())
