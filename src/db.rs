use rusqlite::{Connection, Result, params, Row};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::{Duration, Instant};

pub fn open(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)?;

    // Performance PRAGMAs - must be set before schema
    conn.execute_batch("
        PRAGMA journal_mode = WAL;
        PRAGMA synchronous = NORMAL;
        PRAGMA cache_size = -8000;
        PRAGMA mmap_size = 67108864;
        PRAGMA foreign_keys = ON;
    ")?;

    conn.execute_batch(SCHEMA)?;
    Ok(conn)
}

const SCHEMA: &str = "
-- Dated events (past and future)
CREATE TABLE IF NOT EXISTS events (
    id         INTEGER PRIMARY KEY,
    event      TEXT NOT NULL,
    datetime   TEXT NOT NULL,
    note       TEXT,
    tags       TEXT,
    importance INTEGER NOT NULL DEFAULT 5,
    emotion    TEXT,
    location   TEXT,
    people     TEXT,
    source     TEXT,
    created_at TEXT NOT NULL
);

-- Date segmentation via expression index (extracts YYYY-MM-DD from datetime)
CREATE INDEX IF NOT EXISTS idx_events_date ON events(substr(datetime, 1, 10));
-- Fast date + importance sort (covers date-only lookups via leftmost prefix)
CREATE INDEX IF NOT EXISTS idx_events_date_importance ON events(substr(datetime, 1, 10), importance DESC);
-- Sort globally by importance
CREATE INDEX IF NOT EXISTS idx_events_importance ON events(importance DESC);

-- FTS5: search memory-relevant fields only (not source, not created_at)
CREATE VIRTUAL TABLE IF NOT EXISTS events_fts USING fts5(
    event, note, tags, emotion, location, people,
    content=events,
    content_rowid=id
);

-- Keep FTS in sync
CREATE TRIGGER IF NOT EXISTS events_ai AFTER INSERT ON events BEGIN
    INSERT INTO events_fts(rowid, event, note, tags, emotion, location, people)
    VALUES (new.id, new.event, new.note, new.tags, new.emotion, new.location, new.people);
END;
CREATE TRIGGER IF NOT EXISTS events_ad AFTER DELETE ON events BEGIN
    INSERT INTO events_fts(events_fts, rowid, event, note, tags, emotion, location, people)
    VALUES ('delete', old.id, old.event, old.note, old.tags, old.emotion, old.location, old.people);
END;
CREATE TRIGGER IF NOT EXISTS events_au AFTER UPDATE ON events BEGIN
    INSERT INTO events_fts(events_fts, rowid, event, note, tags, emotion, location, people)
    VALUES ('delete', old.id, old.event, old.note, old.tags, old.emotion, old.location, old.people);
    INSERT INTO events_fts(rowid, event, note, tags, emotion, location, people)
    VALUES (new.id, new.event, new.note, new.tags, new.emotion, new.location, new.people);
END;

-- Undated events
CREATE TABLE IF NOT EXISTS events_undated (
    id         INTEGER PRIMARY KEY,
    event      TEXT NOT NULL,
    note       TEXT,
    tags       TEXT,
    importance INTEGER NOT NULL DEFAULT 5,
    emotion    TEXT,
    location   TEXT,
    people     TEXT,
    source     TEXT,
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_events_undated_importance ON events_undated(importance DESC);

-- FTS5 for undated events
CREATE VIRTUAL TABLE IF NOT EXISTS events_undated_fts USING fts5(
    event, note, tags, emotion, location, people,
    content=events_undated,
    content_rowid=id
);

CREATE TRIGGER IF NOT EXISTS events_undated_ai AFTER INSERT ON events_undated BEGIN
    INSERT INTO events_undated_fts(rowid, event, note, tags, emotion, location, people)
    VALUES (new.id, new.event, new.note, new.tags, new.emotion, new.location, new.people);
END;
CREATE TRIGGER IF NOT EXISTS events_undated_ad AFTER DELETE ON events_undated BEGIN
    INSERT INTO events_undated_fts(events_undated_fts, rowid, event, note, tags, emotion, location, people)
    VALUES ('delete', old.id, old.event, old.note, old.tags, old.emotion, old.location, old.people);
END;
CREATE TRIGGER IF NOT EXISTS events_undated_au AFTER UPDATE ON events_undated BEGIN
    INSERT INTO events_undated_fts(events_undated_fts, rowid, event, note, tags, emotion, location, people)
    VALUES ('delete', old.id, old.event, old.note, old.tags, old.emotion, old.location, old.people);
    INSERT INTO events_undated_fts(rowid, event, note, tags, emotion, location, people)
    VALUES (new.id, new.event, new.note, new.tags, new.emotion, new.location, new.people);
END;
";

pub fn insert_event(
    conn: &Connection,
    event: &str,
    datetime: Option<&str>,
    note: Option<&str>,
    tags: Option<&str>,
    importance: u8,
    emotion: Option<&str>,
    location: Option<&str>,
    people: Option<&str>,
    source: Option<&str>,
    created_at: &str,
) -> Result<i64> {
    match datetime {
        Some(dt) => {
            conn.execute(
                "INSERT INTO events (event, datetime, note, tags, importance, emotion, location, people, source, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![event, dt, note, tags, importance, emotion, location, people, source, created_at],
            )?;
            Ok(conn.last_insert_rowid())
        }
        None => {
            conn.execute(
                "INSERT INTO events_undated (event, note, tags, importance, emotion, location, people, source, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![event, note, tags, importance, emotion, location, people, source, created_at],
            )?;
            Ok(conn.last_insert_rowid())
        }
    }
}

/// Memory ID prefix: E = events, U = events_undated, T = things, P = persons, L = places
pub fn memory_id(prefix: &str, row_id: i64) -> String {
    format!("{}{}", prefix, row_id)
}

/// Parse a memory ID like "E3" into (prefix, row_id). Returns None if invalid.
pub fn parse_memory_id(mid: &str) -> Option<(String, i64)> {
    if mid.is_empty() {
        return None;
    }
    let prefix = mid.chars().next()?;
    let row_id: i64 = mid[prefix.len_utf8()..].parse().ok()?;
    Some((prefix.to_uppercase().to_string(), row_id))
}

pub struct RecallFilters {
    pub min_importance: Option<u8>,
    pub date_from: Option<String>,
    pub date_to: Option<String>,
    pub memory_type: Option<String>,
}

pub struct RecallResult {
    pub memory_type: String,
    pub mid: String,
    pub score: f64,
    pub fields: Vec<(String, String)>,
}

/// Search across all memory tables with composite scoring and filters.
/// Score = 0.4 * relevance + 0.3 * importance + 0.3 * recency
pub fn recall(conn: &Connection, query: &str, limit: usize, filters: &RecallFilters) -> Vec<RecallResult> {
    let mut results = Vec::new();
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();

    let skip_dated = filters.memory_type.as_ref().is_some_and(|t| t == "undated");
    let skip_undated = filters.memory_type.as_ref().is_some_and(|t| t == "event");

    // Search dated events
    if !skip_dated {
        let mut where_extra = String::new();
        let mut extra_params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        if let Some(min) = filters.min_importance {
            where_extra.push_str(" AND e.importance >= ?3");
            extra_params.push(Box::new(min));
        }
        if let Some(ref from) = filters.date_from {
            let idx = 3 + extra_params.len();
            where_extra.push_str(&format!(" AND substr(e.datetime, 1, 10) >= ?{}", idx));
            extra_params.push(Box::new(from.clone()));
        }
        if let Some(ref to) = filters.date_to {
            let idx = 3 + extra_params.len();
            where_extra.push_str(&format!(" AND substr(e.datetime, 1, 10) <= ?{}", idx));
            extra_params.push(Box::new(to.clone()));
        }

        let sql = format!(
            "SELECT e.id, rank, e.event, e.datetime, e.note, e.tags, e.importance,
                    e.emotion, e.location, e.people, e.source, e.created_at
             FROM events_fts f
             JOIN events e ON e.id = f.rowid
             WHERE events_fts MATCH ?1{}
             LIMIT ?2",
            where_extra
        );

        recall_fts(conn, &mut results, limit, &sql, query, true, &extra_params, &now);
    }

    // Search undated events (date range filters don't apply)
    if !skip_undated {
        let mut where_extra = String::new();
        let mut extra_params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        if let Some(min) = filters.min_importance {
            where_extra.push_str(" AND u.importance >= ?3");
            extra_params.push(Box::new(min));
        }

        let sql = format!(
            "SELECT u.id, rank, u.event, u.note, u.tags, u.importance,
                    u.emotion, u.location, u.people, u.source, u.created_at
             FROM events_undated_fts f
             JOIN events_undated u ON u.id = f.rowid
             WHERE events_undated_fts MATCH ?1{}
             LIMIT ?2",
            where_extra
        );

        recall_fts(conn, &mut results, limit, &sql, query, false, &extra_params, &now);
    }

    // Sort by composite score descending
    results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    results.truncate(limit);
    results
}

fn recall_fts(
    conn: &Connection,
    results: &mut Vec<RecallResult>,
    limit: usize,
    sql: &str,
    query: &str,
    dated: bool,
    extra_params: &[Box<dyn rusqlite::types::ToSql>],
    now: &str,
) {
    let mut stmt = match conn.prepare(sql) {
        Ok(s) => s,
        Err(e) => { eprintln!("query error: {}", e); return; }
    };

    // Build param list: ?1=query, ?2=limit, ?3..=extra filters
    let mut param_refs: Vec<&dyn rusqlite::types::ToSql> = Vec::new();
    let query_str = query.to_string();
    let limit_val = limit as i64;
    param_refs.push(&query_str);
    param_refs.push(&limit_val);
    for p in extra_params {
        param_refs.push(p.as_ref());
    }

    match stmt.query_map(param_refs.as_slice(), |row| {
        Ok(build_event_result(row, dated, now))
    }) {
        Ok(rows) => {
            for r in rows.flatten() {
                results.push(r);
            }
        }
        Err(e) => eprintln!("search error: {}", e),
    }
}

/// Composite score: 0.4 * relevance + 0.3 * importance_norm + 0.3 * recency
fn composite_score(fts_rank: f64, importance: i64, datetime: Option<&str>, now: &str) -> f64 {
    // FTS5 rank is negative (more negative = more relevant). Normalize to 0..1
    let relevance = 1.0 / (1.0 + fts_rank.abs());

    // Importance normalized to 0..1
    let importance_norm = importance as f64 / 10.0;

    // Recency: exponential decay, half-life 30 days
    let recency = match datetime {
        Some(dt) => {
            let dt_date = &dt[..10.min(dt.len())];
            let now_date = &now[..10.min(now.len())];
            // Simple day difference via string comparison (ISO dates sort lexicographically)
            let days = date_diff_days(dt_date, now_date).abs() as f64;
            let half_life = 30.0;
            (-days * 2.0_f64.ln() / half_life).exp()
        }
        None => 0.5, // undated gets neutral recency
    };

    0.4 * relevance + 0.3 * importance_norm + 0.3 * recency
}

/// Approximate day difference between two ISO dates (YYYY-MM-DD).
fn date_diff_days(a: &str, b: &str) -> i64 {
    let parse = |s: &str| -> Option<i64> {
        let parts: Vec<&str> = s.split('-').collect();
        if parts.len() != 3 { return None; }
        let y: i64 = parts[0].parse().ok()?;
        let m: i64 = parts[1].parse().ok()?;
        let d: i64 = parts[2].parse().ok()?;
        Some(y * 365 + m * 30 + d) // rough approximation, good enough for scoring
    };
    match (parse(a), parse(b)) {
        (Some(da), Some(db)) => da - db,
        _ => 0,
    }
}

fn build_event_result(row: &Row, dated: bool, now: &str) -> RecallResult {
    let id: i64 = row.get(0).unwrap_or(0);
    let fts_rank: f64 = row.get(1).unwrap_or(0.0);
    let mut fields = Vec::new();

    let event: String = row.get(2).unwrap_or_default();
    fields.push(("What".into(), event));

    let datetime: Option<String> = if dated {
        let dt: Option<String> = row.get(3).ok();
        if let Some(ref dt) = dt {
            fields.push(("When".into(), dt.clone()));
        }
        dt
    } else {
        None
    };

    let labels = &["Note", "Tags", "Importance", "Emotion", "Location", "People", "Source"];
    let start = if dated { 4 } else { 3 };
    let mut importance: i64 = 5;

    for (i, label) in labels.iter().enumerate() {
        let col = start + i;
        if *label == "Importance" {
            if let Ok(v) = row.get::<_, i64>(col) {
                importance = v;
                fields.push((label.to_string(), v.to_string()));
            }
        } else if let Ok(v) = row.get::<_, String>(col) {
            if !v.is_empty() {
                fields.push((label.to_string(), v));
            }
        }
    }

    let score = composite_score(fts_rank, importance, datetime.as_deref(), now);
    let prefix = if dated { "E" } else { "U" };
    RecallResult {
        memory_type: if dated { "event".into() } else { "event (undated)".into() },
        mid: memory_id(prefix, id),
        score,
        fields,
    }
}

/// Delete a memory by its universal ID (e.g. "E3", "U1").
pub fn forget(conn: &Connection, mid: &str) -> Result<bool> {
    let (prefix, row_id) = parse_memory_id(mid)
        .ok_or_else(|| rusqlite::Error::InvalidParameterName(format!("invalid memory ID: {}", mid)))?;
    let deleted = match prefix.as_str() {
        "E" => conn.execute("DELETE FROM events WHERE id = ?1", [row_id])?,
        "U" => conn.execute("DELETE FROM events_undated WHERE id = ?1", [row_id])?,
        _ => return Err(rusqlite::Error::InvalidParameterName(format!("unknown prefix: {}", prefix))),
    };
    Ok(deleted > 0)
}

/// Alter fields of a memory by its universal ID.
/// Takes a list of (field_name, new_value) pairs.
pub fn alter(conn: &Connection, mid: &str, changes: &[(String, String)]) -> Result<bool> {
    if changes.is_empty() {
        return Ok(false);
    }
    let (prefix, row_id) = parse_memory_id(mid)
        .ok_or_else(|| rusqlite::Error::InvalidParameterName(format!("invalid memory ID: {}", mid)))?;
    let table = match prefix.as_str() {
        "E" => "events",
        "U" => "events_undated",
        _ => return Err(rusqlite::Error::InvalidParameterName(format!("unknown prefix: {}", prefix))),
    };

    let valid_fields_dated = [
        "event", "datetime", "note", "tags", "importance",
        "emotion", "location", "people", "source",
    ];
    let valid_fields_undated = [
        "event", "note", "tags", "importance",
        "emotion", "location", "people", "source",
    ];
    let valid = if prefix == "E" { &valid_fields_dated[..] } else { &valid_fields_undated[..] };

    let mut set_clauses = Vec::new();
    let mut values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

    for (field, value) in changes {
        if !valid.contains(&field.as_str()) {
            return Err(rusqlite::Error::InvalidParameterName(format!("invalid field: {}", field)));
        }
        set_clauses.push(format!("{} = ?", field));
        values.push(Box::new(value.clone()));
    }
    values.push(Box::new(row_id));

    let sql = format!(
        "UPDATE {} SET {} WHERE id = ?",
        table,
        set_clauses.join(", ")
    );
    let params: Vec<&dyn rusqlite::types::ToSql> = values.iter().map(|v| v.as_ref()).collect();
    let updated = conn.execute(&sql, params.as_slice())?;
    Ok(updated > 0)
}

// --- Connection Engine ---

pub enum ConnectionKind {
    Direct { mid: String },
    Shared { mid_a: String, mid_b: String, field: String, value: String },
    Bridge { mid_a: String, mid_b: String, via_field: String, via_value: String },
}

pub struct Connection_ {
    pub kind: ConnectionKind,
    pub score: f64,
    pub fields: Vec<(String, String)>,
}

pub struct ConnectionResult {
    pub connections: Vec<Connection_>,
    pub elapsed_ms: u128,
}

/// A memory row with its fields extracted for comparison.
struct MemRow {
    mid: String,
    fields: HashMap<String, String>,
    importance: i64,
}

const LINKABLE_FIELDS: &[&str] = &["tags", "emotion", "location", "people"];

pub fn find_connections(
    conn: &rusqlite::Connection,
    concepts: &[String],
    min_importance: Option<u8>,
    timeout: Duration,
) -> ConnectionResult {
    let start = Instant::now();
    let mut connections: Vec<Connection_> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    // Phase 1: Direct - FTS5 AND query
    let and_query = concepts.iter()
        .map(|c| format!("\"{}\"", c.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" AND ");

    phase_direct(conn, &and_query, min_importance, &mut connections, &mut seen);

    if start.elapsed() >= timeout {
        return ConnectionResult { connections, elapsed_ms: start.elapsed().as_millis() };
    }

    // Phase 2: Shared attributes - search each concept, cross-match fields
    let mut concept_rows: Vec<Vec<MemRow>> = Vec::new();
    for concept in concepts {
        let escaped = format!("\"{}\"", concept.replace('"', "\"\""));
        let rows = search_mem_rows(conn, &escaped, min_importance);
        concept_rows.push(rows);

        if start.elapsed() >= timeout {
            return ConnectionResult { connections, elapsed_ms: start.elapsed().as_millis() };
        }
    }

    phase_shared(&concept_rows, &mut connections, &mut seen);

    if start.elapsed() >= timeout {
        return ConnectionResult { connections, elapsed_ms: start.elapsed().as_millis() };
    }

    // Phase 3: Bridge - one-hop through field values
    phase_bridge(conn, concepts, &concept_rows, min_importance, timeout, start, &mut connections, &mut seen);

    ConnectionResult { connections, elapsed_ms: start.elapsed().as_millis() }
}

struct DirectHit {
    mid: String,
    importance: i64,
    fields: Vec<(String, String)>,
}

fn phase_direct(
    conn: &rusqlite::Connection,
    and_query: &str,
    min_importance: Option<u8>,
    connections: &mut Vec<Connection_>,
    seen: &mut HashSet<String>,
) {
    for (table, fts, prefix, dated) in &[
        ("events", "events_fts", "E", true),
        ("events_undated", "events_undated_fts", "U", false),
    ] {
        let min_clause = if min_importance.is_some() {
            format!(" AND t.importance >= {}", min_importance.unwrap())
        } else {
            String::new()
        };

        let sql = if *dated {
            format!(
                "SELECT t.id, t.event, t.datetime, t.importance
                 FROM {} f JOIN {} t ON t.id = f.rowid
                 WHERE {} MATCH ?1{}
                 LIMIT 20",
                fts, table, fts, min_clause
            )
        } else {
            format!(
                "SELECT t.id, t.event, t.importance
                 FROM {} f JOIN {} t ON t.id = f.rowid
                 WHERE {} MATCH ?1{}
                 LIMIT 20",
                fts, table, fts, min_clause
            )
        };

        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(e) => { eprintln!("phase_direct error: {}", e); continue; }
        };

        let hits: Vec<DirectHit> = match stmt.query_map([and_query], |row| {
            let id: i64 = row.get(0)?;
            let event: String = row.get(1)?;
            let mut fields = vec![("What".into(), event)];

            let imp: i64 = if *dated {
                if let Ok(dt) = row.get::<_, String>(2) {
                    fields.push(("When".into(), dt));
                }
                row.get(3).unwrap_or(5)
            } else {
                row.get(2).unwrap_or(5)
            };
            fields.push(("Importance".into(), imp.to_string()));

            Ok(DirectHit {
                mid: memory_id(prefix, id),
                importance: imp,
                fields,
            })
        }) {
            Ok(rows) => rows.flatten().collect(),
            Err(e) => { eprintln!("phase_direct search: {}", e); continue; }
        };

        for hit in hits {
            if seen.contains(&hit.mid) { continue; }
            seen.insert(hit.mid.clone());
            let score = (hit.importance as f64 / 10.0).max(0.5);
            connections.push(Connection_ {
                kind: ConnectionKind::Direct { mid: hit.mid },
                score,
                fields: hit.fields,
            });
        }
    }
}

fn search_mem_rows(
    conn: &rusqlite::Connection,
    fts_query: &str,
    min_importance: Option<u8>,
) -> Vec<MemRow> {
    let mut rows = Vec::new();

    for (table, fts, prefix, dated) in &[
        ("events", "events_fts", "E", true),
        ("events_undated", "events_undated_fts", "U", false),
    ] {
        let min_clause = if min_importance.is_some() {
            format!(" AND t.importance >= {}", min_importance.unwrap())
        } else {
            String::new()
        };

        let sql = if *dated {
            format!(
                "SELECT t.id, t.event, t.datetime, t.importance, t.tags, t.emotion, t.location, t.people
                 FROM {} f JOIN {} t ON t.id = f.rowid
                 WHERE {} MATCH ?1{}
                 LIMIT 50",
                fts, table, fts, min_clause
            )
        } else {
            format!(
                "SELECT t.id, t.event, t.importance, t.tags, t.emotion, t.location, t.people
                 FROM {} f JOIN {} t ON t.id = f.rowid
                 WHERE {} MATCH ?1{}
                 LIMIT 50",
                fts, table, fts, min_clause
            )
        };

        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => continue,
        };

        let hits: Vec<MemRow> = match stmt.query_map([fts_query], |row| {
            let id: i64 = row.get(0)?;
            let mid = memory_id(prefix, id);
            let mut field_map = HashMap::new();

            let imp: i64 = if *dated {
                if let Ok(dt) = row.get::<_, String>(2) {
                    field_map.insert("datetime".into(), dt);
                }
                let imp: i64 = row.get(3).unwrap_or(5);
                for (i, name) in ["tags", "emotion", "location", "people"].iter().enumerate() {
                    if let Ok(v) = row.get::<_, String>(4 + i) {
                        if !v.is_empty() { field_map.insert(name.to_string(), v); }
                    }
                }
                imp
            } else {
                let imp: i64 = row.get(2).unwrap_or(5);
                for (i, name) in ["tags", "emotion", "location", "people"].iter().enumerate() {
                    if let Ok(v) = row.get::<_, String>(3 + i) {
                        if !v.is_empty() { field_map.insert(name.to_string(), v); }
                    }
                }
                imp
            };

            Ok(MemRow { mid, fields: field_map, importance: imp })
        }) {
            Ok(r) => r.flatten().collect(),
            Err(_) => continue,
        };

        rows.extend(hits);
    }
    rows
}

fn phase_shared(
    concept_rows: &[Vec<MemRow>],
    connections: &mut Vec<Connection_>,
    seen: &mut HashSet<String>,
) {
    if concept_rows.len() < 2 { return; }

    // Compare each pair of concept result sets
    for i in 0..concept_rows.len() {
        for j in (i + 1)..concept_rows.len() {
            for row_a in &concept_rows[i] {
                for row_b in &concept_rows[j] {
                    if row_a.mid == row_b.mid { continue; }

                    let pair_key = if row_a.mid < row_b.mid {
                        format!("{}+{}", row_a.mid, row_b.mid)
                    } else {
                        format!("{}+{}", row_b.mid, row_a.mid)
                    };
                    if seen.contains(&pair_key) { continue; }

                    for field in LINKABLE_FIELDS {
                        let field_s = field.to_string();
                        if let (Some(va), Some(vb)) = (row_a.fields.get(&field_s), row_b.fields.get(&field_s)) {
                            // For comma-separated fields (tags, people), check overlap
                            let overlap = find_overlap(va, vb);
                            if let Some(shared_val) = overlap {
                                seen.insert(pair_key.clone());
                                let avg_imp = (row_a.importance + row_b.importance) as f64 / 20.0;
                                connections.push(Connection_ {
                                    kind: ConnectionKind::Shared {
                                        mid_a: row_a.mid.clone(),
                                        mid_b: row_b.mid.clone(),
                                        field: field_s,
                                        value: shared_val,
                                    },
                                    score: 0.7 * avg_imp.max(0.3),
                                    fields: Vec::new(),
                                });
                                break; // one link per pair
                            }
                        }
                    }
                }
            }
        }
    }
}

fn phase_bridge(
    conn: &rusqlite::Connection,
    _concepts: &[String],
    concept_rows: &[Vec<MemRow>],
    min_importance: Option<u8>,
    timeout: Duration,
    start: Instant,
    connections: &mut Vec<Connection_>,
    seen: &mut HashSet<String>,
) {
    if concept_rows.len() < 2 { return; }

    // For concept A's memories, extract field values and search them
    // Check if any result overlaps with concept B's memories
    let rows_a = &concept_rows[0];
    let rows_b_mids: HashSet<String> = concept_rows[1].iter().map(|r| r.mid.clone()).collect();

    for row_a in rows_a {
        if start.elapsed() >= timeout { return; }

        for field in LINKABLE_FIELDS {
            let field_s = field.to_string();
            if let Some(val) = row_a.fields.get(&field_s) {
                // Split comma-separated values and search each
                for token in val.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {
                    if start.elapsed() >= timeout { return; }

                    let escaped = format!("\"{}\"", token.replace('"', "\"\""));
                    let bridge_rows = search_mem_rows(conn, &escaped, min_importance);

                    for bridge in &bridge_rows {
                        if bridge.mid == row_a.mid { continue; }
                        if !rows_b_mids.contains(&bridge.mid) { continue; }

                        let pair_key = if row_a.mid < bridge.mid {
                            format!("{}~{}", row_a.mid, bridge.mid)
                        } else {
                            format!("{}~{}", bridge.mid, row_a.mid)
                        };
                        if seen.contains(&pair_key) { continue; }
                        seen.insert(pair_key);

                        let avg_imp = (row_a.importance + bridge.importance) as f64 / 20.0;
                        connections.push(Connection_ {
                            kind: ConnectionKind::Bridge {
                                mid_a: row_a.mid.clone(),
                                mid_b: bridge.mid.clone(),
                                via_field: field_s.clone(),
                                via_value: token.to_string(),
                            },
                            score: 0.5 * avg_imp.max(0.3),
                            fields: Vec::new(),
                        });
                    }
                }
            }
        }
    }
}

/// Find overlap between two comma-separated value strings.
fn find_overlap(a: &str, b: &str) -> Option<String> {
    let set_a: HashSet<&str> = a.split(',').map(|s| s.trim()).collect();
    let set_b: HashSet<&str> = b.split(',').map(|s| s.trim()).collect();
    let overlap: Vec<&&str> = set_a.intersection(&set_b).collect();
    if overlap.is_empty() {
        // For non-comma fields (emotion, location), exact match
        if a == b { Some(a.to_string()) } else { None }
    } else {
        Some(overlap.iter().map(|s| **s).collect::<Vec<_>>().join(","))
    }
}
