use rusqlite::{Connection, Result, params, Row};
use std::path::Path;

pub fn open(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)?;
    conn.execute_batch(SCHEMA)?;
    Ok(conn)
}

const SCHEMA: &str = "
-- Dated events (past and future)
CREATE TABLE IF NOT EXISTS events (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    event      TEXT NOT NULL,
    datetime   TEXT NOT NULL,
    date       TEXT NOT NULL,
    note       TEXT,
    tags       TEXT,
    importance INTEGER NOT NULL DEFAULT 5,
    emotion    TEXT,
    location   TEXT,
    people     TEXT,
    source     TEXT,
    created_at TEXT NOT NULL
);

-- Date segmentation: fast day/range queries
CREATE INDEX IF NOT EXISTS idx_events_date ON events(date);
CREATE INDEX IF NOT EXISTS idx_events_datetime ON events(datetime);
-- Sort by importance within a date range
CREATE INDEX IF NOT EXISTS idx_events_date_importance ON events(date, importance DESC);
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
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
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
    date: Option<&str>,
    note: Option<&str>,
    tags: Option<&str>,
    importance: u8,
    emotion: Option<&str>,
    location: Option<&str>,
    people: Option<&str>,
    source: Option<&str>,
    created_at: &str,
) -> Result<i64> {
    match (datetime, date) {
        (Some(dt), Some(d)) => {
            conn.execute(
                "INSERT INTO events (event, datetime, date, note, tags, importance, emotion, location, people, source, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![event, dt, d, note, tags, importance, emotion, location, people, source, created_at],
            )?;
            Ok(conn.last_insert_rowid())
        }
        _ => {
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

/// A single recall result with its type and fields.
pub struct RecallResult {
    pub memory_type: String,
    pub mid: String,
    pub rank: f64,
    pub fields: Vec<(String, String)>,
}

/// Search across all memory tables. Returns results sorted by FTS5 rank.
pub fn recall(conn: &Connection, query: &str, limit: usize) -> Vec<RecallResult> {
    let mut results = Vec::new();

    recall_fts(
        conn, &mut results, limit,
        "SELECT e.id, rank, e.event, e.datetime, e.note, e.tags, e.importance,
                e.emotion, e.location, e.people, e.source
         FROM events_fts f
         JOIN events e ON e.id = f.rowid
         WHERE events_fts MATCH ?1
         ORDER BY rank
         LIMIT ?2",
        query, true,
    );

    recall_fts(
        conn, &mut results, limit,
        "SELECT u.id, rank, u.event, u.note, u.tags, u.importance,
                u.emotion, u.location, u.people, u.source
         FROM events_undated_fts f
         JOIN events_undated u ON u.id = f.rowid
         WHERE events_undated_fts MATCH ?1
         ORDER BY rank
         LIMIT ?2",
        query, false,
    );

    // Sort all results by rank (FTS5 rank is negative, closer to 0 = better)
    results.sort_by(|a, b| a.rank.partial_cmp(&b.rank).unwrap_or(std::cmp::Ordering::Equal));
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
) {
    let mut stmt = match conn.prepare(sql) {
        Ok(s) => s,
        Err(e) => { eprintln!("query error: {}", e); return; }
    };
    match stmt.query_map(params![query, limit as i64], |row| {
        Ok(build_event_result(row, dated))
    }) {
        Ok(rows) => {
            for r in rows.flatten() {
                results.push(r);
            }
        }
        Err(e) => eprintln!("search error: {}", e),
    }
}

fn build_event_result(row: &Row, dated: bool) -> RecallResult {
    let id: i64 = row.get(0).unwrap_or(0);
    let rank: f64 = row.get(1).unwrap_or(0.0);
    let mut fields = Vec::new();

    let event: String = row.get(2).unwrap_or_default();
    fields.push(("What".into(), event));

    if dated {
        if let Ok(dt) = row.get::<_, String>(3) {
            fields.push(("When".into(), dt));
        }
    }

    let labels = &["Note", "Tags", "Importance", "Emotion", "Location", "People", "Source"];
    let start = if dated { 4 } else { 3 };

    for (i, label) in labels.iter().enumerate() {
        let col = start + i;
        if *label == "Importance" {
            if let Ok(v) = row.get::<_, i64>(col) {
                fields.push((label.to_string(), v.to_string()));
            }
        } else if let Ok(v) = row.get::<_, String>(col) {
            if !v.is_empty() {
                fields.push((label.to_string(), v));
            }
        }
    }

    let prefix = if dated { "E" } else { "U" };
    RecallResult {
        memory_type: if dated { "event".into() } else { "event (undated)".into() },
        mid: memory_id(prefix, id),
        rank,
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
        "event", "datetime", "date", "note", "tags", "importance",
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
