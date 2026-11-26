#!/usr/bin/env bash
# Inspect the finished archive, not its staging directory.
set -euo pipefail
package=$(realpath "$1")
source_archive=$(realpath "$2")
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
case "$package" in
  *.deb)
    dpkg-deb -x "$package" "$work"
    doc="$work/usr/share/doc/veyra"
    grep -Fxq 'License: GPL-3.0-only' "$doc/copyright"
    ;;
  *.pkg.tar.*)
    bsdtar -xf "$package" -C "$work"
    grep -Fxq 'license = GPL-3.0-only' "$work/.PKGINFO"
    doc="$work/usr/share/licenses/veyra"
    ;;
  *) echo "Unknown package format: $package" >&2; exit 1 ;;
esac
version=$(basename "$source_archive" -source.tar.gz)
tar -xOf "$source_archive" "$version/LICENSE" | cmp - "$doc/LICENSE"
digest=$(sha256sum < "$source_archive" | cut -d' ' -f1)
grep -Fxq "Archive SHA-256: $digest" "$doc/SOURCE"
grep -Fxq "Release source: https://github.com/68RTF/veyra/releases/download/v${version#veyra-}/$version-source.tar.gz" "$doc/SOURCE"
test -x "$work/usr/bin/veyra"
test -x "$work/usr/bin/ym-export"
test -s "$doc/third-party/INDEX"
test -n "$(find "$doc/third-party/rust-stdlib" -type f -size +0c -print -quit)"
while IFS=: read -r crate expression; do
  test -n "$expression"
  test -n "$(find "$doc/third-party/$crate" -type f -size +0c -print -quit)"
done < <(tail -n +2 "$doc/third-party/INDEX")
