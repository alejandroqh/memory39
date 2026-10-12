use rusqlite::{Connection, params};
use rust_stemmers::{Algorithm, Stemmer};
use std::sync::LazyLock;
use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

use super::schema::FTS_TABLES;

/// Words shorter than this get no variants: too many unrelated terms share their letters.
const MIN_VARIANT_LEN: usize = 3;
/// Bounds per query word: indexed terms stem-checked, variants kept.
const MAX_SCANNED_TERMS: i64 = 2000;
const MAX_VARIANTS: usize = 16;

/// Languages whose word forms are matched. Each stemmer finds more forms but also conflates
/// more unrelated words (the Spanish one stems "data" and "date" alike), so keep the list short.
static STEMMERS: LazyLock<Vec<Stemmer>> = LazyLock::new(|| {
    [Algorithm::English, Algorithm::Spanish, Algorithm::German]
        .into_iter().map(Stemmer::create).collect()
});

/// Distinct indexed terms in [?1, ?2) across all FTS tables, via the fts5vocab views in SCHEMA.
static VOCAB_RANGE_SQL: LazyLock<String> = LazyLock::new(|| {
    let selects: Vec<String> = FTS_TABLES.iter()
        .map(|(fts, _)| format!("SELECT term FROM temp.{fts}_vocab WHERE term >= ?1 AND term < ?2"))
        .collect();
    format!("{} LIMIT ?3", selects.join(" UNION "))
});

// --- Tokenization (approximates FTS5 unicode61 remove_diacritics 2) ---
//
// Every query word still reaches FTS5 as typed and FTS5 tokenizes it like the indexed text,
// so where this differs from FTS5 recall loses at most a variant. The bloom filter relies on
// it only for words made of `is_exact_char` characters, where it matches FTS5 exactly.

fn tokenize(text: &str) -> Vec<String> {
    // Diacritics go before splitting: FTS5 keeps "cafe\u{301}s" (decomposed é) one token
    let folded: String = text.to_lowercase().nfd().filter(|c| !is_combining_mark(*c)).collect();
    folded.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(String::from)
        .collect()
}

/// Characters this tokenizer handles exactly like FTS5: ASCII, and letters or digits that
/// decompose to one ASCII letter or digit plus one accent (é, ü, ñ, ç, Å). FTS5 folds other
/// characters differently (it maps ſ to s, keeps ǡ), so words with them can't be decided.
fn is_exact_char(c: char) -> bool {
    if c.is_ascii() {
        return true;
    }
    let mut parts = c.nfd();
    matches!((parts.next(), parts.next(), parts.next()),
        (Some(base), Some(mark), None) if base.is_ascii_alphanumeric() && is_combining_mark(mark))
}

/// Quote as an FTS5 string, so punctuation (Alice's, e-mail, v2.1) is tokenized, not parsed.
fn fts5_string(text: &str) -> String {
    format!("\"{}\"", text.replace('"', "\"\""))
}

/// Snowball stems of an indexed term or query token, one per language, deduplicated.
pub(crate) fn stems(token: &str) -> Vec<String> {
    let mut out: Vec<String> = STEMMERS.iter().map(|s| s.stem(token).into_owned()).collect();
    out.sort();
    out.dedup();
    out
}

/// Bloom filter key for a stem. FTS5 terms never contain '~' (unicode61 splits on it).
pub(crate) fn stem_key(stem: &str) -> String {
    format!("~{stem}")
}

/// Indexed terms with the same stem as `token`, in any of the STEMMERS languages
/// (houses for house, hablar for hablando). Empty when the vocabulary can't be read.
fn variants(conn: &Connection, token: &str) -> Vec<String> {
    let len = token.chars().count();
    if len < MIN_VARIANT_LEN {
        return Vec::new();
    }
    let stems: Vec<String> = STEMMERS.iter().map(|s| s.stem(token).into_owned()).collect();

    // A stem is its word's start up to the last letter (happy and happiness both stem to
    // "happi"), so every variant starts with the letters the word shares with its stems, minus one.
    let shared = stems.iter()
        .map(|s| token.chars().zip(s.chars()).take_while(|(a, b)| a == b).count())
        .min()
        .unwrap_or(len);
    let lo: String = token.chars().take(shared.saturating_sub(1).clamp(MIN_VARIANT_LEN, len)).collect();
    let hi = format!("{lo}\u{10FFFF}");

    let Ok(mut stmt) = conn.prepare_cached(&VOCAB_RANGE_SQL) else { return Vec::new() };
    let Ok(terms) = stmt.query_map(params![lo, hi, MAX_SCANNED_TERMS], |row| row.get::<_, String>(0)) else {
        return Vec::new();
    };
    terms.flatten()
        .filter(|term| term != token
            && STEMMERS.iter().zip(&stems).any(|(s, stem)| s.stem(term) == stem.as_str()))
        .take(MAX_VARIANTS)
        .collect()
}

struct Word {
    /// The word as typed, as an FTS5 string.
    exact: String,
    tokens: Vec<String>,
    /// `tokens` are exactly what FTS5 makes of the word.
    exact_tokens: bool,
    variants: Vec<String>,
}

impl Word {
    /// The word or any of its variants, as an FTS5 expression.
    fn any_form(&self) -> String {
        if self.variants.is_empty() {
            return self.exact.clone();
        }
        let forms: Vec<String> = std::iter::once(self.exact.clone())
            .chain(self.variants.iter().map(|v| fts5_string(v)))
            .collect();
        format!("({})", forms.join(" OR "))
    }

    /// Whether some memory could match this word, as typed or as a variant, given
    /// `known(key)` is false only for keys that are definitely absent from the index.
    fn could_match(&self, known: &impl Fn(&str) -> bool) -> bool {
        if !self.exact_tokens || self.tokens.iter().all(|t| known(t)) {
            return true;
        }
        // A variant shares a stem with the word; the bloom stores every indexed term's stems
        matches!(self.tokens.as_slice(), [t] if stems(t).iter().any(|s| known(&stem_key(s))))
    }
}

/// A plain-words query (no FTS5 syntax), turned into FTS5 queries of decreasing strictness.
pub(crate) struct QueryPlan {
    words: Vec<Word>,
}

impl QueryPlan {
    /// Split into words, without touching the database (no variants yet).
    pub(crate) fn parse(text: &str) -> Self {
        let words = text.split_whitespace()
            .filter_map(|w| {
                let tokens = tokenize(w);
                // Punctuation alone ("-", "&") holds no searchable term
                if tokens.is_empty() {
                    return None;
                }
                let exact_tokens = w.chars().all(is_exact_char);
                Some(Word { exact: fts5_string(w), tokens, exact_tokens, variants: Vec::new() })
            })
            .collect();
        QueryPlan { words }
    }

    /// Split into words and look up each word's variants in the index.
    pub(crate) fn new(conn: &Connection, text: &str) -> Self {
        let mut plan = Self::parse(text);
        for w in &mut plan.words {
            if let [token] = w.tokens.as_slice() {
                w.variants = variants(conn, token);
            }
        }
        plan
    }

    pub(crate) fn len(&self) -> usize {
        self.words.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    pub(crate) fn has_variants(&self) -> bool {
        self.words.iter().any(|w| !w.variants.is_empty())
    }

    /// Words a memory must match to be recalled: the only one, or at least half of several.
    pub(crate) fn min_matched(&self) -> usize {
        if self.len() <= 1 { self.len() } else { self.len().div_ceil(2) }
    }

    /// Every word, as typed.
    pub(crate) fn exact(&self) -> String {
        self.words.iter().map(|w| w.exact.as_str()).collect::<Vec<_>>().join(" ")
    }

    /// Every word, each as typed or as one of its variants.
    pub(crate) fn all_words(&self) -> String {
        self.word_queries().join(" ")
    }

    /// Any word, as typed or as one of its variants.
    pub(crate) fn any_word(&self) -> String {
        self.word_queries().join(" OR ")
    }

    /// One FTS5 expression per word: the word as typed or as one of its variants.
    pub(crate) fn word_queries(&self) -> Vec<String> {
        self.words.iter().map(Word::any_form).collect()
    }

    /// Whether recall could return anything, given `known(key)` is false only for keys
    /// (terms, and `stem_key`s of their stems) definitely absent from the index.
    pub(crate) fn could_match(&self, known: impl Fn(&str) -> bool) -> bool {
        self.words.iter().filter(|w| w.could_match(&known)).count() >= self.min_matched()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn conn_with(texts: &[&str]) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(super::super::schema::SCHEMA).unwrap();
        for t in texts {
            super::super::crud::insert_thing(&conn, t, None, None, None, 5, None, None, 5, None, "2026-10-12 10:00").unwrap();
        }
        conn
    }

    fn variants_of(conn: &Connection, word: &str) -> Vec<String> {
        let mut v = variants(conn, word);
        v.sort();
        v
    }

    #[test]
    fn test_tokenize() {
        assert_eq!(tokenize("Saarland University"), vec!["saarland", "university"]);
        assert_eq!(tokenize("café résumé"), vec!["cafe", "resume"]);
        assert_eq!(tokenize("hello, world! foo-bar"), vec!["hello", "world", "foo", "bar"]);
        assert_eq!(tokenize("cafe\u{301}s mu\u{308}de"), vec!["cafes", "mude"], "decomposed accents");
        assert!(tokenize("  - & ").is_empty());
    }

    /// FTS5's own tokens for `text`, read back through a scratch table.
    fn fts5_tokens(conn: &Connection, text: &str) -> Vec<String> {
        conn.execute("INSERT INTO tok(rowid, a) VALUES (1, ?1)", [text]).unwrap();
        let mut st = conn.prepare_cached("SELECT term FROM tok_v ORDER BY offset").unwrap();
        let out = st.query_map([], |r| r.get::<_, String>(0)).unwrap().flatten().collect();
        conn.execute("DELETE FROM tok", []).unwrap();
        out
    }

    #[test]
    fn test_exact_chars_tokenize_like_fts5() {
        // Exhaustive over every character the bloom filter decides on
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!("
            CREATE VIRTUAL TABLE tok USING fts5(a, tokenize='{}');
            CREATE VIRTUAL TABLE tok_v USING fts5vocab(tok, instance);
        ", super::super::schema::FTS_TOKENIZER)).unwrap();
        let exact: Vec<char> = (1u32..=0x10FFFF).filter_map(char::from_u32).filter(|c| is_exact_char(*c)).collect();
        assert!(exact.len() > 400, "{}", exact.len());
        for c in exact {
            for text in [format!("abc{c}xyz"), format!("{c}"), format!("ab{c}"), format!("ABC{c}XYZ"), format!("x {c}-{c}.{c}'s")] {
                assert_eq!(tokenize(&text), fts5_tokens(&conn, &text), "U+{:04X} in {text:?}", c as u32);
            }
        }
        // Outside the set FTS5 folds differently, so those words are never decided
        for c in ['ſ', 'Ǡ', 'ß', 'ø', 'σ', '\u{301}'] {
            assert!(!is_exact_char(c), "{c:?}");
        }
    }

    #[test]
    fn test_variants_share_stem() {
        let conn = conn_with(&["two red houses", "went for a run", "walking in the park", "hablar con Maria",
            "desarrollo de software", "electric car"]);
        assert_eq!(variants_of(&conn, "house"), vec!["houses"]);
        assert_eq!(variants_of(&conn, "running"), vec!["run"]);
        assert_eq!(variants_of(&conn, "walked"), vec!["walking"]);
        assert_eq!(variants_of(&conn, "hablando"), vec!["hablar"]);
        assert_eq!(variants_of(&conn, "desarrollando"), vec!["desarrollo"]);
        assert_eq!(variants_of(&conn, "cars"), vec!["car"]);
    }

    #[test]
    fn test_variants_skip_shared_prefix_only() {
        // Same first letters, different words: the 6-letter truncation used to match these
        let conn = conn_with(&["transparent pricing", "flight departure", "internet outage", "desarraigo cultural"]);
        for word in ["transportation", "department", "international", "desarrollo"] {
            assert!(variants(&conn, word).is_empty(), "{word}");
        }
    }

    #[test]
    fn test_variants_need_vocab_views() {
        // A connection without SCHEMA's fts5vocab views: no variants, no error
        let conn = Connection::open_in_memory().unwrap();
        assert!(variants(&conn, "house").is_empty());
    }

    #[test]
    fn test_short_words_get_no_variants() {
        let conn = conn_with(&["at the ate"]);
        assert!(variants(&conn, "at").is_empty());
    }

    #[test]
    fn test_plan_queries() {
        let conn = conn_with(&["two red houses"]);
        let plan = QueryPlan::new(&conn, "red house's -");
        assert_eq!(plan.len(), 2, "punctuation-only words are dropped");
        assert_eq!(plan.exact(), "\"red\" \"house's\"");
        // "house's" is two tokens, so it gets no variants; "red" has none in the index
        assert!(!plan.has_variants());

        let plan = QueryPlan::new(&conn, "red house");
        assert!(plan.has_variants());
        assert_eq!(plan.all_words(), "\"red\" (\"house\" OR \"houses\")");
        assert_eq!(plan.any_word(), "\"red\" OR (\"house\" OR \"houses\")");
    }

    #[test]
    fn test_plan_escapes_quotes() {
        let conn = conn_with(&[]);
        assert_eq!(QueryPlan::new(&conn, "say\"hi").exact(), "\"say\"\"hi\"");
    }

    #[test]
    fn test_min_matched() {
        let n = |q: &str| QueryPlan::parse(q).min_matched();
        assert_eq!([n(""), n("a"), n("a b"), n("a b c"), n("a b c d"), n("a b c d e")], [0, 1, 1, 2, 2, 3]);
    }

    #[test]
    fn test_could_match() {
        // What a bloom holding "car", "alice" and their stems would answer
        let mut known: HashSet<String> = ["car", "alice"].iter().map(|s| s.to_string()).collect();
        for t in ["car", "alice"] {
            known.extend(stems(t).iter().map(|s| stem_key(s)));
        }
        let could = |q: &str| QueryPlan::parse(q).could_match(|k| known.contains(k));

        assert!(could("car"));
        assert!(could("cars"), "word form of a known term");
        assert!(!could("CAR's"), "a phrase of car + s needs both tokens; s is unknown");
        assert!(could("alice zzz"), "half the words is enough");
        assert!(!could("zzz"));
        assert!(!could("zzz qqq"));
        assert!(!could("alice zzz qqq"), "1 of 3 is less than half");
        assert!(!could("transportation"), "long words decide too");
        assert!(could("straße"), "ß is not exact: never decided");
        assert!(!could("café"), "é is exact");
        assert!(could(""), "nothing to decide");
    }
}
