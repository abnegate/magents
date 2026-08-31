#!/usr/bin/env bash
# Set [package].version from a release tag and refresh Cargo.lock.
# Usage: set-release-version.sh [0.7.0|v0.7.0]
# Version defaults to $RELEASE_TAG (leading v is stripped).
set -euo pipefail

version="${1:-${RELEASE_TAG:-}}"
version="${version#v}"

if [[ ! $version =~ ^[0-9]+\.[0-9]+\.[0-9]+([.-][0-9A-Za-z.-]*)?$ ]]; then
  echo "invalid release version: ${version:-<empty>}" >&2
  echo "pass a semver tag (optional v prefix) or set RELEASE_TAG" >&2
  exit 1
fi

root="$(cd "$(dirname "$0")/../.." && pwd)"
toml="$root/Cargo.toml"

python3 - "$toml" "$version" <<'PY'
import re
import sys

path, version = sys.argv[1], sys.argv[2]
text = open(path, encoding="utf-8").read()
updated, count = re.subn(
    r'(?m)^version = "[^"]+"',
    f'version = "{version}"',
    text,
    count=1,
)
if count != 1:
    raise SystemExit(f"expected one package version in {path}, found {count}")
if updated == text:
    print(f"package.version already {version}")
else:
    open(path, "w", encoding="utf-8").write(updated)
    print(f"set package.version to {version}")
PY

cd "$root"
cargo update -p magents
