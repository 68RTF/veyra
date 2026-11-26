#!/usr/bin/env bash
# Assemble the same inputs for all distribution builds, including offline crates.
set -euo pipefail
cd "$(dirname "$0")/.."
version=$(sed -n 's/^version = "\([^"]*\)"$/\1/p' Cargo.toml | head -1)
: "${version:?Missing package version}"
if [[ ${GITHUB_REF_TYPE:-} == tag && ${GITHUB_REF_NAME:-} != "v$version" ]]; then
  echo "Release tag must match Cargo.toml (v$version)" >&2
  exit 1
fi
mkdir -p dist
work=$(mktemp -d dist/.source.XXXXXX)
trap 'rm -rf "$work"' EXIT
name="veyra-$version"
root="$work/$name"
mkdir -p "$root"/{scripts,.cargo,.github,docs}
cp -R Cargo.toml Cargo.lock LICENSE README.md veyra.desktop assets src packaging "$root/"
cp docs/{player.png,veyra-logo.svg,demo-cover.svg} "$root/docs/"
cp -R .github/workflows "$root/.github/"
cp scripts/{build-source.sh,build-arch.sh,build-deb.sh,package-licenses.py,check-package.sh,check-source.py,check-ui.sh,check-lyrics.sh,check-lyrics-search.py,measure-resources.py} "$root/scripts/"
(
  cd "$root"
  cargo vendor --locked --versioned-dirs vendor > .cargo/config.toml
  # The two winapi import crates refer to the workspace licenses but omit
  # them from their registry tarballs. Keep vendor checksums intact.
  for crate in vendor/winapi-*-pc-windows-gnu-*; do
    [[ -d "$crate" ]] || continue
    mkdir -p "source-notices/${crate##*/}"
    cp vendor/winapi-[0-9]*/LICENSE-{MIT,APACHE} "source-notices/${crate##*/}/"
  done
  cargo metadata --frozen --format-version 1 > /dev/null
)
tar --sort=name --mtime=@0 --owner=0 --group=0 --numeric-owner -C "$work" -czf "$work/source.tar.gz" "$name"
python3 scripts/check-source.py "$work/source.tar.gz"
mv "$work/source.tar.gz" "dist/$name-source.tar.gz"
