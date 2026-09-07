"""Create a source-only staging archive without credentials, caches or research outputs."""
import hashlib
import json
from pathlib import Path
import subprocess
import tarfile

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "data/releases/operating-run-2026-09-07"
ALLOWED_ROOTS = {"apps", "packages", "crates", "ops", ".github"}
ALLOWED_FILES = {"Cargo.toml", "Cargo.lock", "package.json", "bun.lock", ".dockerignore", ".gitignore", "docker-compose.yml", "deploy.sh"}


def main():
    files = subprocess.check_output(["git", "ls-files", "-co", "--exclude-standard", "-z"], cwd=ROOT).decode().split("\0")
    selected = []
    for name in sorted(set(files)):
        if not name:
            continue
        path = Path(name)
        if any(part.startswith(".env") or part in {"node_modules", "target", "dist", ".venv", "__pycache__"} for part in path.parts):
            continue
        if (path.parts[0] in ALLOWED_ROOTS or name in ALLOWED_FILES
                or name.startswith(("python/app/", "python/tests/"))
                or name in {"python/Dockerfile", "python/requirements.txt", "python/analyze_discovery.py", "python/test_discovery.py", "python/test_discovery_review.py"}
                or name.startswith("docs/discovery-coverage-")):
            if (ROOT / path).is_file():
                selected.append(name)
    OUT.mkdir(parents=True, exist_ok=True)
    manifest = {"base_commit": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT).decode().strip(),
                "files": {p: hashlib.sha256((ROOT / p).read_bytes()).hexdigest() for p in selected}}
    (OUT / "source-manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    with tarfile.open(OUT / "source.tar.gz", "w:gz") as archive:
        for path in selected:
            archive.add(ROOT / path, arcname=path, recursive=False)
        archive.add(OUT / "source-manifest.json", arcname="source-manifest.json")
    print(json.dumps({"files": len(selected), "archive": str(OUT / "source.tar.gz"),
                      "sha256": hashlib.sha256((OUT / "source.tar.gz").read_bytes()).hexdigest()}))


if __name__ == "__main__":
    main()
