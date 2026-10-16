import hashlib
import importlib.util
import io
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
from zipfile import ZipFile

SPEC = importlib.util.spec_from_file_location("verify_eas_preview", Path(__file__).with_name("verify_eas_preview.py"))
assert SPEC and SPEC.loader
preview = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(preview)
SHA = "a" * 40


def result():
    return {"id": "fixture-build", "project": {"id": "fixture-project"}, "gitCommitHash": SHA, "status": "FINISHED", "platform": "ANDROID", "buildProfile": "preview", "distribution": "INTERNAL", "artifacts": {"buildUrl": "https://example.invalid/fixture.apk"}}


class Response(io.BytesIO):
    def geturl(self):
        return "https://example.invalid/fixture.apk"


class EASPreviewTests(unittest.TestCase):
    def test_modern_artifact_url_and_project_binding(self):
        modern = {**result(), "artifacts": {"applicationArchiveUrl": "https://example.invalid/new.apk"}}
        self.assertEqual(preview.require_preview(modern, SHA, "fixture-project")[1], "https://example.invalid/new.apk")
        for project in (None, {}, {"id": "another-project"}):
            with self.assertRaises(ValueError):
                preview.require_preview({**result(), "project": project}, SHA, "fixture-project")

    def test_eas_cli_24_reports_the_project_as_app(self):
        current = {k: v for k, v in result().items() if k != "project"}
        current["app"] = {"id": "fixture-project", "slug": "fixture"}
        self.assertEqual(preview.require_preview(current, SHA, "fixture-project")[0]["id"], "fixture-build")
        for app in (None, {}, {"id": "another-project"}):
            with self.assertRaises(ValueError):
                preview.require_preview({**current, "app": app}, SHA, "fixture-project")
        # An "app" naming another project is not rescued by a matching "project".
        with self.assertRaises(ValueError):
            preview.require_preview({**result(), "app": {"id": "another-project"}}, SHA, "fixture-project")

    def test_app_bundle_zip_is_not_an_apk(self):
        content = io.BytesIO()
        with ZipFile(content, "w") as archive:
            archive.writestr("base/manifest/AndroidManifest.xml", b"fixture")
            archive.writestr("base/dex/classes.dex", b"fixture")
        with tempfile.TemporaryDirectory() as directory, patch.object(preview, "urlopen", return_value=Response(content.getvalue())):
            with self.assertRaises(ValueError):
                preview.save_preview(result(), SHA, directory, "fixture-project")
            self.assertFalse((Path(directory) / "android-preview.apk").exists())

    def test_wrong_source_profile_platform_or_unfinished_build_refused(self):
        for field, value in (("gitCommitHash", "b" * 40), ("status", "IN_PROGRESS"), ("platform", "IOS"), ("buildProfile", "production"), ("distribution", "STORE"), ("id", "")):
            with self.subTest(field=field), self.assertRaises(ValueError):
                preview.require_preview({**result(), field: value}, SHA, "fixture-project")
        for payload in (None, [], [result(), result()], {}, {**result(), "artifacts": None}, {**result(), "artifacts": {"buildUrl": "http://example.invalid/a.apk"}}):
            with self.subTest(payload=payload), self.assertRaises(ValueError):
                preview.require_preview(payload, SHA, "fixture-project")

    def test_matching_build_download_is_hashed_and_not_device_verified(self):
        content = io.BytesIO()
        with ZipFile(content, "w") as archive:
            archive.writestr("AndroidManifest.xml", b"fixture manifest")
            archive.writestr("classes.dex", b"fixture dex")
        data = content.getvalue()
        with tempfile.TemporaryDirectory() as directory, patch.object(preview, "urlopen", return_value=Response(data)):
            preview.save_preview([result()], SHA, directory, "fixture-project")
            import json
            receipt = json.loads((Path(directory) / "android-preview-receipt.json").read_text())
            self.assertEqual(receipt["sha256"], hashlib.sha256(data).hexdigest())
            self.assertEqual(receipt["sourceCommit"], SHA)
            self.assertFalse(receipt["deviceVerified"])
            self.assertEqual((Path(directory) / "android-preview.apk").read_bytes(), data)
            with self.assertRaises(ValueError):
                preview.save_preview(result(), SHA, directory, "fixture-project")

    def test_empty_wrong_format_and_oversized_download_do_not_become_artifacts(self):
        for data, maximum in ((b"", 100), (b"error page", 100), (b"PK\x03\x04too-large", 4)):
            with tempfile.TemporaryDirectory() as directory, patch.object(preview, "urlopen", return_value=Response(data)), patch.object(preview, "MAX_APK_BYTES", maximum):
                with self.assertRaises(ValueError):
                    preview.save_preview(result(), SHA, directory, "fixture-project")
                self.assertFalse((Path(directory) / "android-preview.apk").exists())
                self.assertFalse((Path(directory) / "android-preview-receipt.json").exists())


if __name__ == "__main__":
    unittest.main()
