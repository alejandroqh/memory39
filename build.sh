#!/usr/bin/env bash
set -euo pipefail

# Static musl linking opens many file descriptors at once; raise the limit.
ulimit -n 4096 2>/dev/null || true

NAME="memory39"
VERSION=$(cargo metadata --no-deps --format-version 1 | python3 -c "import sys,json; d=json.load(sys.stdin); print(next(p['version'] for p in d['packages'] if p['name']=='$NAME'))")
# Cargo's target dir may not be ./target (e.g. in a workspace); copy from where it really builds.
TARGET_DIR=$(cargo metadata --no-deps --format-version 1 | python3 -c "import sys,json; print(json.load(sys.stdin)['target_directory'])")
OUT_DIR="dist"

mkdir -p "$OUT_DIR"

echo "=== Building $NAME v$VERSION ==="

# macOS ARM64 (native)
echo ""
echo "--- macOS arm64 (aarch64-apple-darwin) ---"
cargo build --release --target aarch64-apple-darwin
cp "$TARGET_DIR"/aarch64-apple-darwin/release/$NAME "$OUT_DIR/${NAME}-macos-arm64"
echo "  -> $OUT_DIR/${NAME}-macos-arm64"
# Guard against shipping a stale binary: the native build must report this version.
BUILT=$("$OUT_DIR/${NAME}-macos-arm64" --version)
if [ "$BUILT" != "$NAME $VERSION" ]; then
    echo "error: built binary reports '$BUILT', expected '$NAME $VERSION'" >&2
    exit 1
fi

# macOS x86_64 (cross-compile)
echo ""
echo "--- macOS x64 (x86_64-apple-darwin) ---"
cargo build --release --target x86_64-apple-darwin
cp "$TARGET_DIR"/x86_64-apple-darwin/release/$NAME "$OUT_DIR/${NAME}-macos-x64"
echo "  -> $OUT_DIR/${NAME}-macos-x64"

# Linux ARM64 (cross-compile via zigbuild, static musl)
echo ""
echo "--- Linux arm64 (aarch64-unknown-linux-musl) ---"
cargo zigbuild --release --target aarch64-unknown-linux-musl
cp "$TARGET_DIR"/aarch64-unknown-linux-musl/release/$NAME "$OUT_DIR/${NAME}-linux-arm64"
echo "  -> $OUT_DIR/${NAME}-linux-arm64"

# Linux x86_64 (cross-compile via zigbuild, static musl)
echo ""
echo "--- Linux x64 (x86_64-unknown-linux-musl) ---"
cargo zigbuild --release --target x86_64-unknown-linux-musl
cp "$TARGET_DIR"/x86_64-unknown-linux-musl/release/$NAME "$OUT_DIR/${NAME}-linux-x64"
echo "  -> $OUT_DIR/${NAME}-linux-x64"

# Windows x86_64 (cross-compile via cargo-xwin, MSVC target)
echo ""
echo "--- Windows x64 (x86_64-pc-windows-msvc) ---"
cargo xwin build --release --target x86_64-pc-windows-msvc
cp "$TARGET_DIR"/x86_64-pc-windows-msvc/release/$NAME.exe "$OUT_DIR/${NAME}-windows-x64.exe"
echo "  -> $OUT_DIR/${NAME}-windows-x64.exe"

echo ""
echo "=== Done ==="
ls -lh "$OUT_DIR"/${NAME}-*
