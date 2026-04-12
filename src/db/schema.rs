use rusqlite::{Connection, Result};

pub(crate) const FTS_TABLES: &[(&str, &[&str])] = &[
    ("events_fts",         &["events_ai", "events_ad", "events_au"]),
    ("events_undated_fts", &["events_undated_ai", "events_undated_ad", "events_undated_au"]),
    ("things_fts",         &["things_ai", "things_ad", "things_au"]),
    ("persons_fts",        &["persons_ai", "persons_ad", "persons_au"]),
    ("places_fts",         &["places_ai", "places_ad", "places_au"]),
];

/// Migrate FTS5 tables to use unicode61 with diacritics removal.
/// Checks each table's DDL in sqlite_master; if it lacks 'remove_diacritics',
/// drops and recreates it from SCHEMA, then rebuilds the index.
pub(crate) fn migrate_fts_tokenizer(conn: &Connection) -> Result<()> {
    let mut to_migrate: Vec<(&str, &[&str])> = Vec::new();

    for (fts_table, triggers) in FTS_TABLES {
        let needs_migration = match conn.query_row(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name=?1",
            [fts_table],
            |row| row.get::<_, String>(0),
        ) {
            Ok(sql) => !sql.contains("remove_diacritics"),
            Err(_) => false,
        };
        if needs_migration {
            to_migrate.push((fts_table, triggers));
        }
    }

    if to_migrate.is_empty() {
        return Ok(());
    }

    for (fts_table, triggers) in &to_migrate {
        for trigger in *triggers {
            conn.execute_batch(&format!("DROP TRIGGER IF EXISTS {}", trigger))?;
        }
        conn.execute_batch(&format!("DROP TABLE IF EXISTS {}", fts_table))?;
    }

    conn.execute_batch(SCHEMA)?;

    for (fts_table, _) in &to_migrate {
        conn.execute_batch(&format!(
            "INSERT INTO {}({}) VALUES('rebuild')", fts_table, fts_table
        ))?;
    }

    Ok(())
}

/// Migrate things_fts to remove the `related` column from FTS index.
/// `related` is structural metadata for connect, not search content.
pub(crate) fn migrate_things_fts_drop_related(conn: &Connection) -> Result<()> {
    let has_related = match conn.query_row(
        "SELECT sql FROM sqlite_master WHERE type='table' AND name='things_fts'",
        [],
        |row| row.get::<_, String>(0),
    ) {
        Ok(sql) => sql.contains("related"),
        Err(_) => false,
    };
    if !has_related {
        return Ok(());
    }
    for trigger in &["things_ai", "things_ad", "things_au"] {
        conn.execute_batch(&format!("DROP TRIGGER IF EXISTS {}", trigger))?;
    }
    conn.execute_batch("DROP TABLE IF EXISTS things_fts")?;
    conn.execute_batch(SCHEMA)?;
    conn.execute_batch("INSERT INTO things_fts(things_fts) VALUES('rebuild')")?;
    Ok(())
}

pub(crate) const PREFIX_MIN_LEN: usize = 6;

/// Expand a query for prefix matching: truncate words > min_len chars and add '*'.
/// Returns None if query uses FTS5 operators or no words were expanded.
pub(crate) fn expand_query_for_prefix(query: &str) -> Option<String> {
    // Don't mangle queries with explicit FTS5 syntax
    if query.contains('"') || query.contains('*') || query.contains('(')
        || query.contains(')') || query.contains(':')
    {
        return None;
    }
    let upper = query.to_uppercase();
    if upper.contains(" AND ") || upper.contains(" OR ") || upper.contains(" NOT ")
        || upper.contains("NEAR")
    {
        return None;
    }

    let terms: Vec<String> = query.split_whitespace().map(|w| {
        if w.chars().count() > PREFIX_MIN_LEN {
            let prefix: String = w.chars().take(PREFIX_MIN_LEN).collect();
            format!("{}*", prefix)
        } else {
            w.to_string()
        }
    }).collect();

    if terms.iter().any(|t| t.ends_with('*')) { Some(terms.join(" ")) } else { None }
}

pub(crate) const SCHEMA: &str = "
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
    content_rowid=id,
    tokenize='unicode61 remove_diacritics 2'
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
    content_rowid=id,
    tokenize='unicode61 remove_diacritics 2'
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

-- Things (objects, concepts, items worth remembering)
CREATE TABLE IF NOT EXISTS things (
    id         INTEGER PRIMARY KEY,
    thing      TEXT NOT NULL,
    desc       TEXT,
    category   TEXT,
    tags       TEXT,
    importance INTEGER NOT NULL DEFAULT 5,
    emotion    TEXT,
    source     TEXT,
    confidence INTEGER NOT NULL DEFAULT 7,
    related    TEXT,
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_things_importance ON things(importance DESC);

CREATE VIRTUAL TABLE IF NOT EXISTS things_fts USING fts5(
    thing, desc, category, tags, emotion,
    content=things,
    content_rowid=id,
    tokenize='unicode61 remove_diacritics 2'
);

CREATE TRIGGER IF NOT EXISTS things_ai AFTER INSERT ON things BEGIN
    INSERT INTO things_fts(rowid, thing, desc, category, tags, emotion)
    VALUES (new.id, new.thing, new.desc, new.category, new.tags, new.emotion);
END;
CREATE TRIGGER IF NOT EXISTS things_ad AFTER DELETE ON things BEGIN
    INSERT INTO things_fts(things_fts, rowid, thing, desc, category, tags, emotion)
    VALUES ('delete', old.id, old.thing, old.desc, old.category, old.tags, old.emotion);
END;
CREATE TRIGGER IF NOT EXISTS things_au AFTER UPDATE ON things BEGIN
    INSERT INTO things_fts(things_fts, rowid, thing, desc, category, tags, emotion)
    VALUES ('delete', old.id, old.thing, old.desc, old.category, old.tags, old.emotion);
    INSERT INTO things_fts(rowid, thing, desc, category, tags, emotion)
    VALUES (new.id, new.thing, new.desc, new.category, new.tags, new.emotion);
END;

-- Persons (people worth remembering)
CREATE TABLE IF NOT EXISTS persons (
    id           INTEGER PRIMARY KEY,
    name         TEXT NOT NULL,
    role         TEXT,
    relationship TEXT,
    contact      TEXT,
    met_at       TEXT,
    last_seen    TEXT,
    note         TEXT,
    tags         TEXT,
    importance   INTEGER NOT NULL DEFAULT 5,
    emotion      TEXT,
    created_at   TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_persons_importance ON persons(importance DESC);

CREATE VIRTUAL TABLE IF NOT EXISTS persons_fts USING fts5(
    name, role, relationship, note, tags, emotion,
    content=persons,
    content_rowid=id,
    tokenize='unicode61 remove_diacritics 2'
);

CREATE TRIGGER IF NOT EXISTS persons_ai AFTER INSERT ON persons BEGIN
    INSERT INTO persons_fts(rowid, name, role, relationship, note, tags, emotion)
    VALUES (new.id, new.name, new.role, new.relationship, new.note, new.tags, new.emotion);
END;
CREATE TRIGGER IF NOT EXISTS persons_ad AFTER DELETE ON persons BEGIN
    INSERT INTO persons_fts(persons_fts, rowid, name, role, relationship, note, tags, emotion)
    VALUES ('delete', old.id, old.name, old.role, old.relationship, old.note, old.tags, old.emotion);
END;
CREATE TRIGGER IF NOT EXISTS persons_au AFTER UPDATE ON persons BEGIN
    INSERT INTO persons_fts(persons_fts, rowid, name, role, relationship, note, tags, emotion)
    VALUES ('delete', old.id, old.name, old.role, old.relationship, old.note, old.tags, old.emotion);
    INSERT INTO persons_fts(rowid, name, role, relationship, note, tags, emotion)
    VALUES (new.id, new.name, new.role, new.relationship, new.note, new.tags, new.emotion);
END;

-- Places (locations worth remembering)
CREATE TABLE IF NOT EXISTS places (
    id         INTEGER PRIMARY KEY,
    name       TEXT NOT NULL,
    desc       TEXT,
    address    TEXT,
    kind       TEXT,
    note       TEXT,
    tags       TEXT,
    importance INTEGER NOT NULL DEFAULT 5,
    emotion    TEXT,
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_places_importance ON places(importance DESC);

CREATE VIRTUAL TABLE IF NOT EXISTS places_fts USING fts5(
    name, desc, address, kind, note, tags, emotion,
    content=places,
    content_rowid=id,
    tokenize='unicode61 remove_diacritics 2'
);

CREATE TRIGGER IF NOT EXISTS places_ai AFTER INSERT ON places BEGIN
    INSERT INTO places_fts(rowid, name, desc, address, kind, note, tags, emotion)
    VALUES (new.id, new.name, new.desc, new.address, new.kind, new.note, new.tags, new.emotion);
END;
CREATE TRIGGER IF NOT EXISTS places_ad AFTER DELETE ON places BEGIN
    INSERT INTO places_fts(places_fts, rowid, name, desc, address, kind, note, tags, emotion)
    VALUES ('delete', old.id, old.name, old.desc, old.address, old.kind, old.note, old.tags, old.emotion);
END;
CREATE TRIGGER IF NOT EXISTS places_au AFTER UPDATE ON places BEGIN
    INSERT INTO places_fts(places_fts, rowid, name, desc, address, kind, note, tags, emotion)
    VALUES ('delete', old.id, old.name, old.desc, old.address, old.kind, old.note, old.tags, old.emotion);
    INSERT INTO places_fts(rowid, name, desc, address, kind, note, tags, emotion)
    VALUES (new.id, new.name, new.desc, new.address, new.kind, new.note, new.tags, new.emotion);
END;
";
