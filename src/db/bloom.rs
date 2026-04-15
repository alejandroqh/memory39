use bloomfilter::Bloom;
use rusqlite::{Connection, Result};
use std::path::{Path, PathBuf};
use unicode_normalization::UnicodeNormalization;

use super::crud;
use super::recall::{self, RecallFilters, RecallResult};
use super::connect::{self, ConnectionResult};
use super::schema::{has_fts5_operators, PREFIX_MIN_LEN};

const BLOOM_ITEMS_ESTIMATE: usize = 600_000;
const BLOOM_FP_RATE: f64 = 0.00001;

pub struct MemoryDb {
    conn: Connection,
    bloom: Bloom<String>,
    bloom_path: Option<PathBuf>,
    bloom_dirty: bool,
}

// --- Tokenization (matches FTS5 unicode61 remove_diacritics 2) ---

fn normalize_token(word: &str) -> String {
    word.to_lowercase()
        .nfd()
        .filter(|c| !unicode_normalization::char::is_combining_mark(*c))
        .collect()
}

fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(normalize_token)
        .collect()
}

fn bigrams(tokens: &[String]) -> Vec<String> {
    tokens.windows(2)
        .map(|pair| format!("{}+{}", pair[0], pair[1]))
        .collect()
}

fn add_text_to_bloom(bloom: &mut Bloom<String>, text: &str) {
    let tokens = tokenize(text);
    for t in &tokens {
        bloom.set(t);
    }
    for bg in bigrams(&tokens) {
        bloom.set(&bg);
    }
}

fn add_field_to_bloom(bloom: &mut Bloom<String>, field: Option<&str>) {
    if let Some(text) = field
        && !text.is_empty()
    {
        add_text_to_bloom(bloom, text);
    }
}

// --- Decision function ---

fn should_skip_fts(bloom: &Bloom<String>, query: &str) -> bool {
    if has_fts5_operators(query) {
        return false;
    }

    let tokens = tokenize(query);
    if tokens.is_empty() {
        return false;
    }

    // If ALL tokens are absent AND none would be prefix-expanded → skip
    let mut can_skip = true;
    for token in &tokens {
        if token.chars().count() > PREFIX_MIN_LEN {
            // Would be prefix-expanded; bloom can't model prefixes
            can_skip = false;
            break;
        }
        if bloom.check(token) {
            can_skip = false;
            break;
        }
    }
    if can_skip {
        return true;
    }

    // For multi-word queries: if ALL bigrams are absent → skip
    if tokens.len() >= 2 {
        let bgs = bigrams(&tokens);
        if bgs.iter().all(|bg| !bloom.check(bg)) {
            return true;
        }
    }

    false
}

// --- Bloom filter persistence ---

fn bloom_path_for_db(db_path: &Path) -> PathBuf {
    db_path.with_extension("bloom")
}

fn new_bloom() -> Bloom<String> {
    Bloom::new_for_fp_rate(BLOOM_ITEMS_ESTIMATE, BLOOM_FP_RATE)
        .expect("invalid bloom filter parameters")
}

fn load_bloom(path: &Path) -> Option<Bloom<String>> {
    let data = std::fs::read(path).ok()?;
    Bloom::from_bytes(data).ok()
}

fn save_bloom(bloom: &Bloom<String>, path: &Path) {
    let _ = std::fs::write(path, bloom.to_bytes());
}

fn scan_table(bloom: &mut Bloom<String>, conn: &Connection, sql: &str, col_count: usize) {
    if let Ok(mut stmt) = conn.prepare(sql) {
        let _ = stmt.query_map([], |row| {
            for i in 0..col_count {
                if let Ok(v) = row.get::<_, String>(i) {
                    add_field_to_bloom(bloom, Some(&v));
                }
            }
            Ok(())
        }).map(|rows| rows.for_each(|_| {}));
    }
}

fn build_bloom(conn: &Connection) -> Bloom<String> {
    let mut bloom = new_bloom();
    scan_table(&mut bloom, conn, "SELECT event, note, tags, emotion, location, people FROM events", 6);
    scan_table(&mut bloom, conn, "SELECT event, note, tags, emotion, location, people FROM events_undated", 6);
    scan_table(&mut bloom, conn, "SELECT thing, desc, category, tags, emotion FROM things", 5);
    scan_table(&mut bloom, conn, "SELECT name, role, relationship, note, tags, emotion FROM persons", 6);
    scan_table(&mut bloom, conn, "SELECT name, desc, address, kind, note, tags, emotion FROM places", 7);
    bloom
}

// --- MemoryDb implementation ---

impl MemoryDb {
    pub fn new(conn: Connection, db_path: Option<&Path>) -> Self {
        let bloom_path = db_path.map(bloom_path_for_db);

        let bloom = bloom_path.as_ref()
            .and_then(|p| load_bloom(p))
            .unwrap_or_else(|| {
                let b = build_bloom(&conn);
                if let Some(p) = &bloom_path {
                    save_bloom(&b, p);
                }
                b
            });

        MemoryDb { conn, bloom, bloom_path, bloom_dirty: false }
    }

    pub fn new_ram(conn: Connection) -> Self {
        MemoryDb {
            conn,
            bloom: new_bloom(),
            bloom_path: None,
            bloom_dirty: false,
        }
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Rebuild bloom filter from DB (for bulk operations like ingest)
    pub fn rebuild_bloom(&mut self) {
        self.bloom = build_bloom(&self.conn);
        self.bloom_dirty = false;
        if let Some(p) = &self.bloom_path {
            save_bloom(&self.bloom, p);
        }
    }

    fn mark_dirty(&mut self) {
        self.bloom_dirty = true;
    }

    /// Flush bloom to disk if dirty
    pub fn flush(&mut self) {
        if self.bloom_dirty {
            if let Some(p) = &self.bloom_path {
                save_bloom(&self.bloom, p);
            }
            self.bloom_dirty = false;
        }
    }

    // --- Delegate: recall with bloom pre-check ---

    pub fn recall(&self, query: &str, limit: usize, offset: usize, filters: &RecallFilters) -> Vec<RecallResult> {
        let q = query.trim();
        let is_wildcard = q.is_empty() || q == "*";
        if !is_wildcard && should_skip_fts(&self.bloom, q) {
            return Vec::new();
        }
        recall::recall(&self.conn, query, limit, offset, filters)
    }

    // --- Delegate: connect ---

    pub fn find_connections(&self, concepts: &[String], min_importance: Option<u8>, timeout: std::time::Duration) -> ConnectionResult {
        connect::find_connections(&self.conn, concepts, min_importance, timeout)
    }

    // --- Delegate: insert with bloom update ---

    #[allow(clippy::too_many_arguments)]
    pub fn insert_event(
        &mut self, event: &str, datetime: Option<&str>, note: Option<&str>,
        tags: Option<&str>, importance: u8, emotion: Option<&str>,
        location: Option<&str>, people: Option<&str>, source: Option<&str>,
        created_at: &str,
    ) -> Result<i64> {
        let id = crud::insert_event(&self.conn, event, datetime, note, tags, importance, emotion, location, people, source, created_at)?;
        add_text_to_bloom(&mut self.bloom, event);
        for f in [note, tags, emotion, location, people] {
            add_field_to_bloom(&mut self.bloom, f);
        }
        self.mark_dirty();
        Ok(id)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn insert_thing(
        &mut self, thing: &str, desc: Option<&str>, category: Option<&str>,
        tags: Option<&str>, importance: u8, emotion: Option<&str>,
        source: Option<&str>, confidence: u8, related: Option<&str>,
        created_at: &str,
    ) -> Result<i64> {
        let id = crud::insert_thing(&self.conn, thing, desc, category, tags, importance, emotion, source, confidence, related, created_at)?;
        add_text_to_bloom(&mut self.bloom, thing);
        for f in [desc, category, tags, emotion] {
            add_field_to_bloom(&mut self.bloom, f);
        }
        self.mark_dirty();
        Ok(id)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn insert_person(
        &mut self, name: &str, role: Option<&str>, relationship: Option<&str>,
        contact: Option<&str>, met_at: Option<&str>, last_seen: Option<&str>,
        note: Option<&str>, tags: Option<&str>, importance: u8,
        emotion: Option<&str>, created_at: &str,
    ) -> Result<i64> {
        let id = crud::insert_person(&self.conn, name, role, relationship, contact, met_at, last_seen, note, tags, importance, emotion, created_at)?;
        add_text_to_bloom(&mut self.bloom, name);
        for f in [role, relationship, note, tags, emotion] {
            add_field_to_bloom(&mut self.bloom, f);
        }
        self.mark_dirty();
        Ok(id)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn insert_place(
        &mut self, name: &str, desc: Option<&str>, address: Option<&str>,
        kind: Option<&str>, note: Option<&str>, tags: Option<&str>,
        importance: u8, emotion: Option<&str>, created_at: &str,
    ) -> Result<i64> {
        let id = crud::insert_place(&self.conn, name, desc, address, kind, note, tags, importance, emotion, created_at)?;
        add_text_to_bloom(&mut self.bloom, name);
        for f in [desc, address, kind, note, tags, emotion] {
            add_field_to_bloom(&mut self.bloom, f);
        }
        self.mark_dirty();
        Ok(id)
    }

    // --- Delegate: alter with bloom update ---

    pub fn alter(&mut self, mid: &str, changes: &[(String, String)]) -> Result<bool> {
        let result = crud::alter(&self.conn, mid, changes)?;
        if result {
            for (_, value) in changes {
                add_text_to_bloom(&mut self.bloom, value);
            }
            self.mark_dirty();
        }
        Ok(result)
    }

    // --- Delegate: forget (no bloom update needed) ---

    pub fn forget(&self, mid: &str) -> Result<bool> {
        crud::forget(&self.conn, mid)
    }
}

impl Drop for MemoryDb {
    fn drop(&mut self) {
        self.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts() -> String {
        "2026-04-15 12:00".to_string()
    }

    fn test_mdb() -> MemoryDb {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("
            PRAGMA synchronous = OFF;
            PRAGMA cache_size = -16000;
            PRAGMA temp_store = MEMORY;
        ").unwrap();
        conn.execute_batch(super::super::schema::SCHEMA).unwrap();
        MemoryDb::new_ram(conn)
    }

    // --- tokenization ---

    #[test]
    fn test_tokenize_basic() {
        let tokens = tokenize("Saarland University");
        assert_eq!(tokens, vec!["saarland", "university"]);
    }

    #[test]
    fn test_tokenize_diacritics() {
        let tokens = tokenize("café résumé");
        assert_eq!(tokens, vec!["cafe", "resume"]);
    }

    #[test]
    fn test_tokenize_punctuation() {
        let tokens = tokenize("hello, world! foo-bar");
        assert_eq!(tokens, vec!["hello", "world", "foo", "bar"]);
    }

    #[test]
    fn test_tokenize_empty() {
        assert!(tokenize("").is_empty());
        assert!(tokenize("   ").is_empty());
    }

    // --- bigrams ---

    #[test]
    fn test_bigrams() {
        let tokens = tokenize("Saarland University campus");
        let bgs = bigrams(&tokens);
        assert_eq!(bgs, vec!["saarland+university", "university+campus"]);
    }

    #[test]
    fn test_bigrams_single_word() {
        let tokens = tokenize("hello");
        assert!(bigrams(&tokens).is_empty());
    }

    // --- should_skip_fts ---

    #[test]
    fn test_skip_absent_tokens() {
        let bloom = new_bloom();
        // Empty bloom → all tokens absent → skip
        assert!(should_skip_fts(&bloom, "xyz abc"));
    }

    #[test]
    fn test_no_skip_present_token() {
        let mut bloom = new_bloom();
        bloom.set(&"hello".to_string());
        assert!(!should_skip_fts(&bloom, "hello"));
    }

    #[test]
    fn test_no_skip_fts5_operators() {
        let bloom = new_bloom();
        assert!(!should_skip_fts(&bloom, "\"exact phrase\""));
        assert!(!should_skip_fts(&bloom, "hello*"));
        assert!(!should_skip_fts(&bloom, "a AND b"));
        assert!(!should_skip_fts(&bloom, "a OR b"));
    }

    #[test]
    fn test_no_skip_long_word_prefix_expansion() {
        let bloom = new_bloom();
        // "desarrollando" (13 chars > PREFIX_MIN_LEN=6) → would be prefix-expanded
        // Bloom can't model prefix queries → must not skip
        assert!(!should_skip_fts(&bloom, "desarrollando"));
    }

    #[test]
    fn test_skip_via_bigram_absent() {
        let mut bloom = new_bloom();
        // Both words exist individually but bigram absent
        bloom.set(&"alice".to_string());
        bloom.set(&"berlin".to_string());
        // "alice" present, "berlin" present, but "alice+berlin" bigram absent → skip
        assert!(should_skip_fts(&bloom, "alice berlin"));
    }

    #[test]
    fn test_no_skip_bigram_present() {
        let mut bloom = new_bloom();
        bloom.set(&"alice".to_string());
        bloom.set(&"berlin".to_string());
        bloom.set(&"alice+berlin".to_string());
        assert!(!should_skip_fts(&bloom, "alice berlin"));
    }

    #[test]
    fn test_skip_empty_query() {
        let bloom = new_bloom();
        assert!(!should_skip_fts(&bloom, ""));
        assert!(!should_skip_fts(&bloom, "   "));
    }

    // --- MemoryDb integration ---

    #[test]
    fn test_insert_populates_bloom() {
        let mut mdb = test_mdb();
        mdb.insert_event("Met at Saarland University", None, None,
            Some("research"), 7, None, Some("Saarbrücken"), None, None, &ts()).unwrap();

        // Individual tokens should be in bloom
        assert!(!should_skip_fts(&mdb.bloom, "saarland"));
        assert!(!should_skip_fts(&mdb.bloom, "research"));
        // Diacritics normalized: "Saarbrücken" → "saarbrucken"
        assert!(!should_skip_fts(&mdb.bloom, "saarbrucken"));
        // Absent token should skip
        assert!(should_skip_fts(&mdb.bloom, "xyz"));
    }

    #[test]
    fn test_recall_uses_bloom_skip() {
        let mut mdb = test_mdb();
        mdb.insert_thing("Rust programming", None, None, None, 5, None, None, 5, None, &ts()).unwrap();

        let filters = RecallFilters {
            min_importance: None, date_from: None, date_to: None,
            memory_type: None, source: None,
        };
        // Should find it
        let results = mdb.recall("rust", 10, 0, &filters);
        assert!(!results.is_empty());

        // Should be skipped by bloom (no FTS query)
        let results = mdb.recall("nonexistent", 10, 0, &filters);
        assert!(results.is_empty());
    }

    #[test]
    fn test_recall_wildcard_bypasses_bloom() {
        let mut mdb = test_mdb();
        mdb.insert_thing("something", None, None, None, 5, None, None, 5, None, &ts()).unwrap();

        let filters = RecallFilters {
            min_importance: None, date_from: None, date_to: None,
            memory_type: None, source: None,
        };
        // Wildcard should always bypass bloom and return results
        let results = mdb.recall("*", 10, 0, &filters);
        assert!(!results.is_empty());
    }

    #[test]
    fn test_alter_updates_bloom() {
        let mut mdb = test_mdb();
        mdb.insert_thing("old name", None, None, None, 5, None, None, 5, None, &ts()).unwrap();

        assert!(should_skip_fts(&mdb.bloom, "qubit"));

        mdb.alter("T1", &[("thing".into(), "qubit spin".into())]).unwrap();

        assert!(!should_skip_fts(&mdb.bloom, "qubit"));
    }

    #[test]
    fn test_forget_does_not_remove_from_bloom() {
        let mut mdb = test_mdb();
        mdb.insert_thing("unique term", None, None, None, 5, None, None, 5, None, &ts()).unwrap();

        assert!(!should_skip_fts(&mdb.bloom, "unique"));

        mdb.forget("T1").unwrap();

        // Token remains in bloom (benign false positive)
        assert!(!should_skip_fts(&mdb.bloom, "unique"));
    }

    #[test]
    fn test_rebuild_bloom_from_db() {
        let mut mdb = test_mdb();
        // Insert directly via crud (bypasses bloom)
        crud::insert_event(&mdb.conn, "sneak insert", None, None, None, 5, None, None, None, None, &ts()).unwrap();

        // Bloom doesn't know about it
        assert!(should_skip_fts(&mdb.bloom, "sneak"));

        // Rebuild picks it up
        mdb.rebuild_bloom();
        assert!(!should_skip_fts(&mdb.bloom, "sneak"));
    }

    #[test]
    fn test_dirty_flag() {
        let mut mdb = test_mdb();
        assert!(!mdb.bloom_dirty);

        mdb.insert_thing("test", None, None, None, 5, None, None, 5, None, &ts()).unwrap();
        assert!(mdb.bloom_dirty);

        mdb.flush();
        assert!(!mdb.bloom_dirty);
    }

    #[test]
    fn test_all_memory_types_populate_bloom() {
        let mut mdb = test_mdb();

        mdb.insert_event("conference talk", Some("2026-04-15 09:00"), None, None, 5, None, None, None, None, &ts()).unwrap();
        mdb.insert_thing("quantum computing", None, None, None, 5, None, None, 5, None, &ts()).unwrap();
        mdb.insert_person("Marie Curie", None, None, None, None, None, None, None, 5, None, &ts()).unwrap();
        mdb.insert_place("CERN", None, None, None, None, None, 5, None, &ts()).unwrap();

        assert!(!should_skip_fts(&mdb.bloom, "talk"));
        assert!(!should_skip_fts(&mdb.bloom, "curie"));
        assert!(!should_skip_fts(&mdb.bloom, "cern"));
        assert!(should_skip_fts(&mdb.bloom, "nope"));
    }
}
