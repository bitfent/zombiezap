#!/usr/bin/env bash
# Ship build for the browser client: wasm-release + wasm-opt -Oz, then
# precompressed .br/.gz siblings that zz-server serves automatically
# (ServeDir::precompressed_br().precompressed_gzip()).
#
# Requires: trunk, wasm32 target, binaryen (wasm-opt), brotli.
set -euo pipefail
cd "$(dirname "$0")/../web"

trunk build --release   # Trunk.toml maps --release → cargo wasm-release + wasm-opt -Oz

shopt -s nullglob
for f in dist/*.wasm dist/*.js dist/*.css dist/index.html; do
  gzip -9 -kf "$f"
  brotli -f -q 11 -o "$f.br" "$f"
done

echo "— dist sizes —"
ls -la dist/*_bg.wasm* | awk '{printf "%10d  %s\n", $5, $9}'
