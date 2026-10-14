#!/usr/bin/env bash
# Set every product version from a release tag and refresh Cargo.lock.
# Usage: set-release-version.sh [0.7.0|v0.7.0]
# Version defaults to $RELEASE_TAG (leading v is stripped).
# MAGENTS_ROOT overrides the repository root (tests).
set -euo pipefail

version="${1:-${RELEASE_TAG:-}}"
version="${version#v}"

if [[ ! $version =~ ^[0-9]+\.[0-9]+\.[0-9]+([.-][0-9A-Za-z.-]*)?$ ]]; then
  echo "invalid release version: ${version:-<empty>}" >&2
  echo "pass a semver tag (optional v prefix) or set RELEASE_TAG" >&2
  exit 1
fi

root="${MAGENTS_ROOT:-$(cd "$(dirname "$0")/../.." && pwd)}"

python3 - "$root" "$version" <<'PY'
import re
import sys
from pathlib import Path

root = Path(sys.argv[1])
version = sys.argv[2]
version_field = re.compile(r'("version"\s*:\s*")[^"]*(")')
image_tag = re.compile(r'(ghcr\.io/[^"/]+/magents:)[^"]+')

# Product manifests. Cargo.lock is rewritten by `cargo update -p magents`.
json_files = (
    ("gemini-extension.json", 1, 0),
    ("server.json", 1, 1),
    ("plugins/claude/.claude-plugin/plugin.json", 1, 0),
    ("plugins/codex/.codex-plugin/plugin.json", 1, 0),
    ("plugins/cursor/.cursor-plugin/plugin.json", 1, 0),
    (".cursor-plugin/marketplace.json", 1, 0),
)

toml = root / "Cargo.toml"
text = toml.read_text(encoding="utf-8")
updated, count = re.subn(
    r'(?m)^version = "[^"]+"',
    f'version = "{version}"',
    text,
    count=1,
)
if count != 1:
    raise SystemExit(f"expected one package version in {toml}, found {count}")
if updated != text:
    toml.write_text(updated, encoding="utf-8")
    print(f"set package.version to {version}")
else:
    print(f"package.version already {version}")

for relative, version_count, image_count in json_files:
    path = root / relative
    text = path.read_text(encoding="utf-8")
    updated, found_versions = version_field.subn(rf"\g<1>{version}\2", text)
    updated, found_images = image_tag.subn(rf"\g<1>{version}", updated)
    if found_versions != version_count or found_images != image_count:
        raise SystemExit(
            f"{relative}: expected {version_count} version field(s) and "
            f"{image_count} image tag(s), found {found_versions} and {found_images}"
        )
    if updated != text:
        path.write_text(updated, encoding="utf-8")
        print(f"set {relative} to {version}")
    else:
        print(f"{relative} already {version}")
PY

cd "$root"
cargo update -p magents
