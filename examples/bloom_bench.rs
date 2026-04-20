use memory39::db;
use std::time::Instant;

const USAGE: &str = "\
Usage: cargo run --release --example bloom_bench -- <known-fact> <unknown-fact> [db-path]

  <known-fact>     A query string that matches at least one memory in the DB.
                   Exercises the FTS5 path (bloom says \"maybe\" -> search runs).
  <unknown-fact>   A short query (1 word, <=6 chars) that is NOT present anywhere.
                   Exercises the bloom-filter fast path (no disk I/O).
  [db-path]        Optional. Defaults to ~/.memory39/memory39.db.

Example:
  cargo run --release --example bloom_bench -- \"my project name\" xyzqq";

fn main() {
    let mut args = std::env::args().skip(1);
    let known = args.next().unwrap_or_else(|| {
        eprintln!("{USAGE}");
        std::process::exit(2);
    });
    let unknown = args.next().unwrap_or_else(|| {
        eprintln!("{USAGE}");
        std::process::exit(2);
    });
    let db_path = args.next().unwrap_or_else(|| {
        let home = dirs::home_dir().expect("no home dir");
        home.join(".memory39").join("memory39.db").to_string_lossy().into_owned()
    });

    let mdb = db::open(std::path::Path::new(&db_path)).expect("failed to open db");
    let filters = db::RecallFilters {
        min_importance: None,
        date_from: None,
        date_to: None,
        memory_type: None,
        source: None,
    };

    let sanity_hit = mdb.recall(&known, 1, 0, &filters);
    let sanity_miss = mdb.recall(&unknown, 1, 0, &filters);
    if sanity_hit.is_empty() {
        eprintln!("warning: known fact '{known}' returned 0 results; benchmark may be measuring another miss.");
    }
    if !sanity_miss.is_empty() {
        eprintln!("warning: unknown fact '{unknown}' returned results; benchmark may not exercise the fast path.");
    }

    for _ in 0..100 {
        let _ = mdb.recall(&known, 10, 0, &filters);
        let _ = mdb.recall(&unknown, 10, 0, &filters);
    }

    let n: u32 = 50_000;

    let t = Instant::now();
    for _ in 0..n {
        let _ = std::hint::black_box(mdb.recall(&unknown, 10, 0, &filters));
    }
    let miss_ns = t.elapsed().as_nanos() / (n as u128);

    let t = Instant::now();
    for _ in 0..n {
        let _ = std::hint::black_box(mdb.recall(&known, 10, 0, &filters));
    }
    let hit_ns = t.elapsed().as_nanos() / (n as u128);

    println!("DB:       {db_path}");
    println!("Iterations per query: {n}");
    println!();
    println!("{:<25} {:>10}   {:>15}", "query", "avg/op", "throughput");
    println!("{:<25} {:>10}   {:>15}", "-----", "------", "----------");
    println!("{:<25} {:>7} ns   {:>11} ops/s", "unknown fact (bloom)", miss_ns, 1_000_000_000u128 / miss_ns.max(1));
    println!("{:<25} {:>7} ns   {:>11} ops/s", "known fact (FTS5)", hit_ns, 1_000_000_000u128 / hit_ns.max(1));
    println!();
    println!("Speedup (unknown vs known): {:.1}x", hit_ns as f64 / miss_ns.max(1) as f64);
}
