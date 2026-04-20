use crate::db;
use std::sync::{Arc, Mutex};
use turbomcp::prelude::*;

// Compile-time check: MCP server version must match Cargo.toml
const _: () = {
    let cargo = env!("CARGO_PKG_VERSION").as_bytes();
    let mcp = b"1.0.1";
    assert!(cargo.len() == mcp.len(), "MCP server version does not match Cargo.toml — update #[server(version)] below");
    let mut i = 0;
    while i < cargo.len() {
        assert!(cargo[i] == mcp[i], "MCP server version does not match Cargo.toml — update #[server(version)] below");
        i += 1;
    }
};

#[derive(Clone)]
struct Memory39 {
    db: Arc<Mutex<db::MemoryDb>>,
}

impl Memory39 {
    fn lock_db(&self) -> McpResult<std::sync::MutexGuard<'_, db::MemoryDb>> {
        self.db.lock().map_err(|e| McpError::internal(e.to_string()))
    }
}

#[server(
    name = "memory39",
    version = "1.0.1",
    description = "Temporal-priority memory system for AI agents"
)]
#[allow(clippy::too_many_arguments)]
impl Memory39 {
    /// Search across all memories with temporal-priority scoring (0.4×relevance + 0.3×importance + 0.3×recency).
    /// Use "*" or empty string to retrieve all memories (supports pagination via offset).
    /// Note: date filters (from/to) restrict results to events only — persons, places, and things are excluded when date range is set.
    #[tool]
    async fn recall(
        &self,
        #[description("Search query (FTS5 syntax supported). Use '*' to list all memories")] query: String,
        #[description("Max results to return (default 10, max 100)")] limit: Option<u64>,
        #[description("Minimum importance 0-10")] min_importance: Option<u8>,
        #[description("Date range start YYYY-MM-DD. Note: restricts results to events only")] from: Option<String>,
        #[description("Date range end YYYY-MM-DD. Note: restricts results to events only")] to: Option<String>,
        #[description("Filter by kind: event (dated E#), undated (U#), events (both E#+U#), thing, person, place")] kind: Option<String>,
        #[description("Filter by source: experienced, told, read, observed")] source: Option<String>,
        #[description("Skip first N results (for pagination, default 0)")] offset: Option<u64>,
    ) -> McpResult<String> {
        let limit = limit.unwrap_or(10).min(100) as usize;
        let offset = offset.unwrap_or(0) as usize;
        let results = {
            let mdb = self.lock_db()?;
            let filters = db::RecallFilters {
                min_importance,
                date_from: from,
                date_to: to,
                memory_type: kind.filter(|s| !s.is_empty()),
                source: source.filter(|s| !s.is_empty()),
            };
            mdb.recall(&query, limit, offset, &filters)
        };
        if results.is_empty() {
            return Ok(format!("No memories found for: {}", query));
        }
        use std::fmt::Write;
        let mut out = String::with_capacity(results.len() * 200);
        let _ = writeln!(out, "Found {} memories:", results.len());
        for r in &results {
            let _ = writeln!(out, "\n[{}] {} (score: {:.2})", r.mid, r.memory_type, r.score);
            for (k, v) in &r.fields {
                let _ = writeln!(out, "  {}: {}", k, v);
            }
        }
        Ok(out)
    }

    /// Store an event memory (something that happened or will happen).
    /// Omit date for undated events. Returns the memory ID (E# for dated, U# for undated).
    #[tool]
    async fn event(
        &self,
        #[description("What happened (max 255 chars)")] event: String,
        #[description("Date in YYYY-MM-DD format. Omit for undated event")] date: Option<String>,
        #[description("Time in HH:MM format (default 00:00)")] time: Option<String>,
        #[description("Additional note (max 255 chars)")] note: Option<String>,
        #[description("Comma-separated tags (max 255 chars)")] tags: Option<String>,
        #[description("Importance 0-10 (default 5)")] importance: Option<u8>,
        #[description("Emotional valence: positive, negative, neutral, or free text")] emotion: Option<String>,
        #[description("Where it happened (max 255 chars)")] location: Option<String>,
        #[description("Who was involved, comma-separated (max 255 chars)")] people: Option<String>,
        #[description("How you know: experienced, told, read, observed")] source: Option<String>,
    ) -> McpResult<String> {
        let created_at = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();
        let datetime = date.as_ref().map(|d| {
            let t = time.as_deref().unwrap_or("00:00");
            format!("{} {}", d, t)
        });
        let id = {
            let mut mdb = self.lock_db()?;
            mdb.insert_event(
                &event,
                datetime.as_deref(),
                note.as_deref(),
                tags.as_deref(),
                importance.unwrap_or(5),
                emotion.as_deref(),
                location.as_deref(),
                people.as_deref(),
                source.as_deref(),
                &created_at,
            )
            .map_err(|e| McpError::internal(e.to_string()))?
        };
        let mid = if datetime.is_some() {
            format!("E{}", id)
        } else {
            format!("U{}", id)
        };
        Ok(format!("[{}] event stored: {}", mid, event))
    }

    /// Store a thing memory (an object, concept, or fact worth remembering).
    /// Returns the memory ID (T#).
    #[tool]
    async fn thing(
        &self,
        #[description("What to remember (max 255 chars)")] thing: String,
        #[description("Description (max 255 chars)")] desc: Option<String>,
        #[description("Category (free text)")] category: Option<String>,
        #[description("Comma-separated tags (max 255 chars)")] tags: Option<String>,
        #[description("Importance 0-10 (default 5)")] importance: Option<u8>,
        #[description("Emotional valence: positive, negative, neutral, or free text")] emotion: Option<String>,
        #[description("Where this knowledge came from (max 255 chars)")] source: Option<String>,
        #[description("Certainty 0-10 (default 5)")] confidence: Option<u8>,
        #[description("Comma-separated related concepts (max 255 chars)")] related: Option<String>,
    ) -> McpResult<String> {
        let created_at = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();
        let id = {
            let mut mdb = self.lock_db()?;
            mdb.insert_thing(
                &thing,
                desc.as_deref(),
                category.as_deref(),
                tags.as_deref(),
                importance.unwrap_or(5),
                emotion.as_deref(),
                source.as_deref(),
                confidence.unwrap_or(5),
                related.as_deref(),
                &created_at,
            )
            .map_err(|e| McpError::internal(e.to_string()))?
        };
        Ok(format!("[T{}] thing stored: {}", id, thing))
    }

    /// Store a person memory (social memory about someone).
    /// Returns the memory ID (P#).
    #[tool]
    async fn person(
        &self,
        #[description("Person's name (max 255 chars)")] name: String,
        #[description("Role or title (max 255 chars)")] role: Option<String>,
        #[description("Relationship: friend, colleague, family, etc.")] relationship: Option<String>,
        #[description("Contact info: email, phone, handle (max 255 chars)")] contact: Option<String>,
        #[description("Where or when you met them (max 255 chars)")] met_at: Option<String>,
        #[description("Last interaction date YYYY-MM-DD")] last_seen: Option<String>,
        #[description("Additional note (max 255 chars)")] note: Option<String>,
        #[description("Comma-separated tags (max 255 chars)")] tags: Option<String>,
        #[description("Importance 0-10 (default 5)")] importance: Option<u8>,
        #[description("Emotional valence: positive, negative, neutral, or free text")] emotion: Option<String>,
    ) -> McpResult<String> {
        let created_at = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();
        let id = {
            let mut mdb = self.lock_db()?;
            mdb.insert_person(
                &name,
                role.as_deref(),
                relationship.as_deref(),
                contact.as_deref(),
                met_at.as_deref(),
                last_seen.as_deref(),
                note.as_deref(),
                tags.as_deref(),
                importance.unwrap_or(5),
                emotion.as_deref(),
                &created_at,
            )
            .map_err(|e| McpError::internal(e.to_string()))?
        };
        Ok(format!("[P{}] person stored: {}", id, name))
    }

    /// Store a place memory (spatial memory about a location).
    /// Returns the memory ID (L#).
    #[tool]
    async fn place(
        &self,
        #[description("Name of the place (max 255 chars)")] name: String,
        #[description("Description (max 255 chars)")] desc: Option<String>,
        #[description("Address or coordinates (max 255 chars)")] address: Option<String>,
        #[description("Type: city, building, room, outdoor, virtual, etc.")] kind: Option<String>,
        #[description("Additional note (max 255 chars)")] note: Option<String>,
        #[description("Comma-separated tags (max 255 chars)")] tags: Option<String>,
        #[description("Importance 0-10 (default 5)")] importance: Option<u8>,
        #[description("Emotional valence: positive, negative, neutral, or free text")] emotion: Option<String>,
    ) -> McpResult<String> {
        let created_at = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();
        let id = {
            let mut mdb = self.lock_db()?;
            mdb.insert_place(
                &name,
                desc.as_deref(),
                address.as_deref(),
                kind.as_deref(),
                note.as_deref(),
                tags.as_deref(),
                importance.unwrap_or(5),
                emotion.as_deref(),
                &created_at,
            )
            .map_err(|e| McpError::internal(e.to_string()))?
        };
        Ok(format!("[L{}] place stored: {}", id, name))
    }

    /// Delete a memory by its universal ID (e.g. E3, U1, T2, P1, L4).
    #[tool]
    async fn forget(
        &self,
        #[description("Memory ID to delete (e.g. E3, U1, T2, P1, L4)")] id: String,
    ) -> McpResult<String> {
        let mdb = self.lock_db()?;
        match mdb.forget(&id) {
            Ok(true) => Ok(format!("Forgotten: {}", id)),
            Ok(false) => Ok(format!("Not found: {}", id)),
            Err(e) => Err(McpError::internal(e.to_string())),
        }
    }

    /// Modify fields of an existing memory by its universal ID.
    /// Only provide fields you want to change; omit the rest.
    #[tool]
    async fn alter(
        &self,
        #[description("Memory ID to modify (e.g. E3, U1, T2, P1, L4)")] id: String,
        #[description("New primary text (event text for E/U, thing text for T, name for P/L)")] text: Option<String>,
        #[description("New note")] note: Option<String>,
        #[description("New tags")] tags: Option<String>,
        #[description("New importance 0-10")] importance: Option<u8>,
        #[description("New emotion")] emotion: Option<String>,
        #[description("New location (events only)")] location: Option<String>,
        #[description("New people (events only)")] people: Option<String>,
        #[description("New source")] source: Option<String>,
        #[description("New date YYYY-MM-DD (dated events only)")] date: Option<String>,
        #[description("New time HH:MM (dated events only, requires date)")] time: Option<String>,
    ) -> McpResult<String> {
        let mut changes: Vec<(String, String)> = Vec::new();
        if let Some(v) = text {
            changes.push((db::text_field_for_id(&id).into(), v));
        }
        if let Some(v) = note {
            changes.push(("note".into(), v));
        }
        if let Some(v) = tags {
            changes.push(("tags".into(), v));
        }
        if let Some(v) = importance {
            changes.push(("importance".into(), v.to_string()));
        }
        if let Some(v) = emotion {
            changes.push(("emotion".into(), v));
        }
        if let Some(v) = location {
            changes.push(("location".into(), v));
        }
        if let Some(v) = people {
            changes.push(("people".into(), v));
        }
        if let Some(v) = source {
            changes.push(("source".into(), v));
        }
        if let Some(d) = &date {
            let t = time.as_deref().unwrap_or("00:00");
            changes.push(("datetime".into(), format!("{} {}", d, t)));
        } else if time.is_some() {
            return Err(McpError::invalid_params("Cannot set time without date. Provide both date and time together."));
        }
        if changes.is_empty() {
            return Ok("Nothing to alter. Provide at least one field to change.".into());
        }
        let res = {
            let mut mdb = self.lock_db()?;
            mdb.alter(&id, &changes)
        }; match res {
            Ok(true) => Ok(format!("Altered: {}", id)),
            Ok(false) => Ok(format!("Not found: {}", id)),
            Err(e) => Err(McpError::internal(e.to_string())),
        }
    }

    /// Find connections between 2-3 concepts across all memories.
    /// Uses 3-phase discovery: (1) direct — all concepts in one memory,
    /// (2) shared — concepts in different memories linked by a common field value (tags, location, people),
    /// (3) bridge — one-hop connections through shared field values.
    /// If timeout expires, returns partial results from completed phases.
    #[tool]
    async fn connect(
        &self,
        #[description("Concepts to connect, 2-3 items (e.g. [\"Alice\", \"project-x\"] or [\"Bob\", \"Alice\", \"meeting\"])")] concepts: Vec<String>,
        #[description("Minimum importance 0-10")] min_importance: Option<u8>,
        #[description("Timeout in milliseconds (default 2000). Returns partial results if exceeded")] timeout_ms: Option<u64>,
        #[description("Maximum connections to return (default 30)")] limit: Option<usize>,
    ) -> McpResult<String> {
        if concepts.len() < 2 || concepts.len() > 3 {
            return Err(McpError::invalid_params("Provide 2 or 3 concepts"));
        }
        let timeout = std::time::Duration::from_millis(timeout_ms.unwrap_or(2000));
        let result = {
            let mdb = self.lock_db()?;
            mdb.find_connections(&concepts, min_importance, limit, timeout)
        };

        if result.connections.is_empty() {
            return Ok(format!(
                "No connections found for: {} ({}ms)",
                concepts.join(" + "),
                result.elapsed_ms
            ));
        }

        let mut out = format!("Connections for: {}\n", concepts.join(" + "));
        for c in &result.connections {
            match &c.kind {
                db::ConnectionKind::Direct { mid } => {
                    out.push_str(&format!("\n[direct] {} (score: {:.2})\n", mid, c.score));
                    for (k, v) in &c.fields {
                        out.push_str(&format!("  {}: {}\n", k, v));
                    }
                }
                db::ConnectionKind::Shared {
                    mid_a,
                    mid_b,
                    field,
                    value,
                } => {
                    out.push_str(&format!(
                        "\n[shared:{}] {} <> {} (score: {:.2})\n  Linked by: {}\n",
                        field, mid_a, mid_b, c.score, value
                    ));
                }
                db::ConnectionKind::Bridge {
                    mid_a,
                    mid_b,
                    via_field,
                    via_value,
                } => {
                    out.push_str(&format!(
                        "\n[bridge:{}] {} -> {} (score: {:.2})\n  Via: {}\n",
                        via_field, mid_a, mid_b, c.score, via_value
                    ));
                }
            }
        }
        out.push_str(&format!(
            "\nFound {} connections in {}ms",
            result.connections.len(),
            result.elapsed_ms
        ));
        Ok(out)
    }
}

pub async fn run_mcp_stdio() -> Result<(), Box<dyn std::error::Error>> {
    let db_path = db::resolve_path(None)
        .map_err(|e| format!("failed to resolve db path: {}", e))?;
    let mdb = db::open(&db_path)
        .map_err(|e| format!("failed to open database: {}", e))?;

    let server = Memory39 {
        db: Arc::new(Mutex::new(mdb)),
    };
    server.builder()
        .with_protocol(ProtocolConfig::multi_version())
        .serve()
        .await?;
    Ok(())
}
