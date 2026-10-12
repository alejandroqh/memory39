mod schema;
mod crud;
mod query;
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

    fn no_filters() -> RecallFilters {
        RecallFilters { min_importance: None, date_from: None, date_to: None, memory_type: None, source: None }
    }

    fn thing(conn: &rusqlite::Connection, text: &str, importance: u8) {
        insert_thing(conn, text, None, None, None, importance, None, None, 5, None, &ts()).unwrap();
    }

    /// The primary text of each result, in result order.
    fn found(conn: &rusqlite::Connection, query: &str) -> Vec<String> {
        recall(conn, query, 10, 0, &no_filters()).into_iter().map(|r| r.fields[0].1.clone()).collect()
    }

    #[test]
    fn test_schema_uses_fts_tokenizer() {
        // The bloom filter tokenizes new text with FTS_TOKENIZER: it must be the index's
        let tokenize = format!("tokenize='{}'", schema::FTS_TOKENIZER);
        assert_eq!(schema::SCHEMA.matches(&tokenize).count(), schema::FTS_TABLES.len());
    }

    // --- FTS5 syntax detection ---

    #[test]
    fn test_fts5_syntax_detection() {
        for q in ["\"exact phrase\"", "hello AND world", "budget OR savings", "a NOT b", "prefix*", "NEAR(a b)", "tags: rust"] {
            assert!(schema::has_fts5_operators(q), "{q:?}");
        }
        // FTS5 operators are uppercase, so these are plain words
        for q in ["salt and pepper", "this or that", "nearby cafe", "Alice's birthday", "e-mail"] {
            assert!(!schema::has_fts5_operators(q), "{q:?}");
        }
    }

    #[test]
    fn test_fts5_syntax_runs_as_written() {
        let conn = test_db();
        thing(&conn, "budget planning", 5);
        thing(&conn, "savings account", 5);
        thing(&conn, "desarrollo de software", 5);
        assert_eq!(found(&conn, "budget OR savings").len(), 2);
        assert_eq!(found(&conn, "\"savings account\""), vec!["savings account"]);
        assert_eq!(found(&conn, "desarr*"), vec!["desarrollo de software"]);
        assert!(found(&conn, "\"account savings\"").is_empty(), "phrases keep word order");
    }

    // --- word forms ---

    #[test]
    fn test_word_forms_match() {
        for (query, stored) in [
            ("houses", "a red house"),
            ("house", "two red houses"),
            ("cars", "electric car"),
            ("running", "went for a run"),
            ("walked", "walking in the park"),
            ("hablando", "hablar con Maria"),
            ("desarrollando", "desarrollo de software"),
        ] {
            let conn = test_db();
            thing(&conn, stored, 5);
            assert_eq!(found(&conn, query), vec![stored], "{query:?}");
        }
    }

    #[test]
    fn test_shared_prefix_is_not_a_word_form() {
        for (query, stored) in [
            ("transportation", "transparent pricing"),
            ("department", "flight departure"),
            ("international", "internet outage"),
            ("desarrollo", "desarraigo cultural"),
        ] {
            let conn = test_db();
            thing(&conn, stored, 5);
            assert!(found(&conn, query).is_empty(), "{query:?} should not match {stored:?}");
        }
    }

    #[test]
    fn test_exact_words_rank_before_word_forms() {
        let conn = test_db();
        // The word form is more important, yet the word as typed comes first
        thing(&conn, "two red houses", 10);
        thing(&conn, "a red house", 1);
        assert_eq!(found(&conn, "house"), vec!["a red house", "two red houses"]);

        // Pages split the tiers consistently
        let page = |offset| recall(&conn, "house", 1, offset, &no_filters())[0].mid.clone();
        assert_eq!(vec![page(0), page(1)], vec!["T2", "T1"]);
    }

    // --- partial matches ---

    #[test]
    fn test_partial_matches_fill_after_full_matches() {
        let conn = test_db();
        thing(&conn, "the weather was nice", 10);      // 1 of 3 words: left out
        thing(&conn, "the house on the hill", 10);     // 2 of 3, common words only
        thing(&conn, "Mobility House GmbH, EV charging partner", 1); // 2 of 3, incl. the rare one
        assert_eq!(found(&conn, "the mobility house"),
            vec!["Mobility House GmbH, EV charging partner", "the house on the hill"]);

        thing(&conn, "the Mobility House office", 1);
        assert_eq!(found(&conn, "the mobility house")[0], "the Mobility House office", "all words first");
    }

    #[test]
    fn test_partial_match_scores_are_scaled() {
        let conn = test_db();
        thing(&conn, "Mobility House GmbH", 5);
        let full = recall(&conn, "mobility house", 10, 0, &no_filters())[0].score;
        let partial = recall(&conn, "mobility house berlin", 10, 0, &no_filters())[0].score;
        assert!((partial - full * 2.0 / 3.0).abs() < 1e-9, "{partial} vs {full}");
    }

    // --- query text ---

    #[test]
    fn test_punctuation_in_query() {
        for (query, stored) in [
            ("Alice's birthday", "Alice's birthday party"),
            ("e-mail", "send the e-mail"),
            ("v2.1", "released v2.1"),
            ("Saarbrücken, Germany", "moved to Saarbrücken, Germany"),
            ("salt and pepper", "salt and pepper"),
        ] {
            let conn = test_db();
            thing(&conn, stored, 5);
            assert_eq!(found(&conn, query), vec![stored], "{query:?}");
        }
        let conn = test_db();
        thing(&conn, "anything", 5);
        assert!(found(&conn, "- & !").is_empty());
    }

    #[test]
    fn test_non_latin_words_match_exactly() {
        let conn = test_db();
        for d in ["हिंदी भाषा सीखना", "مَرْحَبًا بالعالم", "東京タワー 旅行"] {
            thing(&conn, d, 5);
        }
        for q in ["हिंदी", "भाषा", "مَرْحَبًا", "بالعالم", "東京タワー", "旅行"] {
            assert_eq!(found(&conn, q).len(), 1, "{q:?}");
        }
    }

    // --- ranking ---

    #[test]
    fn test_more_relevant_match_ranks_first() {
        let conn = test_db();
        insert_thing(&conn, "weekly notes", Some("groceries, weather, the cat, a podcast, and the word battery once"),
            None, None, 5, None, None, 5, None, &ts()).unwrap();
        insert_thing(&conn, "battery", Some("battery replacement"), None, None, 5, None, None, 5, None, &ts()).unwrap();
        for filler in ["dentist appointment", "tax forms", "garden plans", "book club", "piano lessons", "car wash"] {
            thing(&conn, filler, 5);
        }
        assert_eq!(found(&conn, "battery"), vec!["battery", "weekly notes"]);
    }

    #[test]
    fn test_best_match_beats_older_matches() {
        let conn = test_db();
        // More older weak matches than the candidate pool holds for one result
        for i in 0..6 {
            insert_thing(&conn, &format!("weekly notes {i}"), Some("groceries, weather, the cat, a podcast, and the word battery once"),
                None, None, 5, None, None, 5, None, &ts()).unwrap();
        }
        insert_thing(&conn, "battery", Some("battery replacement"), None, None, 5, None, None, 5, None, &ts()).unwrap();
        for i in 0..10 {
            thing(&conn, &format!("filler {i}"), 5);
        }
        assert_eq!(recall(&conn, "battery", 1, 0, &no_filters())[0].fields[0].1, "battery");
    }

    // --- connect ---

    #[test]
    fn test_connect_matches_word_forms() {
        let conn = test_db();
        insert_event(&conn, "planning meetings with Alice", Some("2026-04-01 09:00"), None, None, 6, None, None, None, None, &ts()).unwrap();
        let concepts = ["meeting".to_string(), "alice".to_string()];
        let result = find_connections(&conn, &concepts, None, 10, std::time::Duration::from_secs(2));
        assert!(result.connections.iter().any(|c| matches!(&c.kind, ConnectionKind::Direct { mid } if mid == "E1")));

        let concepts = ["alice".to_string(), "-".to_string()];
        assert!(find_connections(&conn, &concepts, None, 10, std::time::Duration::from_secs(2)).connections.is_empty());
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

    #[test]
    fn test_word_forms_with_filters() {
        let conn = test_db();
        insert_event(&conn, "desarrollo de software", Some("2026-04-01 09:00"), None,
            Some("programación,rust"), 8, None, None, None, None, &ts()).unwrap();
        let filters = RecallFilters { min_importance: Some(7), memory_type: Some("event".into()), ..no_filters() };
        assert_eq!(recall(&conn, "desarrollando", 10, 0, &filters).len(), 1);
        let filters = RecallFilters { min_importance: Some(9), ..no_filters() };
        assert!(recall(&conn, "desarrollando", 10, 0, &filters).is_empty());
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
