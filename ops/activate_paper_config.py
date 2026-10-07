"""Configure the explicitly approved VPS paper run; never prints credentials."""
from pathlib import Path
import os
import re
import secrets
import shutil


def main():
    root = Path("/opt/apps/stockspotter")
    backup = Path("/home/wavystack/stockspotter-stage-20260907/private")
    backup.mkdir(mode=0o700, exist_ok=True)
    env = root / ".env"
    saved = backup / "pre-release.env"
    if not saved.exists():
        shutil.copy2(env, saved)
        saved.chmod(0o600)
    content = env.read_text()
    match = re.search(r"^STOCKSPOTTER_API_TOKEN=(.*)$", content, re.M)
    token = match.group(1).strip().strip("\"'") if match else ""
    if len(token) < 32:
        token = secrets.token_urlsafe(36)
    values = {
        "STOCKSPOTTER_API_TOKEN": token,
        "DISCOVERY_AUDIT_DIR": "/app/data/discovery-audit",
        "IGNITION_UNIVERSE_MODE": "1",
        "AUTO_TRADER_EXECUTION_MODE": "paper",
        "AUTO_TRADER_POSITION_SIZE_USD": "500",
        "AUTO_TRADER_MAX_CONCURRENT_POSITIONS": "4",
    }
    for key, value in values.items():
        line = f"{key}={value}"
        if re.search(rf"^{key}=.*$", content, re.M):
            content = re.sub(rf"^{key}=.*$", lambda _: line, content, flags=re.M)
        else:
            content = content.rstrip() + "\n" + line + "\n"
    temporary = root / ".env.operating-run.tmp"
    temporary.write_text(content)
    temporary.chmod(0o600)
    os.replace(temporary, env)
    access = backup / "web-access-key.txt"
    access.write_text(token + "\n")
    access.chmod(0o600)
    print("Paper/discovery configuration prepared; prior environment backed up; access key stored privately.")


if __name__ == "__main__":
    main()
