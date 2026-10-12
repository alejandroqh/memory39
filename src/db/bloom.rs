use bloomfilter::Bloom;
use rusqlite::{Connection, Result};
use std::cell::RefCell;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::crud;
use super::query::{stem_key, stems, QueryPlan};
use super::recall::{self, RecallFilters, RecallResult};
use super::connect::{self, ConnectionResult};
use super::schema::{has_fts5_operators, FTS_TABLES, FTS_TOKENIZER};

/// A bloom is sized for twice the keys it is built with (at least MIN_CAPACITY), and a write
/// that fills it rebuilds it at the new size: few rebuilds, each about the size of the data.
/// At capacity the false-positive rate is BLOOM_FP_RATE; a false positive only costs an FTS5 query.
const MIN_CAPACITY: u64 = 20_000;
const BLOOM_FP_RATE: f64 = 0.0001;

// On-disk layout: magic | format version (u32 LE) | write_gen (i64 LE) | keys (u64 LE)
// | capacity (u64 LE) | bloom bytes (they carry their own size and hash count).
// Version 2: FTS5's own terms plus their stems, sized to the data (version 1 held
// Rust-tokenized words in a fixed 600K-item filter).
const BLOOM_MAGIC: &[u8; 8] = b"M39BLOOM";
const BLOOM_FORMAT_VERSION: u32 = 2;
const BLOOM_HEADER_LEN: usize = 8 + 4 + 8 + 8 + 8;

struct BloomState {
    bloom: Bloom<String>,
    /// Distinct keys set (terms and stem keys), counted as they are added
    keys: u64,
    /// Keys the bloom was sized for
    capacity: u64,
    /// `write_gen` of the DB state the bloom covers; -1 = unknown, forces a sync
    write_gen: i64,
}

impl BloomState {
    /// An empty bloom that covers no known DB state, so recall re-syncs before trusting it.
    fn unsynced() -> Self {
        BloomState { bloom: new_bloom(MIN_CAPACITY), keys: 0, capacity: MIN_CAPACITY, write_gen: -1 }
    }

    fn add_term(&mut self, term: &str) {
        for key in keys_for(term) {
            if !self.bloom.check_and_set(&key) {
                self.keys += 1;
            }
        }
    }

    fn is_full(&self) -> bool {
        self.keys > self.capacity
    }
}

pub struct MemoryDb {
    conn: Connection,
    // RefCell: recall (&self) must refresh the bloom when other connections wrote
    state: RefCell<BloomState>,
    bloom_path: Option<PathBuf>,
}

// --- Terms: exactly what the FTS5 index stores ---

/// A scratch FTS5 table with the index's tokenizer, so new text is split into the same terms
/// the index gets. Per connection, emptied after every use.
fn create_term_tokenizer(conn: &Connection) -> Result<()> {
    conn.execute_batch(&format!("
        CREATE VIRTUAL TABLE IF NOT EXISTS temp.bloom_terms USING fts5(text, tokenize='{FTS_TOKENIZER}');
        CREATE VIRTUAL TABLE IF NOT EXISTS temp.bloom_terms_vocab USING fts5vocab(temp, bloom_terms, row);
    "))
}

/// FTS5's terms for `texts`, through the scratch table.
fn fts5_terms(conn: &Connection, texts: &[Option<&str>]) -> Result<Vec<String>> {
    {
        let mut insert = conn.prepare_cached("INSERT INTO temp.bloom_terms(text) VALUES (?1)")?;
        for text in texts.iter().flatten() {
            insert.execute([text])?;
        }
    }
    let terms = conn.prepare_cached("SELECT term FROM temp.bloom_terms_vocab")?
        .query_map([], |row| row.get(0))?
        .collect::<Result<Vec<String>>>()?;
    conn.execute("DELETE FROM temp.bloom_terms", [])?;
    Ok(terms)
}

/// Bloom keys for an indexed term: the term and its stems. A query word may match the term
/// itself or, through `QueryPlan` variants, any indexed term with the same stem.
fn keys_for(term: &str) -> Vec<String> {
    std::iter::once(term.to_string())
        .chain(stems(term).iter().map(|s| stem_key(s)))
        .collect()
}

// --- Decision function ---

fn should_skip_fts(bloom: &Bloom<String>, query: &str) -> bool {
    // FTS5 syntax runs as written; phrases, prefixes and NOT aren't decided word by word
    if has_fts5_operators(query) {
        return false;
    }
    // Recall needs enough words matching, as typed or as a variant (see QueryPlan::could_match)
    !QueryPlan::parse(query).could_match(|key| bloom.check(&key.to_string()))
}

// --- Bloom filter persistence ---

fn bloom_path_for_db(db_path: &Path) -> PathBuf {
    db_path.with_extension("bloom")
}

fn new_bloom(capacity: u64) -> Bloom<String> {
    Bloom::new_for_fp_rate(capacity as usize, BLOOM_FP_RATE)
        .expect("invalid bloom filter parameters")
}

fn read_write_gen(conn: &Connection) -> Result<i64> {
    conn.prepare_cached("SELECT n FROM write_gen WHERE id = 1")?
        .query_row([], |row| row.get(0))
}

/// Load a persisted bloom. Rejects unknown formats, including version 1 and the
/// headerless pre-1.0.4 layout.
fn load_bloom(path: &Path) -> Option<BloomState> {
    let data = std::fs::read(path).ok()?;
    if data.len() < BLOOM_HEADER_LEN || &data[..8] != BLOOM_MAGIC {
        return None;
    }
    let version = u32::from_le_bytes(data[8..12].try_into().ok()?);
    if version != BLOOM_FORMAT_VERSION {
        return None;
    }
    let field = |at: usize| -> Option<[u8; 8]> { data[at..at + 8].try_into().ok() };
    let write_gen = i64::from_le_bytes(field(12)?);
    let keys = u64::from_le_bytes(field(20)?);
    let capacity = u64::from_le_bytes(field(28)?);
    let bloom = Bloom::from_bytes(data[BLOOM_HEADER_LEN..].to_vec()).ok()?;
    Some(BloomState { bloom, keys, capacity, write_gen })
}

/// Replace the persisted bloom atomically (temp file + rename) so concurrent
/// readers never see a torn file.
fn save_bloom(state: &BloomState, path: &Path) {
    static SAVE_SEQ: AtomicU64 = AtomicU64::new(0);

    let bytes = state.bloom.to_bytes();
    let mut data = Vec::with_capacity(BLOOM_HEADER_LEN + bytes.len());
    data.extend_from_slice(BLOOM_MAGIC);
    data.extend_from_slice(&BLOOM_FORMAT_VERSION.to_le_bytes());
    data.extend_from_slice(&state.write_gen.to_le_bytes());
    data.extend_from_slice(&state.keys.to_le_bytes());
    data.extend_from_slice(&state.capacity.to_le_bytes());
    data.extend_from_slice(&bytes);

    let seq = SAVE_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_extension(format!("bloom.tmp.{}.{}", std::process::id(), seq));
    let saved = std::fs::write(&tmp, &data).is_ok() && std::fs::rename(&tmp, path).is_ok();
    if !saved {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Build from the index's own terms (the fts5vocab views in SCHEMA), sized for twice its keys.
/// Reads `write_gen` and the terms in one read transaction (or the caller's), so the
/// generation matches exactly.
fn build_bloom(conn: &Connection) -> Result<BloomState> {
    let tx = if conn.is_autocommit() { Some(conn.unchecked_transaction()?) } else { None };
    let write_gen = read_write_gen(conn)?;
    let mut keys: HashSet<String> = HashSet::new();
    for (fts, _) in FTS_TABLES {
        let mut stmt = conn.prepare(&format!("SELECT term FROM temp.{fts}_vocab"))?;
        for term in stmt.query_map([], |row| row.get::<_, String>(0))? {
            keys.extend(keys_for(&term?));
        }
    }
    if let Some(tx) = tx {
        tx.commit()?;
    }
    let capacity = (2 * keys.len() as u64).max(MIN_CAPACITY);
    let mut bloom = new_bloom(capacity);
    for key in &keys {
        bloom.set(key);
    }
    Ok(BloomState { bloom, keys: keys.len() as u64, capacity, write_gen })
}

fn build_and_save(conn: &Connection, bloom_path: Option<&Path>) -> Result<BloomState> {
    let state = build_bloom(conn)?;
    if let Some(p) = bloom_path {
        save_bloom(&state, p);
    }
    Ok(state)
}

/// Use the persisted bloom if it was built for the DB's current `write_gen`,
/// otherwise rebuild from the DB and persist.
fn load_or_build(conn: &Connection, bloom_path: Option<&Path>) -> Result<BloomState> {
    let db_gen = read_write_gen(conn)?;
    if let Some(state) = bloom_path.and_then(load_bloom)
        && state.write_gen == db_gen
    {
        return Ok(state);
    }
    build_and_save(conn, bloom_path)
}

// --- MemoryDb implementation ---

impl MemoryDb {
    pub fn new(conn: Connection, db_path: Option<&Path>) -> Self {
        let bloom_path = db_path.map(bloom_path_for_db);
        // Without it writes can't extract terms and mark the bloom stale instead (see write_locked)
        let _ = create_term_tokenizer(&conn);
        // On failure start unsynced: recall retries the sync and lets FTS5 decide meanwhile.
        let state = load_or_build(&conn, bloom_path.as_deref())
            .unwrap_or_else(|_| BloomState::unsynced());
        MemoryDb { conn, state: RefCell::new(state), bloom_path }
    }

    pub fn new_ram(conn: Connection) -> Self {
        Self::new(conn, None)
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Rebuild bloom filter from DB, sized to its current terms, and persist it.
    pub fn rebuild_bloom(&mut self) {
        *self.state.get_mut() = build_and_save(&self.conn, self.bloom_path.as_deref())
            .unwrap_or_else(|_| BloomState::unsynced());
    }

    /// No-op: the bloom is saved on every write. Kept for API compatibility.
    pub fn flush(&mut self) {}

    /// Catch up with writes this MemoryDb didn't make (other processes, other MCP
    /// clients, older binaries): the `write_gen` triggers count them all.
    fn sync(&self) -> Result<()> {
        let db_gen = read_write_gen(&self.conn)?;
        let mut state = self.state.borrow_mut();
        if db_gen != state.write_gen {
            *state = load_or_build(&self.conn, self.bloom_path.as_deref())?;
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
            save_bloom(state, p);
        }
        result
    }

    fn write_locked<T>(&mut self, texts: &[Option<&str>], op: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        // We hold the write lock, so after this sync nobody else can write until COMMIT.
        self.sync()?;
        let v = op(&self.conn)?;
        let terms = fts5_terms(&self.conn, texts);
        let state = self.state.get_mut();
        match terms {
            Ok(terms) => {
                for t in &terms {
                    state.add_term(t);
                }
                state.write_gen = read_write_gen(&self.conn)?;
                // Full: rebuild at twice the keys. Inside this write's transaction, so the new
                // bloom includes the write. If it fails the full one stays: more false positives only.
                if state.is_full()
                    && let Ok(bigger) = build_bloom(&self.conn)
                {
                    *state = bigger;
                }
            }
            // The write stands; the bloom just misses it, so mark it stale and rebuild on recall
            Err(_) => state.write_gen = -1,
        }
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

    // --- should_skip_fts ---

    /// A bloom holding `terms` and their stems, as built from an index with those terms.
    fn bloom_with(terms: &[&str]) -> Bloom<String> {
        let mut state = BloomState::unsynced();
        for t in terms {
            state.add_term(t);
        }
        state.bloom
    }

    #[test]
    fn test_skip_absent_tokens() {
        // Empty bloom → all tokens absent → skip
        assert!(should_skip_fts(&bloom_with(&[]), "xyz abc"));
    }

    #[test]
    fn test_no_skip_present_token() {
        assert!(!should_skip_fts(&bloom_with(&["hello"]), "hello"));
    }

    #[test]
    fn test_no_skip_fts5_operators() {
        let bloom = bloom_with(&[]);
        assert!(!should_skip_fts(&bloom, "\"exact phrase\""));
        assert!(!should_skip_fts(&bloom, "hello*"));
        assert!(!should_skip_fts(&bloom, "a AND b"));
        assert!(!should_skip_fts(&bloom, "a OR b"));
    }

    #[test]
    fn test_no_skip_word_forms() {
        // Recall matches indexed terms with the same stem, so their stems count as present
        let bloom = bloom_with(&["car", "desarrollo", "universe"]);
        assert!(!should_skip_fts(&bloom, "cars"));
        assert!(!should_skip_fts(&bloom, "desarrollando"));
        assert!(!should_skip_fts(&bloom, "university"), "a stemmer conflation recall also makes");
        assert!(should_skip_fts(&bloom, "transportation"), "long words decide too");
    }

    #[test]
    fn test_no_skip_all_words_present_any_order() {
        let bloom = bloom_with(&["alice", "berlin"]);
        // FTS5 matches words anywhere in the row, so adjacency doesn't matter
        assert!(!should_skip_fts(&bloom, "alice berlin"));
        assert!(!should_skip_fts(&bloom, "berlin alice"));
    }

    #[test]
    fn test_skip_needs_less_than_half_the_words() {
        let bloom = bloom_with(&["alice"]);
        // Recall returns memories with at least half the words
        assert!(!should_skip_fts(&bloom, "alice zzz"));
        assert!(should_skip_fts(&bloom, "alice zzz qqq"));
        assert!(should_skip_fts(&bloom, "alice berlin zzz qqq"), "1 of 4 is less than half");
        assert!(!should_skip_fts(&bloom_with(&["alice", "berlin"]), "alice berlin zzz qqq"));
    }

    #[test]
    fn test_no_skip_words_fts5_folds_differently() {
        // Outside ASCII and single-accent Latin letters, FTS5 may fold unlike Rust: never decide
        let bloom = bloom_with(&[]);
        for q in ["straße", "københavn", "λόγος", "東京", "ſtreet"] {
            assert!(!should_skip_fts(&bloom, q), "{q:?}");
        }
        assert!(should_skip_fts(&bloom, "café"), "é folds the same in both");
    }

    #[test]
    fn test_skip_empty_query() {
        let bloom = bloom_with(&[]);
        assert!(!should_skip_fts(&bloom, ""));
        assert!(!should_skip_fts(&bloom, "   "));
    }

    // --- MemoryDb integration ---

    #[test]
    fn test_insert_populates_bloom() {
        let mut mdb = test_mdb();
        mdb.insert_event("Met at Saarland University", None, None,
            Some("research"), 7, None, Some("Saarbrücken"), None, None, &ts()).unwrap();

        assert!(!skips(&mdb, "saarland"));
        assert!(!skips(&mdb, "research"));
        // Diacritics folded by FTS5: "Saarbrücken" → "saarbrucken"
        assert!(!skips(&mdb, "saarbrucken"));
        assert!(!skips(&mdb, "Saarbrücken"));
        // Word forms of stored terms
        assert!(!skips(&mdb, "universities"));
        assert!(skips(&mdb, "xyz"));
    }

    #[test]
    fn test_bloom_holds_fts5_terms() {
        let mut mdb = test_mdb();
        // Decomposed accent (u + U+0308): FTS5 indexes "mude", a Rust split would give "mu" + "de"
        mdb.insert_thing("mu\u{308}de", None, None, None, 5, None, None, 5, None, &ts()).unwrap();
        // FTS5 folds long s to s
        mdb.insert_thing("\u{17f}treet", None, None, None, 5, None, None, 5, None, &ts()).unwrap();
        for q in ["mude", "müde", "street"] {
            assert!(!skips(&mdb, q), "{q:?}");
            assert_eq!(mdb.recall(q, 10, 0, &no_filters()).len(), 1, "{q:?}");
        }
    }

    #[test]
    fn test_recall_uses_bloom_skip() {
        let mut mdb = test_mdb();
        mdb.insert_thing("Rust programming", None, None, None, 5, None, None, 5, None, &ts()).unwrap();

        // Should find it
        let results = mdb.recall("rust", 10, 0, &no_filters());
        assert!(!results.is_empty());

        // Should be skipped by bloom (no FTS query)
        assert!(skips(&mdb, "nonexistent"));
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

        // Rebuild picks it up, word forms included
        mdb.rebuild_bloom();
        assert!(!skips(&mdb, "sneak"));
        assert!(!skips(&mdb, "inserts"));
    }

    #[test]
    fn test_recall_sees_write_that_bypassed_memorydb() {
        let mdb = test_mdb();
        crud::insert_event(&mdb.conn, "sneak insert", None, None, None, 5, None, None, None, None, &ts()).unwrap();

        // write_gen trigger bumped, so recall re-syncs before the pre-check
        assert!(!mdb.recall("sneak", 10, 0, &no_filters()).is_empty());
    }

    #[test]
    fn test_write_without_term_tokenizer_marks_bloom_stale() {
        let mut mdb = test_mdb();
        mdb.conn.execute_batch("DROP TABLE temp.bloom_terms_vocab; DROP TABLE temp.bloom_terms;").unwrap();
        // The write still succeeds; the bloom can't learn its terms, so it is rebuilt on recall
        mdb.insert_thing("kiwi farm", None, None, None, 5, None, None, 5, None, &ts()).unwrap();
        assert_eq!(mdb.state.borrow().write_gen, -1);
        assert_eq!(mdb.recall("kiwi", 10, 0, &no_filters()).len(), 1);
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
            "desarrollando proyecto", // word form
        ] {
            assert!(!mdb.recall(q, 10, 0, &no_filters()).is_empty(), "{q:?} should match");
        }
        // Half the words matching is enough; less is skipped
        assert_eq!(mids(&mdb.recall("alice zzz", 10, 0, &no_filters())), vec!["T1", "T2"]);
        assert!(skips(&mdb, "alice zzz qqq"));
        assert!(mdb.recall("alice zzz qqq", 10, 0, &no_filters()).is_empty());
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
            "two red houses",
            "Mobility House GmbH, EV charging partner",
            // FTS5 and Rust disagree on these
            "mu\u{308}de cafe\u{301}s",
            "\u{17f}treet Ǡbc københavn λόγος",
            "abc\u{e000}def",
        ];
        for d in docs {
            mdb.insert_thing(d, None, None, None, 5, None, None, 5, None, &ts()).unwrap();
        }

        // Every word, every reversed doc, plus hand-picked word-form, partial and miss cases
        let mut queries: Vec<String> = vec![
            "saarbrucken research", "cafe resume", "resume cafe", "munchen strasse",
            "मरहब", "مرحبا", "رحب", "rock n roll", "don't", "unicode plaza", "plaza zzz",
            "zzz", "alice zzz", "alice zzz qqq", "हिंदी zzz", "house", "universities",
            "the mobility house", "mobility house berlin zzz", "mude", "cafes", "street", "streets",
            "s", "a", "abc", "abcdef", "kobenhavn", "λογοσ", "λογος", "ǡbc", "abc a zzz",
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

        let file = load_bloom(&tmp.bloom_path()).expect("bloom file written");
        assert!(!should_skip_fts(&file.bloom, "plum"));

        let b = tmp.open();
        assert_eq!(file.write_gen, read_write_gen(b.conn()).unwrap(), "file is current, no rebuild needed");
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
        std::fs::write(tmp.bloom_path(), bloom_with(&[]).to_bytes()).unwrap();

        assert!(!tmp.open().recall("quartz", 10, 0, &no_filters()).is_empty());
        let data = std::fs::read(tmp.bloom_path()).unwrap();
        assert_eq!(&data[..8], BLOOM_MAGIC);
    }

    #[test]
    fn test_version_1_bloom_file_rebuilt() {
        let tmp = TempDb::new("v1-format");
        tmp.open().insert_thing("quartz", None, None, None, 5, None, None, 5, None, &ts()).unwrap();

        // A 2.1 file for the current write_gen, without the stems "quartzes" needs
        let write_gen = read_write_gen(tmp.open().conn()).unwrap();
        let mut v1 = Vec::new();
        v1.extend_from_slice(BLOOM_MAGIC);
        v1.extend_from_slice(&1u32.to_le_bytes());
        v1.extend_from_slice(&write_gen.to_le_bytes());
        v1.extend_from_slice(&bloom_with(&[]).to_bytes());
        std::fs::write(tmp.bloom_path(), v1).unwrap();

        assert_eq!(tmp.open().recall("quartzes", 10, 0, &no_filters()).len(), 1);
        let data = std::fs::read(tmp.bloom_path()).unwrap();
        assert_eq!(u32::from_le_bytes(data[8..12].try_into().unwrap()), BLOOM_FORMAT_VERSION);
    }

    // --- sizing ---

    /// Store `count` things of 100 distinct words each, from thing number `from`.
    fn fill(mdb: &mut MemoryDb, from: usize, count: usize) {
        for i in from..from + count {
            let words: Vec<String> = (0..100).map(|j| format!("grow{}", i * 100 + j)).collect();
            mdb.insert_thing(&words.join(" "), None, None, None, 5, None, None, 5, None, &ts()).unwrap();
        }
    }

    /// Fill until the bloom outgrows MIN_CAPACITY; returns how many things it took.
    fn fill_until_grown(mdb: &mut MemoryDb) -> usize {
        let mut inserted = 0;
        while mdb.state.borrow().capacity == MIN_CAPACITY {
            assert!(inserted < 1000, "the bloom never grew");
            fill(mdb, inserted, 1);
            inserted += 1;
        }
        inserted
    }

    #[test]
    fn test_bloom_sized_to_data() {
        let tmp = TempDb::new("sized");
        let mut a = tmp.open();
        a.insert_thing("plum tart", None, None, None, 5, None, None, 5, None, &ts()).unwrap();
        let state = a.state.borrow();
        assert_eq!(state.capacity, MIN_CAPACITY);
        assert!((2..10).contains(&state.keys), "{}", state.keys);
        // ~48 KB for 20K keys at 0.01%, where version 1 always took 1.8 MB
        let size = std::fs::metadata(tmp.bloom_path()).unwrap().len();
        assert!(size < 64 * 1024, "{size}");
    }

    #[test]
    fn test_bloom_grows_when_full() {
        let mut mdb = test_mdb();
        let inserted = fill_until_grown(&mut mdb);
        let state = mdb.state.borrow();
        // Rebuilt by the write that overfilled it, at twice the keys
        assert!(state.keys > MIN_CAPACITY && state.capacity == 2 * state.keys,
            "{} keys, capacity {}", state.keys, state.capacity);
        drop(state);
        let last = format!("grow{}", inserted * 100 - 1);
        for q in ["grow0", last.as_str()] {
            assert!(!skips(&mdb, q), "{q}");
        }
        assert!(skips(&mdb, "absentword"));
    }

    #[test]
    fn test_grown_bloom_shared_across_processes() {
        let tmp = TempDb::new("grown");
        let mut a = tmp.open();
        let b = tmp.open();
        let inserted = fill_until_grown(&mut a);

        // b catches up with a's writes: same size and key count, and it sees the newest words
        let last = format!("grow{}", inserted * 100 - 1);
        assert_eq!(b.recall(&last, 10, 0, &no_filters()).len(), 1);
        let (a_state, b_state) = (a.state.borrow(), b.state.borrow());
        assert_eq!((b_state.keys, b_state.capacity), (a_state.keys, a_state.capacity));
        let file = load_bloom(&tmp.bloom_path()).unwrap();
        assert_eq!((file.keys, file.capacity), (a_state.keys, a_state.capacity));
    }

    #[test]
    fn test_key_count_survives_reopen() {
        let tmp = TempDb::new("key-count");
        let mut a = tmp.open();
        fill(&mut a, 0, 3);
        let keys = a.state.borrow().keys;
        assert!(keys >= 600, "{keys}");
        // Reopening loads the file's count rather than recounting
        assert_eq!(tmp.open().state.borrow().keys, keys);
    }

    #[test]
    fn test_rebuild_sizes_to_current_data() {
        let mut mdb = test_mdb();
        let inserted = fill_until_grown(&mut mdb);
        for i in 1..=inserted {
            mdb.forget(&format!("T{i}")).unwrap();
        }
        mdb.rebuild_bloom();
        assert_eq!(mdb.state.borrow().capacity, MIN_CAPACITY, "shrinks back");
        assert!(skips(&mdb, "grow0"), "forgotten words are gone after a rebuild");
    }
}
