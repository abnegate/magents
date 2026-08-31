#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 4 ]]; then
  echo "usage: package-deb.sh <binary> <version> <arch> <outdir>" >&2
  exit 2
fi

binary=$1
version=$2
arch=$3
outdir=$4

if [[ ! -f $binary ]]; then
  echo "missing binary: $binary" >&2
  exit 1
fi

workdir=$(mktemp -d)
trap 'rm -rf "$workdir"' EXIT

pkg="magents_${version}-1_${arch}"
root="$workdir/$pkg"
mkdir -p "$root/DEBIAN" "$root/usr/bin"
cp "$binary" "$root/usr/bin/magents"
chmod 755 "$root/usr/bin/magents"

size_kb=$(du -sk "$root/usr" | awk '{print $1}')

cat >"$root/DEBIAN/control" <<EOF
Package: magents
Version: ${version}-1
Section: utils
Priority: optional
Architecture: ${arch}
Maintainer: Jake Barnby <jakeb994@gmail.com>
Installed-Size: ${size_kb}
Homepage: https://github.com/abnegate/magents
Description: Shared session bus for Claude Code, Codex, Cursor, Grok, and OpenCode
 MCP server and CLI so one agent can continue another agent's session.
EOF

mkdir -p "$outdir"
dpkg-deb --build --root-owner-group "$root" "$outdir/${pkg}.deb"
