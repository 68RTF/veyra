#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
version=$(sed -n 's/^version = "\([^"]*\)"$/\1/p' Cargo.toml | head -1)
distro=${1:-linux}
[[ "$distro" =~ ^[a-zA-Z0-9.-]+$ ]] || { echo 'Invalid distro name' >&2; exit 1; }
arch=$(dpkg --print-architecture)
if [[ $# -lt 2 ]]; then
  bash scripts/build-source.sh
fi
archive=$(realpath "${2:-dist/veyra-$version-source.tar.gz}")
digest=$(sha256sum < "$archive" | cut -d' ' -f1)
mkdir -p dist
out="$PWD/dist"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
tar -C "$work" -xzf "$archive"
cd "$work/veyra-$version"
export CARGO_TARGET_DIR="$work/target"
cargo build --release --frozen --bins --message-format=json-render-diagnostics > "$work/artifacts.jsonl"
root="$work/pkg"
doc="$root/usr/share/doc/veyra"
python3 scripts/package-licenses.py "$work/artifacts.jsonl" "$doc" "$digest" deb
install -Dm755 "$CARGO_TARGET_DIR/release/veyra" "$root/usr/bin/veyra"
install -Dm755 "$CARGO_TARGET_DIR/release/ym-export" "$root/usr/bin/ym-export"
install -Dm644 veyra.desktop "$root/usr/share/applications/veyra.desktop"
install -Dm644 assets/veyra.svg "$root/usr/share/icons/hicolor/scalable/apps/veyra.svg"
mkdir -p "$root/DEBIAN"
cat > "$root/DEBIAN/control" <<CONTROL
Package: veyra
Version: $version
Section: sound
Priority: optional
Architecture: $arch
Maintainer: 68RTF
Depends: libgtk-3-0t64, mpv, ffmpeg, curl
Conflicts: own-player
Description: Local music player for Linux
 Local audio player with a queue, lyrics and optional online artwork.
CONTROL
dpkg-deb --build --root-owner-group "$root" "$out/veyra_${version}_${distro}_${arch}.deb"
