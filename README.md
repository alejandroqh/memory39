```
▄▄▄▄  ▗▞▀▚▖▄▄▄▄   ▄▄▄   ▄▄▄ ▄   ▄ ▄▄▄▄ ▄▄▄▄ 
█ █ █ ▐▛▀▀▘█ █ █ █   █ █    █   █    █ █  █ 
█   █ ▝▚▄▄▖█   █ ▀▄▄▄▀ █     ▀▀▀█ ▀▀▀█ ▀▀▀█ 
                            ▄   █ ▄▄▄█ ▄▄▄█ 
                             ▀▀▀            
```
**One binary. One local SQLite file. One memory, shared by every MCP client on your machine.**

memory39 is a Rust CLI + MCP server backed by SQLite + FTS5. Every MCP-capable AI tool on your machine (Claude Code, Claude Desktop, Codex, OpenCode, OpenClaw) reads and writes the same `~/.memory39/memory39.db`, so a fact learned in one client is instantly recallable from any other. No cloud, no daemon, no sync.

Results are ranked by temporal-priority scoring: **0.4 x relevance + 0.3 x importance + 0.3 x recency** (30-day half-life), so recent important matches surface first.

## Why memory39

- **Persistent across sessions**: memories live in an on-disk SQLite file and survive restarts, CLI invocations, and MCP reconnects.
- **One knowledge base across every MCP client**: Claude Code, Claude Desktop, Codex, OpenCode, and OpenClaw all point at the same `~/.memory39/memory39.db`. A fact stored from Claude is instantly recallable from Codex; a person stored via the CLI shows up in every MCP client. No syncing, no duplication.
- **Local and private**: no cloud, no account, no telemetry. Your memory is a single SQLite file on your machine that you can inspect, back up, or move by copying.
- **Single binary, zero daemon**: CLI for scripting (`memory39 recall ...`), MCP server on demand (`memory39 mcp`). Nothing runs in the background between calls.
- **Portable DB path**: point at a different database by exporting `MEMORY39_DB=/path/to/other.db` (supports `~/` expansion). Useful for project-scoped memory or isolated benchmarks.
- **Cross-type discovery**: the `connect` command links concepts across events, things, persons, and places in a single query, so relationships surface even when facts are stored as different memory types.

## Performance

memory39 uses a **bloom filter** as a pre-check layer before FTS5 queries. On every `recall`, the bloom filter tests whether enough query words could exist anywhere in the database: the word itself for a one-word query, at least half the words otherwise (the same rule recall uses, see [How recall matches](#how-recall-matches)). If not, no memory can match, so FTS5 is skipped entirely and zero results come back in **O(1)** without touching the full-text index.

| Layer | When it runs | Cost |
|-------|-------------|------|
| Bloom filter | Every `recall` query | ~2-4 us: one cached freshness read, stemming, hash probes |
| FTS5 search | Only if bloom says "maybe" | Full-text index scan |

How it works:

- **FTS5's own terms** - the bloom filter holds exactly the terms the FTS5 index holds: built from the index vocabulary, and on every write from the new text run through a scratch FTS5 table with the same tokenizer. Folding quirks (`ſ` becomes `s`, decomposed accents stay inside one word) can't make it disagree with the index.
- **Word-form aware** - every term's Snowball stems (English, Spanish, German) are stored too, so `cars` is a "maybe" when only `car` is stored, just as recall would match it.
- **Order-free, partial-match aware** - FTS5 matches words in any order and in any field, and recall returns memories with at least half the words, so `alice zzz` still runs FTS5 when `alice` exists. `alice zzz qqq` is skipped.
- **Decides only where it is exact** - a query word decides a skip only if every character is ASCII or a Latin letter with one accent (`é`, `ü`, `ñ`, `ç`), where memory39's tokenizer is verified to match FTS5's character by character (an exhaustive unit test). Other words (`straße`, Greek, CJK) always count as "maybe".
- **Persisted and always fresh** - the bloom filter is saved to `<db>.bloom` on every new memory (not on exit, so a killed MCP server loses nothing) and loaded on startup. SQLite triggers keep a write counter that counts every insert and update from any process: other MCP clients, the CLI, even older memory39 versions. The file records the counter value it was built for and every `recall` checks it, so a stale bloom filter is reloaded or rebuilt automatically instead of hiding new memories.
- **Zero false negatives** - if a memory could match, the bloom filter always says "maybe". It can only produce false positives (saying "maybe" when nothing matches), which just fall through to FTS5 as usual.
- **Sized to the data** - each build holds twice the keys it starts with (at least 20K) at a 0.01% false-positive rate, using the standard sizing `m = -n ln(p) / (ln 2)²` bits and `k = -log2(p)` hashes. The file header counts keys as writes add them, and the write that fills the filter rebuilds it at double size. A database with a few hundred memories keeps a ~48 KB file, so saving it on every write and reloading it in other processes stay cheap (~0.3 ms and ~10-25 us).

### Measured cost

Numbers from `cargo run --release --example bloom_bench` on a synthetic DB (~700 memories):

| Query | Path | Avg per call | Throughput |
|-------|------|-------------:|-----------:|
| Unknown fact, one word | Bloom says "no match possible"; FTS5 skipped | **~2.5 us** | ~390K ops/sec |
| Unknown fact, two long words | Same | ~4 us | ~250K ops/sec |
| Known fact | Bloom says "maybe" -> FTS5 search runs | ~210 us | ~4.8K ops/sec |

Negative queries are **~80x faster** than positive ones. The negative path is the freshness check (a single-row read of the write counter, served from SQLite's page cache), stemming each word in three languages, and the bloom probes.

## Install

```bash
cargo install memory39
```

### Install for any AI CLI / IDE

Installs the binary and auto-configures it for every MCP client detected: **Claude Code**, **Claude Desktop**, **Codex**, **OpenCode**, **OpenClaw**.

```sh
curl -fsSL https://raw.githubusercontent.com/alejandroqh/marketplace/main/h39.sh | bash
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
| `--db <path>` | `$MEMORY39_DB`, else `~/.memory39/memory39.db` | SQLite database file (the same one every MCP client uses) |
| `--ram` | off | In-memory database (non-persistent) |

### Commands

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
| `<query>` | yes | Search keywords; a few distinctive words work better than a sentence. Memories with every word come first, then those with at least half of them; word forms match too (see [How recall matches](#how-recall-matches)). FTS5 syntax (`budget OR savings`, `"exact phrase"`, `prefix*`) runs as written; `*` = all |
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
memory39 alter T2 --desc "More detail" --confidence 9
memory39 alter P1 --role "Staff engineer" --last-seen 2026-10-11
```

| Flag | Applies to | Description |
|------|------------|-------------|
| `--text` | all | New primary text (event for `E`/`U`, thing for `T`, name for `P`/`L`) |
| `--note` | all | New note; on things (`T`) it sets the description |
| `--desc` | `T`, `L` | New description |
| `--tags` | all | New comma-separated tags |
| `--importance` | all | New importance 0-10 |
| `--emotion` | all | New emotion |
| `--source` | `E`, `U`, `T` | New source |
| `--location` | `E`, `U` | New location |
| `--people` | `E`, `U` | New people, comma-separated |
| `--date` | `E` | New date `YYYY-MM-DD` |
| `--time` | `E` | New time `HH:MM` (requires `--date`) |
| `--category` | `T` | New category |
| `--confidence` | `T` | New certainty 0-10 |
| `--related` | `T` | New comma-separated related concepts |
| `--role` | `P` | New role or title |
| `--relationship` | `P` | New relationship |
| `--contact` | `P` | New contact info |
| `--met-at` | `P` | New where/when you met |
| `--last-seen` | `P` | New last interaction `YYYY-MM-DD` |
| `--address` | `L` | New address or coordinates |
| `--kind` | `L` | New place type |

A flag that doesn't apply to the memory's type is rejected with the list of valid fields. To date an undated event (`U#`), forget it and store it again with `--date`.

`alter` and `forget` exit with status 1 when the memory is not found or the change is rejected, so scripts can detect failures.

---

## MCP Server

`memory39 mcp` starts the MCP server (STDIO transport). It runs purely against the local SQLite database: no network calls, no API keys, no external services.

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
| Relevance | 0.4 | FTS5 match quality (bm25), mapped to 0-1 |
| Importance | 0.3 | Memory importance (0-10) |
| Recency | 0.3 | Exponential decay, 30-day half-life |

Each FTS table contributes its best bm25 matches (4 per result requested) before the composite score reorders them. Partial matches (see below) are listed after full matches, ordered by how many words they match, and their score is scaled by the fraction of words matched.

### How recall matches

A plain-words query runs in tiers. Each tier only fills the slots the previous ones left:

1. **Every word, as typed.** Words are passed to FTS5 quoted, so punctuation is tokenized rather than parsed: `Alice's`, `e-mail` and `v2.1` work. FTS5 operators are case-sensitive, so `salt and pepper` is three plain words.
2. **Every word, as typed or as a word form.** For each word, memory39 looks in FTS5's own vocabulary (`fts5vocab`) for indexed terms with the same Snowball stem in English, Spanish or German: `house`/`houses`, `running`/`run`, `hablando`/`hablar`, `desarrollando`/`desarrollo`. Words that only share their first letters don't match (`transportation`/`transparent`).
3. **At least half the words.** `the mobility house` still finds "Mobility House GmbH". These matches are ordered by how many words they contain, then by relevance, so a memory matching only common words doesn't win on importance.

A query containing FTS5 syntax (`"`, `*`, `(`, `)`, `:`, or ` AND `/` OR `/` NOT `) runs exactly as written, without these tiers. Use it for synonyms and irregular forms that no stemmer catches: `child OR children`.

### Writing queries

Send only the distinctive words: `kubecon talk`, not `what is the kubecon talk about?`. Common words like `what`, `is` and `the` appear in many memories, which hurts recall in two ways:

- **Noise.** In the at-least-half tier, `what`, `is` and `the` are 3 of the 6 words in `what is the kubecon talk about?`, so "what is the wifi password at home" comes back next to the KubeCon memory.
- **Speed.** That tier ranks every memory containing any query word. A word found in thousands of memories costs milliseconds on large databases, while `kubecon talk` ranks only the memories that contain those words.

Most agents follow the `recall` tool description, which asks for a few distinctive keywords. Smaller models and scripts often pass the user's question through unchanged.

For agent builders, a fast decision model in front of memory39 works better than relying on the main LLM for this. It can pick the distinctive words of the user's question before `recall`, decide whether a question needs memory at all, and score which recalled memories are relevant before they reach the context. One example is [Jev](https://docs.typesafe.ai/concepts/system-one) from TypeSafe AI, a "System One" decision model. It returns typed answers (a choice from a set, a score, or a yes/no probability) with calibrated confidence instead of generated text.

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

memory39 has **no external dependencies at runtime**: no network, no API keys, no `.env` file. The only environment variable it reads is an optional DB-path override.

| Variable | Used by | Purpose |
|----------|---------|---------|
| `MEMORY39_DB` | CLI and MCP server | Override DB path (supports leading `~/`). Default: `~/.memory39/memory39.db`. The CLI's `--db` flag takes precedence. |

## Built With

- [TurboMCP](https://github.com/Epistates/turbomcp) - Rust MCP server framework

## License

Apache-2.0
