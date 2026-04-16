# Changelog

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
