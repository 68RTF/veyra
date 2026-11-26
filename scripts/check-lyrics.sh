#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
if [[ ${1:-} == --search ]]; then
  shift
  cargo build --release --locked --bin veyra
  exec python3 scripts/check-lyrics-search.py "$@"
elif [[ $# == 0 ]]; then
  xvfb-run -a cargo test --locked gtk_lyric_timing_probe -- --ignored --test-threads=1 --nocapture
else
  cargo build --release --locked --bin veyra
  target/release/veyra lyrics-check "$@"
fi
