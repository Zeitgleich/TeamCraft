#!/usr/bin/env python3
"""Sends a release's iOS build from TestFlight to the App Store (docs/release.md, "App Store"), the
way google-play.yml sends the Android bundle to Play: after archive-testflight.sh has uploaded it,

  1. waits until App Store Connect has processed the build,
  2. answers export compliance if the build still asks (the app uses only exempt encryption),
  3. creates the iOS App Store version for the release's version string (App Store Connect copies the
     listing's texts and screenshots from the previous version), or reuses the version in
     preparation (renamed to this version string),
  4. attaches the build, sets the release type (after approval by default) and "What's new",
  5. submits the version for App Review.

Only one version can be in preparation or review at a time. A version already waiting for review
is left alone (`--when-pending skip`, the default: this build stays in TestFlight, the next release
after it goes live is submitted) or withdrawn from review and replaced by this build
(`--when-pending replace`: it loses its place in the queue). A version in review or approved and not
yet live is never touched. What happened goes to the job summary ($GITHUB_STEP_SUMMARY).

Environment: ASC_KEY_ID, ASC_ISSUER_ID, ASC_KEY_PATH (asc_api.py).

Usage: apps/ios/scripts/app-store-submit.py --version 1.2.3 [--whats-new notes.txt] [--dry-run]
  --dry-run   sends only reads; prints every write it would make (needs the key all the same)
"""

import argparse
import os
import re
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Callable

sys.path.insert(0, str(Path(__file__).resolve().parent))
from asc_api import ApiError, Client  # noqa: E402

# appStoreState values (with their appVersionState names where those differ).
EDITABLE_STATES = {
  "PREPARE_FOR_SUBMISSION", "DEVELOPER_REJECTED", "REJECTED", "METADATA_REJECTED", "INVALID_BINARY",
  "READY_FOR_REVIEW",
}
# Submitted and not yet in review: the submission can be withdrawn.
WAITING_STATES = {"WAITING_FOR_REVIEW", "WAITING_FOR_EXPORT_COMPLIANCE"}
RELEASED_STATES = {
  "READY_FOR_SALE", "READY_FOR_DISTRIBUTION", "REPLACED_WITH_NEW_VERSION", "REMOVED_FROM_SALE",
  "DEVELOPER_REMOVED_FROM_SALE",
}
# Every other state (IN_REVIEW, PENDING_DEVELOPER_RELEASE, PROCESSING_FOR_APP_STORE, ...) is not ours
# to change.
RELEASE_TYPES = ("AFTER_APPROVAL", "MANUAL")
POLL_INTERVAL_SECONDS = 30
WITHDRAW_TIMEOUT_SECONDS = 10 * 60


def main() -> None:
  parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
  parser.add_argument("--version", required=True, help="the release's version, X.Y.Z")
  parser.add_argument("--whats-new", type=Path, help="file with the \"What's new\" text")
  parser.add_argument("--bundle-id", default="de.zeitgleich.teamcraft")
  parser.add_argument("--mode", choices=("submit", "prepare"), default="submit",
                      help="prepare: the version with build and notes, without submitting it")
  parser.add_argument("--when-pending", choices=("skip", "replace"), default="skip")
  parser.add_argument("--release-type", choices=RELEASE_TYPES, default="AFTER_APPROVAL")
  parser.add_argument("--processing-timeout-minutes", type=int, default=90)
  parser.add_argument("--dry-run", action="store_true")
  args = parser.parse_args()

  if not re.fullmatch(r"\d+\.\d+\.\d+", args.version):
    report(f"{args.version} is a candidate or prerelease: only releases go to the App Store.")
    return
  settings = Settings(
    version=args.version,
    whats_new=args.whats_new.read_text().strip() if args.whats_new else "",
    bundle_id=args.bundle_id,
    submit=args.mode == "submit",
    when_pending=args.when_pending,
    release_type=args.release_type,
    processing_timeout_seconds=args.processing_timeout_minutes * 60,
  )
  try:
    release(Client.from_env(dry_run=args.dry_run), settings)
  except (ApiError, ReleaseError) as error:
    report(f"App Store: {settings.version} not submitted: {error}", level="error")
    sys.exit(1)


@dataclass
class Settings:
  version: str
  whats_new: str
  bundle_id: str
  submit: bool
  when_pending: str
  release_type: str
  processing_timeout_seconds: int


class ReleaseError(Exception):
  """A state the release can't go on from (no app, the build failed processing, ...)."""


def release(client: Client, settings: Settings, sleep: Callable[[float], None] = time.sleep,
            clock: Callable[[], float] = time.monotonic) -> None:
  app = find_app(client, settings.bundle_id)
  build = wait_for_build(client, app, settings, sleep, clock)
  answer_export_compliance(client, build)
  versions = client.get(f"/apps/{app}/appStoreVersions?filter[platform]=IOS&limit=200")["data"]
  action = choose(client, versions, settings, build["id"])
  if isinstance(action, Skip):
    report(action.reason, level="warning" if action.is_problem else "notice")
    return
  if isinstance(action, Replace):
    withdraw(client, app, action.version, sleep, clock)
    action = Reuse(action.version, note=f", in place of {version_string(action.version)}, withdrawn from review")
  version = prepare_version(client, app, action, settings, build["id"])
  set_whats_new(client, version, versions, settings.whats_new)
  if not settings.submit:
    report(f"App Store: {settings.version} with build {build_number(settings.version)} is ready; "
           f"submit it in App Store Connect (mode prepare).")
    return
  submit(client, app, version)
  report(f"App Store: {settings.version} (build {build_number(settings.version)}) submitted for App Review"
         f"{action.note}; release: {settings.release_type}.")


def find_app(client: Client, bundle_id: str) -> str:
  apps = client.get(f"/apps?filter[bundleId]={bundle_id}")["data"]
  if not apps:
    raise ReleaseError(f"no app with the bundle ID {bundle_id} in App Store Connect")
  return apps[0]["id"]


def wait_for_build(client: Client, app: str, settings: Settings, sleep: Callable[[float], None],
                   clock: Callable[[], float]) -> dict:
  """The uploaded build once processed. It shows up a few minutes after the upload."""
  number = build_number(settings.version)
  path = (f"/builds?filter[app]={app}&filter[version]={number}"
          f"&filter[preReleaseVersion.version]={settings.version}&limit=1")
  deadline = clock() + settings.processing_timeout_seconds
  while True:
    builds = client.get(path)["data"]
    state = builds[0]["attributes"].get("processingState") if builds else "NOT_UPLOADED_YET"
    if state == "VALID":
      return builds[0]
    if state in ("FAILED", "INVALID"):
      raise ReleaseError(f"build {number} is {state} in App Store Connect (see the mail from Apple)")
    if client.dry_run:
      # Shows the rest of the run for a build not uploaded yet, without waiting for it.
      print(f"[dry run] build {number} is {state}; going on with a placeholder")
      return {"id": "dry-run-build", "attributes": {"usesNonExemptEncryption": None}}
    if clock() >= deadline:
      raise ReleaseError(f"build {number} is still {state} after {settings.processing_timeout_seconds // 60} "
                         "minutes; run the App Store workflow again later")
    print(f"build {number}: {state}, waiting")
    sleep(POLL_INTERVAL_SECONDS)


def answer_export_compliance(client: Client, build: dict) -> None:
  """ITSAppUsesNonExemptEncryption is false in project.yml, so the build normally carries the
  answer; a build without it would wait as "Missing Compliance"."""
  if build["attributes"].get("usesNonExemptEncryption") is not None:
    return
  client.patch(f"/builds/{build['id']}", {
    "data": {"type": "builds", "id": build["id"], "attributes": {"usesNonExemptEncryption": False}},
  })


@dataclass
class Create:
  note: str = ""


@dataclass
class Reuse:
  version: dict
  # What the summary says about the version it was before.
  note: str = ""


@dataclass
class Replace:
  version: dict


@dataclass
class Skip:
  reason: str
  is_problem: bool = False


def choose(client: Client, versions: list[dict], settings: Settings, build: str) -> Create | Reuse | Replace | Skip:
  """What to do with the App Store versions as they are: at most one is in preparation or review."""
  target = parse_version(settings.version)
  released = [v for v in versions if state_of(v) in RELEASED_STATES]
  newest_live = max((parse_version(version_string(v)) for v in released), default=None)
  if newest_live is not None and target <= newest_live:
    return Skip(f"App Store: {settings.version} is not newer than the version on the App Store "
                f"({'.'.join(map(str, newest_live))}); nothing to submit.")
  pending = [v for v in versions if state_of(v) not in RELEASED_STATES]
  if not pending:
    return Create()
  # The one matching this release first, should there ever be several.
  version = sorted(pending, key=lambda v: version_string(v) != settings.version)[0]
  name, state = version_string(version), state_of(version)
  if state in EDITABLE_STATES:
    return Reuse(version, note=f", in the version that was {name} in preparation" if name != settings.version else "")
  if state in WAITING_STATES:
    attached = client.get(f"/appStoreVersions/{version['id']}/build")["data"]
    if name == settings.version and attached and attached["id"] == build:
      return Skip(f"App Store: {name} with this build is already waiting for review.")
    if settings.when_pending == "replace":
      return Replace(version)
    return Skip(f"App Store: {name} is waiting for review, so {settings.version} was not submitted and "
                f"stays in TestFlight (TEAMCRAFT_APP_STORE_WHEN_PENDING=skip). The next release after "
                f"{name} is live goes to review.", is_problem=True)
  return Skip(f"App Store: {name} is {state}, so {settings.version} was not submitted and stays in "
              f"TestFlight. The next release after {name} is live goes to review.", is_problem=True)


def withdraw(client: Client, app: str, version: dict, sleep: Callable[[float], None],
             clock: Callable[[], float]) -> None:
  """Cancels the review submission that holds the version and waits until it is editable again."""
  submissions = client.get(f"/reviewSubmissions?filter[app]={app}&filter[platform]=IOS"
                           "&filter[state]=WAITING_FOR_REVIEW,UNRESOLVED_ISSUES")["data"]
  holding = next((s["id"] for s in submissions if version["id"] in submission_versions(client, s["id"])), None)
  if holding is None:
    raise ReleaseError(f"{version_string(version)} is waiting for review, but no review submission holds it")
  client.patch(f"/reviewSubmissions/{holding}", {
    "data": {"type": "reviewSubmissions", "id": holding, "attributes": {"canceled": True}},
  })
  if client.dry_run:
    return
  deadline = clock() + WITHDRAW_TIMEOUT_SECONDS
  while state_of(client.get(f"/appStoreVersions/{version['id']}")["data"]) not in EDITABLE_STATES:
    if clock() >= deadline:
      raise ReleaseError(f"{version_string(version)} did not leave the review queue within "
                         f"{WITHDRAW_TIMEOUT_SECONDS // 60} minutes")
    sleep(POLL_INTERVAL_SECONDS)


def prepare_version(client: Client, app: str, action: Create | Reuse, settings: Settings, build: str) -> str:
  """The version for this release with the build attached and the release type set; its ID."""
  build_link = {"data": {"type": "builds", "id": build}}
  if isinstance(action, Create):
    return client.post("/appStoreVersions", {
      "data": {
        "type": "appStoreVersions",
        "attributes": {"platform": "IOS", "versionString": settings.version, "releaseType": settings.release_type},
        "relationships": {"app": {"data": {"type": "apps", "id": app}}, "build": build_link},
      },
    })["data"]["id"]
  version = action.version["id"]
  client.patch(f"/appStoreVersions/{version}", {
    "data": {
      "type": "appStoreVersions",
      "id": version,
      "attributes": {"versionString": settings.version, "releaseType": settings.release_type},
    },
  })
  client.patch(f"/appStoreVersions/{version}/relationships/build", build_link)
  return version


def set_whats_new(client: Client, version: str, versions: list[dict], text: str) -> None:
  """The same text in every language of the listing. The first version of an app has none."""
  if not text:
    return
  if not any(state_of(v) in RELEASED_STATES for v in versions):
    print("first version on the App Store: no \"What's new\"")
    return
  if version.startswith("dry-run-"):
    print("[dry run] \"What's new\" goes to every localization the new version copies:")
    print(text)
    return
  for localization in client.get(f"/appStoreVersions/{version}/appStoreVersionLocalizations")["data"]:
    client.patch(f"/appStoreVersionLocalizations/{localization['id']}", {
      "data": {"type": "appStoreVersionLocalizations", "id": localization["id"], "attributes": {"whatsNew": text}},
    })
    print(f"What's new: {localization['attributes']['locale']}")


def submit(client: Client, app: str, version: str) -> None:
  """Adds the version to the app's draft review submission (or a new one) and submits it."""
  drafts = client.get(f"/reviewSubmissions?filter[app]={app}&filter[platform]=IOS&filter[state]=READY_FOR_REVIEW")["data"]
  if drafts:
    submission = drafts[0]["id"]
    has_version = version in submission_versions(client, submission)
  else:
    submission = client.post("/reviewSubmissions", {
      "data": {
        "type": "reviewSubmissions",
        "attributes": {"platform": "IOS"},
        "relationships": {"app": {"data": {"type": "apps", "id": app}}},
      },
    })["data"]["id"]
    has_version = False
  if not has_version:
    client.post("/reviewSubmissionItems", {
      "data": {
        "type": "reviewSubmissionItems",
        "relationships": {
          "reviewSubmission": {"data": {"type": "reviewSubmissions", "id": submission}},
          "appStoreVersion": {"data": {"type": "appStoreVersions", "id": version}},
        },
      },
    })
  client.patch(f"/reviewSubmissions/{submission}", {
    "data": {"type": "reviewSubmissions", "id": submission, "attributes": {"submitted": True}},
  })


def submission_versions(client: Client, submission: str) -> set[str]:
  items = client.get(f"/reviewSubmissions/{submission}/items?include=appStoreVersion")["data"]
  return {
    item["relationships"]["appStoreVersion"]["data"]["id"]
    for item in items
    if (item.get("relationships", {}).get("appStoreVersion") or {}).get("data")
  }


def build_number(version: str) -> int:
  """The release's build number (docs/release.md, "Build numbers"): stage 99."""
  major, minor, patch = parse_version(version)
  return major * 1_000_000 + minor * 10_000 + patch * 100 + 99


def parse_version(version: str) -> tuple[int, ...]:
  return tuple(int(part) for part in re.findall(r"\d+", version)[:3])


def version_string(version: dict) -> str:
  return version["attributes"]["versionString"]


def state_of(version: dict) -> str:
  attributes = version["attributes"]
  return attributes.get("appStoreState") or attributes.get("appVersionState") or "UNKNOWN"


def report(message: str, level: str = "notice") -> None:
  """To the log (as a workflow annotation) and to the job summary."""
  print(f"::{level}::{message}")
  summary = os.environ.get("GITHUB_STEP_SUMMARY")
  if summary:
    with open(summary, "a") as file:
      file.write(message + "\n\n")


if __name__ == "__main__":
  main()
