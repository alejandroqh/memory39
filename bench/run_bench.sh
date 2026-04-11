#!/usr/bin/env bash
# run_bench.sh — Run Agent Memory Benchmark against memory39
#
# Usage:
#   ./bench/run_bench.sh                          # one document, personamem 32k
#   ./bench/run_bench.sh --all                    # all documents
#   ./bench/run_bench.sh --dataset locomo --split 32k
#   ./bench/run_bench.sh --llm ollama --model lfm2.5-thinking
#   ./bench/run_bench.sh --query-limit 5          # only 5 queries

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
AMB_DIR="$PROJECT_DIR/agent-memory-benchmark"
VENV_DIR="$AMB_DIR/.venv"

# Defaults
DATASET="personamem"
SPLIT="32k"
QUERY_LIMIT="1"
LLM="deepseek"
MODEL=""
EXTRA_ARGS=()

# Parse args
while [[ $# -gt 0 ]]; do
    case "$1" in
        --all)       QUERY_LIMIT=""; shift ;;
        --dataset)   DATASET="$2"; shift 2 ;;
        --split)     SPLIT="$2"; shift 2 ;;
        --llm)       LLM="$2"; shift 2 ;;
        --model)     MODEL="$2"; shift 2 ;;
        --query-limit) QUERY_LIMIT="$2"; shift 2 ;;
        *)           EXTRA_ARGS+=("$1"); shift ;;
    esac
done

echo "=== memory39 benchmark ==="
echo "  dataset:     $DATASET"
echo "  split:       $SPLIT"
echo "  llm:         $LLM${MODEL:+ ($MODEL)}"
echo "  query-limit: ${QUERY_LIMIT:-all}"
echo ""

# Step 1: Build memory39
echo "--- Building memory39 ---"
cd "$PROJECT_DIR"
cargo build --release 2>&1 | tail -1
MEMORY39_BIN="$PROJECT_DIR/target/release/memory39"

# Step 2: Clone AMB if needed
if [ ! -d "$AMB_DIR" ]; then
    echo "--- Cloning agent-memory-benchmark ---"
    git clone --depth 1 https://github.com/vectorize-io/agent-memory-benchmark.git "$AMB_DIR"
fi

# Step 3: Set up venv if needed
if [ ! -d "$VENV_DIR" ]; then
    echo "--- Creating Python venv ---"
    python3 -m venv "$VENV_DIR"
fi
source "$VENV_DIR/bin/activate"
if ! python3 -c "import memory_bench" 2>/dev/null; then
    echo "--- Installing AMB dependencies ---"
    pip install -e "$AMB_DIR" 2>&1 | tail -3
fi

# Step 4: Register memory39 provider
# Copy adapter into AMB's memory providers directory
cp "$SCRIPT_DIR/memory39_provider.py" "$AMB_DIR/src/memory_bench/memory/memory39.py"

# Patch registry if memory39 not yet registered
INIT_FILE="$AMB_DIR/src/memory_bench/memory/__init__.py"
if ! grep -q "memory39" "$INIT_FILE"; then
    echo "--- Registering memory39 provider ---"
    python3 -c "
import re
f = '$INIT_FILE'
txt = open(f).read()
# Add import after base import
txt = txt.replace(
    'from .base import MemoryProvider',
    'from .base import MemoryProvider\nfrom .memory39 import Memory39MemoryProvider'
)
# Add to registry
txt = txt.replace(
    '\"supermemory\": SupermemoryMemoryProvider,',
    '\"supermemory\": SupermemoryMemoryProvider,\n    \"memory39\": Memory39MemoryProvider,'
)
open(f, 'w').write(txt)
"
fi

# Step 5: Export env vars
export MEMORY39_BIN="$MEMORY39_BIN"
export MEMORY39_LLM="$LLM"
export MEMORY39_MODEL="${MODEL:-}"
# Load GEMINI_API_KEY from project .env for AMB's judge/answer LLM
if [ -f "$PROJECT_DIR/.env" ]; then
    GEMINI_KEY=$(grep '^GEMINI_API_KEY=' "$PROJECT_DIR/.env" | cut -d= -f2-)
    if [ -n "$GEMINI_KEY" ]; then
        export GEMINI_API_KEY="$GEMINI_KEY"
    fi
fi

# Step 6: Run benchmark
echo ""
echo "--- Running benchmark ---"
cd "$AMB_DIR"

CMD=(python -m memory_bench run --memory memory39 --dataset "$DATASET" --split "$SPLIT")
if [ -n "$QUERY_LIMIT" ]; then
    CMD+=(--query-limit "$QUERY_LIMIT")
fi
if [ ${#EXTRA_ARGS[@]} -gt 0 ]; then
    CMD+=("${EXTRA_ARGS[@]}")
fi

echo "$ ${CMD[*]}"
echo ""
"${CMD[@]}"

echo ""
echo "=== Done ==="
