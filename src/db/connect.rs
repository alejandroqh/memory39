use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use super::crud::memory_id;
use super::schema::expand_query_for_prefix;

const LINKABLE_FIELDS: &[&str] = &["tags", "emotion", "location", "people", "related"];
pub const DEFAULT_CONNECT_LIMIT: usize = 30;

pub enum ConnectionKind {
    Direct { mid: String },
    Shared { mid_a: String, mid_b: String, field: String, value: String },
    Bridge { mid_a: String, mid_b: String, via_field: String, via_value: String },
}

pub struct Connection_ {
    pub kind: ConnectionKind,
    pub score: f64,
    pub fields: Vec<(String, String)>,
}

pub struct ConnectionResult {
    pub connections: Vec<Connection_>,
    pub elapsed_ms: u128,
}

/// A memory row with its fields extracted for comparison.
struct MemRow {
    mid: String,
    fields: HashMap<String, String>,
    importance: i64,
}

struct DirectHit {
    mid: String,
    importance: i64,
    fields: Vec<(String, String)>,
}

pub fn find_connections(
    conn: &rusqlite::Connection,
    concepts: &[String],
    min_importance: Option<u8>,
    limit: usize,
    timeout: Duration,
) -> ConnectionResult {
    let start = Instant::now();
    let mut connections: Vec<Connection_> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    // Phase 1: Direct - FTS5 AND query (exact then prefix fallback)
    let and_query = concepts.iter()
        .map(|c| format!("\"{}\"", c.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" AND ");

    phase_direct(conn, &and_query, min_importance, &mut connections, &mut seen);

    if connections.is_empty() {
        // Prefix fallback for direct phase
        let prefix_and = concepts.iter()
            .filter_map(|c| expand_query_for_prefix(c))
            .collect::<Vec<_>>();
        if prefix_and.len() == concepts.len() {
            let and_expanded = prefix_and.join(" AND ");
            phase_direct(conn, &and_expanded, min_importance, &mut connections, &mut seen);
        }
    }

    if connections.len() >= limit || start.elapsed() >= timeout {
        connections.truncate(limit);
        return ConnectionResult { connections, elapsed_ms: start.elapsed().as_millis() };
    }

    // Phase 2: Shared attributes - search each concept, cross-match fields
    let mut concept_rows: Vec<Vec<MemRow>> = Vec::new();
    for concept in concepts {
        let escaped = format!("\"{}\"", concept.replace('"', "\"\""));
        let mut rows = search_mem_rows(conn, &escaped, min_importance);
        // Prefix fallback if exact match found few rows
        if rows.len() < 5 {
            if let Some(expanded) = expand_query_for_prefix(concept) {
                let seen_mids: HashSet<String> = rows.iter().map(|r| r.mid.clone()).collect();
                let extra = search_mem_rows(conn, &expanded, min_importance);
                rows.extend(extra.into_iter().filter(|r| !seen_mids.contains(&r.mid)));
            }
        }
        concept_rows.push(rows);

        if start.elapsed() >= timeout {
            return ConnectionResult { connections, elapsed_ms: start.elapsed().as_millis() };
        }
    }

    phase_shared(&concept_rows, &mut connections, &mut seen);

    if connections.len() >= limit || start.elapsed() >= timeout {
        connections.truncate(limit);
        return ConnectionResult { connections, elapsed_ms: start.elapsed().as_millis() };
    }

    // Phase 3: Bridge - one-hop through field values
    phase_bridge(conn, &concept_rows, min_importance, timeout, start, &mut connections, &mut seen);

    connections.truncate(limit);
    ConnectionResult { connections, elapsed_ms: start.elapsed().as_millis() }
}

fn phase_direct(
    conn: &rusqlite::Connection,
    and_query: &str,
    min_importance: Option<u8>,
    connections: &mut Vec<Connection_>,
    seen: &mut HashSet<String>,
) {
    // Event tables (dated and undated)
    for (table, fts, prefix, dated) in &[
        ("events", "events_fts", "E", true),
        ("events_undated", "events_undated_fts", "U", false),
    ] {
        let min_clause = if min_importance.is_some() {
            format!(" AND t.importance >= {}", min_importance.unwrap())
        } else {
            String::new()
        };

        let sql = if *dated {
            format!(
                "SELECT t.id, t.event, t.datetime, t.importance
                 FROM {} f JOIN {} t ON t.id = f.rowid
                 WHERE {} MATCH ?1{}
                 LIMIT 20",
                fts, table, fts, min_clause
            )
        } else {
            format!(
                "SELECT t.id, t.event, t.importance
                 FROM {} f JOIN {} t ON t.id = f.rowid
                 WHERE {} MATCH ?1{}
                 LIMIT 20",
                fts, table, fts, min_clause
            )
        };

        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(e) => { eprintln!("phase_direct error: {}", e); continue; }
        };

        let hits: Vec<DirectHit> = match stmt.query_map([and_query], |row| {
            let id: i64 = row.get(0)?;
            let event: String = row.get(1)?;
            let mut fields = vec![("What".into(), event)];

            let imp: i64 = if *dated {
                if let Ok(dt) = row.get::<_, String>(2) {
                    fields.push(("When".into(), dt));
                }
                row.get(3).unwrap_or(5)
            } else {
                row.get(2).unwrap_or(5)
            };
            fields.push(("Importance".into(), imp.to_string()));

            Ok(DirectHit {
                mid: memory_id(prefix, id),
                importance: imp,
                fields,
            })
        }) {
            Ok(rows) => rows.flatten().collect(),
            Err(e) => { eprintln!("phase_direct search: {}", e); continue; }
        };

        for hit in hits {
            if seen.contains(&hit.mid) { continue; }
            seen.insert(hit.mid.clone());
            let score = (hit.importance as f64 / 10.0).max(0.5);
            connections.push(Connection_ {
                kind: ConnectionKind::Direct { mid: hit.mid },
                score,
                fields: hit.fields,
            });
        }
    }

    // Non-event tables: things, persons, places
    for (table, fts, prefix, label_col, label_name) in &[
        ("things", "things_fts", "T", "thing", "Thing"),
        ("persons", "persons_fts", "P", "name", "Name"),
        ("places", "places_fts", "L", "name", "Name"),
    ] {
        let min_clause = if min_importance.is_some() {
            format!(" AND t.importance >= {}", min_importance.unwrap())
        } else {
            String::new()
        };

        let sql = format!(
            "SELECT t.id, t.{}, t.importance
             FROM {} f JOIN {} t ON t.id = f.rowid
             WHERE {} MATCH ?1{}
             LIMIT 20",
            label_col, fts, table, fts, min_clause
        );

        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(e) => { eprintln!("phase_direct error: {}", e); continue; }
        };

        let hits: Vec<DirectHit> = match stmt.query_map([and_query], |row| {
            let id: i64 = row.get(0)?;
            let name: String = row.get(1)?;
            let imp: i64 = row.get(2).unwrap_or(5);
            let fields = vec![
                (label_name.to_string(), name),
                ("Importance".into(), imp.to_string()),
            ];
            Ok(DirectHit {
                mid: memory_id(prefix, id),
                importance: imp,
                fields,
            })
        }) {
            Ok(rows) => rows.flatten().collect(),
            Err(e) => { eprintln!("phase_direct search: {}", e); continue; }
        };

        for hit in hits {
            if seen.contains(&hit.mid) { continue; }
            seen.insert(hit.mid.clone());
            let score = (hit.importance as f64 / 10.0).max(0.5);
            connections.push(Connection_ {
                kind: ConnectionKind::Direct { mid: hit.mid },
                score,
                fields: hit.fields,
            });
        }
    }
}

fn search_mem_rows(
    conn: &rusqlite::Connection,
    fts_query: &str,
    min_importance: Option<u8>,
) -> Vec<MemRow> {
    let mut rows = Vec::new();

    for (table, fts, prefix, dated) in &[
        ("events", "events_fts", "E", true),
        ("events_undated", "events_undated_fts", "U", false),
    ] {
        let min_clause = if min_importance.is_some() {
            format!(" AND t.importance >= {}", min_importance.unwrap())
        } else {
            String::new()
        };

        let sql = if *dated {
            format!(
                "SELECT t.id, t.event, t.datetime, t.importance, t.tags, t.emotion, t.location, t.people
                 FROM {} f JOIN {} t ON t.id = f.rowid
                 WHERE {} MATCH ?1{}
                 LIMIT 50",
                fts, table, fts, min_clause
            )
        } else {
            format!(
                "SELECT t.id, t.event, t.importance, t.tags, t.emotion, t.location, t.people
                 FROM {} f JOIN {} t ON t.id = f.rowid
                 WHERE {} MATCH ?1{}
                 LIMIT 50",
                fts, table, fts, min_clause
            )
        };

        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => continue,
        };

        let hits: Vec<MemRow> = match stmt.query_map([fts_query], |row| {
            let id: i64 = row.get(0)?;
            let mid = memory_id(prefix, id);
            let mut field_map = HashMap::new();

            let imp: i64 = if *dated {
                if let Ok(dt) = row.get::<_, String>(2) {
                    field_map.insert("datetime".into(), dt);
                }
                let imp: i64 = row.get(3).unwrap_or(5);
                for (i, name) in ["tags", "emotion", "location", "people"].iter().enumerate() {
                    if let Ok(v) = row.get::<_, String>(4 + i) {
                        if !v.is_empty() { field_map.insert(name.to_string(), v); }
                    }
                }
                imp
            } else {
                let imp: i64 = row.get(2).unwrap_or(5);
                for (i, name) in ["tags", "emotion", "location", "people"].iter().enumerate() {
                    if let Ok(v) = row.get::<_, String>(3 + i) {
                        if !v.is_empty() { field_map.insert(name.to_string(), v); }
                    }
                }
                imp
            };

            Ok(MemRow { mid, fields: field_map, importance: imp })
        }) {
            Ok(r) => r.flatten().collect(),
            Err(_) => continue,
        };

        rows.extend(hits);
    }

    // Non-event tables: things, persons, places
    for (table, fts, prefix, linkable_fields) in &[
        ("things", "things_fts", "T", &["tags", "emotion", "related"][..]),
        ("persons", "persons_fts", "P", &["tags", "emotion"][..]),
        ("places", "places_fts", "L", &["tags", "emotion"][..]),
    ] {
        let min_clause = if min_importance.is_some() {
            format!(" AND t.importance >= {}", min_importance.unwrap())
        } else {
            String::new()
        };

        let extra_cols = linkable_fields.iter()
            .map(|f| format!("t.{}", f))
            .collect::<Vec<_>>()
            .join(", ");

        let sql = format!(
            "SELECT t.id, t.importance, {}
             FROM {} f JOIN {} t ON t.id = f.rowid
             WHERE {} MATCH ?1{}
             LIMIT 50",
            extra_cols, fts, table, fts, min_clause
        );

        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => continue,
        };

        let lf: Vec<String> = linkable_fields.iter().map(|s| s.to_string()).collect();

        let hits: Vec<MemRow> = match stmt.query_map([fts_query], |row| {
            let id: i64 = row.get(0)?;
            let mid = memory_id(prefix, id);
            let mut field_map = HashMap::new();

            let imp: i64 = row.get(1).unwrap_or(5);
            for (i, name) in lf.iter().enumerate() {
                if let Ok(v) = row.get::<_, String>(2 + i) {
                    if !v.is_empty() { field_map.insert(name.clone(), v); }
                }
            }

            Ok(MemRow { mid, fields: field_map, importance: imp })
        }) {
            Ok(r) => r.flatten().collect(),
            Err(_) => continue,
        };

        rows.extend(hits);
    }

    rows
}

fn phase_shared(
    concept_rows: &[Vec<MemRow>],
    connections: &mut Vec<Connection_>,
    seen: &mut HashSet<String>,
) {
    if concept_rows.len() < 2 { return; }

    for i in 0..concept_rows.len() {
        for j in (i + 1)..concept_rows.len() {
            for row_a in &concept_rows[i] {
                for row_b in &concept_rows[j] {
                    if row_a.mid == row_b.mid { continue; }

                    let pk = pair_key(&row_a.mid, &row_b.mid);
                    if seen.contains(&pk) { continue; }

                    for field in LINKABLE_FIELDS {
                        let field_s = field.to_string();
                        if let (Some(va), Some(vb)) = (row_a.fields.get(&field_s), row_b.fields.get(&field_s)) {
                            let overlap = find_overlap(va, vb);
                            if let Some(shared_val) = overlap {
                                seen.insert(pk.clone());
                                let avg_imp = (row_a.importance + row_b.importance) as f64 / 20.0;
                                connections.push(Connection_ {
                                    kind: ConnectionKind::Shared {
                                        mid_a: row_a.mid.clone(),
                                        mid_b: row_b.mid.clone(),
                                        field: field_s,
                                        value: shared_val,
                                    },
                                    score: 0.7 * avg_imp.max(0.3),
                                    fields: Vec::new(),
                                });
                                break;
                            }
                        }
                    }
                }
            }
        }
    }
}

fn phase_bridge(
    conn: &rusqlite::Connection,
    concept_rows: &[Vec<MemRow>],
    min_importance: Option<u8>,
    timeout: Duration,
    start: Instant,
    connections: &mut Vec<Connection_>,
    seen: &mut HashSet<String>,
) {
    if concept_rows.len() < 2 { return; }

    for i in 0..concept_rows.len() {
        for j in (i + 1)..concept_rows.len() {
            let rows_a = &concept_rows[i];
            let rows_b_mids: HashSet<String> = concept_rows[j].iter().map(|r| r.mid.clone()).collect();

            for row_a in rows_a {
                if start.elapsed() >= timeout { return; }

                for field in LINKABLE_FIELDS {
                    let field_s = field.to_string();
                    if let Some(val) = row_a.fields.get(&field_s) {
                        for token in val.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {
                            if start.elapsed() >= timeout { return; }

                            let escaped = format!("\"{}\"", token.replace('"', "\"\""));
                            let bridge_rows = search_mem_rows(conn, &escaped, min_importance);

                            for bridge in &bridge_rows {
                                if bridge.mid == row_a.mid { continue; }
                                if !rows_b_mids.contains(&bridge.mid) { continue; }

                                let pair_key = pair_key(&row_a.mid, &bridge.mid);
                                if seen.contains(&pair_key) { continue; }
                                seen.insert(pair_key);

                                let avg_imp = (row_a.importance + bridge.importance) as f64 / 20.0;
                                connections.push(Connection_ {
                                    kind: ConnectionKind::Bridge {
                                        mid_a: row_a.mid.clone(),
                                        mid_b: bridge.mid.clone(),
                                        via_field: field_s.clone(),
                                        via_value: token.to_string(),
                                    },
                                    score: 0.5 * avg_imp.max(0.3),
                                    fields: Vec::new(),
                                });
                            }
                        }
                    }
                }
            }
        }
    }
}

fn pair_key(a: &str, b: &str) -> String {
    if a < b { format!("{}+{}", a, b) } else { format!("{}+{}", b, a) }
}

/// Find overlap between two comma-separated value strings.
/// Uses linear scan — faster than HashSet for typical small tag lists (<10 items).
fn find_overlap(a: &str, b: &str) -> Option<String> {
    if a == b { return Some(a.to_string()); }

    let tags_b: Vec<&str> = b.split(',').map(|s| s.trim()).collect();
    let mut overlaps = Vec::new();
    for ta in a.split(',').map(|s| s.trim()) {
        if tags_b.contains(&ta) {
            overlaps.push(ta);
        }
    }

    if overlaps.is_empty() { None } else { Some(overlaps.join(",")) }
}
