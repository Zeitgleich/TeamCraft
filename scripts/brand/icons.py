#!/usr/bin/env python3
"""Writes every app icon of TeamCraft from the brand masters (scripts/brand/mark.py).

The mark sits on the platform's own backplate: a rounded square for desktop and web, the full
square that Android, iOS and the web's maskable icon cut to shape themselves, and Apple's macOS
grid for the .icns. Below 40 px the people get muddy, so small icons use the glyph: the speech
bubble with its three dots on the brand blue. One-colour icons (Android notification and themed
icon, macOS menu bar) are silhouettes.

Needs python3 and either rsvg-convert or ImageMagick 7 built with librsvg (`magick`); the contact
sheet needs ImageMagick. Run from the repository root:
python3 scripts/brand/icons.py [--contact-sheet out.png]
"""

import argparse
import json
import shutil
import struct
import subprocess
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import mark  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
BRAND = ROOT / "assets/brand"
DESKTOP = ROOT / "apps/desktop/icons"
WEB = ROOT / "crates/teamcraft-ui/icons"
IOS = ROOT / "apps/ios/TeamCraft/Resources/Assets.xcassets"
ANDROID = ROOT / "apps/android/app/src/main/res"

# The mark's drawn extent in its own coordinates (heads and bodies included).
MARK_BOX = (70.8, 115.5, 1182.8, 1166.0)
# Farthest point of the mark from the centre of MARK_BOX (for circular safe zones).
MARK_RADIUS = 557.0
# The bubble with its tail, which the glyph is made of.
BUBBLE_BOX = (413.0, 593.5, 840.5, 1022.0)
GLYPH_DOT_RADIUS = 44.0

# A soft drop shadow for the macOS backplate (feDropShadow isn't supported by every renderer).
SHADOW = """    <filter id="shadow" x="-10%" y="-10%" width="120%" height="125%">
      <feGaussianBlur in="SourceAlpha" stdDeviation="12"/>
      <feOffset dy="10" result="blur"/>
      <feFlood flood-color="#000" flood-opacity="0.28"/>
      <feComposite in2="blur" operator="in"/>
      <feMerge><feMergeNode/><feMergeNode in="SourceGraphic"/></feMerge>
    </filter>"""
LIGHT_BACKPLATE = ("#ffffff", "#eaf1fb")
DARK_BACKPLATE = ("#16284a", "#0b1426")
GLYPH_BACKPLATE = ("#2b8cff", "#0050e0")
ANDROID_DENSITIES = {"mdpi": 1, "hdpi": 1.5, "xhdpi": 2, "xxhdpi": 3, "xxxhdpi": 4}


def main() -> None:
  parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
  parser.add_argument("--contact-sheet", type=Path, help="also write an overview of all icons")
  args = parser.parse_args()
  written = []
  written += brand_masters()
  written += desktop()
  written += web()
  written += ios()
  written += android()
  for path in written:
    print(path.relative_to(ROOT))
  if args.contact_sheet:
    contact_sheet([p for p in written if p.suffix == ".png"], args.contact_sheet)


# --- Outputs -------------------------------------------------------------------------------------


def brand_masters() -> list[Path]:
  """The SVG sources of every icon, kept next to the mark."""
  files = {
    "icon.svg": tile(LIGHT_BACKPLATE, "rounded"),
    "glyph.svg": glyph("rounded"),
  }
  for name, svg in files.items():
    (BRAND / name).write_text(svg)
  return [BRAND / name for name in files]


def desktop() -> list[Path]:
  """Tauri's bundle icons (Linux hicolor, Windows .ico, macOS .icns) and the tray icons."""
  icon = tile(LIGHT_BACKPLATE, "rounded")
  small = glyph("rounded")
  out = []
  for name, size, svg in (
    ("32x32.png", 32, small),
    ("64x64.png", 64, icon),
    ("128x128.png", 128, icon),
    ("128x128@2x.png", 256, icon),
    ("icon.png", 512, icon),
    ("icon-1024.png", 1024, icon),
    # The tray: the glyph in colour (Windows, Linux) and as a template image (macOS menu bar).
    ("tray.png", 64, small),
    ("tray-template.png", 44, glyph_silhouette("#000000", padding=0.06)),
  ):
    out.append(render(svg, size, DESKTOP / name))
  ico_sizes = {16: small, 20: small, 24: small, 32: small, 40: icon, 48: icon, 64: icon, 256: icon}
  write_ico({size: png_bytes(svg, size) for size, svg in ico_sizes.items()}, DESKTOP / "icon.ico")
  out.append(DESKTOP / "icon.ico")
  macos = tile(LIGHT_BACKPLATE, "macos")
  macos_small = glyph("macos")
  icns_entries = {
    "icp4": (16, macos_small), "icp5": (32, macos_small), "ic11": (32, macos_small),
    "ic12": (64, macos), "ic07": (128, macos), "ic13": (256, macos), "ic08": (256, macos),
    "ic14": (512, macos), "ic09": (512, macos), "ic10": (1024, macos),
  }
  write_icns({kind: png_bytes(svg, size) for kind, (size, svg) in icns_entries.items()},
             DESKTOP / "icon.icns")
  out.append(DESKTOP / "icon.icns")
  return out


def web() -> list[Path]:
  """Install icons of the web app, the favicon and the images of the sign-in and loading screen."""
  out = [
    render(tile(LIGHT_BACKPLATE, "rounded"), 192, WEB / "icon-192.png"),
    render(tile(LIGHT_BACKPLATE, "rounded"), 512, WEB / "icon-512.png"),
    # Maskable: the platform cuts any shape out of the square; the mark stays in the 80 % circle.
    render(tile(LIGHT_BACKPLATE, "full", safe_radius=0.39), 512, WEB / "icon-maskable-512.png"),
    render(tile(LIGHT_BACKPLATE, "full", width=0.74), 180, WEB / "apple-touch-icon.png"),
  ]
  (WEB / "favicon.svg").write_text(glyph("rounded"))
  write_ico({size: png_bytes(glyph("rounded"), size) for size in (16, 32, 48)}, WEB / "favicon.ico")
  shutil.copy(BRAND / "mark.svg", WEB / "mark.svg")
  # The loading screen animates the dots itself (style/loader.css).
  (WEB / "loader-mark.svg").write_text(mark.svg(with_dots=False))
  shutil.copy(BRAND / "wordmark.svg", WEB / "wordmark.svg")
  shutil.copy(BRAND / "wordmark-dark.svg", WEB / "wordmark-dark.svg")
  names = ("favicon.svg", "favicon.ico", "mark.svg", "loader-mark.svg", "wordmark.svg",
           "wordmark-dark.svg")
  return out + [WEB / name for name in names]


def ios() -> list[Path]:
  """The single-size app icon with its dark and tinted appearances, and the launch screen logo."""
  icons = IOS / "AppIcon.appiconset"
  out = [
    render(tile(LIGHT_BACKPLATE, "full", width=0.76), 1024, icons / "icon-1024.png", opaque=True),
    render(tile(DARK_BACKPLATE, "full", width=0.76), 1024, icons / "icon-1024-dark.png", opaque=True),
    render(tile(("#000000", "#000000"), "full", width=0.76), 1024, icons / "icon-1024-tinted.png",
           opaque=True, gray=True),
  ]
  appearance = lambda value: [{"appearance": "luminosity", "value": value}]  # noqa: E731
  contents = {
    "images": [
      {"filename": "icon-1024.png", "idiom": "universal", "platform": "ios", "size": "1024x1024"},
      {"appearances": appearance("dark"), "filename": "icon-1024-dark.png", "idiom": "universal",
       "platform": "ios", "size": "1024x1024"},
      {"appearances": appearance("tinted"), "filename": "icon-1024-tinted.png",
       "idiom": "universal", "platform": "ios", "size": "1024x1024"},
    ],
    "info": {"author": "xcode", "version": 1},
  }
  (icons / "Contents.json").write_text(json.dumps(contents, indent=2) + "\n")
  launch = IOS / "LaunchLogo.imageset"
  for scale in (1, 2, 3):
    out.append(render(mark_only(), 120 * scale, launch / f"logo@{scale}x.png"))
  return out


def android() -> list[Path]:
  """The adaptive launcher icon (foreground, monochrome) per density and the notification icon."""
  out = []
  # 108 dp layers; the mark stays inside the 66 dp circle every launcher mask keeps.
  foreground = layer(mark.defs("m-") + "", mark.shapes("m-"), safe=66 / 108)
  monochrome = layer("", silhouette("#ffffff"), safe=66 / 108)
  for density, scale in ANDROID_DENSITIES.items():
    size = round(108 * scale)
    folder = ANDROID / f"mipmap-{density}"
    out.append(render(foreground, size, folder / "ic_launcher_foreground.png"))
    out.append(render(monochrome, size, folder / "ic_launcher_monochrome.png"))
  notification = ANDROID / "drawable/ic_notification.xml"
  notification.write_text(notification_vector())
  return out + [notification]


# --- Compositions --------------------------------------------------------------------------------


def tile(backplate, shape: str, width: float = 0.74, safe_radius: float | None = None) -> str:
  """The mark on a backplate in a 1024 square.

  `shape`: `rounded` (rounded square with a small margin), `full` (the whole square, for
  platforms that mask it) or `macos` (Apple's icon grid with its shadow). `width` is the mark's
  width as a share of the plate; `safe_radius` instead keeps the whole mark inside that circle.
  """
  plate, plate_box = backplate_shape(shape)
  x0, y0, size = plate_box
  if safe_radius is not None:
    scale = safe_radius * 1024 / MARK_RADIUS
  else:
    scale = width * size / (MARK_BOX[2] - MARK_BOX[0])
  top, bottom = backplate
  return svg_doc(
    f"""  <defs>
    <linearGradient id="plate" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="{top}"/><stop offset="1" stop-color="{bottom}"/>
    </linearGradient>
{SHADOW}
{mark.defs("m-")}
  </defs>
  {plate}
{place_mark(mark.shapes("m-"), scale, x0 + size / 2, y0 + size / 2)}"""
  )


def glyph(shape: str) -> str:
  """The small-size icon: the white speech bubble with blue dots on the brand blue."""
  plate, (x0, y0, size) = backplate_shape(shape)
  top, bottom = GLYPH_BACKPLATE
  bubble_width = BUBBLE_BOX[2] - BUBBLE_BOX[0]
  scale = 0.66 * size / bubble_width
  cx = x0 + size / 2
  cy = y0 + size / 2
  return svg_doc(
    f"""  <defs>
    <linearGradient id="plate" x1="0" y1="0" x2="1" y2="1">
      <stop offset="0" stop-color="{top}"/><stop offset="1" stop-color="{bottom}"/>
    </linearGradient>
{SHADOW}
  </defs>
  {plate}
{place_bubble(f'<path fill="#fff" d="{mark.outline(mark.bubble())}"/>'
              + mark.dots('fill="#0058e6"', GLYPH_DOT_RADIUS), scale, cx, cy)}"""
  )


def glyph_silhouette(color: str, padding: float) -> str:
  """The bubble in one colour with the dots cut out (menu bar, notifications)."""
  bubble_width = BUBBLE_BOX[2] - BUBBLE_BOX[0]
  scale = (1 - 2 * padding) * 1024 / bubble_width
  return svg_doc(
    place_bubble(
      f'<path fill="{color}" fill-rule="evenodd" d="{bubble_with_holes()}"/>', scale, 512, 512
    )
  )


def mark_only() -> str:
  """The mark alone on a transparent square (launch screen)."""
  scale = 1024 / (MARK_BOX[2] - MARK_BOX[0])
  return svg_doc(
    f"  <defs>\n{mark.defs('m-')}\n  </defs>\n" + place_mark(mark.shapes("m-"), scale, 512, 512)
  )


def layer(defs: str, shapes: str, safe: float) -> str:
  """An Android adaptive icon layer: the mark inside the circle of diameter `safe` × size."""
  scale = safe / 2 * 1024 / MARK_RADIUS
  return svg_doc(f"  <defs>\n{defs}\n  </defs>\n" + place_mark(shapes, scale, 512, 512))


def silhouette(color: str) -> str:
  """The people in one colour, the bubble cut out and its dots kept (themed icons)."""
  people = "".join(f'<path d="{d}"/>' for d in mark.silhouette_paths())
  return f"""  <mask id="bubble-cut" maskUnits="userSpaceOnUse" x="0" y="0" width="1254" height="1254">
    <rect width="1254" height="1254" fill="#fff"/>
    <path fill="#000" d="{mark.outline(mark.bubble())}"/>
  </mask>
  <g fill="{color}" mask="url(#bubble-cut)">{people}</g>
  {mark.dots(f'fill="{color}"')}"""


def backplate_shape(shape: str) -> tuple[str, tuple[float, float, float]]:
  """The backplate element and the square (x, y, size) it covers."""
  if shape == "full":
    return '<rect width="1024" height="1024" fill="url(#plate)"/>', (0, 0, 1024)
  if shape == "macos":
    # Apple's grid: an 824 px body at 100 px, corner radius 185 px, with a drop shadow.
    return (
      '<rect x="100" y="100" width="824" height="824" rx="185" fill="url(#plate)" '
      'filter="url(#shadow)"/>',
      (100, 100, 824),
    )
  # Rounded: 3 % margin and the corner radius of the platform icons (22 %).
  return (
    '<rect x="32" y="32" width="960" height="960" rx="212" fill="url(#plate)"/>',
    (32, 32, 960),
  )


def place_mark(shapes: str, scale: float, cx: float, cy: float) -> str:
  mx = (MARK_BOX[0] + MARK_BOX[2]) / 2
  my = (MARK_BOX[1] + MARK_BOX[3]) / 2
  return transformed(shapes, scale, cx - mx * scale, cy - my * scale)


def place_bubble(shapes: str, scale: float, cx: float, cy: float) -> str:
  bx = (BUBBLE_BOX[0] + BUBBLE_BOX[2]) / 2
  by = (BUBBLE_BOX[1] + BUBBLE_BOX[3]) / 2
  return transformed(shapes, scale, cx - bx * scale, cy - by * scale)


def transformed(shapes: str, scale: float, dx: float, dy: float) -> str:
  return (
    f'  <g transform="translate({mark.fmt(dx)} {mark.fmt(dy)}) scale({scale:.5f})">\n'
    f"{shapes}\n  </g>"
  )


def bubble_with_holes() -> str:
  """The bubble's path with the dots as holes (for an even-odd fill)."""
  holes = "".join(mark.circle_path((x, y, GLYPH_DOT_RADIUS)) for x, y in mark.DOTS)
  return mark.outline(mark.bubble()) + holes


def notification_vector() -> str:
  """Android's notification icon as a vector drawable: the white bubble, dots cut out."""
  x0, y0, x1, y1 = BUBBLE_BOX
  # 24 dp with 2 dp padding: the bubble's larger side becomes 20 dp.
  side = max(x1 - x0, y1 - y0) * 24 / 20
  vx = (x0 + x1) / 2 - side / 2
  vy = (y0 + y1) / 2 - side / 2
  return f"""<?xml version="1.0" encoding="utf-8"?>
<!-- The speech bubble of the TeamCraft mark; generated by scripts/brand/icons.py. -->
<vector xmlns:android="http://schemas.android.com/apk/res/android"
  android:width="24dp"
  android:height="24dp"
  android:viewportWidth="{mark.fmt(side)}"
  android:viewportHeight="{mark.fmt(side)}">
  <group android:translateX="{mark.fmt(-vx)}" android:translateY="{mark.fmt(-vy)}">
    <path
      android:fillColor="#FFFFFFFF"
      android:fillType="evenOdd"
      android:pathData="{bubble_with_holes()}" />
  </group>
</vector>
"""


def svg_doc(body: str) -> str:
  return (
    '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1024 1024">\n'
    "  <!-- Generated by scripts/brand/icons.py from the mark (scripts/brand/mark.py). -->\n"
    f"{body}\n</svg>\n"
  )


# --- Rendering and containers --------------------------------------------------------------------


def render(svg: str, size: int, out: Path, opaque: bool = False, gray: bool = False) -> Path:
  out.parent.mkdir(parents=True, exist_ok=True)
  out.write_bytes(png_bytes(svg, size, opaque, gray))
  return out


def png_bytes(svg: str, size: int, opaque: bool = False, gray: bool = False) -> bytes:
  """`svg` rasterised to a `size` square PNG."""
  with tempfile.TemporaryDirectory() as tmp:
    source = Path(tmp) / "in.svg"
    raster = Path(tmp) / "raster.png"
    target = Path(tmp) / "out.png"
    source.write_text(svg)
    if shutil.which("rsvg-convert"):
      subprocess.run(["rsvg-convert", "-w", str(size), "-h", str(size), "-o", str(raster),
                      str(source)], check=True)
    else:
      # The SVGs are 1024 wide; librsvg renders at density / 96 px per unit.
      density = 96 * size / 1024
      subprocess.run(["magick", "-background", "none", "-density", f"{density:.4f}",
                      str(source), "-resize", f"{size}x{size}!", str(raster)], check=True)
    finish = ["magick", str(raster)]
    if gray:
      finish += ["-colorspace", "Gray"]
    if opaque:
      finish += ["-background", "#000000", "-alpha", "remove", "-alpha", "off"]
    # No timestamps, so a re-run without changes leaves the files byte for byte as they are.
    finish += ["-strip", "-define", "png:exclude-chunks=date,time", "-define",
               "png:compression-level=9", f"PNG32:{target}" if not opaque else f"PNG24:{target}"]
    subprocess.run(finish, check=True)
    return target.read_bytes()


def write_ico(images: dict[int, bytes], out: Path) -> None:
  """A Windows .ico with PNG-compressed entries (supported since Windows Vista)."""
  header = struct.pack("<HHH", 0, 1, len(images))
  offset = 6 + 16 * len(images)
  entries = b""
  data = b""
  for size, png in sorted(images.items()):
    dim = 0 if size >= 256 else size
    entries += struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(png), offset + len(data))
    data += png
  out.write_bytes(header + entries + data)


def write_icns(images: dict[str, bytes], out: Path) -> None:
  """A macOS .icns with PNG entries of the given types (icp4 … ic10)."""
  body = b"".join(kind.encode() + struct.pack(">I", 8 + len(png)) + png
                  for kind, png in images.items())
  out.write_bytes(b"icns" + struct.pack(">I", 8 + len(body)) + body)


def contact_sheet(images: list[Path], out: Path) -> None:
  """All PNG icons, and the images inside the .ico and .icns files, labelled, at most 180 px."""
  with tempfile.TemporaryDirectory() as tmp:
    entries = [(short_label(path), path) for path in images]
    for container in (DESKTOP / "icon.icns", DESKTOP / "icon.ico", WEB / "favicon.ico"):
      for name, png in container_images(container):
        extracted = Path(tmp) / f"{container.stem}-{container.suffix[1:]}-{name}.png"
        extracted.write_bytes(png)
        entries.append((f"{short_label(container)} {name}", extracted))
    arguments = []
    for label, path in entries:
      arguments += ["-label", label, str(path)]
    subprocess.run([
      "magick", "montage", "-background", "#dfe4ea", "-fill", "#1b2230", "-pointsize", "10",
      *arguments, "-tile", "8x", "-geometry", "180x180>+8+8", "-depth", "8", "-colors", "256",
      "-define", "png:compression-level=9", str(out),
    ], check=True)


def short_label(path: Path) -> str:
  label = str(path.relative_to(ROOT))
  for prefix in ("apps/android/app/src/main/res/", "apps/ios/TeamCraft/Resources/Assets.xcassets/",
                 "apps/", "crates/teamcraft-ui/"):
    label = label.replace(prefix, "")
  return label


def container_images(path: Path) -> list[tuple[str, bytes]]:
  """The PNG images inside an .ico or .icns written by this script, named by size or type."""
  data = path.read_bytes()
  images = []
  if path.suffix == ".ico":
    count = struct.unpack("<H", data[4:6])[0]
    for index in range(count):
      width, _, _, _, _, _, length, offset = struct.unpack("<BBBBHHII", data[6 + 16 * index:22 + 16 * index])
      images.append((str(width or 256), data[offset:offset + length]))
  else:
    offset = 8
    while offset < len(data):
      kind = data[offset:offset + 4].decode()
      length = struct.unpack(">I", data[offset + 4:offset + 8])[0]
      images.append((kind, data[offset + 8:offset + length]))
      offset += length
  return images


if __name__ == "__main__":
  main()
