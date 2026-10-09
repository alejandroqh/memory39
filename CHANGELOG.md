# Changelog

## 2.0.0 (2026-10-09)

### Upgrade notes

- **On-disk changes.** The first run of 2.0.0 adds a `write_gen` table and INSERT/UPDATE triggers to the database, and rewrites `<db>.bloom` in a new headered format (the old file is rebuilt once, automatically). No data migration and no manual steps.
- **Restart every MCP client** after installing, so no long-running 1.x server keeps serving a stale bloom filter. Mixed versions sharing one database stay safe: each rejects the other's bloom file and rebuilds it from the database.
- Library API (`memory39::db`) is unchanged.

### Fixed

- `recall` no longer misses memories on multi-word queries. The bloom pre-check required the query's adjacent word pairs to appear together, but FTS5 matches words in any order and any field, so queries like `shop coffee`, `alice berlin` (words in different fields) or `desarrollando proyecto` (prefix fallback) returned nothing. Bigrams are gone; the pre-check now skips FTS5 only when a short query word appears in no memory.
- `recall` no longer misses memories written by another process. A running MCP server never saw writes from the CLI or other MCP clients, and memories stored through MCP were lost from `<db>.bloom` when the client killed the server (the file was only saved on clean exit). The bloom file is now saved on every write and validated against a write counter kept by SQLite triggers (new `write_gen` table), so stale files are reloaded or rebuilt automatically. Existing `.bloom` files are rebuilt once on upgrade.

### Changed

- Negative `recall` now costs ~1.7 us instead of ~150 ns: each call reads the write counter to make sure the bloom filter is current.

## 1.0.3 (2026-04-20)

### Removed

- `ingest` subcommand and all LLM integration. memory39 now runs offline with no network, API keys, or `.env` file. The `--llm` and `--model` CLI flags are gone; the `llm` module and `reqwest` dependency have been removed.

### Changed

- MCP server resolves the DB path via a shared helper with precedence `--db` > `MEMORY39_DB` (supports leading `~/`) > `~/.memory39/memory39.db`, and creates the parent directory on first run.
- README reorganized around persistence, cross-client shared knowledge base, and zero external dependencies.

## 1.0.1 — 2026-04-16

First public release. Temporal-priority memory system for AI agents.

### Features

- **10 CLI subcommands** — `ingest`, `event`, `thing`, `person`, `place`, `forget`, `alter`, `recall`, `connect`, `mcp`
- **MCP server** — built-in via `memory39 mcp` (STDIO transport, TurboMCP). 8 tools: `recall`, `event`, `thing`, `person`, `place`, `forget`, `alter`, `connect`
- **Unified binary** — single binary serves both CLI and MCP modes
- **LLM-driven ingestion** — conversation chunking with iterative tool-calling loop (up to 10 rounds/chunk). Supports DeepSeek, Groq, OpenAI, Gemini, Ollama
- **5 memory types** — events (dated `E#`), events undated (`U#`), things (`T#`), persons (`P#`), places (`L#`)
- **Composite scoring** — `0.4×relevance + 0.3×importance + 0.3×recency` with 30-day half-life
- **3-phase connection discovery** — direct FTS AND, shared field values, one-hop bridge through tags/emotion/location/people
- **Bloom filter** — pre-check layer before FTS5 queries. Unigram + bigram tokens, unicode-normalized, prefix-safe. Persisted to `<db>.bloom`, auto-rebuilt after ingest. 600K items at 0.001% FP rate
- **SQLite + FTS5** — WAL journal mode, 64MB mmap. 5 main tables with companion FTS5 virtual tables synced via triggers. Expression indices on date substrings and importance
- **Memory ID system** — prefix + rowid (`E3`, `T12`, `P1`) for unified cross-table `forget` and `alter`
- **Cross-compilation** — build script for macOS arm64/x64, Linux arm64/x64 (musl), Windows x64 (MSVC)
- **Benchmark adapter** — `bench/memory39_provider.py` for the Agent Memory Benchmark framework
