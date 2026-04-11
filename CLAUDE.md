# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

memory39 — temporal-priority memory system for AI agents. Rust CLI backed by SQLite + FTS5. LLM-driven fact extraction from conversations via OpenAI-compatible tool-calling APIs (deepseek, groq, openai, ollama).

## Build & Run

```bash
cargo build --release          # release binaries → target/release/{memory39,mcp}
cargo build                    # debug build
cargo check                    # type-check without codegen
cargo clippy                   # lint
./build.sh                     # release build → dist/{memory39,memory39-mcp}
```

No cargo test suite exists yet. Primary validation is via the Agent Memory Benchmark:

```bash
./bench/run_bench.sh                                    # default: personamem, 32k split
./bench/run_bench.sh --all                              # all benchmark documents
./bench/run_bench.sh --dataset locomo --split 32k       # specific dataset
./bench/run_bench.sh --llm ollama --model llama2.5-thinking  # specific LLM
```

## Architecture

Library (`src/lib.rs`) + two binaries:

- **`src/main.rs`** — Clap CLI with 10 subcommands (`ingest`, `event`, `thing`, `person`, `place`, `forget`, `alter`, `recall`, `connect`). Routes to db/llm. Async tokio runtime.
- **`src/bin/mcp.rs`** — MCP server (via TurboMCP) exposing all non-LLM tools over STDIO. 8 tools: `recall`, `event`, `thing`, `person`, `place`, `forget`, `alter`, `connect`. DB path via `MEMORY39_DB` env var (default `memory39.db`).
- **`src/db.rs`** — All SQLite operations. Schema creation, CRUD, FTS5 search, composite scoring, connection discovery. ~1250 lines.
- **`src/llm.rs`** — LLM API integration. Conversation chunking (splits on actor switches), iterative tool-calling loop (up to 10 rounds per chunk), provider presets.

### Data flow

**Ingest:** stdin → `llm::ingest_conversation()` → chunks conversation → LLM tool-calling loop → `Vec<MemoryAction>` → `db::insert_*/alter/forget` → prints summary.

**Recall:** query + filters → `db::recall()` → searches 5 FTS tables → composite scoring (0.4×relevance + 0.3×importance + 0.3×recency with 30-day half-life) → sorted results.

**Connect:** 2-3 concepts → 3-phase discovery: (1) direct FTS AND query, (2) shared field values across separate searches, (3) one-hop bridge through linkable fields (tags, emotion, location, people).

### Memory ID system

Prefix + rowid uniquely identifies any memory across tables: `E`=events, `U`=undated events, `T`=things, `P`=persons, `L`=places. Used by `forget` and `alter` for unified cross-table operations.

### Database

Single `memory39.db` file. WAL journal mode, 64MB mmap. Five main tables (events, events_undated, things, persons, places), each with a companion FTS5 virtual table kept in sync via INSERT/UPDATE/DELETE triggers. Expression indices on date substrings and importance columns.

## MCP Server

`src/bin/mcp.rs` exposes memory39's database tools as an MCP server using [TurboMCP](https://github.com/Epistates/turbomcp). STDIO transport, MCP protocol version 2025-11-25.

Configure in Claude Desktop or any MCP client:
```json
{
  "mcpServers": {
    "memory39": {
      "command": "/path/to/mcp",
      "env": { "MEMORY39_DB": "/path/to/memory39.db" }
    }
  }
}
```

The `ingest` command (LLM-driven) is intentionally excluded — only direct db operations are exposed via MCP.

## Environment

CLI requires `.env` with API keys (`DEEPSEEK_API_KEY`, `GROQ_API_KEY`, `OPENAI_API_KEY`, `GEMINI_API_KEY`). Loaded via `dotenv` at startup. The MCP server does not need API keys (no LLM calls).

## Benchmark adapter

`bench/memory39_provider.py` wraps the CLI binary for the Agent Memory Benchmark framework. Passes documents via stdin, parses `recall` output back into Document objects. Fresh DB per benchmark run.
