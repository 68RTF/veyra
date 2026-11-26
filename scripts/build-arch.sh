#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
version=$(sed -n 's/^version = "\([^"]*\)"$/\1/p' Cargo.toml | head -1)
if [[ $# == 0 ]]; then
  bash scripts/build-source.sh
fi
archive=$(realpath "${1:-dist/veyra-$version-source.tar.gz}")
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
file="veyra-$version-source.tar.gz"
ln -s "$archive" "$work/$file"
tar -xOf "$archive" "veyra-$version/packaging/arch/PKGBUILD" > "$work/PKGBUILD"
export VEYRA_SOURCE_SHA256=$(sha256sum < "$archive" | cut -d' ' -f1)
mkdir -p dist
export PKGDEST="$PWD/dist"
(cd "$work" && makepkg --clean --force --nodeps)
