#!/usr/bin/env bash
# Set every product version from a release tag and refresh Cargo.lock.
# Discovers tracked files that already carry the crate version, plus
# plugin / marketplace / Gemini / server manifests with a version field,
# so a new host package does not need a script edit.
# Usage: set-release-version.sh [0.7.0|v0.7.0]
# Version defaults to $RELEASE_TAG (leading v is stripped).
# MAGENTS_ROOT overrides the repository root (tests).
# RELEASE_VERSION_STAGING, when set, receives a copy of every version file
# (for the CI artifact). RELEASE_VERSION_LIST optionally receives the path list.
set -euo pipefail

version="${1:-${RELEASE_TAG:-}}"
version="${version#v}"

if [[ ! $version =~ ^[0-9]+\.[0-9]+\.[0-9]+([.-][0-9A-Za-z.-]*)?$ ]]; then
  echo "invalid release version: ${version:-<empty>}" >&2
  echo "pass a semver tag (optional v prefix) or set RELEASE_TAG" >&2
  exit 1
fi

root="${MAGENTS_ROOT:-$(cd "$(dirname "$0")/../.." && pwd)}"

created_list=0
list="${RELEASE_VERSION_LIST:-}"
if [[ -n "${RELEASE_VERSION_STAGING:-}" && -z "$list" ]]; then
  list="$(mktemp)"
  created_list=1
fi
export RELEASE_VERSION_LIST="${list:-}"

python3 - "$root" "$version" <<'PY'
import os
import re
import subprocess
import sys
from pathlib import Path

root = Path(sys.argv[1])
new_version = sys.argv[2]
skip_dirs = {".git", "target", "node_modules", ".magents-pr-work"}
manifest_names = {
    "plugin.json",
    "marketplace.json",
    "gemini-extension.json",
    "server.json",
}
version_field = re.compile(r'("version"\s*:\s*")[^"]*(")')
image_tag = re.compile(r'(ghcr\.io/[^"/]+/magents:)[^"]+')
package_version_line = re.compile(r'(?m)^version = "[^"]+"')


def package_version(text: str) -> str:
    for line in text.splitlines():
        prefix = 'version = "'
        if line.startswith(prefix) and line.endswith('"'):
            return line[len(prefix) : -1]
    raise SystemExit(f"expected one package version in {root / 'Cargo.toml'}")


def repo_files() -> list[Path]:
    # Only trust git when this directory is the work-tree root. Walking
    # upward would let a fixture inside another checkout rewrite that repo.
    if (root / ".git").exists():
        try:
            toplevel = (
                subprocess.check_output(
                    ["git", "-C", str(root), "rev-parse", "--show-toplevel"],
                    stderr=subprocess.DEVNULL,
                )
                .decode()
                .strip()
            )
            if Path(toplevel).resolve() == root.resolve():
                out = subprocess.check_output(
                    ["git", "-C", str(root), "ls-files", "-z"],
                    stderr=subprocess.DEVNULL,
                )
                return [root / path for path in out.decode().split("\0") if path]
        except (OSError, subprocess.CalledProcessError):
            pass
    found: list[Path] = []
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames[:] = [name for name in dirnames if name not in skip_dirs]
        found.extend(Path(dirpath) / name for name in filenames)
    return found


toml = root / "Cargo.toml"
text = toml.read_text(encoding="utf-8")
old_version = package_version(text)
updated, count = package_version_line.subn(
    f'version = "{new_version}"',
    text,
    count=1,
)
if count != 1:
    raise SystemExit(f"expected one package version in {toml}, found {count}")
if updated != text:
    toml.write_text(updated, encoding="utf-8")
    print(f"set package.version to {new_version}")
else:
    print(f"package.version already {new_version}")

version_files = ["Cargo.toml", "Cargo.lock"]
for path in repo_files():
    relative = path.relative_to(root).as_posix()
    if relative in {"Cargo.toml", "Cargo.lock"}:
        continue
    try:
        text = path.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError):
        continue

    is_manifest = path.name in manifest_names
    carries_old = old_version in text
    if not is_manifest and not carries_old:
        continue

    updated = text
    if is_manifest:
        updated, found_versions = version_field.subn(
            rf"\g<1>{new_version}\2", updated
        )
        updated, found_images = image_tag.subn(rf"\g<1>{new_version}", updated)
        if found_versions == 0 and found_images == 0 and not carries_old:
            continue
    elif old_version != new_version:
        updated = updated.replace(old_version, new_version)

    if updated != text:
        path.write_text(updated, encoding="utf-8")
        print(f"set {relative} to {new_version}")
    else:
        print(f"{relative} already {new_version}")
    version_files.append(relative)

list_path = os.environ.get("RELEASE_VERSION_LIST")
if list_path:
    Path(list_path).write_text(
        "\n".join(dict.fromkeys(version_files)) + "\n",
        encoding="utf-8",
    )
PY

cd "$root"
cargo update -p magents

if [[ -n "${RELEASE_VERSION_STAGING:-}" ]]; then
  mkdir -p "$RELEASE_VERSION_STAGING"
  while IFS= read -r file; do
    [[ -n "$file" ]] || continue
    mkdir -p "$RELEASE_VERSION_STAGING/$(dirname -- "$file")"
    cp "$root/$file" "$RELEASE_VERSION_STAGING/$file"
  done < "$RELEASE_VERSION_LIST"
  if [[ "$created_list" -eq 1 ]]; then
    rm -f "$list"
  fi
fi
