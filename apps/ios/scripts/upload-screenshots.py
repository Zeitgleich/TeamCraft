#!/usr/bin/env python3
"""Uploads the App Store screenshots (<dir>/<locale>/<device>/*.jpg, from
scripts/screenshots/ios-store.sh <dir>) to the
iOS version being prepared in App Store Connect, through the App Store Connect API. Optional: the
pictures can as well be dragged into App Store Connect by hand (docs/release.md, "App Store
screenshots").

For every locale folder that the version has a localization for, it replaces the screenshots of
the iPhone 6.3" (required), iPhone 6.9" and iPad 13" (required) sets with the folder's, in file
name order. Other display sizes are left alone (App Store Connect scales these down for them).

Environment, the same API key as the TestFlight upload (archive-testflight.sh; client in asc_api.py):
  ASC_KEY_ID, ASC_ISSUER_ID   the App Store Connect API key (role App Manager)
  ASC_KEY_PATH                its .p8 file

Usage: apps/ios/scripts/upload-screenshots.py <dir> [--dry-run] [--bundle-id de.zeitgleich.teamcraft]
  --dry-run   only check the pictures' sizes and list what would be uploaded; no network, no key

Needs python3 with `cryptography` (for the API's ES256 token) and Pillow (for the size check).
"""

import argparse
import hashlib
import sys
import urllib.request
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from asc_api import ApiError, Client  # noqa: E402

# Folder per display type, with the pixel sizes App Store Connect takes for it (portrait).
DISPLAYS = {
  # Required: iPhone with Dynamic Island, medium display (6.1" and 6.3").
  "iphone-6.3": ("APP_IPHONE_61", {(1206, 2622), (1179, 2556)}),
  # Optional: large display (6.7" and 6.9"); App Store Connect scales nothing up to it.
  "iphone-6.9": ("APP_IPHONE_67", {(1320, 2868), (1290, 2796), (1260, 2736)}),
  "ipad-13": ("APP_IPAD_PRO_3GEN_129", {(2064, 2752), (2048, 2732)}),
}
# Versions whose screenshots can still change.
EDITABLE_STATES = {"PREPARE_FOR_SUBMISSION", "DEVELOPER_REJECTED", "REJECTED", "METADATA_REJECTED"}


def main() -> None:
  parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
  parser.add_argument("dir", type=Path)
  parser.add_argument("--dry-run", action="store_true")
  parser.add_argument("--bundle-id", default="de.zeitgleich.teamcraft")
  args = parser.parse_args()

  shots = collect(args.dir)
  if args.dry_run:
    for (locale, folder), files in sorted(shots.items()):
      print(f"{locale} {DISPLAYS[folder][0]}: {', '.join(f.name for f in files)}")
    return

  try:
    upload_all(Client.from_env(), args.bundle_id, shots)
  except ApiError as error:
    sys.exit(str(error))


def upload_all(client: Client, bundle_id: str, shots: dict[tuple[str, str], list[Path]]) -> None:
  version = editable_version(client, bundle_id)
  localizations = {
    item["attributes"]["locale"]: item["id"]
    for item in client.get(f"/appStoreVersions/{version}/appStoreVersionLocalizations")["data"]
  }
  for (locale, folder), files in sorted(shots.items()):
    if locale not in localizations:
      print(f"{locale}: the version has no such localization; skipped (add the language in App Store Connect)")
      continue
    replace_set(client, localizations[locale], DISPLAYS[folder][0], files)
    print(f"{locale} {DISPLAYS[folder][0]}: {len(files)} uploaded")


def collect(store: Path) -> dict[tuple[str, str], list[Path]]:
  """The pictures per locale and display folder, after checking their pixel sizes."""
  from PIL import Image

  shots = {}
  for locale_dir in sorted(p for p in store.iterdir() if p.is_dir()):
    for folder, (display, sizes) in DISPLAYS.items():
      files = sorted((locale_dir / folder).glob("*.jpg"))
      for file in files:
        with Image.open(file) as image:
          if image.size not in sizes or image.mode != "RGB":
            sys.exit(f"{file}: {image.size} {image.mode}, {display} needs RGB in one of {sorted(sizes)}")
      if len(files) > 10:
        sys.exit(f"{locale_dir.name}/{folder}: {len(files)} pictures, App Store Connect takes at most 10")
      if files:
        shots[(locale_dir.name, folder)] = files
  if not shots:
    sys.exit(f"no screenshots in {store}; run scripts/screenshots/ios-store.sh {store} first")
  return shots


def editable_version(client: Client, bundle_id: str) -> str:
  apps = client.get(f"/apps?filter[bundleId]={bundle_id}")["data"]
  if not apps:
    sys.exit(f"no app with the bundle ID {bundle_id} in App Store Connect")
  versions = client.get(f"/apps/{apps[0]['id']}/appStoreVersions?filter[platform]=IOS")["data"]
  for version in versions:
    if version["attributes"]["appStoreState"] in EDITABLE_STATES:
      print(f"version {version['attributes']['versionString']} ({version['attributes']['appStoreState']})")
      return version["id"]
  sys.exit("no iOS version in preparation; create one in App Store Connect (+ Version or Platform)")


def replace_set(client: Client, localization: str, display: str, files: list[Path]) -> None:
  sets = client.get(f"/appStoreVersionLocalizations/{localization}/appScreenshotSets?filter[screenshotDisplayType]={display}")["data"]
  if sets:
    screenshot_set = sets[0]["id"]
    for old in client.get(f"/appScreenshotSets/{screenshot_set}/appScreenshots")["data"]:
      client.delete(f"/appScreenshots/{old['id']}")
  else:
    screenshot_set = client.post("/appScreenshotSets", {
      "data": {
        "type": "appScreenshotSets",
        "attributes": {"screenshotDisplayType": display},
        "relationships": {"appStoreVersionLocalization": {"data": {"type": "appStoreVersionLocalizations", "id": localization}}},
      },
    })["data"]["id"]
  for file in files:
    upload(client, screenshot_set, file)


def upload(client: Client, screenshot_set: str, file: Path) -> None:
  """Reserves the screenshot, sends its parts to the places the API names, then commits it."""
  data = file.read_bytes()
  reservation = client.post("/appScreenshots", {
    "data": {
      "type": "appScreenshots",
      "attributes": {"fileName": file.name, "fileSize": len(data)},
      "relationships": {"appScreenshotSet": {"data": {"type": "appScreenshotSets", "id": screenshot_set}}},
    },
  })["data"]
  for operation in reservation["attributes"]["uploadOperations"]:
    part = data[operation["offset"]:operation["offset"] + operation["length"]]
    headers = {header["name"]: header["value"] for header in operation.get("requestHeaders", [])}
    request = urllib.request.Request(operation["url"], data=part, method=operation["method"], headers=headers)
    with urllib.request.urlopen(request, timeout=120):
      pass
  client.patch(f"/appScreenshots/{reservation['id']}", {
    "data": {
      "type": "appScreenshots",
      "id": reservation["id"],
      "attributes": {"uploaded": True, "sourceFileChecksum": hashlib.md5(data).hexdigest()},
    },
  })


if __name__ == "__main__":
  main()
