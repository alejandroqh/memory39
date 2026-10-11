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
    match mid.as_bytes().first().map(u8::to_ascii_uppercase) {
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
            return Err(rusqlite::Error::InvalidParameterName(format!(
                "invalid field for {}: {} (valid: {})", mid, field, valid.join(", ")
            )));
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

/// User-facing `alter` parameters, shared by the CLI and the MCP server.
/// `text` and `note` are generic: `alter_changes` maps them to each type's column.
#[derive(Debug, Default)]
pub struct AlterFields {
    pub text: Option<String>,
    pub note: Option<String>,
    pub desc: Option<String>,
    pub tags: Option<String>,
    pub importance: Option<u8>,
    pub emotion: Option<String>,
    pub source: Option<String>,
    pub location: Option<String>,
    pub people: Option<String>,
    pub date: Option<String>,
    pub time: Option<String>,
    pub category: Option<String>,
    pub confidence: Option<u8>,
    pub related: Option<String>,
    pub role: Option<String>,
    pub relationship: Option<String>,
    pub contact: Option<String>,
    pub met_at: Option<String>,
    pub last_seen: Option<String>,
    pub address: Option<String>,
    pub kind: Option<String>,
}

// `alter` parameters each memory type accepts, named as in `AlterFields`.
const ALTER_DATED: &[&str] = &[
    "text", "note", "tags", "importance", "emotion", "location", "people", "source", "date", "time",
];
const ALTER_UNDATED: &[&str] = &[
    "text", "note", "tags", "importance", "emotion", "location", "people", "source",
];
const ALTER_THINGS: &[&str] = &[
    "text", "desc", "note", "category", "tags", "importance", "emotion", "source", "confidence", "related",
];
const ALTER_PERSONS: &[&str] = &[
    "text", "role", "relationship", "contact", "met_at", "last_seen", "note", "tags", "importance", "emotion",
];
const ALTER_PLACES: &[&str] = &[
    "text", "desc", "address", "kind", "note", "tags", "importance", "emotion",
];

/// Turn user-facing `alter` parameters into `(column, value)` pairs for `alter`.
/// Parameters that don't apply to the memory's type are rejected with the valid
/// ones listed, so callers never see raw column names like `datetime`.
/// On things, `note` is an alias of `desc` (things have no note column).
pub fn alter_changes(mid: &str, fields: AlterFields) -> std::result::Result<Vec<(String, String)>, String> {
    let invalid_id = || format!("invalid memory ID: {} (expected e.g. E3, U1, T2, P1, L4)", mid);
    let (prefix, _) = parse_memory_id(mid).ok_or_else(invalid_id)?;
    let (kind_name, valid) = match prefix.as_str() {
        "E" => ("dated events", ALTER_DATED),
        "U" => ("undated events", ALTER_UNDATED),
        "T" => ("things", ALTER_THINGS),
        "P" => ("persons", ALTER_PERSONS),
        "L" => ("places", ALTER_PLACES),
        _ => return Err(invalid_id()),
    };

    let AlterFields {
        text, note, desc, tags, importance, emotion, source, location, people, date, time,
        category, confidence, related, role, relationship, contact, met_at, last_seen, address, kind,
    } = fields;
    let given = [
        ("text", text), ("note", note), ("desc", desc), ("tags", tags),
        ("importance", importance.map(|v| v.to_string())), ("emotion", emotion),
        ("source", source), ("location", location), ("people", people),
        ("date", date), ("time", time), ("category", category),
        ("confidence", confidence.map(|v| v.to_string())), ("related", related),
        ("role", role), ("relationship", relationship), ("contact", contact),
        ("met_at", met_at), ("last_seen", last_seen), ("address", address), ("kind", kind),
    ];

    let mut changes: Vec<(String, String)> = Vec::new();
    let (mut date, mut time) = (None, None);
    for (param, value) in given {
        let Some(value) = value else { continue };
        if !valid.contains(&param) {
            let mut msg = format!(
                "{}: '{}' does not apply to {}. Valid fields: {}",
                mid, param, kind_name, valid.join(", ")
            );
            if prefix == "U" && param == "date" {
                msg.push_str(". To date an undated event, forget it and store it again with a date");
            }
            return Err(msg);
        }
        match param {
            "date" => date = Some(value),
            "time" => time = Some(value),
            _ => {
                let column = match param {
                    "text" => text_field_for_id(&prefix),
                    "note" if prefix == "T" => "desc",
                    other => other,
                };
                // Only the note alias can collide with another parameter's column.
                if changes.iter().any(|(c, _)| c == column) {
                    return Err(format!("{}: 'note' and 'desc' both set a thing's description; pass only one", mid));
                }
                changes.push((column.to_string(), value));
            }
        }
    }
    match (date, time) {
        (Some(d), t) => changes.push(("datetime".into(), format!("{} {}", d, t.as_deref().unwrap_or("00:00")))),
        (None, Some(_)) => return Err(format!("{}: 'time' requires 'date' (pass both to change the datetime)", mid)),
        (None, None) => {}
    }
    Ok(changes)
}
