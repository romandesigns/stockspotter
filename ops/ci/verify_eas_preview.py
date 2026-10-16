"""Require a finished, matching-source Android preview and hash its APK."""
import argparse
import hashlib
import json
from pathlib import Path
import re
from urllib.parse import urlsplit
from urllib.request import Request, urlopen
from zipfile import ZipFile

MAX_APK_BYTES = 512 * 1024 * 1024
# Expo's artifact CDN answers 403 to urllib's default User-Agent.
USER_AGENT = "stockspotter-ci/1"


def require_preview(payload, commit, project_id):
    if not re.fullmatch(r"[0-9a-f]{40}", commit):
        raise ValueError("Invalid expected source commit")
    if isinstance(payload, list):
        if len(payload) != 1:
            raise ValueError("Expected exactly one Android build")
        payload = payload[0]
    if not isinstance(payload, dict):
        raise ValueError("Malformed EAS result")
    # eas-cli 24 reports the project as "app" (older releases: "project");
    # either way its id must be exactly this repository's Expo project.
    project = payload.get("app", payload.get("project"))
    if not project_id or not isinstance(project, dict) or project.get("id") != project_id:
        raise ValueError("Unexpected Expo project")
    expected = {"gitCommitHash": commit, "status": "FINISHED", "platform": "ANDROID", "buildProfile": "preview", "distribution": "INTERNAL"}
    for name, value in expected.items():
        if payload.get(name) != value:
            raise ValueError(f"Unexpected EAS {name}")
    artifacts = payload.get("artifacts")
    url = (artifacts.get("applicationArchiveUrl") or artifacts.get("buildUrl")) if isinstance(artifacts, dict) else None
    if not isinstance(url, str) or urlsplit(url).scheme != "https" or not urlsplit(url).hostname:
        raise ValueError("Missing HTTPS APK artifact")
    if not isinstance(payload.get("id"), str) or not payload["id"]:
        raise ValueError("Missing build identity")
    return payload, url


def save_preview(payload, commit, output, project_id):
    build, url = require_preview(payload, commit, project_id)
    output = Path(output)
    apk = output / "android-preview.apk"
    part = output / "android-preview.apk.part"
    receipt = output / "android-preview-receipt.json"
    if any(path.exists() for path in (apk, part, receipt)):
        raise ValueError("Refusing to reuse an existing artifact")
    digest = hashlib.sha256()
    total = 0
    with urlopen(Request(url, headers={"User-Agent": USER_AGENT}), timeout=60) as response, part.open("xb") as target:
        if urlsplit(response.geturl()).scheme != "https":
            raise ValueError("Artifact redirected to an insecure URL")
        while chunk := response.read(1024 * 1024):
            total += len(chunk)
            if total > MAX_APK_BYTES:
                raise ValueError("APK exceeds download limit")
            target.write(chunk)
            digest.update(chunk)
    with part.open("rb") as handle:
        if handle.read(4) != b"PK\x03\x04":
            raise ValueError("Artifact is not an APK ZIP")
    with ZipFile(part) as archive:
        names = set(archive.namelist())
        if "AndroidManifest.xml" not in names or "classes.dex" not in names:
            raise ValueError("Artifact lacks the Android APK structure")
    part.replace(apk)
    receipt.write_text(json.dumps({"sourceCommit": commit, "expoProjectId": project_id, "easBuildId": build["id"], "platform": "android", "profile": "preview", "status": "FINISHED", "sha256": digest.hexdigest(), "bytes": total, "deviceVerified": False}, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("result", type=Path)
    parser.add_argument("--commit", required=True)
    parser.add_argument("--project-id", required=True)
    parser.add_argument("--output-dir", required=True, type=Path)
    args = parser.parse_args()
    save_preview(json.loads(args.result.read_text(encoding="utf-8")), args.commit, args.output_dir, args.project_id)
