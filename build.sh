#!/usr/bin/env bash
set -euo pipefail

# Static musl linking opens many file descriptors at once; raise the limit.
ulimit -n 4096 2>/dev/null || true

VERSION=$(cargo metadata --no-deps --format-version 1 | python3 -c "import sys,json; print(json.load(sys.stdin)['packages'][0]['version'])")
NAME="memory39"
MCP="mcp"
OUT_DIR="dist"

mkdir -p "$OUT_DIR"

echo "=== Building $NAME v$VERSION ==="

# --- .env.sample ---
cat > "$OUT_DIR/.env.sample" <<'ENVEOF'
DEEPSEEK_API_KEY=
GROQ_API_KEY=
OPENAI_API_KEY=
GEMINI_API_KEY=
ENVEOF
echo "  -> $OUT_DIR/.env.sample"

# build_target LABEL TARGET CARGO_CMD SUFFIX
build_target() {
  local label="$1" target="$2" cmd="$3" suffix="${4:-}"
  echo ""
  echo "--- $label ($target) ---"
  $cmd --release --target "$target"
  cp "target/$target/release/${NAME}${suffix}" "$OUT_DIR/${NAME}-cli-${label}${suffix}"
  cp "target/$target/release/${MCP}${suffix}"  "$OUT_DIR/${NAME}-mcp-${label}${suffix}"
  echo "  -> $OUT_DIR/${NAME}-cli-${label}${suffix}"
  echo "  -> $OUT_DIR/${NAME}-mcp-${label}${suffix}"
}

build_target macos-arm64 aarch64-apple-darwin       "cargo build"
build_target macos-x64   x86_64-apple-darwin        "cargo build"
build_target linux-arm64 aarch64-unknown-linux-musl  "cargo zigbuild"
build_target linux-x64   x86_64-unknown-linux-musl   "cargo zigbuild"
build_target windows-x64 x86_64-pc-windows-msvc      "cargo xwin build" .exe

echo ""
echo "=== Done ==="
ls -lh "$OUT_DIR"/${NAME}-cli-* "$OUT_DIR"/${NAME}-mcp-*
