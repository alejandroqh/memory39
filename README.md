# memory39

Temporal-priority memory system for AI agents. Rust CLI + MCP server backed by SQLite + FTS5.

Memories are scored by **0.4 x relevance + 0.3 x importance + 0.3 x recency** (30-day half-life), so recent important matches surface first.

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

#### `ingest` — LLM-driven fact extraction

Reads a conversation and automatically extracts memories via tool-calling.

```bash
# From stdin
cat conversation.txt | memory39-cli ingest -

# Inline
memory39-cli ingest "Alice said she's moving to Berlin in March"
```

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

#### `event` — Store an event

```bash
memory39-cli event "Had coffee with Alice" --date 2025-03-15 --people Alice --tags coffee,social
```

| Arg/Flag | Required | Description |
|----------|----------|-------------|
| `<event>` | yes | What happened (max 255 chars) |
| `--date` | no | `YYYY-MM-DD` — omit for undated |
| `--time` | no | `HH:MM` (default `00:00`) |
| `--note` | no | Additional note |
| `--tags` | no | Comma-separated tags |
| `--importance` | no | 0-10 (default 5) |
| `--emotion` | no | `positive`, `negative`, `neutral`, or free text |
| `--location` | no | Where it happened |
| `--people` | no | Comma-separated names |
| `--source` | no | `experienced`, `told`, `read`, `observed` |

#### `thing` — Store a fact or concept

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

#### `person` — Store a social memory

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

#### `place` — Store a spatial memory

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

#### `recall` — Search memories

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

#### `connect` — Find connections between concepts

```bash
memory39-cli connect Alice Berlin meeting
```

| Arg/Flag | Required | Description |
|----------|----------|-------------|
| `<concepts...>` | yes | 2-3 concepts to connect |
| `--min` | no | Minimum importance 0-10 |
| `--timeout` | no | Timeout in ms (default 2000) |

Three-phase discovery: **(1) direct** — all concepts in one memory, **(2) shared** — concepts in separate memories linked by a common field, **(3) bridge** — one-hop connections through shared field values.

#### `forget` — Delete a memory

```bash
memory39-cli forget E3
```

#### `alter` — Modify a memory

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

`memory39-mcp` exposes all database tools over MCP (STDIO transport). The `ingest` command is excluded — only direct operations are available.

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
