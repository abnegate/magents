#!/usr/bin/env bash
# Stamp server.json with a release version/image tag and enforce MCP Registry
# field limits (description and title max 100 characters).
# Usage: prepare-mcp-server-json.sh <version> <image-tag>
set -euo pipefail

version="${1:?usage: prepare-mcp-server-json.sh <version> <image-tag>}"
tag="${2:?usage: prepare-mcp-server-json.sh <version> <image-tag>}"
root="${MAGENTS_ROOT:-$(cd "$(dirname "$0")/../.." && pwd)}"

python3 - "$root/server.json" "$version" "$tag" <<'PY'
import json
import re
import sys
from pathlib import Path

path = Path(sys.argv[1])
version = sys.argv[2]
tag = sys.argv[3]
if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+([.-][0-9A-Za-z.-]*)?", version):
    raise SystemExit(f"invalid release version: {version}")
if not tag or any(ch.isspace() for ch in tag):
    raise SystemExit(f"invalid image tag: {tag}")

server = json.loads(path.read_text(encoding="utf-8"))
server["version"] = version
packages = server.get("packages")
if isinstance(packages, list):
    for package in packages:
        if not isinstance(package, dict):
            continue
        identifier = package.get("identifier")
        if isinstance(identifier, str) and ":" in identifier:
            package["identifier"] = re.sub(r":[^:]+$", f":{tag}", identifier)
for field, limit in (("description", 100), ("title", 100)):
    value = server.get(field) or ""
    if not value and field == "title":
        continue
    if not 1 <= len(value) <= limit:
        raise SystemExit(
            f"{path.name} {field} is {len(value)} chars; MCP Registry max is {limit}"
        )
path.write_text(json.dumps(server, indent=2) + "\n", encoding="utf-8")
print(path.read_text(encoding="utf-8"), end="")
PY
