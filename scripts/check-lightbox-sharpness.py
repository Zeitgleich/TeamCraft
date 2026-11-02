#!/usr/bin/env python3
"""Renders a zoomed picture in the lightbox's real stylesheet and DOM in WebKitGTK, the desktop app's
engine on Linux, and measures how sharp it is on screen (#1089).

A stylesheet can read correct and still show a magnified bitmap: `filter: drop-shadow` on a
transformed image makes WebKit paint the image into a buffer at its unscaled size. Only the rendered
pixels tell, so this compares them, in the same engine, with the same picture drawn plainly at the
same size. Chromium doesn't show that bug at all, so a Chromium check would pass on the old rules.

Needs PyGObject, WebKit2 4.1, numpy and a display (CI: xvfb-run). Exit code 1 when a case is too soft.

    scripts/check-lightbox-sharpness.py [--stylesheet FILE] [--out DIR]
"""

import argparse
import json
import pathlib
import struct
import sys
import tempfile
import zlib

import gi

gi.require_version("Gtk", "3.0")
gi.require_version("WebKit2", "4.1")
import cairo  # noqa: E402
import numpy as np  # noqa: E402
from gi.repository import GLib, Gtk, WebKit2  # noqa: E402

ROOT = pathlib.Path(__file__).resolve().parent.parent
STYLESHEET = ROOT / "crates/teamcraft-ui/style/main/380-lightbox.css"
WINDOW_WIDTH, WINDOW_HEIGHT = 1400, 900
PICTURE_WIDTH, PICTURE_HEIGHT = 2000, 1400
# Zoom in the picture's own pixels: what the issue's report saw as blurry.
OWN_PIXEL_ZOOM = 1.4
# A case must keep this share of the plainly drawn picture's edge energy.
MIN_SHARPNESS = 0.9
# Mean difference per channel (0 to 255) from the picture's own pixels at 100 %.
MAX_ACTUAL_SIZE_DIFFERENCE = 1.0
TIMEOUT_S = 120
SETTLE_MS = 800

STAGE_ROOM = WINDOW_WIDTH - 2 * 64
FIT = min(STAGE_ROOM / PICTURE_WIDTH, WINDOW_HEIGHT / PICTURE_HEIGHT, 1.0)
TRANSFORMED = f"transform:translate(0px,0px) rotate(0deg) scale({OWN_PIXEL_ZOOM / FIT:.4f})"
LAID_OUT = (
    f"width:{PICTURE_WIDTH * OWN_PIXEL_ZOOM}px;height:{PICTURE_HEIGHT * OWN_PIXEL_ZOOM}px;"
    "transform:translate(0px,0px) rotate(0deg) scale(1)"
)
ACTUAL_SIZE = f"width:{PICTURE_WIDTH}px;height:{PICTURE_HEIGHT}px;transform:translate(0px,0px) rotate(0deg) scale(1)"
# The stage's classes and the picture's style, as components/lightbox.rs renders them.
CASES = {
    "at rest, laid out at its zoom": ("zoomed", "is-laid-out", LAID_OUT),
    "while a zoom settles, transformed": ("zoomed", "", TRANSFORMED),
    "panning, a layer of its own": ("zoomed is-live", "is-laid-out", LAID_OUT),
}
ACTUAL_SIZE_CASE = "100 %, its own pixels"

PAGE = """<!doctype html><html><head><meta charset="utf-8"><style>
:root {{ --lightbox-bg: #111; --lightbox-text: #fff; --dur-fast: 0ms; --dur-base: 0ms; --ease-out: linear; --ease-standard: linear; }}
html, body {{ margin: 0; height: 100%; background: #111; }}
* {{ box-sizing: border-box; }}
#root {{ position: fixed; inset: 0; display: flex; }}
{stylesheet}
</style></head><body><div id="root"><div class="lightbox-stage {stage}"><div class="lightbox-track" style="transform: translate3d(0px, 0px, 0) scale(1); opacity: 1">
<div class="lightbox-slot is-current"><img class="lightbox-image {image}" src="picture.png" style="{style}"></div>
</div></div></div></body></html>"""
# The same picture at the same place, without the lightbox.
PLAIN = """<!doctype html><html><body style="margin:0;background:#111;overflow:hidden">
<img src="picture.png" style="position:fixed;left:{left}px;top:{top}px;width:{width}px;height:{height}px"></body></html>"""


def main() -> int:
    args = parse_args()
    directory = pathlib.Path(args.out or tempfile.mkdtemp(prefix="lightbox-sharpness-")).resolve()
    directory.mkdir(parents=True, exist_ok=True)
    pixels = write_picture(directory / "picture.png")
    stylesheet = pathlib.Path(args.stylesheet).read_text()
    pages = [("plain", plain_page())]
    pages += [(name, PAGE.format(stylesheet=stylesheet, stage=stage, image=image, style=style)) for name, (stage, image, style) in CASES.items()]
    stage, image, style = "zoomed", "is-laid-out", ACTUAL_SIZE
    pages.append((ACTUAL_SIZE_CASE, PAGE.format(stylesheet=stylesheet, stage=stage, image=image, style=style)))
    shots = Renderer(directory, pages).run()
    return report(shots, pixels)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--stylesheet", default=STYLESHEET)
    parser.add_argument("--out", help="keep the picture and the screenshots here")
    return parser.parse_args()


def plain_page() -> str:
    width, height = PICTURE_WIDTH * OWN_PIXEL_ZOOM, PICTURE_HEIGHT * OWN_PIXEL_ZOOM
    return PLAIN.format(
        left=(WINDOW_WIDTH - width) / 2,
        top=(WINDOW_HEIGHT - height) / 2,
        width=width,
        height=height,
    )


def report(shots: dict, pixels: np.ndarray) -> int:
    reference = edge_energy(shots["plain"])
    print(f"{'plain <img> at the same size':40} edge energy {reference:6.2f}")
    has_failed = False
    for name in CASES:
        energy = edge_energy(shots[name])
        share = energy / reference
        is_sharp = share >= MIN_SHARPNESS
        has_failed |= not is_sharp
        print(f"{name:40} edge energy {energy:6.2f}  {share:4.0%} of plain  {'ok' if is_sharp else 'TOO SOFT'}")
    difference = actual_size_difference(shots[ACTUAL_SIZE_CASE], pixels)
    is_exact = difference <= MAX_ACTUAL_SIZE_DIFFERENCE
    has_failed |= not is_exact
    print(f"{ACTUAL_SIZE_CASE:40} differs by {difference:5.2f} of 255  {'ok' if is_exact else 'RESAMPLED'}")
    return 1 if has_failed else 0


def edge_energy(shot: np.ndarray) -> float:
    """Mean difference between neighbouring pixels in the window's middle."""
    height, width = shot.shape[:2]
    gray = shot[height // 4 : height * 3 // 4, width // 4 : width * 3 // 4].astype(float).mean(axis=2)
    return float(np.abs(np.diff(gray, axis=1)).mean() + np.abs(np.diff(gray, axis=0)).mean())


def actual_size_difference(shot: np.ndarray, pixels: np.ndarray) -> float:
    """Compares the window's middle with the picture's: the picture is centred in the window."""
    height, width = shot.shape[:2]
    top = (PICTURE_HEIGHT - height) // 2
    left = (PICTURE_WIDTH - width) // 2
    crop = (height // 4, height * 3 // 4, width // 4, width * 3 // 4)
    shown = shot[crop[0] : crop[1], crop[2] : crop[3]].astype(float)
    own = pixels[top + crop[0] : top + crop[1], left + crop[2] : left + crop[3]].astype(float)
    return float(np.abs(shown - own).mean())


def write_picture(path: pathlib.Path) -> np.ndarray:
    """Random pixels in colour: all edges, no fonts, the same on every machine."""
    rng = np.random.default_rng(1089)
    pixels = rng.integers(0, 2, (PICTURE_HEIGHT, PICTURE_WIDTH, 3), dtype=np.uint8) * 255
    raw = b"".join(b"\0" + row.tobytes() for row in pixels)
    header = struct.pack(">IIBBBBB", PICTURE_WIDTH, PICTURE_HEIGHT, 8, 2, 0, 0, 0)
    path.write_bytes(b"\x89PNG\r\n\x1a\n" + png_chunk(b"IHDR", header) + png_chunk(b"IDAT", zlib.compress(raw, 1)) + png_chunk(b"IEND", b""))
    return pixels


def png_chunk(kind: bytes, data: bytes) -> bytes:
    return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))


class Renderer:
    """Loads each page in a real window and reads what the widget shows, compositor included."""

    def __init__(self, directory: pathlib.Path, pages: list):
        self.directory = directory
        self.pages = list(pages)
        self.shots = {}
        self.maybe_failure = None
        self.window = Gtk.Window()
        self.window.set_default_size(WINDOW_WIDTH, WINDOW_HEIGHT)
        self.view = WebKit2.WebView()
        self.view.set_size_request(WINDOW_WIDTH, WINDOW_HEIGHT)
        self.view.get_settings().set_allow_file_access_from_file_urls(True)
        self.view.connect("load-changed", self.on_load)
        self.window.add(self.view)
        self.window.show_all()

    def run(self) -> dict:
        GLib.idle_add(self.load_next)
        GLib.timeout_add_seconds(TIMEOUT_S, self.give_up)
        Gtk.main()
        if self.maybe_failure:
            sys.exit(self.maybe_failure)
        return self.shots

    def give_up(self) -> bool:
        self.maybe_failure = f"no result after {TIMEOUT_S} s; rendered: {sorted(self.shots)}"
        Gtk.main_quit()
        return False

    def load_next(self) -> bool:
        if not self.pages:
            Gtk.main_quit()
            return False
        self.name, page = self.pages.pop(0)
        path = self.directory / f"page-{len(self.shots)}.html"
        path.write_text(page)
        self.view.load_uri(path.as_uri())
        return False

    def on_load(self, _view, event) -> None:
        if event == WebKit2.LoadEvent.FINISHED:
            GLib.timeout_add(SETTLE_MS, self.check_picture)

    def check_picture(self) -> bool:
        script = "JSON.stringify([document.images[0].naturalWidth, innerWidth, innerHeight])"
        self.view.evaluate_javascript(script, -1, None, None, None, self.on_checked)
        return False

    def on_checked(self, view, result) -> None:
        natural, width, height = json.loads(view.evaluate_javascript_finish(result).to_string())
        if (natural, width, height) != (PICTURE_WIDTH, WINDOW_WIDTH, WINDOW_HEIGHT):
            self.maybe_failure = f"{self.name}: picture {natural} px wide in a {width}x{height} window"
            Gtk.main_quit()
            return
        self.shots[self.name] = self.draw()
        GLib.idle_add(self.load_next)

    def draw(self) -> np.ndarray:
        scale = self.view.get_scale_factor()
        surface = cairo.ImageSurface(cairo.FORMAT_ARGB32, WINDOW_WIDTH * scale, WINDOW_HEIGHT * scale)
        context = cairo.Context(surface)
        context.scale(scale, scale)
        self.view.draw(context)
        surface.flush()
        height, stride = surface.get_height(), surface.get_stride()
        bgra = np.frombuffer(surface.get_data(), np.uint8).reshape(height, stride // 4, 4)
        return bgra[:, : surface.get_width(), [2, 1, 0]].copy()


if __name__ == "__main__":
    sys.exit(main())
