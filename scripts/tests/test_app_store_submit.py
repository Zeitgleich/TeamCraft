"""Tests of apps/ios/scripts/app-store-submit.py: python3 -m unittest discover -s scripts/tests"""
import importlib.util
import pathlib
import sys
import unittest

IOS_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "apps" / "ios" / "scripts"
sys.path.insert(0, str(IOS_SCRIPTS))
SPEC = importlib.util.spec_from_file_location("app_store_submit", IOS_SCRIPTS / "app-store-submit.py")
submit = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(submit)

APP = "6800000000"
BUILD = "build-1004099"
VERSION = "1.0.40"


class AppStoreSubmitTest(unittest.TestCase):
    def test_build_number_is_the_release_stage(self):
        self.assertEqual(submit.build_number("1.0.39"), 1003999)
        self.assertEqual(submit.build_number("1.10.2"), 1100299)

    def test_new_version_gets_build_notes_and_submission(self):
        client = FakeClient(versions=[version("v39", "1.0.39", "READY_FOR_SALE")])
        run(client)
        create = client.write("POST", "/appStoreVersions")
        attributes = create["data"]["attributes"]
        self.assertEqual(attributes["versionString"], VERSION)
        self.assertEqual(attributes["releaseType"], "AFTER_APPROVAL")
        self.assertEqual(create["data"]["relationships"]["build"]["data"]["id"], BUILD)
        notes = [body for method, path, body in client.writes if path.startswith("/appStoreVersionLocalizations/")]
        self.assertEqual([n["data"]["id"] for n in notes], ["loc-de", "loc-en"])
        self.assertEqual(notes[0]["data"]["attributes"]["whatsNew"], "• Threads")
        self.assertIsNotNone(client.write("POST", "/reviewSubmissions"))
        item = client.write("POST", "/reviewSubmissionItems")
        self.assertEqual(item["data"]["relationships"]["appStoreVersion"]["data"]["id"], "new-version")
        self.assertEqual(client.write("PATCH", "/reviewSubmissions/new-submission")["data"]["attributes"],
                         {"submitted": True})

    def test_first_version_gets_no_whats_new(self):
        client = FakeClient(versions=[])
        run(client)
        self.assertFalse(any(path.startswith("/appStoreVersionLocalizations/") for _, path, _ in client.writes))
        self.assertIsNotNone(client.write("PATCH", "/reviewSubmissions/new-submission"))

    def test_version_in_preparation_is_renamed_and_reused(self):
        client = FakeClient(versions=[version("v39", "1.0.39", "PREPARE_FOR_SUBMISSION")])
        run(client)
        self.assertIsNone(client.write("POST", "/appStoreVersions"))
        rename = client.write("PATCH", "/appStoreVersions/v39")
        self.assertEqual(rename["data"]["attributes"]["versionString"], VERSION)
        self.assertEqual(client.write("PATCH", "/appStoreVersions/v39/relationships/build")["data"]["id"], BUILD)

    def test_mode_prepare_does_not_submit(self):
        client = FakeClient(versions=[])
        run(client, do_submit=False)
        self.assertIsNotNone(client.write("POST", "/appStoreVersions"))
        self.assertIsNone(client.write("POST", "/reviewSubmissions"))

    def test_draft_submission_holding_the_version_is_submitted_as_is(self):
        client = FakeClient(versions=[version("v40", VERSION, "READY_FOR_REVIEW")],
                            drafts=[{"id": "draft", "items": ["v40"]}])
        run(client)
        self.assertIsNone(client.write("POST", "/reviewSubmissionItems"))
        self.assertIsNotNone(client.write("PATCH", "/reviewSubmissions/draft"))

    def test_export_compliance_answered_when_missing(self):
        client = FakeClient(versions=[], encryption=None)
        run(client)
        answer = client.write("PATCH", f"/builds/{BUILD}")
        self.assertEqual(answer["data"]["attributes"], {"usesNonExemptEncryption": False})

    def test_export_compliance_left_alone_when_answered(self):
        client = FakeClient(versions=[])
        run(client)
        self.assertIsNone(client.write("PATCH", f"/builds/{BUILD}"))


class PendingTest(unittest.TestCase):
    def test_waiting_version_is_skipped_by_default(self):
        client = FakeClient(versions=[version("v39", "1.0.39", "WAITING_FOR_REVIEW")], attached={"v39": "build-39"})
        run(client)
        self.assertEqual(client.writes, [])

    def test_waiting_version_is_withdrawn_and_replaced(self):
        client = FakeClient(versions=[version("v39", "1.0.39", "WAITING_FOR_REVIEW")], attached={"v39": "build-39"},
                            waiting=[{"id": "queued", "items": ["v39"]}])
        run(client, when_pending="replace")
        self.assertEqual(client.write("PATCH", "/reviewSubmissions/queued")["data"]["attributes"], {"canceled": True})
        self.assertEqual(client.write("PATCH", "/appStoreVersions/v39")["data"]["attributes"]["versionString"], VERSION)
        self.assertIsNotNone(client.write("PATCH", "/reviewSubmissions/new-submission"))
        paths = [path for _, path, _ in client.writes]
        cancel, rename = paths.index("/reviewSubmissions/queued"), paths.index("/appStoreVersions/v39")
        self.assertLess(cancel, rename)

    def test_same_build_already_waiting_changes_nothing(self):
        client = FakeClient(versions=[version("v40", VERSION, "WAITING_FOR_REVIEW")], attached={"v40": BUILD})
        run(client, when_pending="replace")
        self.assertEqual(client.writes, [])

    def test_version_in_review_is_never_touched(self):
        client = FakeClient(versions=[version("v39", "1.0.39", "IN_REVIEW")])
        run(client, when_pending="replace")
        self.assertEqual(client.writes, [])

    def test_version_not_newer_than_live_is_skipped(self):
        client = FakeClient(versions=[version("v41", "1.0.41", "READY_FOR_SALE")])
        run(client)
        self.assertEqual(client.writes, [])


class ProcessingTest(unittest.TestCase):
    def test_waits_until_the_build_is_processed(self):
        client = FakeClient(versions=[], processing=["NOT_UPLOADED", "PROCESSING", "VALID"])
        clock = FakeClock()
        run(client, clock=clock)
        self.assertEqual(clock.sleeps, 2)

    def test_failed_processing_stops(self):
        client = FakeClient(versions=[], processing=["PROCESSING", "INVALID"])
        with self.assertRaises(submit.ReleaseError):
            run(client)
        self.assertEqual(client.writes, [])

    def test_gives_up_after_the_timeout(self):
        client = FakeClient(versions=[], processing=["PROCESSING"] * 1000)
        with self.assertRaises(submit.ReleaseError):
            run(client, timeout_seconds=120)


def run(client, do_submit=True, when_pending="skip", clock=None, timeout_seconds=3600):
    clock = clock or FakeClock()
    settings = submit_settings(do_submit, when_pending, timeout_seconds)
    submit.release(client, settings, sleep=clock.sleep, clock=clock.now)


def submit_settings(do_submit, when_pending, timeout_seconds):
    return submit.Settings(
        version=VERSION, whats_new="• Threads", bundle_id="de.zeitgleich.teamcraft", submit=do_submit,
        when_pending=when_pending, release_type="AFTER_APPROVAL", processing_timeout_seconds=timeout_seconds,
    )


def version(identifier, name, state):
    return {"id": identifier, "type": "appStoreVersions", "attributes": {"versionString": name, "appStoreState": state}}


class FakeClock:
    def __init__(self):
        self.time = 0.0
        self.sleeps = 0

    def now(self):
        return self.time

    def sleep(self, seconds):
        self.time += seconds
        self.sleeps += 1


class FakeClient:
    """App Store Connect as far as the script reads it; records every write."""

    dry_run = False

    def __init__(self, versions, attached=None, drafts=(), waiting=(), encryption=False, processing=("VALID",)):
        self.versions = versions
        self.attached = attached or {}
        self.drafts = list(drafts)
        self.waiting = list(waiting)
        self.encryption = encryption
        self.processing = list(processing)
        self.writes = []

    def write(self, method, path):
        """The body of the first write of that method to that path, or None."""
        return next((body for m, p, body in self.writes if m == method and p == path), None)

    def get(self, path):
        if path.startswith("/apps?"):
            return {"data": [{"id": APP}]}
        if path.startswith("/builds?"):
            state = self.processing.pop(0) if len(self.processing) > 1 else self.processing[0]
            if state == "NOT_UPLOADED":
                return {"data": []}
            return {"data": [{"id": BUILD, "attributes": {"processingState": state,
                                                          "usesNonExemptEncryption": self.encryption}}]}
        if path.startswith(f"/apps/{APP}/appStoreVersions"):
            return {"data": self.versions}
        if path.endswith("/build"):
            identifier = path.split("/")[2]
            return {"data": {"id": self.attached[identifier]} if identifier in self.attached else None}
        if path.endswith("/appStoreVersionLocalizations"):
            return {"data": [{"id": "loc-de", "attributes": {"locale": "de-DE"}},
                             {"id": "loc-en", "attributes": {"locale": "en-US"}}]}
        if path.startswith("/appStoreVersions/"):
            # After a withdrawal the version is editable again.
            found = next(v for v in self.versions if v["id"] == path.split("/")[2])
            return {"data": version(found["id"], found["attributes"]["versionString"], "DEVELOPER_REJECTED")}
        if path.startswith("/reviewSubmissions?") and "READY_FOR_REVIEW" in path:
            return {"data": [{"id": s["id"]} for s in self.drafts]}
        if path.startswith("/reviewSubmissions?"):
            return {"data": [{"id": s["id"]} for s in self.waiting]}
        if "/items" in path:
            identifier = path.split("/")[2]
            submission = next(s for s in self.drafts + self.waiting if s["id"] == identifier)
            return {"data": [{"relationships": {"appStoreVersion": {"data": {"id": v}}}} for v in submission["items"]]}
        raise AssertionError(f"unexpected GET {path}")

    def post(self, path, body):
        self.writes.append(("POST", path, body))
        created = {"/appStoreVersions": "new-version", "/reviewSubmissions": "new-submission"}
        return {"data": {"id": created.get(path, "new-item")}}

    def patch(self, path, body):
        self.writes.append(("PATCH", path, body))
        return {}


if __name__ == "__main__":
    unittest.main()
