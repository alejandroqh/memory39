# memory39

Temporal-priority memory system for AI agents. Rust CLI + MCP server backed by SQLite + FTS5.

Memories are scored by **0.4 x relevance + 0.3 x importance + 0.3 x recency** (30-day half-life), so recent important matches surface first.

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

This installs two binaries: **`memory39-cli`** and **`memory39-mcp`**.

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
cat conversation.txt | memory39-cli ingest -

# Inline
memory39-cli ingest "Alice said she's moving to Berlin in March"

# Chat logs, transcripts, meeting notes - all work
cat slack_export.txt | memory39-cli ingest - --llm groq
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
memory39-cli event "Had coffee with Alice" --date 2025-03-15 --people Alice --tags coffee,social
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
memory39-cli thing "Rust edition 2024 requires rustc 1.85+" --category programming --confidence 9
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
memory39-cli person "Alice" --role "ML engineer" --relationship colleague --met-at "KubeCon 2024"
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
memory39-cli place "Blue Bottle Coffee" --address "123 Main St, SF" --kind building --tags coffee,work
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
memory39-cli recall "coffee" --limit 5 --min 3 --kind event
memory39-cli recall "*"  # list all
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
memory39-cli connect Alice Berlin meeting
```

| Arg/Flag | Required | Description |
|----------|----------|-------------|
| `<concepts...>` | yes | 2-3 concepts to connect |
| `--min` | no | Minimum importance 0-10 |
| `--timeout` | no | Timeout in ms (default 2000) |

Three-phase discovery: **(1) direct** - all concepts in one memory, **(2) shared** - concepts in separate memories linked by a common field, **(3) bridge** - one-hop connections through shared field values.

#### `forget` - Delete a memory

```bash
memory39-cli forget E3
```

#### `alter` - Modify a memory

```bash
memory39-cli alter T2 --text "Updated fact" --importance 8
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

`memory39-mcp` exposes all database tools over MCP (STDIO transport). The `ingest` command is excluded - only direct operations are available.

Database path: `~/.memory39/memory39.db` (auto-created).

### Configuration

Add to your MCP client (Claude Desktop, Claude Code, etc.):

```json
{
  "mcpServers": {
    "memory39": {
      "command": "memory39-mcp"
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

Create a `.env` file (only needed for `ingest`):

```bash
DEEPSEEK_API_KEY=sk-...
GROQ_API_KEY=gsk_...
OPENAI_API_KEY=sk-...
```

## License

MIT
