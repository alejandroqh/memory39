use rusqlite::{Connection, Result, params};

#[allow(clippy::too_many_arguments)]
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

#[allow(clippy::too_many_arguments)]
pub fn insert_thing(
    conn: &Connection,
    thing: &str,
    desc: Option<&str>,
    category: Option<&str>,
    tags: Option<&str>,
    importance: u8,
    emotion: Option<&str>,
    source: Option<&str>,
    confidence: u8,
    related: Option<&str>,
    created_at: &str,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO things (thing, desc, category, tags, importance, emotion, source, confidence, related, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![thing, desc, category, tags, importance, emotion, source, confidence, related, created_at],
    )?;
    Ok(conn.last_insert_rowid())
}

#[allow(clippy::too_many_arguments)]
pub fn insert_person(
    conn: &Connection,
    name: &str,
    role: Option<&str>,
    relationship: Option<&str>,
    contact: Option<&str>,
    met_at: Option<&str>,
    last_seen: Option<&str>,
    note: Option<&str>,
    tags: Option<&str>,
    importance: u8,
    emotion: Option<&str>,
    created_at: &str,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO persons (name, role, relationship, contact, met_at, last_seen, note, tags, importance, emotion, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![name, role, relationship, contact, met_at, last_seen, note, tags, importance, emotion, created_at],
    )?;
    Ok(conn.last_insert_rowid())
}

#[allow(clippy::too_many_arguments)]
pub fn insert_place(
    conn: &Connection,
    name: &str,
    desc: Option<&str>,
    address: Option<&str>,
    kind: Option<&str>,
    note: Option<&str>,
    tags: Option<&str>,
    importance: u8,
    emotion: Option<&str>,
    created_at: &str,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO places (name, desc, address, kind, note, tags, importance, emotion, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![name, desc, address, kind, note, tags, importance, emotion, created_at],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Memory ID prefix: E = events, U = events_undated, T = things, P = persons, L = places
pub fn memory_id(prefix: &str, row_id: i64) -> String {
    format!("{}{}", prefix, row_id)
}

/// Maps a memory ID prefix to its primary text field name for ALTER operations.
/// E/U → "event", T → "thing", P/L → "name"
pub fn text_field_for_id(mid: &str) -> &'static str {
    match mid.as_bytes().first() {
        Some(b'E') | Some(b'U') => "event",
        Some(b'T') => "thing",
        Some(b'P') | Some(b'L') => "name",
        _ => "event",
    }
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

/// Delete a memory by its universal ID (e.g. "E3", "U1").
pub fn forget(conn: &Connection, mid: &str) -> Result<bool> {
    let (prefix, row_id) = parse_memory_id(mid)
        .ok_or_else(|| rusqlite::Error::InvalidParameterName(format!("invalid memory ID: {}", mid)))?;
    let deleted = match prefix.as_str() {
        "E" => conn.execute("DELETE FROM events WHERE id = ?1", [row_id])?,
        "U" => conn.execute("DELETE FROM events_undated WHERE id = ?1", [row_id])?,
        "T" => conn.execute("DELETE FROM things WHERE id = ?1", [row_id])?,
        "P" => conn.execute("DELETE FROM persons WHERE id = ?1", [row_id])?,
        "L" => conn.execute("DELETE FROM places WHERE id = ?1", [row_id])?,
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
        "T" => "things",
        "P" => "persons",
        "L" => "places",
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
    let valid_fields_things = [
        "thing", "desc", "category", "tags", "importance",
        "emotion", "source", "confidence", "related",
    ];
    let valid_fields_persons = [
        "name", "role", "relationship", "contact", "met_at",
        "last_seen", "note", "tags", "importance", "emotion",
    ];
    let valid_fields_places = [
        "name", "desc", "address", "kind", "note",
        "tags", "importance", "emotion",
    ];
    let valid = match prefix.as_str() {
        "E" => &valid_fields_dated[..],
        "U" => &valid_fields_undated[..],
        "T" => &valid_fields_things[..],
        "P" => &valid_fields_persons[..],
        "L" => &valid_fields_places[..],
        _ => unreachable!(),
    };

    let mut set_clauses = Vec::new();
    let mut values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

    for (field, value) in changes {
        if !valid.contains(&field.as_str()) {
            return Err(rusqlite::Error::InvalidParameterName(format!("invalid field: {}", field)));
        }
        set_clauses.push(format!("`{}` = ?", field));
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
