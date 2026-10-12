# Changelog

## 2.5.0 (2026-10-12)

### Upgrade notes

- **`<db>.bloom` moves to format version 2** (FTS5's own terms plus their word stems, sized to the data: ~48 KB for a small database instead of a fixed 1.8 MB). The first run rebuilds it once, automatically. Mixed versions sharing one database stay safe: each rejects the other's bloom file and rebuilds it from the database. The database file is unchanged.
- **`recall` returns more.** Word forms match (`houses` finds "house"), and memories with at least half the query words come back after the full matches. A query that returned nothing may now return partial matches.
- **Scores change.** Relevance now grows with match quality (it was inverted). Partial matches have their score scaled by the fraction of words matched. `*` listings report relevance 0, so their scores are 0.4 lower than before; their order is unchanged.
- Library API (`memory39::db`): `RecallResult` has a new public field, `relevance`. Code that builds `RecallResult` values must set it.
- New dependency: `rust-stemmers` (Snowball stemmers, pure Rust).

### Fixed

- Ranking was inverted: FTS5's bm25 rank was mapped so that a stronger match got a lower relevance, and a memory mentioning the query once ranked above the memory about it.
- When more memories matched than the limit, `recall` ranked an arbitrary subset (the oldest rows). Each table now contributes its best bm25 matches.
- Queries with punctuation inside a word (`Alice's`, `e-mail`, `v2.1`) failed with an FTS5 syntax error and returned nothing.
- The 6-letter prefix fallback matched unrelated words (`transportation` found "transparent", `department` found "departure") and never matched forms of short words (`houses`/`house`, `cars`/`car`, `running`/`run`). It is replaced by stem-based word forms.
- A word missing from every memory made the whole query return nothing: `the mobility house` or `mobility house meeting` didn't find "Mobility House GmbH".
- The bloom pre-check could hide memories stored with decomposed accents (`u` followed by U+0308 for `ü`, as macOS often writes them) or with characters FTS5 folds differently (`ſ`), because it tokenized text itself and disagreed with the index. It now holds the index's own terms.
- Queries containing lowercase `and`, `or`, `not`, or the letters `near` (`nearby`) were treated as FTS5 syntax and skipped the fallback. FTS5 operators are uppercase, so these are plain words.

### Changed

- The bloom pre-check now also decides on words longer than 6 letters, and skips multi-word queries where fewer than half the words could match.
- The bloom filter is sized to the data instead of a fixed 600K items at 0.001% false positives: each build holds twice its keys (at least 20K) at 0.01%, the file header counts keys as writes add them, and the write that fills it rebuilds it at double size. Saving it on each write drops from ~0.6 ms to ~0.3 ms, and reloading it after another process wrote from ~0.2-0.3 ms to ~10-25 us.
- Negative `recall` costs ~2.5-4 us instead of ~1.7 us: each word is stemmed in three languages.
- `recall` on very common words in large databases is slower, because ranking scores every matching row (~0.6 us per row). A word in 40% of 50k memories takes ~7 ms, against ~0.4 ms before, when the first rows found were returned unranked.
- `connect` matches word forms of each concept, and a multi-word concept matches its words in any order rather than as an exact phrase. The prefix fallback is gone.
- `recall` help (CLI and MCP tool description) explains the matching tiers and that FTS5 syntax runs as written. The README adds "How recall matches" and "Writing queries", which recommends a decision model in front of memory39 for agent builders.

## 2.1.0 (2026-10-11)

### Upgrade notes

- **The CLI now uses the shared database by default.** Without `--db` it opens `$MEMORY39_DB`, else `~/.memory39/memory39.db` (the same file the MCP server uses), instead of `memory39.db` in the current directory. If you kept memories in a local `memory39.db`, pass `--db memory39.db` to keep using it.
- **`alter` and `forget` exit with status 1 on failure** (rejected change, malformed ID, memory not found, nothing to alter) and print the reason on stderr. They used to exit 0.
- **`alter --time` without `--date` is now an error** in the CLI, as it already was over MCP. It used to print a warning and apply the other fields.
- No on-disk changes. Library API (`memory39::db`) changes are additive: `AlterFields` and `alter_changes`.

### Fixed

- `alter` with `note` on a thing (`T#`) failed with `invalid field: note`. Things have no note column, so `note` now sets the thing's description.
- A thing's or place's description could not be changed once stored. `alter` now takes `desc` (things and places).
- `alter` now covers every field the database stores. New: `category`, `confidence`, `related` (things); `role`, `relationship`, `contact`, `met_at`, `last_seen` (persons, `--met-at` and `--last-seen` in the CLI); `address`, `kind` (places). These were write-once before.
- A field that doesn't apply to the memory's type (`location` on a thing, `date` on a person) failed with a cryptic `Invalid parameter name: invalid field: ...`, sometimes naming a column the caller never used (`datetime`). The error now names the parameter, the memory type and the valid fields; `date` on an undated event also explains how to date it.
- `alter t1 --text ...` (lowercase ID) failed with `invalid field: event`.
- The CLI ignored `MEMORY39_DB` and defaulted to `memory39.db` in the current directory, so the CLI and MCP clients could silently write to different databases.
- Over MCP, bad memory IDs and fields in `alter` and `forget` were reported as internal server errors. They are now `invalid_params`.

### Changed

- TurboMCP 3.0.14 → 3.6.0. Tool errors now reach the client as tool results (`isError: true`) with the message and an `io.turbomcp/errorKind` in `_meta`, instead of JSON-RPC errors, so models can read them and correct the call. Clients on protocol `2025-06-18` are served through version adapters. Includes upstream security fixes.
- The CLI and the MCP server share one `alter` parameter mapping, and each parameter's help names the memory types it applies to.
- `recall` documentation now says that every query word must match and that `OR` matches any of them.
- Builds from source use the same release profile as the published binaries (strip, LTO, single codegen unit, abort on panic).

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
