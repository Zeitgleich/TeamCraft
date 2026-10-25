#!/usr/bin/env python3
"""Writes the TeamCraft mark's parts as code for the apps' animated loading screens (issue #463).

The web loading screen inlines the mark in index.html and animates its parts in CSS. The apps draw
the same parts on a canvas: this reads the master (assets/brand/mark.svg, from scripts/brand/mark.py)
and writes one file per platform with every part's outline, fill and bounding box, in the mark's own
coordinates. Arcs become cubic Béziers, so both platforms draw them the same way.

It also writes the Android system splash's icon: an animated vector that plays the mark's assembly
(the same timing as crates/teamcraft-ui/style/loader.css) up to the moment everything rests, where
the app's own loading screen takes over (LoadingMark.kt).

Run from the repository root after mark.py: python3 scripts/brand/mark_parts.py
"""

import math
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
MASTER = ROOT / "assets/brand/mark.svg"
KOTLIN = ROOT / "apps/android/app/src/main/java/de/zeitgleich/teamcraft/ui/theme/MarkParts.kt"
SWIFT = ROOT / "apps/ios/TeamCraft/Design/MarkParts.swift"
SPLASH = ROOT / "apps/android/app/src/main/res/drawable-v31/splash_mark.xml"

# Paint order and names of the master's shapes; the shade is the left body's darker upper face.
PATHS = ("body_right", "body_top", "body_left", "body_left_shade", "bubble")
HEADS = ("head_top", "head_left", "head_right")


def main() -> None:
  master = MASTER.read_text()
  view = [float(value) for value in re.search(r'viewBox="([^"]+)"', master).group(1).split()]
  gradients = {
    match["id"]: (
      tuple(float(match[key]) for key in ("x1", "y1", "x2", "y2")),
      re.findall(r'stop-color="#([0-9a-f]{6})"', match["stops"]),
    )
    for match in re.finditer(
      r'<linearGradient id="(?P<id>[^"]+)"[^>]*x1="(?P<x1>[^"]+)" y1="(?P<y1>[^"]+)" '
      r'x2="(?P<x2>[^"]+)" y2="(?P<y2>[^"]+)">(?P<stops>.*?)</linearGradient>',
      master,
      re.S,
    )
  }
  clip = re.search(r'<clipPath id="[^"]+"><path d="([^"]+)"/>', master).group(1)
  paths = re.findall(r'<path fill="([^"]+)"(?: clip-path="[^"]+")? d="([^"]+)"/>', master)
  circles = re.findall(r'<circle fill="url\(#([^)]+)\)" cx="([^"]+)" cy="([^"]+)" r="([^"]+)"/>', master)
  dots = re.search(r'<g fill="#([0-9a-f]{6})">(.*?)</g>', master, re.S)
  parts = []
  for name, (fill, data) in zip(PATHS, paths, strict=True):
    segments = cubic_segments(data)
    parts.append({"name": name, "kind": "path", "fill": paint(fill, gradients), "segments": segments,
                  "box": bounds(segments), "d": data})
  for name, (gradient, x, y, r) in zip(HEADS, circles, strict=True):
    x, y, r = float(x), float(y), float(r)
    parts.append({"name": name, "kind": "circle", "fill": paint(f"url(#{gradient})", gradients),
                  "circle": (x, y, r), "box": (x - r, y - r, x + r, y + r), "d": circle_data(x, y, r)})
  dot_circles = [tuple(float(v) for v in found) for found in
                 re.findall(r'cx="([^"]+)" cy="([^"]+)" r="([^"]+)"', dots.group(2))]
  shade_clip = cubic_segments(clip)
  KOTLIN.write_text(kotlin(view, parts, shade_clip, dots.group(1), dot_circles))
  SWIFT.write_text(swift(view, parts, shade_clip, dots.group(1), dot_circles))
  SPLASH.parent.mkdir(parents=True, exist_ok=True)
  SPLASH.write_text(splash(view, {part["name"]: part for part in parts}, clip, dots.group(1), dot_circles))


def paint(fill: str, gradients: dict) -> dict:
  found = re.fullmatch(r"url\(#([^)]+)\)", fill)
  if not found:
    color = fill.lstrip("#")
    # Short form (#fff) to six digits.
    return {"color": "".join(digit * 2 for digit in color) if len(color) == 3 else color}
  points, stops = gradients[found.group(1)]
  return {"points": points, "stops": stops}


# --- Paths: absolute M/L/A/Z to M/L/C/Z -----------------------------------------------------------


def cubic_segments(data: str) -> list[tuple]:
  """("M", x, y), ("L", x, y), ("C", x1, y1, x2, y2, x, y) and ("Z",) in absolute coordinates."""
  tokens = re.findall(r"[MLAZ]|-?\d+(?:\.\d+)?", data)
  out, index, current, start = [], 0, (0.0, 0.0), (0.0, 0.0)
  while index < len(tokens):
    command = tokens[index]
    index += 1
    if command == "Z":
      out.append(("Z",))
      current = start
      continue
    count = {"M": 2, "L": 2, "A": 7}[command]
    values = [float(token) for token in tokens[index:index + count]]
    index += count
    if command == "M":
      current = start = (values[0], values[1])
      out.append(("M", *current))
    elif command == "L":
      current = (values[0], values[1])
      out.append(("L", *current))
    else:
      rx, _, _, large, sweep, x, y = values
      out += arc_to_cubics(current, rx, bool(large), bool(sweep), (x, y))
      current = (x, y)
  return out


def arc_to_cubics(start, radius: float, large: bool, sweep: bool, end) -> list[tuple]:
  """A circular SVG arc as cubic Béziers of at most 90° each (SVG 1.1 implementation notes, F.6)."""
  (x1, y1), (x2, y2) = start, end
  dx, dy = (x1 - x2) / 2, (y1 - y2) / 2
  radius = max(radius, math.hypot(dx, dy))
  factor = math.sqrt(max(0.0, radius * radius / (dx * dx + dy * dy) - 1))
  if large == sweep:
    factor = -factor
  cx = factor * dy + (x1 + x2) / 2
  cy = -factor * dx + (y1 + y2) / 2
  theta = math.atan2(y1 - cy, x1 - cx)
  delta = math.atan2(y2 - cy, x2 - cx) - theta
  if sweep and delta < 0:
    delta += 2 * math.pi
  elif not sweep and delta > 0:
    delta -= 2 * math.pi
  pieces = max(1, math.ceil(abs(delta) / (math.pi / 2) - 1e-9))
  step = delta / pieces
  handle = 4 / 3 * math.tan(step / 4) * radius
  out = []
  for piece in range(pieces):
    a, b = theta + piece * step, theta + (piece + 1) * step
    p0 = (cx + radius * math.cos(a), cy + radius * math.sin(a))
    p3 = (cx + radius * math.cos(b), cy + radius * math.sin(b))
    p1 = (p0[0] - handle * math.sin(a), p0[1] + handle * math.cos(a))
    p2 = (p3[0] + handle * math.sin(b), p3[1] - handle * math.cos(b))
    out.append(("C", *p1, *p2, *p3))
  # The arc ends exactly where the path says, not where floating point put it.
  out[-1] = (*out[-1][:5], x2, y2)
  return out


def bounds(segments: list[tuple]) -> tuple[float, float, float, float]:
  """The outline's tight box (as the browser's fill-box), from points sampled along each curve."""
  points, current = [], (0.0, 0.0)
  for segment in segments:
    if segment[0] in ("M", "L"):
      current = segment[1:3]
      points.append(current)
    elif segment[0] == "C":
      x1, y1, x2, y2, x3, y3 = segment[1:]
      for step in range(1, 33):
        t = step / 32
        u = 1 - t
        points.append((u**3 * current[0] + 3 * u * u * t * x1 + 3 * u * t * t * x2 + t**3 * x3,
                       u**3 * current[1] + 3 * u * u * t * y1 + 3 * u * t * t * y2 + t**3 * y3))
      current = (x3, y3)
  xs, ys = [x for x, _ in points], [y for _, y in points]
  return min(xs), min(ys), max(xs), max(ys)


def fmt(value: float) -> str:
  text = f"{value:.2f}".rstrip("0").rstrip(".")
  return "0" if text == "-0" else text


# --- Kotlin ---------------------------------------------------------------------------------------


def kotlin(view, parts, clip, dot_color, dots) -> str:
  def path(segments) -> str:
    calls = []
    for segment in segments:
      kind, values = segment[0], ", ".join(f"{fmt(v)}f" for v in segment[1:])
      calls.append({"M": f"moveTo({values})", "L": f"lineTo({values})", "C": f"cubicTo({values})",
                    "Z": "close()"}[kind])
    return "Path().apply {\n" + "".join(f"      {call}\n" for call in calls) + "    }"

  def fill(paint) -> str:
    if "color" in paint:
      return f"MarkFill.Solid(Color(0xFF{paint['color'].upper()}))"
    x1, y1, x2, y2 = paint["points"]
    colors = ", ".join(f"Color(0xFF{stop.upper()})" for stop in paint["stops"])
    return f"MarkFill.Gradient(listOf({colors}), Offset({fmt(x1)}f, {fmt(y1)}f), Offset({fmt(x2)}f, {fmt(y2)}f))"

  def box(values) -> str:
    return "Rect(" + ", ".join(f"{fmt(v)}f" for v in values) + ")"

  lines = []
  for part in parts:
    name = camel(part["name"])
    if part["kind"] == "path":
      outline = path(part["segments"])
    else:
      x, y, r = part["circle"]
      outline = f"Path().apply {{ addOval(Rect(Offset({fmt(x)}f, {fmt(y)}f), {fmt(r)}f)) }}"
    lines.append(f"  val {name} = MarkPart(\n    {outline},\n    {fill(part['fill'])},\n    {box(part['box'])},\n  )\n")
  dot_list = ", ".join(f"Offset({fmt(x)}f, {fmt(y)}f)" for x, y, _ in dots)
  return f"""// Generated by scripts/brand/mark_parts.py from assets/brand/mark.svg; edit the construction there.
package de.zeitgleich.teamcraft.ui.theme

import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Rect
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path

/** The TeamCraft mark's parts in its own coordinates, for the animated loading screen. */
object MarkParts {{
  /** The square the mark is drawn in: left, top and side. */
  val view = Rect(Offset({fmt(view[0])}f, {fmt(view[1])}f), androidx.compose.ui.geometry.Size({fmt(view[2])}f, {fmt(view[3])}f))
{"".join(lines)}
  /** The left body's darker upper face is the left body clipped to this. */
  val bodyLeftShadeClip = {path(clip)}

  val dotColor = Color(0xFF{dot_color.upper()})
  val dotRadius = {fmt(dots[0][2])}f
  val dots = listOf({dot_list})
}}

/** One part: its outline, its fill and the box its own transforms (scale, rise) refer to. */
class MarkPart(val path: Path, val fill: MarkFill, val box: Rect)

sealed interface MarkFill {{
  data class Solid(val color: Color) : MarkFill

  data class Gradient(val colors: List<Color>, val start: Offset, val end: Offset) : MarkFill
}}
"""


def camel(name: str) -> str:
  first, *rest = name.split("_")
  return first + "".join(word.capitalize() for word in rest)


# --- Swift ----------------------------------------------------------------------------------------


def swift(view, parts, clip, dot_color, dots) -> str:
  def path(segments, indent: str) -> str:
    calls = []
    for segment in segments:
      kind, v = segment[0], [fmt(value) for value in segment[1:]]
      calls.append({
        "M": lambda: f"path.move(to: CGPoint(x: {v[0]}, y: {v[1]}))",
        "L": lambda: f"path.addLine(to: CGPoint(x: {v[0]}, y: {v[1]}))",
        "C": lambda: (f"path.addCurve(to: CGPoint(x: {v[4]}, y: {v[5]}), "
                      f"control1: CGPoint(x: {v[0]}, y: {v[1]}), control2: CGPoint(x: {v[2]}, y: {v[3]}))"),
        "Z": lambda: "path.closeSubpath()",
      }[kind]())
    body = "".join(f"{indent}  {call}\n" for call in calls)
    return f"Path {{ path in\n{body}{indent}}}"

  def fill(paint) -> str:
    if "color" in paint:
      return f".solid(hex: 0x{paint['color'].upper()})"
    x1, y1, x2, y2 = (fmt(value) for value in paint["points"])
    stops = ", ".join(f"0x{stop.upper()}" for stop in paint["stops"])
    return (f".gradient(hexes: [{stops}], start: CGPoint(x: {x1}, y: {y1}), "
            f"end: CGPoint(x: {x2}, y: {y2}))")

  def box(values) -> str:
    x0, y0, x1, y1 = values
    return f"CGRect(x: {fmt(x0)}, y: {fmt(y0)}, width: {fmt(x1 - x0)}, height: {fmt(y1 - y0)})"

  lines = []
  for part in parts:
    name = camel(part["name"])
    if part["kind"] == "path":
      outline = path(part["segments"], "    ")
    else:
      x, y, r = part["circle"]
      outline = f"Path(ellipseIn: CGRect(x: {fmt(x - r)}, y: {fmt(y - r)}, width: {fmt(2 * r)}, height: {fmt(2 * r)}))"
    lines.append(f"  static let {name} = MarkPart(\n    path: {outline},\n    fill: {fill(part['fill'])},\n"
                 f"    box: {box(part['box'])})\n")
  dot_list = ", ".join(f"CGPoint(x: {fmt(x)}, y: {fmt(y)})" for x, y, _ in dots)
  return f"""// Generated by scripts/brand/mark_parts.py from assets/brand/mark.svg; edit the construction there.
import SwiftUI

/// The TeamCraft mark's parts in its own coordinates, for the animated loading screen.
enum MarkParts {{
  /// The square the mark is drawn in.
  static let view = CGRect(x: {fmt(view[0])}, y: {fmt(view[1])}, width: {fmt(view[2])}, height: {fmt(view[3])})
{"".join(lines)}
  /// The left body's darker upper face is the left body clipped to this.
  static let bodyLeftShadeClip = {path(clip, "  ")}

  static let dotHex: UInt32 = 0x{dot_color.upper()}
  static let dotRadius: CGFloat = {fmt(dots[0][2])}
  static let dots = [{dot_list}]
}}

/// One part: its outline, its fill and the box its own transforms (scale, rise) refer to.
struct MarkPart {{
  let path: Path
  let fill: MarkFill
  let box: CGRect
}}

enum MarkFill {{
  case solid(hex: UInt32)
  case gradient(hexes: [UInt32], start: CGPoint, end: CGPoint)
}}
"""


# --- Android system splash -------------------------------------------------------------------------

# The splash icon is drawn in a 288 dp square (no icon background); the mark takes 88 dp of it, as
# on the web's loading screen, well inside the 192 dp circle the system keeps.
SPLASH_DP = 288
SPLASH_MARK_DP = 88
# Easings and timing of loader.css, in milliseconds.
EMPHASIZED = (0.05, 0.7, 0.1, 1)
POP = (0.34, 1.56, 0.64, 1)
EASE_OUT = (0, 0, 0.58, 1)
LINEAR = (0, 0, 1, 1)
SWIRL = (820, -150, 0.4)  # duration, start angle, start scale
HEAD_DELAYS = {"head_top": 0, "head_left": 70, "head_right": 140}
RISE = (640, 0.18)  # duration, share of the body's height
BODY_DELAYS = {"body_top": 160, "body_left": 220, "body_right": 280}
POP_TIMING = (420, 460, 0.6)  # delay, duration, start scale
DOT_IN = (160, (760, 820, 880), 0.55)  # duration, delays, resting opacity
# Everything rests from here; the splash ends and the app's loading screen goes on.
SPLASH_END_MS = 1040


def splash(view, parts, clip, dot_color, dots) -> str:
  x0, y0, side = view[0], view[1], view[2]
  viewport = side * SPLASH_DP / SPLASH_MARK_DP
  offset = (viewport - side) / 2
  centre = (x0 + side / 2, y0 + side / 2)
  groups, targets = [], []

  for name in ("body_right", "body_top", "body_left"):
    part = parts[name]
    rise = part["box"][3] - part["box"][1]
    inner = [path_element(name, part)]
    if name == "body_left":
      inner.append(f'<group><clip-path android:pathData="{clip}" />{path_element("body_left_shade", parts["body_left_shade"])}</group>')
    groups.append(f'<group android:name="{name}_rise" android:translateY="{fmt(rise * RISE[1])}">{"".join(inner)}</group>')
    delay = BODY_DELAYS[name]
    targets.append(target(f"{name}_rise", [animator("translateY", rise * RISE[1], 0, delay, RISE[0], EMPHASIZED)]))
    for path in [name] + (["body_left_shade"] if name == "body_left" else []):
      targets.append(target(path, [animator("fillAlpha", 0, 1, delay, RISE[0], EMPHASIZED)]))

  for name in ("head_top", "head_left", "head_right"):
    part = parts[name]
    cx, cy, _ = part["circle"]
    duration, angle, scale = SWIRL
    delay = HEAD_DELAYS[name]
    groups.append(
      f'<group android:name="{name}_swirl" android:pivotX="{fmt(centre[0])}" android:pivotY="{fmt(centre[1])}" '
      f'android:rotation="{fmt(angle)}"><group android:name="{name}_grow" android:pivotX="{fmt(cx)}" '
      f'android:pivotY="{fmt(cy)}" android:scaleX="{fmt(scale)}" android:scaleY="{fmt(scale)}">'
      f'{path_element(name, part)}</group></group>'
    )
    targets.append(target(f"{name}_swirl", [animator("rotation", angle, 0, delay, duration, EMPHASIZED)]))
    targets.append(target(f"{name}_grow", [animator("scaleX", scale, 1, delay, duration, EMPHASIZED),
                                           animator("scaleY", scale, 1, delay, duration, EMPHASIZED)]))
    targets.append(target(name, [animator("fillAlpha", 0, 1, delay, duration * 0.4, EMPHASIZED)]))

  bubble = parts["bubble"]
  bx0, _, bx1, by1 = bubble["box"]
  pop_delay, pop_duration, pop_scale = POP_TIMING
  dot_paths = "".join(
    f'<path android:name="dot_{index}" android:fillAlpha="0" android:fillColor="#FF{dot_color.upper()}" '
    f'android:pathData="{circle_data(x, y, r)}" />'
    for index, (x, y, r) in enumerate(dots)
  )
  groups.append(
    f'<group android:name="bubble_pop" android:pivotX="{fmt(bx0 + (bx1 - bx0) * 0.48)}" android:pivotY="{fmt(by1)}" '
    f'android:scaleX="{fmt(pop_scale)}" android:scaleY="{fmt(pop_scale)}">{path_element("bubble", bubble)}{dot_paths}</group>'
  )
  targets.append(target("bubble_pop", [animator("scaleX", pop_scale, 1, pop_delay, pop_duration, POP),
                                       animator("scaleY", pop_scale, 1, pop_delay, pop_duration, POP)]))
  # The pop overshoots; opacity only goes up to 1, so it takes the ease-out share of the curve.
  targets.append(target("bubble", [animator("fillAlpha", 0, 1, pop_delay, pop_duration * 0.4, EASE_OUT)]))
  duration, delays, rest = DOT_IN
  for index, delay in enumerate(delays):
    targets.append(target(f"dot_{index}", [animator("fillAlpha", 0, rest, delay, duration, EASE_OUT)]))

  return f"""<?xml version="1.0" encoding="utf-8"?>
<!-- Generated by scripts/brand/mark_parts.py from assets/brand/mark.svg; edit the construction there.
     The system splash's icon (Android 12+): the TeamCraft mark assembling, as on the web's loading
     screen, until {SPLASH_END_MS} ms; LoadingMark.kt takes over from there. -->
<animated-vector xmlns:android="http://schemas.android.com/apk/res/android"
  xmlns:aapt="http://schemas.android.com/aapt">
  <aapt:attr name="android:drawable">
    <vector android:width="{SPLASH_DP}dp" android:height="{SPLASH_DP}dp"
      android:viewportWidth="{fmt(viewport)}" android:viewportHeight="{fmt(viewport)}">
      <group android:translateX="{fmt(offset - x0)}" android:translateY="{fmt(offset - y0)}">
        {(chr(10) + "        ").join(groups)}
      </group>
    </vector>
  </aapt:attr>
  {(chr(10) + "  ").join(targets)}
</animated-vector>
"""


def path_element(name: str, part: dict) -> str:
  fill = part["fill"]
  if "color" in fill:
    return (f'<path android:name="{name}" android:fillAlpha="0" android:fillColor="#FF{fill["color"].upper()}" '
            f'android:pathData="{part["d"]}" />')
  x1, y1, x2, y2 = (fmt(value) for value in fill["points"])
  stops = "".join(
    f'<item android:offset="{fmt(index / (len(fill["stops"]) - 1))}" android:color="#FF{stop.upper()}" />'
    for index, stop in enumerate(fill["stops"])
  )
  return (f'<path android:name="{name}" android:fillAlpha="0" android:pathData="{part["d"]}">'
          f'<aapt:attr name="android:fillColor"><gradient android:type="linear" android:startX="{x1}" '
          f'android:startY="{y1}" android:endX="{x2}" android:endY="{y2}">{stops}</gradient></aapt:attr></path>')


def target(name: str, animators: list[str]) -> str:
  body = animators[0] if len(animators) == 1 else "<set>" + "".join(animators) + "</set>"
  return f'<target android:name="{name}"><aapt:attr name="android:animation">{body}</aapt:attr></target>'


def animator(prop: str, start: float, end: float, delay: float, duration: float, curve) -> str:
  x1, y1, x2, y2 = curve
  return (f'<objectAnimator android:propertyName="{prop}" android:valueFrom="{fmt(start)}" '
          f'android:valueTo="{fmt(end)}" android:valueType="floatType" android:startOffset="{round(delay)}" '
          f'android:duration="{round(duration)}"><aapt:attr name="android:interpolator"><pathInterpolator '
          f'android:controlX1="{x1}" android:controlY1="{y1}" android:controlX2="{x2}" android:controlY2="{y2}" />'
          f'</aapt:attr></objectAnimator>')


def circle_data(x: float, y: float, r: float) -> str:
  return f"M{fmt(x - r)} {fmt(y)}A{fmt(r)} {fmt(r)} 0 1 0 {fmt(x + r)} {fmt(y)}A{fmt(r)} {fmt(r)} 0 1 0 {fmt(x - r)} {fmt(y)}Z"


if __name__ == "__main__":
  main()
