mod schema;
mod crud;
mod recall;
mod connect;
mod bloom;

use rusqlite::{Connection, Result};
use std::path::{Path, PathBuf};

pub use bloom::MemoryDb;
pub use crud::{insert_event, insert_thing, insert_person, insert_place, memory_id, parse_memory_id, text_field_for_id, forget, alter, AlterFields, alter_changes};
pub use recall::{RecallFilters, RecallResult, recall};
pub use connect::{ConnectionKind, Connection_, ConnectionResult, find_connections};

/// Resolve the DB path. Precedence:
/// (1) explicit `override_path` (e.g., a `--db` flag);
/// (2) `MEMORY39_DB` env var (supports a leading `~/`);
/// (3) default `~/.memory39/memory39.db`.
/// The parent directory is created so the first run doesn't fail.
pub fn resolve_path(override_path: Option<&Path>) -> std::io::Result<PathBuf> {
    let path = pick_path(override_path)?;
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    Ok(path)
}

fn pick_path(override_path: Option<&Path>) -> std::io::Result<PathBuf> {
    if let Some(p) = override_path {
        return Ok(p.to_path_buf());
    }
    if let Ok(raw) = std::env::var("MEMORY39_DB") {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return Ok(expand_tilde(trimmed));
        }
    }
    let home = dirs::home_dir().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "home directory unknown")
    })?;
    Ok(home.join(".memory39").join("memory39.db"))
}

fn expand_tilde(s: &str) -> PathBuf {
    if let Some(rest) = s.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    PathBuf::from(s)
}

fn open_conn(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)?;

    conn.execute_batch("
        PRAGMA journal_mode = WAL;
        PRAGMA synchronous = NORMAL;
        PRAGMA cache_size = -8000;
        PRAGMA mmap_size = 67108864;
        PRAGMA foreign_keys = ON;
    ")?;

    conn.execute_batch(schema::SCHEMA)?;
    schema::migrate_fts_tokenizer(&conn)?;
    schema::migrate_things_fts_drop_related(&conn)?;
    Ok(conn)
}

pub fn open(path: &Path) -> Result<MemoryDb> {
    let conn = open_conn(path)?;
    Ok(MemoryDb::new(conn, Some(path)))
}

pub fn open_ram() -> Result<MemoryDb> {
    let conn = Connection::open_in_memory()?;
    conn.execute_batch("
        PRAGMA synchronous = OFF;
        PRAGMA cache_size = -16000;
        PRAGMA temp_store = MEMORY;
    ")?;
    conn.execute_batch(schema::SCHEMA)?;
    Ok(MemoryDb::new_ram(conn))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("
            PRAGMA synchronous = OFF;
            PRAGMA cache_size = -16000;
            PRAGMA temp_store = MEMORY;
        ").unwrap();
        conn.execute_batch(schema::SCHEMA).unwrap();
        conn
    }

    fn ts() -> String {
        "2026-04-11 12:00".to_string()
    }

    // --- expand_query_for_prefix ---

    #[test]
    fn test_prefix_expansion_long_words() {
        let result = schema::expand_query_for_prefix("desarrollando proyecto");
        assert_eq!(result, Some("desarr* proyec*".to_string()));
    }

    #[test]
    fn test_prefix_expansion_short_words_unchanged() {
        assert_eq!(schema::expand_query_for_prefix("café"), None);
        assert_eq!(schema::expand_query_for_prefix("hola mundo"), None);
    }

    #[test]
    fn test_prefix_expansion_mixed() {
        let result = schema::expand_query_for_prefix("el desarrollo");
        assert_eq!(result, Some("el desarr*".to_string()));
    }

    #[test]
    fn test_prefix_expansion_skips_fts5_syntax() {
        assert_eq!(schema::expand_query_for_prefix("\"exact phrase\""), None);
        assert_eq!(schema::expand_query_for_prefix("hello AND world"), None);
        assert_eq!(schema::expand_query_for_prefix("prefix*"), None);
    }

    // --- diacritics removal ---

    #[test]
    fn test_diacritics_match() {
        let conn = test_db();
        insert_thing(&conn, "café con leche", None, None, None, 7, None, None, 5, None, &ts()).unwrap();

        let filters = RecallFilters { min_importance: None, date_from: None, date_to: None, memory_type: None, source: None };
        let results = recall(&conn, "cafe", 10, 0, &filters);
        assert!(!results.is_empty(), "café should match cafe");
    }

    #[test]
    fn test_diacritics_spanish_accents() {
        let conn = test_db();
        insert_event(&conn, "reunión con María", Some("2026-04-11 10:00"), None, None, 6, None, None, None, None, &ts()).unwrap();

        let filters = RecallFilters { min_importance: None, date_from: None, date_to: None, memory_type: None, source: None };
        let results = recall(&conn, "reunion maria", 10, 0, &filters);
        assert!(!results.is_empty(), "reunión/María should match reunion/maria");
    }

    // --- prefix fallback for Spanish morphology ---

    #[test]
    fn test_prefix_fallback_spanish() {
        let conn = test_db();
        insert_event(&conn, "desarrollo de software", Some("2026-04-01 09:00"), None,
            Some("programación,rust"), 8, None, None, None, None, &ts()).unwrap();

        let filters = RecallFilters { min_importance: None, date_from: None, date_to: None, memory_type: None, source: None };
        let results = recall(&conn, "desarrollando", 10, 0, &filters);
        assert!(!results.is_empty(), "desarrollando should match desarrollo via prefix fallback");
    }

    #[test]
    fn test_exact_match_preferred_over_prefix() {
        let conn = test_db();
        insert_thing(&conn, "desarrollo ágil", None, None, None, 7, None, None, 5, None, &ts()).unwrap();
        insert_thing(&conn, "desarraigo cultural", None, None, None, 7, None, None, 5, None, &ts()).unwrap();

        let filters = RecallFilters { min_importance: None, date_from: None, date_to: None, memory_type: None, source: None };
        let results = recall(&conn, "desarrollo", 10, 0, &filters);
        assert!(!results.is_empty());
        assert!(results[0].fields.iter().any(|(_, v)| v.contains("desarrollo")),
            "exact match should rank first");
    }

    // --- migration ---

    #[test]
    fn test_migration_idempotent() {
        let mdb = open_ram().unwrap();
        schema::migrate_fts_tokenizer(mdb.conn()).unwrap();
        insert_event(mdb.conn(), "test event", None, None, None, 5, None, None, None, None, &ts()).unwrap();
    }

    #[test]
    fn test_recall_with_no_results_no_panic() {
        let conn = test_db();
        let filters = RecallFilters { min_importance: None, date_from: None, date_to: None, memory_type: None, source: None };
        let results = recall(&conn, "nonexistent query", 10, 0, &filters);
        assert!(results.is_empty());
    }

    // --- alter: user-facing parameters ---

    fn alter_with(conn: &rusqlite::Connection, mid: &str, fields: AlterFields) -> bool {
        let changes = alter_changes(mid, fields).unwrap();
        alter(conn, mid, &changes).unwrap()
    }

    fn column(conn: &rusqlite::Connection, table: &str, col: &str, id: i64) -> String {
        conn.query_row(&format!("SELECT \"{}\" FROM {} WHERE id = ?1", col, table), [id], |r| r.get(0)).unwrap()
    }

    #[test]
    fn test_alter_note_on_thing_sets_desc() {
        let conn = test_db();
        insert_thing(&conn, "budget", None, None, None, 5, None, None, 5, None, &ts()).unwrap();
        assert!(alter_with(&conn, "T1", AlterFields { note: Some("monthly savings plan".into()), ..Default::default() }));
        assert_eq!(column(&conn, "things", "desc", 1), "monthly savings plan");

        let filters = RecallFilters { min_importance: None, date_from: None, date_to: None, memory_type: None, source: None };
        assert!(!recall(&conn, "savings", 10, 0, &filters).is_empty(), "altered desc should be searchable");
    }

    #[test]
    fn test_alter_note_stays_note_outside_things() {
        for mid in ["E1", "U1", "P1", "L1"] {
            let changes = alter_changes(mid, AlterFields { note: Some("n".into()), ..Default::default() }).unwrap();
            assert_eq!(changes, vec![("note".to_string(), "n".to_string())], "{mid}");
        }
    }

    #[test]
    fn test_alter_place_fields() {
        let conn = test_db();
        insert_place(&conn, "Blue Bottle", None, None, None, None, None, 5, None, &ts()).unwrap();
        assert!(alter_with(&conn, "L1", AlterFields {
            desc: Some("quiet corner".into()),
            address: Some("123 Main St".into()),
            kind: Some("building".into()),
            ..Default::default()
        }));
        assert_eq!(column(&conn, "places", "desc", 1), "quiet corner");
        assert_eq!(column(&conn, "places", "address", 1), "123 Main St");
        assert_eq!(column(&conn, "places", "kind", 1), "building");
    }

    #[test]
    fn test_alter_thing_and_person_fields() {
        let conn = test_db();
        insert_thing(&conn, "rust 2024", None, None, None, 5, None, None, 5, None, &ts()).unwrap();
        assert!(alter_with(&conn, "T1", AlterFields {
            desc: Some("edition".into()),
            category: Some("programming".into()),
            confidence: Some(9),
            related: Some("rustc".into()),
            ..Default::default()
        }));
        assert_eq!(column(&conn, "things", "category", 1), "programming");
        assert_eq!(column(&conn, "things", "related", 1), "rustc");
        let confidence: i64 = conn.query_row("SELECT confidence FROM things WHERE id = 1", [], |r| r.get(0)).unwrap();
        assert_eq!(confidence, 9);

        insert_person(&conn, "Alice", None, None, None, None, None, None, None, 5, None, &ts()).unwrap();
        assert!(alter_with(&conn, "P1", AlterFields {
            role: Some("ML engineer".into()),
            relationship: Some("colleague".into()),
            contact: Some("alice@example.com".into()),
            met_at: Some("KubeCon".into()),
            last_seen: Some("2026-10-01".into()),
            ..Default::default()
        }));
        assert_eq!(column(&conn, "persons", "role", 1), "ML engineer");
        assert_eq!(column(&conn, "persons", "relationship", 1), "colleague");
        assert_eq!(column(&conn, "persons", "contact", 1), "alice@example.com");
        assert_eq!(column(&conn, "persons", "met_at", 1), "KubeCon");
        assert_eq!(column(&conn, "persons", "last_seen", 1), "2026-10-01");
    }

    #[test]
    fn test_alter_rejects_field_for_other_type() {
        let err = alter_changes("T1", AlterFields { location: Some("x".into()), ..Default::default() }).unwrap_err();
        assert!(err.contains("'location'") && err.contains("things"), "{err}");
        assert!(err.contains("Valid fields:") && err.contains("desc"), "{err}");

        let err = alter_changes("P1", AlterFields { date: Some("2026-01-01".into()), ..Default::default() }).unwrap_err();
        assert!(err.contains("'date'") && !err.contains("datetime"), "{err}");
    }

    #[test]
    fn test_alter_date_on_undated_explains() {
        let err = alter_changes("U1", AlterFields { date: Some("2026-01-01".into()), ..Default::default() }).unwrap_err();
        assert!(err.contains("'date'") && !err.contains("datetime"), "{err}");
        assert!(err.contains("forget it and store it again"), "{err}");
    }

    #[test]
    fn test_alter_date_and_time() {
        let changes = alter_changes("E1", AlterFields {
            date: Some("2026-01-02".into()),
            time: Some("10:30".into()),
            ..Default::default()
        }).unwrap();
        assert_eq!(changes, vec![("datetime".to_string(), "2026-01-02 10:30".to_string())]);

        let changes = alter_changes("E1", AlterFields { date: Some("2026-01-02".into()), ..Default::default() }).unwrap();
        assert_eq!(changes, vec![("datetime".to_string(), "2026-01-02 00:00".to_string())]);

        let err = alter_changes("E1", AlterFields { time: Some("10:30".into()), ..Default::default() }).unwrap_err();
        assert!(err.contains("'time' requires 'date'"), "{err}");
    }

    #[test]
    fn test_alter_note_and_desc_conflict_on_thing() {
        let err = alter_changes("T1", AlterFields {
            note: Some("a".into()),
            desc: Some("b".into()),
            ..Default::default()
        }).unwrap_err();
        assert!(err.contains("pass only one"), "{err}");
    }

    #[test]
    fn test_alter_lowercase_id() {
        assert_eq!(text_field_for_id("t1"), "thing");
        assert_eq!(text_field_for_id("p1"), "name");
        let changes = alter_changes("t1", AlterFields { text: Some("x".into()), ..Default::default() }).unwrap();
        assert_eq!(changes, vec![("thing".to_string(), "x".to_string())]);
    }

    #[test]
    fn test_alter_invalid_id() {
        for mid in ["", "X1", "T", "Tx"] {
            let err = alter_changes(mid, AlterFields { tags: Some("x".into()), ..Default::default() }).unwrap_err();
            assert!(err.contains("invalid memory ID"), "{mid}: {err}");
        }
    }

    #[test]
    fn test_alter_empty_fields_no_changes() {
        assert!(alter_changes("T1", AlterFields::default()).unwrap().is_empty());
    }
}
