# memory39

Temporal-priority memory system for AI agents. Rust CLI + MCP server backed by SQLite + FTS5.

Memories are scored by **0.4 x relevance + 0.3 x importance + 0.3 x recency** (30-day half-life), so recent important matches surface first.

## Why memory39

- **Persistent across sessions**: memories live in an on-disk SQLite file and survive restarts, CLI invocations, and MCP reconnects. 
- **One knowledge base across every MCP client**: Claude Code, Claude Desktop, Codex, OpenCode, and OpenClaw all point at the same `~/.memory39/memory39.db`. A fact ingested from Claude is instantly recallable from Codex; a person stored via the CLI shows up in every MCP client. No syncing, no duplication.
- **Local and private**: no cloud, no account, no telemetry. Your memory is a single SQLite file on your machine that you can inspect, back up, or move by copying.
- **Single binary, zero daemon**: CLI for scripting (`memory39 recall ...`), MCP server on demand (`memory39 mcp`). Nothing runs in the background between calls.
- **Portable DB path**: point at a different database by exporting `MEMORY39_DB=/path/to/other.db` (supports `~/` expansion). Useful for project-scoped memory or isolated benchmarks.
- **Cross-type discovery**: the `connect` command links concepts across events, things, persons, and places in a single query, so relationships surface even when facts are stored as different memory types.

## Performance

memory39 uses a **bloom filter** as a pre-check layer before FTS5 queries. On every `recall`, the bloom filter tests whether the query tokens exist anywhere in the database - if they definitely don't, the FTS5 query is skipped entirely, returning zero results in **O(1)** with no disk I/O.

| Layer | When it runs | Cost |
|-------|-------------|------|
| Bloom filter | Every `recall` query | ~nanoseconds, in-memory |
| FTS5 search | Only if bloom says "maybe" | Full-text index scan |

How it works:

- **Unigrams + bigrams** - every memory field is tokenized into individual words and adjacent-word pairs, all stored in the bloom filter. A query for `"alice berlin"` checks both tokens and the `alice+berlin` bigram.
- **Unicode-normalized** - tokens are lowercased with diacritics removed (matching FTS5's `unicode61 remove_diacritics 2`), so `café` and `cafe` hit the same entry.
- **Prefix-safe** - long tokens (>6 chars) that FTS5 would prefix-expand are never skipped, avoiding false negatives.
- **Persisted** - the bloom filter is saved to `<db>.bloom` alongside the database and loaded on startup. Rebuilt automatically after bulk `ingest` operations.
- **Zero false negatives** - if a token exists in any memory, the bloom filter always says "maybe". It can only produce false positives (saying "maybe" when nothing matches), which just fall through to FTS5 as usual.

Configured for 600K items at 0.001% false positive rate.

## Install

```bash
cargo install memory39
```

### Install for any AI CLI / IDE

Installs the binary and auto-configures it for every MCP client detected: **Claude Code**, **Claude Desktop**, **Codex**, **OpenCode**, **OpenClaw**.

```sh
curl -fsSL https://raw.githubusercontent.com/alejandroqh/marketplace/main/h39.sh | sh
```

Single binary: CLI by default, MCP server with `memory39 mcp`.

## Memory Types

| Prefix | Type | What it stores |
|--------|------|----------------|
| `E#` | Event (dated) | Something that happened/will happen, with date+time |
| `U#` | Event (undated) | Same, without a specific date |
| `T#` | Thing | Object, concept, or fact |
| `P#` | Person | Social memory about someone |
| `L#` | Place | Spatial memory about a location |

Every memory has `importance` (0-10), `emotion`, and `tags`. The prefix + rowid (e.g. `E3`, `T12`) is the universal ID used by `forget` and `alter`.

---

## CLI

### Global Flags

| Flag | Default | Description |
|------|---------|-------------|
| `--db <path>` | `memory39.db` | SQLite database file |
| `--ram` | off | In-memory database (non-persistent) |
| `--llm <provider>` | `deepseek` | LLM provider for `ingest` |
| `--model <name>` | per-provider | Override default model |

### Commands

#### `ingest` - LLM-driven fact extraction

Reads a conversation and uses an LLM agent loop (up to 10 tool-calling rounds per chunk) to automatically extract and store memories. The LLM decides what type each fact is (event, thing, person, place), checks for duplicates via `recall`, and uses `alter`/`forget` to keep existing memories up to date.

**Input format:** plain text, passed inline or via stdin. The chunker auto-detects conversation structure by recognizing actor prefixes:

| Prefix pattern | Detected as |
|----------------|-------------|
| `User:`, `Human:`, `Customer:` | user turn |
| `Assistant:`, `Agent:`, `Bot:`, `AI:` | agent turn |
| `[...] User:`, `[...] Human:` | timestamped user turn |
| `[...] Assistant:`, `[...] Agent:` | timestamped agent turn |
| `Speaker_A:`, `Speaker_1:` | user turn |
| `Speaker_B:`, `Speaker_2:` | agent turn |

Conversations are split into ~8K-char chunks on actor switches (every 2 switches or when size limit is reached). Plain text without actor prefixes is treated as a single chunk.

```bash
# From stdin (any text format)
cat conversation.txt | memory39 ingest -

# Inline
memory39 ingest "Alice said she's moving to Berlin in March"

# Chat logs, transcripts, meeting notes - all work
cat slack_export.txt | memory39 ingest - --llm groq
```

**What gets extracted:**

| Memory type | Stored when the fact has... | Example |
|-------------|----------------------------|---------|
| Event (dated) | A specific date or time | "Alice joined the team in March" |
| Event (undated) | Temporal context but no date | "They met last year at a conference" |
| Person | Attributes about someone | "Alice is a backend engineer" |
| Place | A location | "The office is on 5th Ave" |
| Thing | Knowledge, preferences, facts | "We use Python for the backend" |

**Importance scale assigned by the LLM:**

| Range | Level | Example |
|-------|-------|---------|
| 1-2 | Trivia | Offhand remarks, small talk |
| 3-4 | Context | Background details, mild preferences |
| 5-6 | Factual | Job, skills, tools, team info |
| 7-8 | Actionable | Allergies, deadlines, decisions |
| 9-10 | Critical | Emergencies, key life events |

| Flag | Description |
|------|-------------|
| `--llm <provider>` | `deepseek` (default), `groq`, `openai`, `ollama` |
| `--model <name>` | Override model (e.g. `--model gpt-4o`) |

**LLM Provider Defaults:**

| Provider | Default Model | API Key Env Var |
|----------|---------------|-----------------|
| `deepseek` | `deepseek-chat` | `DEEPSEEK_API_KEY` |
| `groq` | `llama-3.3-70b-versatile` | `GROQ_API_KEY` |
| `openai` | `gpt-4o-mini` | `OPENAI_API_KEY` |
| `ollama` | `llama3.2` | none (local) |

#### `event` - Store an event

```bash
memory39 event "Had coffee with Alice" --date 2025-03-15 --people Alice --tags coffee,social
```

| Arg/Flag | Required | Description |
|----------|----------|-------------|
| `<event>` | yes | What happened (max 255 chars) |
| `--date` | no | `YYYY-MM-DD` - omit for undated |
| `--time` | no | `HH:MM` (default `00:00`) |
| `--note` | no | Additional note |
| `--tags` | no | Comma-separated tags |
| `--importance` | no | 0-10 (default 5) |
| `--emotion` | no | `positive`, `negative`, `neutral`, or free text |
| `--location` | no | Where it happened |
| `--people` | no | Comma-separated names |
| `--source` | no | `experienced`, `told`, `read`, `observed` |

#### `thing` - Store a fact or concept

```bash
memory39 thing "Rust edition 2024 requires rustc 1.85+" --category programming --confidence 9
```

| Arg/Flag | Required | Description |
|----------|----------|-------------|
| `<thing>` | yes | What to remember (max 255 chars) |
| `--desc` | no | Description |
| `--category` | no | Free-text category |
| `--tags` | no | Comma-separated tags |
| `--importance` | no | 0-10 (default 5) |
| `--emotion` | no | Emotional valence |
| `--source` | no | Where this knowledge came from |
| `--confidence` | no | Certainty 0-10 (default 5) |
| `--related` | no | Comma-separated related concepts |

#### `person` - Store a social memory

```bash
memory39 person "Alice" --role "ML engineer" --relationship colleague --met-at "KubeCon 2024"
```

| Arg/Flag | Required | Description |
|----------|----------|-------------|
| `<name>` | yes | Person's name |
| `--role` | no | Role or title |
| `--relationship` | no | `friend`, `colleague`, `family`, etc. |
| `--contact` | no | Email, phone, handle |
| `--met-at` | no | Where/when you met |
| `--last-seen` | no | Last interaction `YYYY-MM-DD` |
| `--note` | no | Additional note |
| `--tags` | no | Comma-separated tags |
| `--importance` | no | 0-10 (default 5) |
| `--emotion` | no | Emotional valence |

#### `place` - Store a spatial memory

```bash
memory39 place "Blue Bottle Coffee" --address "123 Main St, SF" --kind building --tags coffee,work
```

| Arg/Flag | Required | Description |
|----------|----------|-------------|
| `<name>` | yes | Place name |
| `--desc` | no | Description |
| `--address` | no | Address or coordinates |
| `--kind` | no | `city`, `building`, `room`, `outdoor`, `virtual`, etc. |
| `--note` | no | Additional note |
| `--tags` | no | Comma-separated tags |
| `--importance` | no | 0-10 (default 5) |
| `--emotion` | no | Emotional valence |

#### `recall` - Search memories

```bash
memory39 recall "coffee" --limit 5 --min 3 --kind event
memory39 recall "*"  # list all
```

| Arg/Flag | Required | Description |
|----------|----------|-------------|
| `<query>` | yes | FTS5 search query (`*` = all) |
| `-l`, `--limit` | no | Max results (default 10) |
| `--min` | no | Minimum importance 0-10 |
| `--from` | no | Date start `YYYY-MM-DD` (events only) |
| `--to` | no | Date end `YYYY-MM-DD` (events only) |
| `--kind` | no | `event`, `undated`, `events`, `thing`, `person`, `place` |
| `--source` | no | `experienced`, `told`, `read`, `observed` |
| `--offset` | no | Skip first N results (default 0) |

#### `connect` - Find connections between concepts

```bash
memory39 connect Alice Berlin meeting
```

| Arg/Flag | Required | Description |
|----------|----------|-------------|
| `<concepts...>` | yes | 2-3 concepts to connect |
| `--min` | no | Minimum importance 0-10 |
| `--timeout` | no | Timeout in ms (default 2000) |

Three-phase discovery: **(1) direct** - all concepts in one memory, **(2) shared** - concepts in separate memories linked by a common field, **(3) bridge** - one-hop connections through shared field values.

#### `forget` - Delete a memory

```bash
memory39 forget E3
```

#### `alter` - Modify a memory

```bash
memory39 alter T2 --text "Updated fact" --importance 8
```

| Flag | Description |
|------|-------------|
| `--text` | New primary text |
| `--note` | New note |
| `--tags` | New tags |
| `--importance` | New importance 0-10 |
| `--emotion` | New emotion |
| `--location` | New location (events only) |
| `--people` | New people (events only) |
| `--source` | New source |
| `--date` | New date (dated events only) |
| `--time` | New time (requires `--date`) |

---

## MCP Server

`memory39 mcp` starts the MCP server (STDIO transport). The `ingest` command is excluded; only direct database operations are available. **No LLM API keys are needed to run the MCP server**, since the LLM is only used by `ingest`.

Database path: `~/.memory39/memory39.db` (auto-created). This path is **shared across every MCP client on the machine**, so configuring memory39 in Claude Code, Claude Desktop, Codex, OpenCode, and OpenClaw gives all of them the same memory. Override with `MEMORY39_DB=/path/to/other.db` for project-scoped or isolated databases.

### Configuration

Add to your MCP client config:

```json
{
  "mcpServers": {
    "memory39": {
      "command": "memory39",
      "args": ["mcp"]
    }
  }
}
```

### Tools

| Tool | Description | Required Params |
|------|-------------|-----------------|
| `recall` | Search with temporal-priority scoring | `query` |
| `event` | Store an event (dated or undated) | `event` |
| `thing` | Store a fact, concept, or object | `thing` |
| `person` | Store a social memory | `name` |
| `place` | Store a spatial memory | `name` |
| `forget` | Delete a memory by ID | `id` |
| `alter` | Modify fields of an existing memory | `id` |
| `connect` | Find connections between 2-3 concepts | `concepts` |

All optional parameters match their CLI counterparts. See the tool schemas for full details.

### Scoring

Results from `recall` are ranked by composite score:

| Component | Weight | Description |
|-----------|--------|-------------|
| Relevance | 0.4 | FTS5 match quality |
| Importance | 0.3 | Memory importance (0-10) |
| Recency | 0.3 | Exponential decay, 30-day half-life |

### Memory IDs

All tools use the same universal ID system:

| Prefix | Table |
|--------|-------|
| `E` | Dated events |
| `U` | Undated events |
| `T` | Things |
| `P` | Persons |
| `L` | Places |

---

## Environment

**LLM API keys are only required for the `ingest` command.** Everything else (every other CLI command: `event`, `thing`, `person`, `place`, `recall`, `connect`, `forget`, `alter`, and the entire MCP server) runs purely against the local SQLite database and needs no keys, no network, and no `.env` file.

| Variable | Used by | Purpose |
|----------|---------|---------|
| `MEMORY39_DB` | MCP server | Override DB path (supports leading `~/`). Default: `~/.memory39/memory39.db`. For the CLI, use the `--db` flag instead. |
| `DEEPSEEK_API_KEY` | `ingest` only | DeepSeek API key |
| `GROQ_API_KEY` | `ingest` only | Groq API key |
| `OPENAI_API_KEY` | `ingest` only | OpenAI API key |

If (and only if) you plan to use `ingest`, create a `.env` file with the key for the provider you want:

```bash
DEEPSEEK_API_KEY=sk-...
GROQ_API_KEY=gsk_...
OPENAI_API_KEY=sk-...
```

Using a local model with `--llm ollama` requires no API key at all.

## Built With

- [TurboMCP](https://github.com/Epistates/turbomcp) - Rust MCP server framework

## License

Apache-2.0
