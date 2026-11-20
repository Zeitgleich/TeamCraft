#!/usr/bin/env python3
"""Uploads the App Store screenshots (<dir>/<locale>/<device>/*.jpg, from
scripts/screenshots/ios-store.sh <dir>) to the
iOS version being prepared in App Store Connect, through the App Store Connect API. Optional: the
pictures can as well be dragged into App Store Connect by hand (docs/release.md, "App Store
screenshots").

For every locale folder that the version has a localization for, it replaces the screenshots of
the iPhone 6.9" and iPad 13" sets with the folder's, in file name order. Other display sizes are
left alone (App Store Connect scales the 6.9" and 13" ones down for them).

Environment, the same API key as the TestFlight upload (archive-testflight.sh):
  ASC_KEY_ID, ASC_ISSUER_ID   the App Store Connect API key (role App Manager)
  ASC_KEY_PATH                its .p8 file

Usage: apps/ios/scripts/upload-screenshots.py <dir> [--dry-run] [--bundle-id de.zeitgleich.teamcraft]
  --dry-run   only check the pictures' sizes and list what would be uploaded; no network, no key

Needs python3 with `cryptography` (for the API's ES256 token) and Pillow (for the size check).
"""

import argparse
import base64
import hashlib
import json
import os
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

API = "https://api.appstoreconnect.apple.com/v1"
# Folder per display type, with the pixel sizes App Store Connect takes for it (portrait).
DISPLAYS = {
  "iphone-6.9": ("APP_IPHONE_67", {(1320, 2868), (1290, 2796), (1260, 2736)}),
  "ipad-13": ("APP_IPAD_PRO_3GEN_129", {(2064, 2752), (2048, 2732)}),
}
# Versions whose screenshots can still change.
EDITABLE_STATES = {"PREPARE_FOR_SUBMISSION", "DEVELOPER_REJECTED", "REJECTED", "METADATA_REJECTED"}
TOKEN_LIFETIME_SECONDS = 15 * 60


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

  client = Client(env("ASC_KEY_ID"), env("ASC_ISSUER_ID"), Path(env("ASC_KEY_PATH")).read_bytes())
  version = editable_version(client, args.bundle_id)
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


def editable_version(client: "Client", bundle_id: str) -> str:
  apps = client.get(f"/apps?filter[bundleId]={bundle_id}")["data"]
  if not apps:
    sys.exit(f"no app with the bundle ID {bundle_id} in App Store Connect")
  versions = client.get(f"/apps/{apps[0]['id']}/appStoreVersions?filter[platform]=IOS")["data"]
  for version in versions:
    if version["attributes"]["appStoreState"] in EDITABLE_STATES:
      print(f"version {version['attributes']['versionString']} ({version['attributes']['appStoreState']})")
      return version["id"]
  sys.exit("no iOS version in preparation; create one in App Store Connect (+ Version or Platform)")


def replace_set(client: "Client", localization: str, display: str, files: list[Path]) -> None:
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


def upload(client: "Client", screenshot_set: str, file: Path) -> None:
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


class Client:
  """The App Store Connect API with a short-lived ES256 token from the API key."""

  def __init__(self, key_id: str, issuer_id: str, private_key_pem: bytes) -> None:
    from cryptography.hazmat.primitives.serialization import load_pem_private_key

    self.key_id = key_id
    self.issuer_id = issuer_id
    self.private_key = load_pem_private_key(private_key_pem, password=None)
    self.token = ""
    self.token_expiry = 0.0

  def get(self, path: str) -> dict:
    return self.request("GET", path)

  def post(self, path: str, body: dict) -> dict:
    return self.request("POST", path, body)

  def patch(self, path: str, body: dict) -> dict:
    return self.request("PATCH", path, body)

  def delete(self, path: str) -> dict:
    return self.request("DELETE", path)

  def request(self, method: str, path: str, body: dict | None = None) -> dict:
    data = json.dumps(body).encode() if body is not None else None
    request = urllib.request.Request(API + path, data=data, method=method, headers={
      "Authorization": f"Bearer {self.bearer()}",
      "Content-Type": "application/json",
    })
    try:
      with urllib.request.urlopen(request, timeout=60) as response:
        payload = response.read()
    except urllib.error.HTTPError as error:
      sys.exit(f"{method} {path}: HTTP {error.code}: {error.read().decode(errors='replace')}")
    return json.loads(payload) if payload else {}

  def bearer(self) -> str:
    if time.time() < self.token_expiry - 60:
      return self.token
    from cryptography.hazmat.primitives import hashes
    from cryptography.hazmat.primitives.asymmetric import ec
    from cryptography.hazmat.primitives.asymmetric.utils import decode_dss_signature

    now = int(time.time())
    header = {"alg": "ES256", "kid": self.key_id, "typ": "JWT"}
    claims = {"iss": self.issuer_id, "iat": now, "exp": now + TOKEN_LIFETIME_SECONDS, "aud": "appstoreconnect-v1"}
    signing_input = f"{b64url(json.dumps(header).encode())}.{b64url(json.dumps(claims).encode())}"
    # A JWT's ES256 signature is r and s, 32 bytes each, not the DER the library returns.
    r, s = decode_dss_signature(self.private_key.sign(signing_input.encode(), ec.ECDSA(hashes.SHA256())))
    signature = r.to_bytes(32, "big") + s.to_bytes(32, "big")
    self.token = f"{signing_input}.{b64url(signature)}"
    self.token_expiry = now + TOKEN_LIFETIME_SECONDS
    return self.token


def b64url(data: bytes) -> str:
  return base64.urlsafe_b64encode(data).rstrip(b"=").decode()


def env(name: str) -> str:
  value = os.environ.get(name)
  if not value:
    sys.exit(f"{name} is not set (or use --dry-run)")
  return value


if __name__ == "__main__":
  main()
