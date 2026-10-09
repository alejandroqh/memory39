use bloomfilter::Bloom;
use rusqlite::{Connection, Result};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use unicode_normalization::UnicodeNormalization;

use super::crud;
use super::recall::{self, RecallFilters, RecallResult};
use super::connect::{self, ConnectionResult};
use super::schema::{has_fts5_operators, PREFIX_MIN_LEN};

const BLOOM_ITEMS_ESTIMATE: usize = 600_000;
const BLOOM_FP_RATE: f64 = 0.00001;

// On-disk layout: magic | format version (u32 LE) | write_gen (i64 LE) | bloom bytes
const BLOOM_MAGIC: &[u8; 8] = b"M39BLOOM";
const BLOOM_FORMAT_VERSION: u32 = 1;
const BLOOM_HEADER_LEN: usize = 8 + 4 + 8;

struct BloomState {
    bloom: Bloom<String>,
    /// `write_gen` of the DB state the bloom covers; -1 = unknown, forces a sync
    write_gen: i64,
}

pub struct MemoryDb {
    conn: Connection,
    // RefCell: recall (&self) must refresh the bloom when other connections wrote
    state: RefCell<BloomState>,
    bloom_path: Option<PathBuf>,
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

fn add_text_to_bloom(bloom: &mut Bloom<String>, text: &str) {
    for t in tokenize(text) {
        bloom.set(&t);
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

    // FTS5 ANDs query words (any order, any column), so one absent word rules out
    // a match. Words longer than PREFIX_MIN_LEN are prefix-expanded by the fallback
    // pass (same split as expand_query_for_prefix); the bloom can't model prefixes,
    // so they never decide.
    query.split_whitespace()
        .filter(|word| word.chars().count() <= PREFIX_MIN_LEN)
        .flat_map(tokenize)
        .any(|token| !bloom.check(&token))
}

// --- Bloom filter persistence ---

fn bloom_path_for_db(db_path: &Path) -> PathBuf {
    db_path.with_extension("bloom")
}

fn new_bloom() -> Bloom<String> {
    Bloom::new_for_fp_rate(BLOOM_ITEMS_ESTIMATE, BLOOM_FP_RATE)
        .expect("invalid bloom filter parameters")
}

fn read_write_gen(conn: &Connection) -> Result<i64> {
    conn.prepare_cached("SELECT n FROM write_gen WHERE id = 1")?
        .query_row([], |row| row.get(0))
}

/// Load a persisted bloom and the `write_gen` it was built for.
/// Rejects unknown formats, including the headerless pre-1.0.4 layout.
fn load_bloom(path: &Path) -> Option<(Bloom<String>, i64)> {
    let data = std::fs::read(path).ok()?;
    if data.len() < BLOOM_HEADER_LEN || &data[..8] != BLOOM_MAGIC {
        return None;
    }
    let version = u32::from_le_bytes(data[8..12].try_into().ok()?);
    if version != BLOOM_FORMAT_VERSION {
        return None;
    }
    let write_gen = i64::from_le_bytes(data[12..20].try_into().ok()?);
    let bloom = Bloom::from_bytes(data[BLOOM_HEADER_LEN..].to_vec()).ok()?;
    Some((bloom, write_gen))
}

/// Replace the persisted bloom atomically (temp file + rename) so concurrent
/// readers never see a torn file.
fn save_bloom(bloom: &Bloom<String>, write_gen: i64, path: &Path) {
    static SAVE_SEQ: AtomicU64 = AtomicU64::new(0);

    let bytes = bloom.to_bytes();
    let mut data = Vec::with_capacity(BLOOM_HEADER_LEN + bytes.len());
    data.extend_from_slice(BLOOM_MAGIC);
    data.extend_from_slice(&BLOOM_FORMAT_VERSION.to_le_bytes());
    data.extend_from_slice(&write_gen.to_le_bytes());
    data.extend_from_slice(&bytes);

    let seq = SAVE_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_extension(format!("bloom.tmp.{}.{}", std::process::id(), seq));
    let saved = std::fs::write(&tmp, &data).is_ok() && std::fs::rename(&tmp, path).is_ok();
    if !saved {
        let _ = std::fs::remove_file(&tmp);
    }
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

/// Build from the DB. Reads `write_gen` and scans in one read transaction (or the
/// caller's), so the returned generation matches exactly the rows scanned.
fn build_bloom(conn: &Connection) -> Result<(Bloom<String>, i64)> {
    let tx = if conn.is_autocommit() { Some(conn.unchecked_transaction()?) } else { None };
    let write_gen = read_write_gen(conn)?;
    let mut bloom = new_bloom();
    scan_table(&mut bloom, conn, "SELECT event, note, tags, emotion, location, people FROM events", 6);
    scan_table(&mut bloom, conn, "SELECT event, note, tags, emotion, location, people FROM events_undated", 6);
    scan_table(&mut bloom, conn, "SELECT thing, desc, category, tags, emotion FROM things", 5);
    scan_table(&mut bloom, conn, "SELECT name, role, relationship, note, tags, emotion FROM persons", 6);
    scan_table(&mut bloom, conn, "SELECT name, desc, address, kind, note, tags, emotion FROM places", 7);
    if let Some(tx) = tx {
        tx.commit()?;
    }
    Ok((bloom, write_gen))
}

fn build_and_save(conn: &Connection, bloom_path: Option<&Path>) -> Result<(Bloom<String>, i64)> {
    let (bloom, write_gen) = build_bloom(conn)?;
    if let Some(p) = bloom_path {
        save_bloom(&bloom, write_gen, p);
    }
    Ok((bloom, write_gen))
}

/// Use the persisted bloom if it was built for the DB's current `write_gen`,
/// otherwise rebuild from the DB and persist.
fn load_or_build(conn: &Connection, bloom_path: Option<&Path>) -> Result<(Bloom<String>, i64)> {
    let db_gen = read_write_gen(conn)?;
    if let Some((bloom, file_gen)) = bloom_path.and_then(load_bloom)
        && file_gen == db_gen
    {
        return Ok((bloom, file_gen));
    }
    build_and_save(conn, bloom_path)
}

// --- MemoryDb implementation ---

impl MemoryDb {
    pub fn new(conn: Connection, db_path: Option<&Path>) -> Self {
        let bloom_path = db_path.map(bloom_path_for_db);
        // On failure start unsynced: recall retries the sync and lets FTS5 decide meanwhile.
        let (bloom, write_gen) = load_or_build(&conn, bloom_path.as_deref())
            .unwrap_or_else(|_| (new_bloom(), -1));
        MemoryDb { conn, state: RefCell::new(BloomState { bloom, write_gen }), bloom_path }
    }

    pub fn new_ram(conn: Connection) -> Self {
        Self::new(conn, None)
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Rebuild bloom filter from DB and persist it.
    pub fn rebuild_bloom(&mut self) {
        let state = self.state.get_mut();
        (state.bloom, state.write_gen) = build_and_save(&self.conn, self.bloom_path.as_deref())
            .unwrap_or_else(|_| (new_bloom(), -1));
    }

    /// No-op: the bloom is saved on every write. Kept for API compatibility.
    pub fn flush(&mut self) {}

    /// Catch up with writes this MemoryDb didn't make (other processes, other MCP
    /// clients, older binaries): the `write_gen` triggers count them all.
    fn sync(&self) -> Result<()> {
        let db_gen = read_write_gen(&self.conn)?;
        let mut state = self.state.borrow_mut();
        if db_gen != state.write_gen {
            (state.bloom, state.write_gen) = load_or_build(&self.conn, self.bloom_path.as_deref())?;
        }
        Ok(())
    }

    /// Run a write inside an IMMEDIATE transaction and add its text to the bloom.
    /// The bloom file is saved only after COMMIT, so its `write_gen` label always
    /// names a committed state.
    fn write<T>(&mut self, texts: &[Option<&str>], op: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = self.write_locked(texts, op)
            .and_then(|v| self.conn.execute_batch("COMMIT").map(|_| v));
        let state = self.state.get_mut();
        if result.is_err() {
            let _ = self.conn.execute_batch("ROLLBACK");
            // The in-memory bloom may describe a rolled-back state; force a re-sync.
            state.write_gen = -1;
            return result;
        }
        if let Some(p) = &self.bloom_path {
            save_bloom(&state.bloom, state.write_gen, p);
        }
        result
    }

    fn write_locked<T>(&mut self, texts: &[Option<&str>], op: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        // We hold the write lock, so after this sync nobody else can write until COMMIT.
        self.sync()?;
        let v = op(&self.conn)?;
        let state = self.state.get_mut();
        for t in texts {
            add_field_to_bloom(&mut state.bloom, *t);
        }
        state.write_gen = read_write_gen(&self.conn)?;
        Ok(v)
    }

    // --- Delegate: recall with bloom pre-check ---

    pub fn recall(&self, query: &str, limit: usize, offset: usize, filters: &RecallFilters) -> Vec<RecallResult> {
        let q = query.trim();
        let is_wildcard = q.is_empty() || q == "*";
        // If the bloom can't be brought up to date, skip the pre-check and let FTS5 decide.
        if !is_wildcard && self.sync().is_ok() && should_skip_fts(&self.state.borrow().bloom, q) {
            return Vec::new();
        }
        recall::recall(&self.conn, query, limit, offset, filters)
    }

    // --- Delegate: connect ---

    pub fn find_connections(&self, concepts: &[String], min_importance: Option<u8>, limit: Option<usize>, timeout: std::time::Duration) -> ConnectionResult {
        connect::find_connections(&self.conn, concepts, min_importance, limit.unwrap_or(connect::DEFAULT_CONNECT_LIMIT), timeout)
    }

    // --- Delegate: insert with bloom update ---

    #[allow(clippy::too_many_arguments)]
    pub fn insert_event(
        &mut self, event: &str, datetime: Option<&str>, note: Option<&str>,
        tags: Option<&str>, importance: u8, emotion: Option<&str>,
        location: Option<&str>, people: Option<&str>, source: Option<&str>,
        created_at: &str,
    ) -> Result<i64> {
        self.write(&[Some(event), note, tags, emotion, location, people], |conn| {
            crud::insert_event(conn, event, datetime, note, tags, importance, emotion, location, people, source, created_at)
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn insert_thing(
        &mut self, thing: &str, desc: Option<&str>, category: Option<&str>,
        tags: Option<&str>, importance: u8, emotion: Option<&str>,
        source: Option<&str>, confidence: u8, related: Option<&str>,
        created_at: &str,
    ) -> Result<i64> {
        self.write(&[Some(thing), desc, category, tags, emotion], |conn| {
            crud::insert_thing(conn, thing, desc, category, tags, importance, emotion, source, confidence, related, created_at)
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn insert_person(
        &mut self, name: &str, role: Option<&str>, relationship: Option<&str>,
        contact: Option<&str>, met_at: Option<&str>, last_seen: Option<&str>,
        note: Option<&str>, tags: Option<&str>, importance: u8,
        emotion: Option<&str>, created_at: &str,
    ) -> Result<i64> {
        self.write(&[Some(name), role, relationship, note, tags, emotion], |conn| {
            crud::insert_person(conn, name, role, relationship, contact, met_at, last_seen, note, tags, importance, emotion, created_at)
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn insert_place(
        &mut self, name: &str, desc: Option<&str>, address: Option<&str>,
        kind: Option<&str>, note: Option<&str>, tags: Option<&str>,
        importance: u8, emotion: Option<&str>, created_at: &str,
    ) -> Result<i64> {
        self.write(&[Some(name), desc, address, kind, note, tags, emotion], |conn| {
            crud::insert_place(conn, name, desc, address, kind, note, tags, importance, emotion, created_at)
        })
    }

    // --- Delegate: alter with bloom update ---

    pub fn alter(&mut self, mid: &str, changes: &[(String, String)]) -> Result<bool> {
        let texts: Vec<Option<&str>> = changes.iter().map(|(_, v)| Some(v.as_str())).collect();
        self.write(&texts, |conn| crud::alter(conn, mid, changes))
    }

    // --- Delegate: forget (no bloom update needed) ---

    pub fn forget(&self, mid: &str) -> Result<bool> {
        crud::forget(&self.conn, mid)
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

    fn skips(mdb: &MemoryDb, query: &str) -> bool {
        should_skip_fts(&mdb.state.borrow().bloom, query)
    }

    fn no_filters() -> RecallFilters {
        RecallFilters {
            min_importance: None, date_from: None, date_to: None,
            memory_type: None, source: None,
        }
    }

    fn mids(results: &[RecallResult]) -> Vec<String> {
        let mut ids: Vec<String> = results.iter().map(|r| r.mid.clone()).collect();
        ids.sort();
        ids
    }

    /// A DB file in its own temp dir, removed on drop.
    struct TempDb {
        dir: PathBuf,
    }

    impl TempDb {
        fn new(name: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
            let dir = std::env::temp_dir()
                .join(format!("memory39-{}-{}-{}", name, std::process::id(), nanos));
            std::fs::create_dir_all(&dir).unwrap();
            TempDb { dir }
        }

        fn path(&self) -> PathBuf {
            self.dir.join("test.db")
        }

        fn bloom_path(&self) -> PathBuf {
            bloom_path_for_db(&self.path())
        }

        fn open(&self) -> MemoryDb {
            super::super::open(&self.path()).unwrap()
        }
    }

    impl Drop for TempDb {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
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
        assert!(!should_skip_fts(&bloom, "desarrollando proyecto"));
    }

    #[test]
    fn test_no_skip_all_words_present_any_order() {
        let mut bloom = new_bloom();
        bloom.set(&"alice".to_string());
        bloom.set(&"berlin".to_string());
        // FTS5 matches words anywhere in the row, so adjacency doesn't matter
        assert!(!should_skip_fts(&bloom, "alice berlin"));
        assert!(!should_skip_fts(&bloom, "berlin alice"));
    }

    #[test]
    fn test_skip_one_short_word_absent() {
        let mut bloom = new_bloom();
        bloom.set(&"alice".to_string());
        // Implicit AND: an absent short word rules out every row
        assert!(should_skip_fts(&bloom, "alice zzz"));
        // Even next to a long (prefix-expanded) word
        assert!(should_skip_fts(&bloom, "desarrollando zzz"));
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
        assert!(!skips(&mdb, "saarland"));
        assert!(!skips(&mdb, "research"));
        // Diacritics normalized: "Saarbrücken" → "saarbrucken"
        assert!(!skips(&mdb, "saarbrucken"));
        // Absent token should skip
        assert!(skips(&mdb, "xyz"));
    }

    #[test]
    fn test_recall_uses_bloom_skip() {
        let mut mdb = test_mdb();
        mdb.insert_thing("Rust programming", None, None, None, 5, None, None, 5, None, &ts()).unwrap();

        // Should find it
        let results = mdb.recall("rust", 10, 0, &no_filters());
        assert!(!results.is_empty());

        // Should be skipped by bloom (no FTS query)
        let results = mdb.recall("nonexistent", 10, 0, &no_filters());
        assert!(results.is_empty());
    }

    #[test]
    fn test_recall_wildcard_bypasses_bloom() {
        let mut mdb = test_mdb();
        mdb.insert_thing("something", None, None, None, 5, None, None, 5, None, &ts()).unwrap();

        // Wildcard should always bypass bloom and return results
        let results = mdb.recall("*", 10, 0, &no_filters());
        assert!(!results.is_empty());
    }

    #[test]
    fn test_alter_updates_bloom() {
        let mut mdb = test_mdb();
        mdb.insert_thing("old name", None, None, None, 5, None, None, 5, None, &ts()).unwrap();

        assert!(skips(&mdb, "qubit"));

        mdb.alter("T1", &[("thing".into(), "qubit spin".into())]).unwrap();

        assert!(!skips(&mdb, "qubit"));
    }

    #[test]
    fn test_forget_does_not_remove_from_bloom() {
        let mut mdb = test_mdb();
        mdb.insert_thing("unique term", None, None, None, 5, None, None, 5, None, &ts()).unwrap();

        assert!(!skips(&mdb, "unique"));

        mdb.forget("T1").unwrap();

        // Token remains in bloom (benign false positive)
        assert!(!skips(&mdb, "unique"));
    }

    #[test]
    fn test_rebuild_bloom_from_db() {
        let mut mdb = test_mdb();
        // Insert directly via crud (bypasses bloom)
        crud::insert_event(&mdb.conn, "sneak insert", None, None, None, 5, None, None, None, None, &ts()).unwrap();

        // Bloom doesn't know about it
        assert!(skips(&mdb, "sneak"));

        // Rebuild picks it up
        mdb.rebuild_bloom();
        assert!(!skips(&mdb, "sneak"));
    }

    #[test]
    fn test_recall_sees_write_that_bypassed_memorydb() {
        let mdb = test_mdb();
        crud::insert_event(&mdb.conn, "sneak insert", None, None, None, 5, None, None, None, None, &ts()).unwrap();

        // write_gen trigger bumped, so recall re-syncs before the pre-check
        assert!(!mdb.recall("sneak", 10, 0, &no_filters()).is_empty());
    }

    #[test]
    fn test_all_memory_types_populate_bloom() {
        let mut mdb = test_mdb();

        mdb.insert_event("conference talk", Some("2026-04-15 09:00"), None, None, 5, None, None, None, None, &ts()).unwrap();
        mdb.insert_thing("quantum computing", None, None, None, 5, None, None, 5, None, &ts()).unwrap();
        mdb.insert_person("Marie Curie", None, None, None, None, None, None, None, 5, None, &ts()).unwrap();
        mdb.insert_place("CERN", None, None, None, None, None, 5, None, &ts()).unwrap();

        assert!(!skips(&mdb, "talk"));
        assert!(!skips(&mdb, "curie"));
        assert!(!skips(&mdb, "cern"));
        assert!(skips(&mdb, "nope"));
    }

    // --- multi-word queries ---

    fn seed_multiword(mdb: &mut MemoryDb) {
        mdb.insert_thing("Alice moved to Berlin last spring", None, None, None, 5, None, None, 5, None, &ts()).unwrap();
        mdb.insert_thing("Berlin apartment", None, None, Some("alice"), 5, None, None, 5, None, &ts()).unwrap();
        mdb.insert_thing("coffee shop on the corner", None, None, None, 5, None, None, 5, None, &ts()).unwrap();
        mdb.insert_thing("red apple pie recipe", None, None, None, 5, None, None, 5, None, &ts()).unwrap();
        mdb.insert_thing("desarrollar proyecto nuevo", None, None, None, 5, None, None, 5, None, &ts()).unwrap();
        mdb.insert_person("Marie Curie", Some("physicist"), None, None, None, None,
            Some("won two Nobel prizes"), None, 5, None, &ts()).unwrap();
    }

    #[test]
    fn test_multiword_recall_any_order_any_field() {
        let mut mdb = test_mdb();
        seed_multiword(&mut mdb);

        for q in [
            "shop coffee",            // reordered
            "alice berlin",           // non-adjacent, and split across fields
            "curie physicist",        // name + role
            "marie nobel",            // name + note
            "red pie apple",          // 3 words reordered
            "desarrollando proyecto", // prefix fallback
        ] {
            assert!(!mdb.recall(q, 10, 0, &no_filters()).is_empty(), "{q:?} should match");
        }
        assert!(mdb.recall("alice zzz", 10, 0, &no_filters()).is_empty());
    }

    #[test]
    fn test_bloom_never_hides_fts_matches() {
        let mut mdb = test_mdb();
        seed_multiword(&mut mdb);
        mdb.insert_event("Met at Saarland University", None, None,
            Some("research"), 7, None, Some("Saarbrücken"), None, None, &ts()).unwrap();
        let docs = [
            "café résumé",
            "Straße in München",
            "हिंदी भाषा सीखना",
            "مَرْحَبًا بالعالم",
            "東京タワー 旅行",
            "don't stop rock-n-roll",
            "Ünïcödé Plaza",
        ];
        for d in docs {
            mdb.insert_thing(d, None, None, None, 5, None, None, 5, None, &ts()).unwrap();
        }

        // Every word, every reversed doc, plus hand-picked cross-field and miss cases
        let mut queries: Vec<String> = vec![
            "saarbrucken research", "cafe resume", "resume cafe", "munchen strasse",
            "मरहब", "مرحبا", "رحب", "rock n roll", "don't", "unicode plaza", "plaza zzz",
            "zzz", "alice zzz", "हिंदी zzz",
        ].into_iter().map(String::from).collect();
        for d in docs.iter().copied().chain(["Alice moved to Berlin last spring", "Marie Curie physicist"]) {
            let words: Vec<&str> = d.split_whitespace().collect();
            queries.extend(words.iter().map(|w| w.to_string()));
            queries.push(words.iter().rev().copied().collect::<Vec<_>>().join(" "));
        }

        for q in &queries {
            let with_bloom = mids(&mdb.recall(q, 50, 0, &no_filters()));
            let fts_only = mids(&recall::recall(&mdb.conn, q, 50, 0, &no_filters()));
            assert_eq!(with_bloom, fts_only, "bloom changed results for {q:?}");
        }
    }

    // --- persistence and cross-process freshness ---

    #[test]
    fn test_sees_writes_from_other_connection() {
        let tmp = TempDb::new("other-conn");
        let a = tmp.open();
        let mut b = tmp.open();
        assert!(a.recall("kiwi", 10, 0, &no_filters()).is_empty());

        b.insert_thing("kiwi farm", None, None, None, 5, None, None, 5, None, &ts()).unwrap();

        assert!(!a.recall("kiwi", 10, 0, &no_filters()).is_empty());
    }

    #[test]
    fn test_bloom_saved_on_write_not_on_exit() {
        let tmp = TempDb::new("killed");
        let mut a = tmp.open();
        a.insert_thing("plum tart", None, None, None, 5, None, None, 5, None, &ts()).unwrap();
        // A killed process never runs destructors
        std::mem::forget(a);

        let (file_bloom, file_gen) = load_bloom(&tmp.bloom_path()).expect("bloom file written");
        assert!(!should_skip_fts(&file_bloom, "plum"));

        let b = tmp.open();
        assert_eq!(file_gen, read_write_gen(b.conn()).unwrap(), "file is current, no rebuild needed");
        assert!(!b.recall("plum", 10, 0, &no_filters()).is_empty());
    }

    #[test]
    fn test_external_writer_invalidates_bloom() {
        let tmp = TempDb::new("external");
        let mut a = tmp.open();
        a.insert_thing("seed entry", None, None, None, 5, None, None, 5, None, &ts()).unwrap();

        // An old binary or the sqlite3 shell: writes the DB, never touches the bloom file
        let raw = Connection::open(tmp.path()).unwrap();
        crud::insert_thing(&raw, "zinc mine", None, None, None, 5, None, None, 5, None, &ts()).unwrap();

        assert!(!a.recall("zinc", 10, 0, &no_filters()).is_empty());
        assert!(!tmp.open().recall("zinc", 10, 0, &no_filters()).is_empty());
    }

    #[test]
    fn test_old_format_bloom_file_rebuilt() {
        let tmp = TempDb::new("old-format");
        tmp.open().insert_thing("quartz", None, None, None, 5, None, None, 5, None, &ts()).unwrap();

        // Pre-1.0.4 layout: raw bloom bytes, no header, missing "quartz"
        std::fs::write(tmp.bloom_path(), new_bloom().to_bytes()).unwrap();

        assert!(!tmp.open().recall("quartz", 10, 0, &no_filters()).is_empty());
        let data = std::fs::read(tmp.bloom_path()).unwrap();
        assert_eq!(&data[..8], BLOOM_MAGIC);
    }
}
