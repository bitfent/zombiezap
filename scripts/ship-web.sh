#!/usr/bin/env bash
# Ship build for the browser client: wasm-release + wasm-opt -Oz, then
# precompressed .br/.gz siblings that zz-server serves automatically
# (ServeDir::precompressed_br().precompressed_gzip()).
#
# Requires: trunk, wasm32 target, binaryen (wasm-opt), brotli.
#
# M19b canaries (fail the ship if the engine was DCE'd):
#   1. ship wasm must be ≥ 15 MB (empty-engine stubs land ~5–6 MB)
#   2. generated JS must call __wbindgen_start (boot entry wired)
#   3. wasm must contain boot canary string zz-boot-entry-v1
set -euo pipefail
cd "$(dirname "$0")/../web"

# Some agent shells export NO_COLOR=1 + FORCE_COLOR=1 together; trunk 0.21
# then passes a bad --no-color flag. Clear both so cargo/trunk stay happy.
unset NO_COLOR FORCE_COLOR CLICOLOR_FORCE || true

trunk build --release   # Trunk.toml maps --release → cargo wasm-release + wasm-opt -Oz

# ── M19b engine-present canaries ────────────────────────────────────────────
MIN_WASM_BYTES=$((15 * 1024 * 1024))
wasm_files=(dist/*_bg.wasm)
if [[ ! -e "${wasm_files[0]}" ]]; then
  echo "ship-web: FAIL — no dist/*_bg.wasm after trunk build" >&2
  exit 1
fi

for wasm in "${wasm_files[@]}"; do
  size=$(wc -c <"$wasm" | tr -d ' ')
  echo "ship-web: $wasm = $size bytes"
  if (( size < MIN_WASM_BYTES )); then
    echo "ship-web: FAIL — wasm is ${size} bytes (< ${MIN_WASM_BYTES})." >&2
    echo "  Engine almost certainly dead-code-eliminated (see M19b / Instant-on-wasm)." >&2
    echo "  A healthy ship wasm is ~20–30 MB raw after wasm-opt -Oz." >&2
    exit 1
  fi
  if ! grep -a -q 'zz-boot-entry-v1' "$wasm"; then
    echo "ship-web: FAIL — boot canary string 'zz-boot-entry-v1' missing from $wasm" >&2
    echo "  run() was not linked; boot entry is unreachable." >&2
    exit 1
  fi
  if ! grep -a -q 'ZombieZap' "$wasm"; then
    echo "ship-web: FAIL — 'ZombieZap' window title missing from $wasm (engine not linked)" >&2
    exit 1
  fi
done

js_files=(dist/zz-client-*.js)
# Exclude precompressed siblings if globs ever pick them up.
js_ok=0
for js in "${js_files[@]}"; do
  [[ -f "$js" ]] || continue
  [[ "$js" == *.br || "$js" == *.gz ]] && continue
  if grep -q '__wbindgen_start' "$js"; then
    js_ok=1
    echo "ship-web: boot export ok — __wbindgen_start in $(basename "$js")"
    break
  fi
done
if (( js_ok != 1 )); then
  echo "ship-web: FAIL — no generated JS exports/calls __wbindgen_start" >&2
  echo "  Trunk/wasm-bindgen boot entry is missing; engine will never run." >&2
  exit 1
fi

shopt -s nullglob
for f in dist/*.wasm dist/*.js dist/*.css dist/index.html; do
  gzip -9 -kf "$f"
  brotli -f -q 11 -o "$f.br" "$f"
done

echo "— dist sizes —"
ls -la dist/*_bg.wasm* | awk '{printf "%10d  %s\n", $5, $9}'
echo "ship-web: OK (size + boot canaries passed)"
