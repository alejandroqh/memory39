mod db;
mod llm;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "memory39",
    version,
    about = "Temporal-priority memory system for AI agents"
)]
struct Cli {
    /// Path to SQLite database file
    #[arg(long, default_value = "memory39.db")]
    db: PathBuf,

    /// Use in-memory database (for benchmarks/testing, not persistent)
    #[arg(long)]
    ram: bool,

    /// LLM provider: deepseek, groq, openai, ollama (for ingest command)
    #[arg(long, default_value = "deepseek")]
    llm: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Ingest a conversation: LLM extracts key facts and stores them
    Ingest {
        /// Conversation text (or - for stdin)
        input: String,
        /// Optional user/namespace for isolation
        #[arg(long)]
        user_id: Option<String>,
        /// Optional timestamp (YYYY-MM-DD)
        #[arg(long)]
        timestamp: Option<String>,
    },
    /// Remember an event (something that happened)
    Event {
        /// What happened (max 255 chars)
        event: String,
        /// Date in ISO 8601 format (YYYY-MM-DD). Omit for undated event
        #[arg(long)]
        date: Option<String>,
        /// Time in HH:MM format. Defaults to now
        #[arg(long)]
        time: Option<String>,
        /// Additional note (max 255 chars)
        #[arg(long)]
        note: Option<String>,
        /// Comma-separated tags (max 255 chars)
        #[arg(long)]
        tags: Option<String>,
        /// Importance level 0-10 (0 = not important, 10 = extreme)
        #[arg(long, default_value = "5")]
        importance: u8,
        /// Emotional valence: positive, negative, neutral, or free text
        #[arg(long)]
        emotion: Option<String>,
        /// Where it happened (max 255 chars)
        #[arg(long)]
        location: Option<String>,
        /// Who was involved, comma-separated (max 255 chars)
        #[arg(long)]
        people: Option<String>,
        /// How you know: experienced, told, read, observed
        #[arg(long)]
        source: Option<String>,
    },
    /// Remember a thing (something that exists)
    Thing {
        /// What to remember (max 255 chars)
        thing: String,
        /// Description (max 255 chars)
        #[arg(long)]
        desc: Option<String>,
        /// Category (free text)
        #[arg(long)]
        category: Option<String>,
        /// Comma-separated tags (max 255 chars)
        #[arg(long)]
        tags: Option<String>,
        /// Importance level 0-10 (0 = not important, 10 = extreme)
        #[arg(long, default_value = "5")]
        importance: u8,
        /// Emotional valence: positive, negative, neutral, or free text
        #[arg(long)]
        emotion: Option<String>,
        /// Where this knowledge came from (max 255 chars)
        #[arg(long)]
        source: Option<String>,
        /// How certain 0-10 (0 = guess, 10 = absolute fact)
        #[arg(long, default_value = "5")]
        confidence: u8,
        /// Comma-separated related concepts (max 255 chars)
        #[arg(long)]
        related: Option<String>,
    },
    /// Remember a person (social memory)
    Person {
        /// Person's name (max 255 chars)
        name: String,
        /// Role or title (max 255 chars)
        #[arg(long)]
        role: Option<String>,
        /// Relationship to you: friend, colleague, family, etc.
        #[arg(long)]
        relationship: Option<String>,
        /// Contact info: email, phone, handle (max 255 chars)
        #[arg(long)]
        contact: Option<String>,
        /// Where you met or know them from (max 255 chars)
        #[arg(long)]
        met_at: Option<String>,
        /// Last time you interacted (YYYY-MM-DD)
        #[arg(long)]
        last_seen: Option<String>,
        /// Additional note (max 255 chars)
        #[arg(long)]
        note: Option<String>,
        /// Comma-separated tags (max 255 chars)
        #[arg(long)]
        tags: Option<String>,
        /// Emotional valence: positive, negative, neutral, or free text
        #[arg(long)]
        emotion: Option<String>,
        /// Importance level 0-10 (0 = not important, 10 = extreme)
        #[arg(long, default_value = "5")]
        importance: u8,
    },
    /// Forget a memory by its ID (e.g. E3, U1)
    Forget {
        /// Memory ID to delete
        id: String,
    },
    /// Alter a memory by its ID (e.g. E3, U1)
    Alter {
        /// Memory ID to modify
        id: String,
        /// New event/thing text
        #[arg(long)]
        event: Option<String>,
        /// New note
        #[arg(long)]
        note: Option<String>,
        /// New tags
        #[arg(long)]
        tags: Option<String>,
        /// New importance 0-10
        #[arg(long)]
        importance: Option<u8>,
        /// New emotion
        #[arg(long)]
        emotion: Option<String>,
        /// New location
        #[arg(long)]
        location: Option<String>,
        /// New people
        #[arg(long)]
        people: Option<String>,
        /// New source
        #[arg(long)]
        source: Option<String>,
        /// New date (YYYY-MM-DD), only for dated events
        #[arg(long)]
        date: Option<String>,
        /// New time (HH:MM), only for dated events
        #[arg(long)]
        time: Option<String>,
    },
    /// Search across all memories
    Recall {
        /// Search query
        query: String,
        /// Max results to return
        #[arg(short, long, default_value = "10")]
        limit: usize,
        /// Minimum importance (0-10)
        #[arg(long)]
        min: Option<u8>,
        /// Filter by date range start (YYYY-MM-DD)
        #[arg(long)]
        from: Option<String>,
        /// Filter by date range end (YYYY-MM-DD)
        #[arg(long)]
        to: Option<String>,
        /// Filter by memory kind: event, undated, thing, person, place
        #[arg(long)]
        kind: Option<String>,
    },
    /// Find connections between 2-3 concepts across all memories
    Connect {
        /// Concepts to connect (2-3 required)
        concepts: Vec<String>,
        /// Minimum importance (0-10)
        #[arg(long)]
        min: Option<u8>,
        /// Timeout in milliseconds
        #[arg(long, default_value = "2000")]
        timeout: u64,
    },
    /// Remember a place (spatial memory)
    Place {
        /// Name of the place (max 255 chars)
        name: String,
        /// Description (max 255 chars)
        #[arg(long)]
        desc: Option<String>,
        /// Address or coordinates (max 255 chars)
        #[arg(long)]
        address: Option<String>,
        /// Type: city, building, room, outdoor, virtual, etc.
        #[arg(long)]
        kind: Option<String>,
        /// Additional note (max 255 chars)
        #[arg(long)]
        note: Option<String>,
        /// Comma-separated tags (max 255 chars)
        #[arg(long)]
        tags: Option<String>,
        /// Emotional valence: positive, negative, neutral, or free text
        #[arg(long)]
        emotion: Option<String>,
        /// Importance level 0-10 (0 = not important, 10 = extreme)
        #[arg(long, default_value = "5")]
        importance: u8,
    },
}

#[tokio::main]
async fn main() {
    llm::load_dotenv();
    let cli = Cli::parse();
    let conn = if cli.ram {
        db::open_ram().expect("failed to open in-memory database")
    } else {
        db::open(&cli.db).expect("failed to open database")
    };

    match cli.command {
        Command::Ingest { input, user_id: _, timestamp: _ } => {
            let config = llm::LlmConfig::preset(&cli.llm)
                .unwrap_or_else(|| {
                    eprintln!("Unknown LLM provider: {}. Use: deepseek, groq, openai, ollama", cli.llm);
                    std::process::exit(1);
                });

            let text = if input == "-" {
                use std::io::Read;
                let mut buf = String::new();
                std::io::stdin().read_to_string(&mut buf).expect("failed to read stdin");
                buf
            } else {
                input
            };

            eprintln!("Memory agent via {} ({})...", cli.llm, config.model);

            // The recall callback: LLM calls this to check existing memories
            let recall_fn = |query: &str| -> String {
                let filters = db::RecallFilters {
                    min_importance: None,
                    date_from: None,
                    date_to: None,
                    memory_type: None,
                };
                let results = db::recall(&conn, query, 5, &filters);
                if results.is_empty() {
                    "No existing memories found.".into()
                } else {
                    let mut out = String::new();
                    for r in &results {
                        out.push_str(&format!("[{}] {} (score: {:.2})\n", r.mid, r.memory_type, r.score));
                        for (k, v) in &r.fields {
                            out.push_str(&format!("  {}: {}\n", k, v));
                        }
                    }
                    out
                }
            };

            let actions = match llm::ingest_conversation(&config, &text, &recall_fn).await {
                Ok(a) => a,
                Err(e) => {
                    eprintln!("Failed: {}", e);
                    std::process::exit(1);
                }
            };

            let created_at = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();
            let mut stored = 0;
            let mut updated = 0;
            let mut forgotten = 0;

            for action in &actions {
                match action {
                    llm::MemoryAction::Recall { query } => {
                        eprintln!("  recall: {}", query);
                    }
                    llm::MemoryAction::Event { event, date, time, note, tags, importance, emotion, location, people, source } => {
                        let datetime = date.as_ref().map(|d| {
                            let t = time.as_deref().unwrap_or("00:00");
                            format!("{} {}", d, t)
                        });
                        let id = db::insert_event(&conn, event, datetime.as_deref(), note.as_deref(), tags.as_deref(), *importance, emotion.as_deref(), location.as_deref(), people.as_deref(), source.as_deref(), &created_at).expect("failed to store");
                        let mid = if datetime.is_some() { format!("E{}", id) } else { format!("U{}", id) };
                        println!("[{}] event: {}", mid, event);
                        stored += 1;
                    }
                    llm::MemoryAction::Thing { thing, desc, category, tags, importance, emotion, source, confidence, related } => {
                        let id = db::insert_thing(&conn, thing, desc.as_deref(), category.as_deref(), tags.as_deref(), *importance, emotion.as_deref(), source.as_deref(), *confidence, related.as_deref(), &created_at).expect("failed to store");
                        let mid = format!("T{}", id);
                        println!("[{}] thing: {}", mid, thing);
                        stored += 1;
                    }
                    llm::MemoryAction::Person { name, role, relationship, note, tags, importance, emotion } => {
                        let id = db::insert_person(&conn, name, role.as_deref(), relationship.as_deref(), None, None, None, note.as_deref(), tags.as_deref(), *importance, emotion.as_deref(), &created_at).expect("failed to store");
                        let mid = format!("P{}", id);
                        println!("[{}] person: {}", mid, name);
                        stored += 1;
                    }
                    llm::MemoryAction::Place { name, desc, address, kind, note, tags, importance, emotion } => {
                        let id = db::insert_place(&conn, name, desc.as_deref(), address.as_deref(), kind.as_deref(), note.as_deref(), tags.as_deref(), *importance, emotion.as_deref(), &created_at).expect("failed to store");
                        let mid = format!("L{}", id);
                        println!("[{}] place: {}", mid, name);
                        stored += 1;
                    }
                    llm::MemoryAction::Alter { id, fields } => {
                        match db::alter(&conn, id, fields) {
                            Ok(true) => { println!("[{}] altered", id); updated += 1; }
                            Ok(false) => eprintln!("  alter {}: not found", id),
                            Err(e) => eprintln!("  alter {}: {}", id, e),
                        }
                    }
                    llm::MemoryAction::Forget { id } => {
                        match db::forget(&conn, id) {
                            Ok(true) => { println!("[{}] forgotten", id); forgotten += 1; }
                            Ok(false) => eprintln!("  forget {}: not found", id),
                            Err(e) => eprintln!("  forget {}: {}", id, e),
                        }
                    }
                }
            }

            println!("\nDone: {} stored, {} updated, {} forgotten", stored, updated, forgotten);
        }
        Command::Event {
            event,
            date,
            time,
            note,
            tags,
            importance,
            emotion,
            location,
            people,
            source,
        } => {
            let created_at = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();
            let datetime = date.as_ref().map(|d| {
                let t = time.as_deref().unwrap_or("00:00");
                format!("{} {}", d, t)
            });
            let id = db::insert_event(
                &conn,
                &event,
                datetime.as_deref(),
                note.as_deref(),
                tags.as_deref(),
                importance,
                emotion.as_deref(),
                location.as_deref(),
                people.as_deref(),
                source.as_deref(),
                &created_at,
            )
            .expect("failed to save event");

            let mid = if datetime.is_some() { format!("E{}", id) } else { format!("U{}", id) };
            println!("Event remembered [{}]:", mid);
            if let Some(dt) = &datetime {
                println!("  When:       {}", dt);
            }
            println!("  What:       {}", event);
            if let Some(v) = &note {
                println!("  Note:       {}", v);
            }
            if let Some(v) = &emotion {
                println!("  Emotion:    {}", v);
            }
            if let Some(v) = &location {
                println!("  Location:   {}", v);
            }
            if let Some(v) = &people {
                println!("  People:     {}", v);
            }
            if let Some(v) = &source {
                println!("  Source:      {}", v);
            }
            if let Some(v) = &tags {
                println!("  Tags:       {}", v);
            }
            println!("  Importance: {}", importance);
        }
        Command::Forget { id } => {
            match db::forget(&conn, &id) {
                Ok(true) => println!("Forgotten: {}", id),
                Ok(false) => println!("Not found: {}", id),
                Err(e) => eprintln!("Error: {}", e),
            }
        }
        Command::Alter {
            id,
            event,
            note,
            tags,
            importance,
            emotion,
            location,
            people,
            source,
            date,
            time,
        } => {
            let mut changes: Vec<(String, String)> = Vec::new();
            if let Some(v) = event { changes.push(("event".into(), v)); }
            if let Some(v) = note { changes.push(("note".into(), v)); }
            if let Some(v) = tags { changes.push(("tags".into(), v)); }
            if let Some(v) = importance { changes.push(("importance".into(), v.to_string())); }
            if let Some(v) = emotion { changes.push(("emotion".into(), v)); }
            if let Some(v) = location { changes.push(("location".into(), v)); }
            if let Some(v) = people { changes.push(("people".into(), v)); }
            if let Some(v) = source { changes.push(("source".into(), v)); }
            if let Some(d) = &date {
                let t = time.as_deref().unwrap_or("00:00");
                changes.push(("datetime".into(), format!("{} {}", d, t)));
            } else if time.is_some() {
                eprintln!("Warning: --time without --date ignored (need both to update datetime)");
            }

            if changes.is_empty() {
                println!("Nothing to alter. Provide at least one field to change.");
            } else {
                match db::alter(&conn, &id, &changes) {
                    Ok(true) => println!("Altered: {}", id),
                    Ok(false) => println!("Not found: {}", id),
                    Err(e) => eprintln!("Error: {}", e),
                }
            }
        }
        Command::Recall { query, limit, min, from, to, kind } => {
            let filters = db::RecallFilters {
                min_importance: min,
                date_from: from,
                date_to: to,
                memory_type: kind,
            };
            let results = db::recall(&conn, &query, limit, &filters);
            if results.is_empty() {
                println!("No memories found for: {}", query);
            } else {
                println!("Found {} memories for: {}", results.len(), query);
                for r in &results {
                    println!("\n[{}] {} (score: {:.2})", r.mid, r.memory_type, r.score);
                    for (key, val) in &r.fields {
                        println!("  {:10} {}", format!("{}:", key), val);
                    }
                }
            }
        }
        Command::Connect { concepts, min, timeout } => {
            if concepts.len() < 2 || concepts.len() > 3 {
                eprintln!("Error: provide 2 or 3 concepts");
                std::process::exit(1);
            }
            let timeout_dur = std::time::Duration::from_millis(timeout);
            let result = db::find_connections(&conn, &concepts, min, timeout_dur);
            let elapsed = result.elapsed_ms;

            if result.connections.is_empty() {
                println!("No connections found for: {}", concepts.join(" + "));
            } else {
                println!("Connections for: {}\n", concepts.join(" + "));
                for c in &result.connections {
                    match &c.kind {
                        db::ConnectionKind::Direct { mid } => {
                            println!("[direct] {} (score: {:.2})", mid, c.score);
                            println!("  All concepts in same memory");
                            for (key, val) in &c.fields {
                                println!("  {:10} {}", format!("{}:", key), val);
                            }
                        }
                        db::ConnectionKind::Shared { mid_a, mid_b, field, value } => {
                            println!("[shared:{}] {} <> {} (score: {:.2})", field, mid_a, mid_b, c.score);
                            println!("  Linked by: {}", value);
                        }
                        db::ConnectionKind::Bridge { mid_a, mid_b, via_field, via_value } => {
                            println!("[bridge:{}] {} -> {} (score: {:.2})", via_field, mid_a, mid_b, c.score);
                            println!("  Via: {}", via_value);
                        }
                    }
                    println!();
                }
            }
            println!("Found {} connections in {}ms", result.connections.len(), elapsed);
        }
        Command::Thing { thing, desc, category, tags, importance, emotion, source, confidence, related } => {
            let created_at = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();
            let id = db::insert_thing(&conn, &thing, desc.as_deref(), category.as_deref(), tags.as_deref(), importance, emotion.as_deref(), source.as_deref(), confidence, related.as_deref(), &created_at).expect("failed to store thing");
            println!("Thing remembered [T{}]:", id);
            println!("  What:       {}", thing);
            if let Some(v) = &desc { println!("  Desc:       {}", v); }
            if let Some(v) = &category { println!("  Category:   {}", v); }
            if let Some(v) = &emotion { println!("  Emotion:    {}", v); }
            if let Some(v) = &source { println!("  Source:      {}", v); }
            if let Some(v) = &related { println!("  Related:    {}", v); }
            if let Some(v) = &tags { println!("  Tags:       {}", v); }
            println!("  Confidence: {}", confidence);
            println!("  Importance: {}", importance);
        }
        Command::Person { name, role, relationship, contact, met_at, last_seen, note, tags, emotion, importance } => {
            let created_at = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();
            let id = db::insert_person(&conn, &name, role.as_deref(), relationship.as_deref(), contact.as_deref(), met_at.as_deref(), last_seen.as_deref(), note.as_deref(), tags.as_deref(), importance, emotion.as_deref(), &created_at).expect("failed to store person");
            println!("Person remembered [P{}]:", id);
            println!("  Name:       {}", name);
            if let Some(v) = &role { println!("  Role:       {}", v); }
            if let Some(v) = &relationship { println!("  Relation:   {}", v); }
            if let Some(v) = &contact { println!("  Contact:    {}", v); }
            if let Some(v) = &met_at { println!("  Met at:     {}", v); }
            if let Some(v) = &last_seen { println!("  Last seen:  {}", v); }
            if let Some(v) = &note { println!("  Note:       {}", v); }
            if let Some(v) = &emotion { println!("  Emotion:    {}", v); }
            if let Some(v) = &tags { println!("  Tags:       {}", v); }
            println!("  Importance: {}", importance);
        }
        Command::Place { name, desc, address, kind, note, tags, emotion, importance } => {
            let created_at = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();
            let id = db::insert_place(&conn, &name, desc.as_deref(), address.as_deref(), kind.as_deref(), note.as_deref(), tags.as_deref(), importance, emotion.as_deref(), &created_at).expect("failed to store place");
            println!("Place remembered [L{}]:", id);
            println!("  Name:       {}", name);
            if let Some(v) = &desc { println!("  Desc:       {}", v); }
            if let Some(v) = &address { println!("  Address:    {}", v); }
            if let Some(v) = &kind { println!("  Kind:       {}", v); }
            if let Some(v) = &note { println!("  Note:       {}", v); }
            if let Some(v) = &emotion { println!("  Emotion:    {}", v); }
            if let Some(v) = &tags { println!("  Tags:       {}", v); }
            println!("  Importance: {}", importance);
        }
    }
}
