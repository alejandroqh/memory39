mod schema;
mod crud;
mod recall;
mod connect;

use rusqlite::{Connection, Result};
use std::path::Path;

pub use crud::{insert_event, insert_thing, insert_person, insert_place, memory_id, parse_memory_id, text_field_for_id, forget, alter};
pub use recall::{RecallFilters, RecallResult, recall};
pub use connect::{ConnectionKind, Connection_, ConnectionResult, find_connections};

pub fn open(path: &Path) -> Result<Connection> {
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

pub fn open_ram() -> Result<Connection> {
    let conn = Connection::open_in_memory()?;
    conn.execute_batch("
        PRAGMA synchronous = OFF;
        PRAGMA cache_size = -16000;
        PRAGMA temp_store = MEMORY;
    ")?;
    conn.execute_batch(schema::SCHEMA)?;
    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db() -> Connection {
        open_ram().expect("failed to create test db")
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
        let conn = open_ram().unwrap();
        schema::migrate_fts_tokenizer(&conn).unwrap();
        insert_event(&conn, "test event", None, None, None, 5, None, None, None, None, &ts()).unwrap();
    }

    #[test]
    fn test_recall_with_no_results_no_panic() {
        let conn = test_db();
        let filters = RecallFilters { min_importance: None, date_from: None, date_to: None, memory_type: None, source: None };
        let results = recall(&conn, "nonexistent query", 10, 0, &filters);
        assert!(results.is_empty());
    }
}
