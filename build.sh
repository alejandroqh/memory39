#!/usr/bin/env bash
set -euo pipefail

cargo build --release
mkdir -p dist
cp target/release/memory39 dist/
cp target/release/mcp dist/memory39-mcp
echo "Built: dist/memory39, dist/memory39-mcp"
