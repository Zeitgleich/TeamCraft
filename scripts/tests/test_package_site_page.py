"""Tests of scripts/package-site-page.py: python3 -m unittest discover -s scripts/tests"""
import importlib.util
import json
import pathlib
import tempfile
import unittest

SCRIPT = pathlib.Path(__file__).resolve().parent.parent / "package-site-page.py"
SPEC = importlib.util.spec_from_file_location("package_site_page", SCRIPT)
page = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(page)

REPO = "Example-Org/TeamCraft"
SITE_URL = "https://example-org.github.io/TeamCraft"
FINGERPRINT = "0123456789ABCDEF0123456789ABCDEF01234567"
RELEASE_FILES = [
    "TeamCraft_1.0.1_x64-setup.exe",
    "TeamCraft_1.0.1_x64-setup.exe.sig",
    "TeamCraft_1.0.1_arm64-setup.exe",
    "TeamCraft_1.0.1_x64_en-US.msi",
    "TeamCraft_1.0.1_arm64_en-US.msi",
    "TeamCraft_1.0.1_universal.dmg",
    "TeamCraft_1.0.1_universal.app.tar.gz",
    "TeamCraft_1.0.1_amd64.AppImage",
    "TeamCraft_1.0.1_aarch64.AppImage",
    "TeamCraft_1.0.1_amd64.deb",
    "TeamCraft_1.0.1_arm64.deb",
    "TeamCraft-1.0.1-1.x86_64.rpm",
    "TeamCraft-1.0.1-1.aarch64.rpm",
    "teamcraft-desktop-x86_64.flatpak",
    "teamcraft-desktop-aarch64.flatpak",
    "teamcraft-android-1.0.1-full-release.apk",
    "teamcraft-android-1.0.1-libre-release.apk",
    "teamcraft-android-1.0.1-full-debug-signed.apk",
    "teamcraft-server-1.0.1.spdx.json",
    "latest.json",
    "SHA256SUMS",
]


class ClassifyAssetsTest(unittest.TestCase):
    def test_every_platform_package_is_offered(self):
        downloads = page.classify_assets(assets(RELEASE_FILES), allow_debug=True)
        kinds = {(d.os, d.kind, d.arch) for d in downloads}
        self.assertEqual(kinds, {
            ("windows", "exe", "x64"), ("windows", "exe", "arm64"),
            ("windows", "msi", "x64"), ("windows", "msi", "arm64"),
            ("macos", "dmg", "universal"),
            ("linux", "appimage", "x64"), ("linux", "appimage", "arm64"),
            ("linux", "deb", "x64"), ("linux", "deb", "arm64"),
            ("linux", "rpm", "x64"), ("linux", "rpm", "arm64"),
            ("linux", "flatpak", "x64"), ("linux", "flatpak", "arm64"),
            ("android", "apk-full", ""), ("android", "apk-libre", ""),
        })

    def test_signatures_sboms_and_debug_builds_are_skipped_when_release_apks_exist(self):
        names = {d.name for d in page.classify_assets(assets(RELEASE_FILES), allow_debug=True)}
        self.assertNotIn("TeamCraft_1.0.1_x64-setup.exe.sig", names)
        self.assertNotIn("teamcraft-android-1.0.1-full-debug-signed.apk", names)
        self.assertFalse(any(n.endswith((".json", ".tar.gz")) or n == "SHA256SUMS" for n in names))

    def test_unsigned_apks_are_never_offered(self):
        files = ["teamcraft-android-1.0.1-full-release-unsigned.apk"]
        self.assertEqual(page.classify_assets(assets(files), allow_debug=False), [])

    def test_prerelease_falls_back_to_debug_signed_apks(self):
        files = ["teamcraft-android-1.0.1-full-release-unsigned.apk", "teamcraft-android-1.0.1-full-debug-signed.apk"]
        self.assertEqual([d.name for d in page.classify_assets(assets(files), allow_debug=True)], [files[1]])
        self.assertEqual(page.classify_assets(assets(files), allow_debug=False), [])


class RenderTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        root = pathlib.Path(self.temp.name)
        self.work = root / "work"
        self.site = root / "site"
        self.site.mkdir()

    def tearDown(self):
        self.temp.cleanup()

    def test_without_stable_release_the_page_announces_it_and_offers_the_prerelease(self):
        self.add_release("prerelease", "v1.0.0-rc.1")
        self.add_repositories("prerelease")
        html = self.render()
        self.assertIn("first stable release is coming soon", html)
        self.assertIn('id="linux-prerelease" data-tab-title="Beta" data-selected', html)
        self.assertIn("rpm/prerelease/teamcraft-prerelease.repo", html)
        self.assertIn('id="apt-prerelease"', html)
        self.assertIn("Switch back from beta to stable", html)
        self.assertNotIn("rpm/stable", html)
        self.assertNotIn('id="apt-stable"', html)
        self.assertEqual(page.check_links(self.site, SITE_URL), [])

    def test_with_stable_release_the_stable_channel_comes_first(self):
        self.add_release("stable", "v1.0.0")
        self.add_release("prerelease", "v1.1.0-rc.1")
        self.add_repositories("stable")
        self.add_repositories("prerelease")
        html = self.render()
        self.assertNotIn("coming soon", html)
        self.assertLess(html.index('id="apt-stable"'), html.index('id="apt-prerelease"'))
        self.assertIn('id="linux-stable" data-tab-title="Stable" data-selected', html)
        self.assertNotIn('data-tab-title="Beta" data-selected', html)
        self.assertIn("rpm/stable/teamcraft.repo", html)
        self.assertEqual(page.check_links(self.site, SITE_URL), [])

    def test_downloads_link_the_release_assets(self):
        self.add_release("prerelease", "v1.0.0-rc.1")
        html = self.render()
        self.assertIn(f"https://github.com/{REPO}/releases/download/v1.0.0-rc.1/TeamCraft_1.0.1_universal.dmg", html)
        self.assertNotIn(".sig\"", html)

    def test_fingerprint_and_store_link_come_from_the_environment(self):
        self.add_release("prerelease", "v1.0.0-rc.1")
        self.add_repositories("prerelease")
        env = {"GPG_KEY_ID": FINGERPRINT, "PLAY_STORE_URL": "https://play.example.com/teamcraft"}
        html = self.render(env)
        self.assertIn("0123 4567 89AB CDEF", html)
        self.assertIn("https://play.example.com/teamcraft", html)
        self.assertNotIn("Google Play</a>", self.render({}))

    def test_without_repositories_no_setup_snippets(self):
        self.add_release("prerelease", "v1.0.0-rc.1")
        html = self.render()
        self.assertNotIn('id="linux"', html)
        self.assertNotIn("teamcraft-archive-keyring", html)
        self.assertEqual(page.check_links(self.site, SITE_URL), [])

    def test_test_build_warning_only_for_debug_signed_apks(self):
        self.add_release("prerelease", "v1.0.0-rc.1", files=[
            "teamcraft-android-1.0.0-rc.1-full-release-unsigned.apk",
            "teamcraft-android-1.0.0-rc.1-full-debug-signed.apk",
        ])
        self.assertIn("throwaway key", self.render())

    def test_signed_release_apks_have_no_test_build_warning(self):
        self.add_release("prerelease", "v1.0.0-rc.1")
        html = self.render()
        self.assertIn("teamcraft-android-1.0.1-full-release.apk", html)
        self.assertNotIn("throwaway key", html)
        self.assertNotIn("debug-signed", html)

    def test_static_files_and_illustrations_are_published(self):
        self.add_release("prerelease", "v1.0.0-rc.1")
        html = self.render()
        for name in ("style.css", "site.js", "fonts/inter-latin-variable.woff2", "fonts/OFL.txt", "logo.png"):
            self.assertTrue((self.site / name).is_file(), name)
        self.assertIn('class="showcase"', html)
        self.assertNotIn("<!--", html)
        self.assertNotIn(' style="', html)
        self.assertEqual(page.check_links(self.site, SITE_URL), [])

    def test_app_store_tile_follows_the_environment(self):
        self.add_release("prerelease", "v1.0.0-rc.1")
        self.assertIn("Coming soon", self.render())
        self.assertIn("App Store", self.render({"APP_STORE_URL": "https://apps.example.com/teamcraft"}))

    def test_html_in_release_names_is_escaped(self):
        self.add_release("prerelease", "v1.0.0-rc.1", name="<script>x</script>")
        self.assertNotIn("<script>x", self.render())

    def render(self, env=None):
        channels = page.load_channels(self.work)
        page.copy_static(self.site)
        html = page.render_page(channels, self.site, SITE_URL, REPO, env or {})
        (self.site / "index.html").write_text(html, encoding="utf-8")
        return html

    def add_release(self, channel, tag, name=None, files=None):
        (self.work / channel).mkdir(parents=True)
        release = {
            "tag_name": tag,
            "name": name or f"TeamCraft {tag}",
            "published_at": "2026-11-02T10:00:00Z",
            "html_url": f"https://github.com/{REPO}/releases/tag/{tag}",
            "assets": assets(files or RELEASE_FILES, tag),
        }
        (self.work / channel / "release.json").write_text(json.dumps(release), encoding="utf-8")

    def add_repositories(self, channel):
        for name in ("teamcraft-archive-keyring.asc", "teamcraft-archive-keyring.gpg"):
            (self.site / name).write_text("key", encoding="utf-8")
        suite = self.site / "apt" / "dists" / channel
        (suite / "main" / "binary-amd64").mkdir(parents=True)
        (suite / "InRelease").write_text("signed", encoding="utf-8")
        (suite / "main" / "binary-amd64" / "Packages").write_text("Package: team-craft\n", encoding="utf-8")
        repo_name = "teamcraft" if channel == "stable" else f"teamcraft-{channel}"
        (self.site / "rpm" / channel).mkdir(parents=True)
        (self.site / "rpm" / channel / f"{repo_name}.repo").write_text("[teamcraft]\n", encoding="utf-8")


class CheckLinksTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.site = pathlib.Path(self.temp.name)
        (self.site / "apt").mkdir()
        (self.site / "key.asc").write_text("key", encoding="utf-8")

    def tearDown(self):
        self.temp.cleanup()

    def test_existing_files_and_external_links_pass(self):
        self.write_page('<a href="key.asc">k</a><a href="https://example.com/x">x</a><a href="#top">t</a>'
                        f'<pre>URIs: {SITE_URL}/apt\ncurl {SITE_URL}/key.asc</pre>')
        self.assertEqual(page.check_links(self.site, SITE_URL), [])

    def test_missing_relative_link_fails(self):
        self.write_page('<a href="rpm/stable/teamcraft.repo">repo</a>')
        self.assertEqual(page.check_links(self.site, SITE_URL), [("index.html", "rpm/stable/teamcraft.repo")])

    def test_missing_site_url_in_a_snippet_fails(self):
        self.write_page(f"<pre>sudo curl -o x {SITE_URL}/rpm/stable/teamcraft.repo</pre>")
        self.assertEqual(len(page.check_links(self.site, SITE_URL)), 1)

    def test_missing_file_in_a_style_sheet_fails(self):
        self.write_page("")
        (self.site / "style.css").write_text('@font-face { src: url("fonts/missing.woff2"); }\n'
                                             'a { background: url(key.asc); }', encoding="utf-8")
        self.assertEqual(page.check_links(self.site, SITE_URL), [("style.css", "fonts/missing.woff2")])

    def test_directory_link_without_index_fails(self):
        self.write_page('<a href="apt/">apt</a>')
        self.assertEqual(len(page.check_links(self.site, SITE_URL)), 1)

    def write_page(self, body):
        (self.site / "index.html").write_text(f"<!doctype html><html><body>{body}</body></html>", encoding="utf-8")


def assets(names, tag="v1.0.1"):
    return [
        {"name": n, "size": 1_000_000, "browser_download_url": f"https://github.com/{REPO}/releases/download/{tag}/{n}"}
        for n in names
    ]


if __name__ == "__main__":
    unittest.main()
