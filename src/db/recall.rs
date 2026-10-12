use rusqlite::{Connection, Row};
use std::collections::{HashMap, HashSet};

use super::crud::{memory_id, parse_memory_id};
use super::query::QueryPlan;
use super::schema::has_fts5_operators;

/// FTS candidates fetched per table for each result wanted, best bm25 first, so importance
/// and recency rerank a wider pool than the final results.
const CANDIDATES_PER_RESULT: usize = 4;

pub struct RecallFilters {
    pub min_importance: Option<u8>,
    pub date_from: Option<String>,
    pub date_to: Option<String>,
    pub memory_type: Option<String>,
    pub source: Option<String>,
}

pub struct RecallResult {
    pub memory_type: String,
    pub mid: String,
    pub score: f64,
    /// Text relevance 0..1 from FTS5's bm25 rank (0 when listing with "*").
    pub relevance: f64,
    pub fields: Vec<(String, String)>,
}

/// Search across all memory tables with composite scoring and filters.
/// Score = 0.4 * relevance + 0.3 * importance + 0.3 * recency
/// Plain words go through `recall_words`; a query in FTS5 syntax runs exactly as written.
pub fn recall(conn: &Connection, query: &str, limit: usize, offset: usize, filters: &RecallFilters) -> Vec<RecallResult> {
    let query = query.trim();
    let is_wildcard = query.is_empty() || query == "*";
    let fetch = limit + offset;

    let mut results = if is_wildcard {
        recall_all_inner(conn, fetch, filters)
    } else if has_fts5_operators(query) {
        let mut results = recall_inner(conn, query, fetch * CANDIDATES_PER_RESULT, filters);
        results.truncate(fetch);
        results
    } else {
        recall_words(conn, query, fetch, filters)
    };

    if offset > 0 && offset < results.len() {
        results.drain(..offset);
    } else if offset >= results.len() {
        return Vec::new();
    }
    results.truncate(limit);
    results
}

/// Plain words, in tiers that each fill only the slots the previous ones left:
/// (1) every word as typed, (2) every word as typed or as a word form found in the index
/// (house/houses, hablando/hablar), (3) at least half the words, most matched first, then
/// by relevance alone: matching only common words ("the", "house") shouldn't win on importance.
fn recall_words(conn: &Connection, query: &str, fetch: usize, filters: &RecallFilters) -> Vec<RecallResult> {
    let plan = QueryPlan::new(conn, query);
    if plan.is_empty() {
        return Vec::new();
    }
    let candidates = fetch * CANDIDATES_PER_RESULT;

    let mut results = recall_inner(conn, &plan.exact(), candidates, filters);
    results.truncate(fetch);

    if results.len() < fetch && plan.has_variants() {
        let more = recall_inner(conn, &plan.all_words(), candidates, filters);
        append_new(&mut results, more, fetch);
    }

    let n = plan.len();
    if results.len() < fetch && n > 1 {
        let candidates = recall_inner(conn, &plan.any_word(), candidates, filters);
        let matched = matched_words(conn, &candidates, &plan.word_queries());
        let mut partial: Vec<(usize, RecallResult)> = candidates.into_iter()
            .map(|mut r| {
                let matched = matched.get(&r.mid).copied().unwrap_or(0);
                r.score *= matched as f64 / n as f64;
                (matched, r)
            })
            .filter(|(matched, _)| *matched >= plan.min_matched())
            .collect();
        partial.sort_by(|a, b| b.0.cmp(&a.0)
            .then(b.1.relevance.partial_cmp(&a.1.relevance).unwrap_or(std::cmp::Ordering::Equal)));
        append_new(&mut results, partial.into_iter().map(|(_, r)| r).collect(), fetch);
    }
    results
}

/// How many of `word_queries` FTS5 matches in each result, by memory ID. Asking FTS5 keeps
/// the count exact, which the bloom filter's skip rule relies on (see `QueryPlan::could_match`).
fn matched_words(conn: &Connection, results: &[RecallResult], word_queries: &[String]) -> HashMap<String, usize> {
    let mut ids_by_prefix: HashMap<String, Vec<i64>> = HashMap::new();
    for r in results {
        if let Some((prefix, id)) = parse_memory_id(&r.mid) {
            ids_by_prefix.entry(prefix).or_default().push(id);
        }
    }
    let mut counts: HashMap<String, usize> = HashMap::new();
    for (prefix, ids) in &ids_by_prefix {
        let Some(fts) = fts_table_for_prefix(prefix) else { continue };
        let id_list = ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
        let sql = format!("SELECT rowid FROM {fts} WHERE {fts} MATCH ?1 AND rowid IN ({id_list})");
        let Ok(mut stmt) = conn.prepare(&sql) else { continue };
        for q in word_queries {
            let Ok(rows) = stmt.query_map([q], |row| row.get::<_, i64>(0)) else { continue };
            for id in rows.flatten() {
                *counts.entry(memory_id(prefix, id)).or_default() += 1;
            }
        }
    }
    counts
}

fn fts_table_for_prefix(prefix: &str) -> Option<&'static str> {
    match prefix {
        "E" => Some("events_fts"),
        "U" => Some("events_undated_fts"),
        "T" => Some("things_fts"),
        "P" => Some("persons_fts"),
        "L" => Some("places_fts"),
        _ => None,
    }
}

/// Append results not already present, up to `fetch` in total.
fn append_new(results: &mut Vec<RecallResult>, more: Vec<RecallResult>, fetch: usize) {
    let seen: HashSet<String> = results.iter().map(|r| r.mid.clone()).collect();
    let room = fetch.saturating_sub(results.len());
    results.extend(more.into_iter().filter(|r| !seen.contains(&r.mid)).take(room));
}

/// Build filter WHERE clauses and params for recall queries.
fn build_recall_filters(
    alias: &str,
    filters: &RecallFilters,
    idx_start: usize,
    include_dates: bool,
    include_source: bool,
) -> (String, Vec<Box<dyn rusqlite::types::ToSql>>) {
    let mut clauses = String::new();
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

    if let Some(min) = filters.min_importance {
        clauses.push_str(&format!(" AND {}.importance >= ?{}", alias, idx_start + params.len()));
        params.push(Box::new(min));
    }
    if include_dates {
        if let Some(ref from) = filters.date_from {
            clauses.push_str(&format!(" AND substr({}.datetime, 1, 10) >= ?{}", alias, idx_start + params.len()));
            params.push(Box::new(from.clone()));
        }
        if let Some(ref to) = filters.date_to {
            clauses.push_str(&format!(" AND substr({}.datetime, 1, 10) <= ?{}", alias, idx_start + params.len()));
            params.push(Box::new(to.clone()));
        }
    }
    if include_source
        && let Some(ref source) = filters.source {
            clauses.push_str(&format!(" AND lower({}.source) = lower(?{})", alias, idx_start + params.len()));
            params.push(Box::new(source.clone()));
        }

    (clauses, params)
}

/// Run an FTS5 query on every table the filters allow: up to `candidates` best-ranked rows
/// per table, sorted by composite score.
fn recall_inner(conn: &Connection, query: &str, candidates: usize, filters: &RecallFilters) -> Vec<RecallResult> {
    let mut results = Vec::new();
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();

    let has_date_filter = filters.date_from.is_some() || filters.date_to.is_some();
    let skip_dated = filters.memory_type.as_ref().is_some_and(|t| !matches!(t.as_str(), "event" | "events"));
    let skip_undated = filters.memory_type.as_ref().is_some_and(|t| !matches!(t.as_str(), "undated" | "events")) || has_date_filter;

    if !skip_dated {
        let (fc, fp) = build_recall_filters("e", filters, 3, true, true);
        let sql = format!(
            "SELECT e.id, rank, e.event, e.datetime, e.note, e.tags, e.importance,
                    e.emotion, e.location, e.people, e.source, e.created_at
             FROM events_fts f JOIN events e ON e.id = f.rowid
             WHERE events_fts MATCH ?1{} ORDER BY rank LIMIT ?2", fc);
        let params = fts_params(query, candidates, fp);
        recall_event_query(conn, &mut results, &sql, &params, true, &now);
    }

    if !skip_undated {
        let (fc, fp) = build_recall_filters("u", filters, 3, false, true);
        let sql = format!(
            "SELECT u.id, rank, u.event, u.note, u.tags, u.importance,
                    u.emotion, u.location, u.people, u.source, u.created_at
             FROM events_undated_fts f JOIN events_undated u ON u.id = f.rowid
             WHERE events_undated_fts MATCH ?1{} ORDER BY rank LIMIT ?2", fc);
        let params = fts_params(query, candidates, fp);
        recall_event_query(conn, &mut results, &sql, &params, false, &now);
    }

    let skip_things = filters.memory_type.as_ref().is_some_and(|t| t != "thing") || has_date_filter;
    let skip_persons = filters.memory_type.as_ref().is_some_and(|t| t != "person") || filters.source.is_some() || has_date_filter;
    let skip_places = filters.memory_type.as_ref().is_some_and(|t| t != "place") || filters.source.is_some() || has_date_filter;

    if !skip_things {
        let (fc, fp) = build_recall_filters("t", filters, 3, false, true);
        let sql = format!(
            "SELECT t.id, rank, t.thing, t.desc, t.category, t.tags, t.importance,
                    t.emotion, t.source, t.confidence, t.related, t.created_at
             FROM things_fts f JOIN things t ON t.id = f.rowid
             WHERE things_fts MATCH ?1{} ORDER BY rank LIMIT ?2", fc);
        let params = fts_params(query, candidates, fp);
        recall_generic_query(conn, &mut results, &sql, &params, &now,
            "thing", "T", &["Thing", "Description", "Category", "Tags", "Importance",
                            "Emotion", "Source", "Confidence", "Related", "Stored"]);
    }

    if !skip_persons {
        let (fc, fp) = build_recall_filters("p", filters, 3, false, false);
        let sql = format!(
            "SELECT p.id, rank, p.name, p.role, p.relationship, p.contact, p.met_at,
                    p.last_seen, p.note, p.tags, p.importance, p.emotion, p.created_at
             FROM persons_fts f JOIN persons p ON p.id = f.rowid
             WHERE persons_fts MATCH ?1{} ORDER BY rank LIMIT ?2", fc);
        let params = fts_params(query, candidates, fp);
        recall_generic_query(conn, &mut results, &sql, &params, &now,
            "person", "P", &["Name", "Role", "Relationship", "Contact", "Met at",
                             "Last seen", "Note", "Tags", "Importance", "Emotion", "Stored"]);
    }

    if !skip_places {
        let (fc, fp) = build_recall_filters("l", filters, 3, false, false);
        let sql = format!(
            "SELECT l.id, rank, l.name, l.desc, l.address, l.kind, l.note, l.tags,
                    l.importance, l.emotion, l.created_at
             FROM places_fts f JOIN places l ON l.id = f.rowid
             WHERE places_fts MATCH ?1{} ORDER BY rank LIMIT ?2", fc);
        let params = fts_params(query, candidates, fp);
        recall_generic_query(conn, &mut results, &sql, &params, &now,
            "place", "L", &["Name", "Description", "Address", "Kind", "Note", "Tags",
                            "Importance", "Emotion", "Stored"]);
    }

    results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    results
}

/// Recall all memories without FTS (wildcard query). Scans tables directly.
fn recall_all_inner(conn: &Connection, limit: usize, filters: &RecallFilters) -> Vec<RecallResult> {
    let mut results = Vec::new();
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();

    let has_date_filter = filters.date_from.is_some() || filters.date_to.is_some();
    let skip_dated = filters.memory_type.as_ref().is_some_and(|t| !matches!(t.as_str(), "event" | "events"));
    let skip_undated = filters.memory_type.as_ref().is_some_and(|t| !matches!(t.as_str(), "undated" | "events")) || has_date_filter;

    if !skip_dated {
        let (fc, fp) = build_recall_filters("e", filters, 1, true, true);
        let (where_sql, mut params) = scan_where(fc, fp);
        params.push(Box::new(limit as i64));
        let sql = format!(
            "SELECT e.id, 0.0 as rank, e.event, e.datetime, e.note, e.tags, e.importance,
                    e.emotion, e.location, e.people, e.source, e.created_at
             FROM events e {} ORDER BY e.importance DESC, e.created_at DESC LIMIT ?{}",
            where_sql, params.len());
        recall_event_query(conn, &mut results, &sql, &params, true, &now);
    }

    if !skip_undated {
        let (fc, fp) = build_recall_filters("u", filters, 1, false, true);
        let (where_sql, mut params) = scan_where(fc, fp);
        params.push(Box::new(limit as i64));
        let sql = format!(
            "SELECT u.id, 0.0 as rank, u.event, u.note, u.tags, u.importance,
                    u.emotion, u.location, u.people, u.source, u.created_at
             FROM events_undated u {} ORDER BY u.importance DESC, u.created_at DESC LIMIT ?{}",
            where_sql, params.len());
        recall_event_query(conn, &mut results, &sql, &params, false, &now);
    }

    let skip_things = filters.memory_type.as_ref().is_some_and(|t| t != "thing") || has_date_filter;
    let skip_persons = filters.memory_type.as_ref().is_some_and(|t| t != "person") || filters.source.is_some() || has_date_filter;
    let skip_places = filters.memory_type.as_ref().is_some_and(|t| t != "place") || filters.source.is_some() || has_date_filter;

    if !skip_things {
        let (fc, fp) = build_recall_filters("t", filters, 1, false, true);
        let (where_sql, mut params) = scan_where(fc, fp);
        params.push(Box::new(limit as i64));
        let sql = format!(
            "SELECT t.id, 0.0 as rank, t.thing, t.desc, t.category, t.tags, t.importance,
                    t.emotion, t.source, t.confidence, t.related, t.created_at
             FROM things t {} ORDER BY t.importance DESC, t.created_at DESC LIMIT ?{}",
            where_sql, params.len());
        recall_generic_query(conn, &mut results, &sql, &params, &now,
            "thing", "T", &["Thing", "Description", "Category", "Tags", "Importance",
                            "Emotion", "Source", "Confidence", "Related", "Stored"]);
    }

    if !skip_persons {
        let (fc, fp) = build_recall_filters("p", filters, 1, false, false);
        let (where_sql, mut params) = scan_where(fc, fp);
        params.push(Box::new(limit as i64));
        let sql = format!(
            "SELECT p.id, 0.0 as rank, p.name, p.role, p.relationship, p.contact, p.met_at,
                    p.last_seen, p.note, p.tags, p.importance, p.emotion, p.created_at
             FROM persons p {} ORDER BY p.importance DESC, p.created_at DESC LIMIT ?{}",
            where_sql, params.len());
        recall_generic_query(conn, &mut results, &sql, &params, &now,
            "person", "P", &["Name", "Role", "Relationship", "Contact", "Met at",
                             "Last seen", "Note", "Tags", "Importance", "Emotion", "Stored"]);
    }

    if !skip_places {
        let (fc, fp) = build_recall_filters("l", filters, 1, false, false);
        let (where_sql, mut params) = scan_where(fc, fp);
        params.push(Box::new(limit as i64));
        let sql = format!(
            "SELECT l.id, 0.0 as rank, l.name, l.desc, l.address, l.kind, l.note, l.tags,
                    l.importance, l.emotion, l.created_at
             FROM places l {} ORDER BY l.importance DESC, l.created_at DESC LIMIT ?{}",
            where_sql, params.len());
        recall_generic_query(conn, &mut results, &sql, &params, &now,
            "place", "L", &["Name", "Description", "Address", "Kind", "Note", "Tags",
                            "Importance", "Emotion", "Stored"]);
    }

    results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    results.truncate(limit);
    results
}

/// Prepend FTS query + limit to filter params.
fn fts_params(query: &str, limit: usize, filter_params: Vec<Box<dyn rusqlite::types::ToSql>>) -> Vec<Box<dyn rusqlite::types::ToSql>> {
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::with_capacity(2 + filter_params.len());
    params.push(Box::new(query.to_string()));
    params.push(Box::new(limit as i64));
    params.extend(filter_params);
    params
}

/// Convert " AND ..." filter clauses into a standalone WHERE clause for non-FTS queries.
fn scan_where(clauses: String, params: Vec<Box<dyn rusqlite::types::ToSql>>) -> (String, Vec<Box<dyn rusqlite::types::ToSql>>) {
    let where_sql = if clauses.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", clauses.trim_start_matches(" AND "))
    };
    (where_sql, params)
}

/// Execute a query and build event results.
fn recall_event_query(
    conn: &Connection,
    results: &mut Vec<RecallResult>,
    sql: &str,
    params: &[Box<dyn rusqlite::types::ToSql>],
    dated: bool,
    now: &str,
) {
    let mut stmt = match conn.prepare(sql) {
        Ok(s) => s,
        Err(e) => { eprintln!("query error: {}", e); return; }
    };
    let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    match stmt.query_map(param_refs.as_slice(), |row| {
        Ok(build_event_result(row, dated, now))
    }) {
        Ok(rows) => {
            for r in rows.flatten() {
                results.push(r);
            }
        }
        Err(e) => eprintln!("search error: {}", e),
    }
}

#[allow(clippy::too_many_arguments)]
fn recall_generic_query(
    conn: &Connection,
    results: &mut Vec<RecallResult>,
    sql: &str,
    params: &[Box<dyn rusqlite::types::ToSql>],
    now: &str,
    memory_type: &str,
    prefix: &str,
    labels: &[&str],
) {
    let mut stmt = match conn.prepare(sql) {
        Ok(s) => s,
        Err(e) => { eprintln!("query error: {}", e); return; }
    };
    let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();

    let mem_type = memory_type.to_string();
    let pfx = prefix.to_string();
    let labels_owned: Vec<String> = labels.iter().map(|s| s.to_string()).collect();

    match stmt.query_map(param_refs.as_slice(), |row| {
        let id: i64 = row.get(0).unwrap_or(0);
        let fts_rank: f64 = row.get(1).unwrap_or(0.0);
        let mut fields = Vec::new();
        let mut importance: i64 = 5;

        for (i, label) in labels_owned.iter().enumerate() {
            let col = 2 + i;
            if label == "Importance" || label == "Confidence" {
                if let Ok(v) = row.get::<_, i64>(col) {
                    if label == "Importance" { importance = v; }
                    fields.push((label.clone(), v.to_string()));
                }
            } else if let Ok(v) = row.get::<_, String>(col)
                && !v.is_empty() {
                    fields.push((label.clone(), v));
                }
        }

        let relevance = relevance(fts_rank);
        let score = composite_score(relevance, importance, None, now);
        Ok(RecallResult {
            memory_type: mem_type.clone(),
            mid: memory_id(&pfx, id),
            score,
            relevance,
            fields,
        })
    }) {
        Ok(rows) => {
            for r in rows.flatten() {
                results.push(r);
            }
        }
        Err(e) => eprintln!("search error: {}", e),
    }
}

/// FTS5 rank (bm25) is negative, more negative = more relevant. Map to 0..1, higher = more relevant.
fn relevance(fts_rank: f64) -> f64 {
    fts_rank.abs() / (1.0 + fts_rank.abs())
}

/// Composite score: 0.4 * relevance + 0.3 * importance_norm + 0.3 * recency
fn composite_score(relevance: f64, importance: i64, datetime: Option<&str>, now: &str) -> f64 {
    // Importance normalized to 0..1
    let importance_norm = importance as f64 / 10.0;

    // Recency: exponential decay, half-life 30 days
    let recency = match datetime {
        Some(dt) => {
            let dt_date = &dt[..10.min(dt.len())];
            let now_date = &now[..10.min(now.len())];
            let days = date_diff_days(dt_date, now_date).abs() as f64;
            let half_life = 30.0;
            (-days * 2.0_f64.ln() / half_life).exp()
        }
        None => 0.5, // undated gets neutral recency
    };

    0.4 * relevance + 0.3 * importance_norm + 0.3 * recency
}

/// Approximate day difference between two ISO dates (YYYY-MM-DD).
fn date_diff_days(a: &str, b: &str) -> i64 {
    let parse = |s: &str| -> Option<i64> {
        let parts: Vec<&str> = s.split('-').collect();
        if parts.len() != 3 { return None; }
        let y: i64 = parts[0].parse().ok()?;
        let m: i64 = parts[1].parse().ok()?;
        let d: i64 = parts[2].parse().ok()?;
        Some(y * 365 + m * 30 + d)
    };
    match (parse(a), parse(b)) {
        (Some(da), Some(db)) => da - db,
        _ => 0,
    }
}

fn build_event_result(row: &Row, dated: bool, now: &str) -> RecallResult {
    let id: i64 = row.get(0).unwrap_or(0);
    let fts_rank: f64 = row.get(1).unwrap_or(0.0);
    let mut fields = Vec::new();

    let event: String = row.get(2).unwrap_or_default();
    fields.push(("What".into(), event));

    let datetime: Option<String> = if dated {
        let dt: Option<String> = row.get(3).ok();
        if let Some(ref dt) = dt {
            fields.push(("When".into(), dt.clone()));
        }
        dt
    } else {
        None
    };

    let labels = &["Note", "Tags", "Importance", "Emotion", "Location", "People", "Source", "Stored"];
    let start = if dated { 4 } else { 3 };
    let mut importance: i64 = 5;

    for (i, label) in labels.iter().enumerate() {
        let col = start + i;
        if *label == "Importance" {
            if let Ok(v) = row.get::<_, i64>(col) {
                importance = v;
                fields.push((label.to_string(), v.to_string()));
            }
        } else if let Ok(v) = row.get::<_, String>(col)
            && !v.is_empty() {
                fields.push((label.to_string(), v));
            }
    }

    let relevance = relevance(fts_rank);
    let score = composite_score(relevance, importance, datetime.as_deref(), now);
    let prefix = if dated { "E" } else { "U" };
    RecallResult {
        memory_type: if dated { "event".into() } else { "event (undated)".into() },
        mid: memory_id(prefix, id),
        score,
        relevance,
        fields,
    }
}
