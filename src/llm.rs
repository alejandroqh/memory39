use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Load .env file from current dir or exe dir. Sets vars into process env.
pub fn load_dotenv() {
    let paths = [
        std::env::current_dir().ok().map(|p| p.join(".env")),
        std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join(".env"))),
    ];
    for path in paths.iter().flatten() {
        if let Ok(content) = std::fs::read_to_string(path) {
            for line in content.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') { continue; }
                if let Some((key, val)) = line.split_once('=') {
                    let key = key.trim();
                    let val = val.trim();
                    if !val.is_empty() && std::env::var(key).is_err() {
                        // SAFETY: called once at startup before any threads spawn
                        unsafe { std::env::set_var(key, val); }
                    }
                }
            }
            return;
        }
    }
}

// --- LLM Config ---

pub struct LlmConfig {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
}

impl LlmConfig {
    pub fn preset(name: &str) -> Option<Self> {
        let api_key_var = match name {
            "deepseek" => "DEEPSEEK_API_KEY",
            "groq" => "GROQ_API_KEY",
            "openai" => "OPENAI_API_KEY",
            "ollama" => "",
            _ => return None,
        };
        let api_key = if api_key_var.is_empty() {
            String::new()
        } else {
            std::env::var(api_key_var).unwrap_or_default()
        };
        Some(match name {
            "deepseek" => Self { base_url: "https://api.deepseek.com".into(), api_key, model: "deepseek-chat".into() },
            "groq" => Self { base_url: "https://api.groq.com/openai".into(), api_key, model: "llama-3.3-70b-versatile".into() },
            "openai" => Self { base_url: "https://api.openai.com".into(), api_key, model: "gpt-4o-mini".into() },
            "ollama" => Self { base_url: "http://localhost:11434".into(), api_key, model: "llama3.2".into() },
            _ => return None,
        })
    }
}

// --- Conversation chunker ---

#[derive(Debug)]
pub struct Chunk {
    pub index: usize,
    pub text: String,
}

pub fn split_chunks(conversation: &str) -> Vec<Chunk> {
    let lines: Vec<&str> = conversation.lines().collect();
    if lines.is_empty() {
        return Vec::new();
    }

    let mut tagged: Vec<(Option<&str>, &str)> = Vec::new();
    let mut current_actor: Option<&str> = None;

    for line in &lines {
        let trimmed = line.trim();
        if trimmed.is_empty() { continue; }
        if let Some(actor) = detect_actor(trimmed) {
            current_actor = Some(actor);
        }
        tagged.push((current_actor, trimmed));
    }

    if tagged.is_empty() {
        return vec![Chunk { index: 0, text: conversation.to_string() }];
    }

    let mut chunks: Vec<Chunk> = Vec::new();
    let mut chunk_lines: Vec<&str> = Vec::new();
    let mut prev_actor: Option<&str> = None;
    let mut actor_switches = 0;

    for (actor, line) in &tagged {
        if *actor != prev_actor && prev_actor.is_some() {
            actor_switches += 1;
            if actor_switches >= 2 {
                chunks.push(Chunk { index: chunks.len(), text: chunk_lines.join("\n") });
                chunk_lines.clear();
                actor_switches = 0;
            }
        }
        chunk_lines.push(line);
        prev_actor = *actor;
    }
    if !chunk_lines.is_empty() {
        chunks.push(Chunk { index: chunks.len(), text: chunk_lines.join("\n") });
    }
    chunks
}

fn detect_actor(line: &str) -> Option<&'static str> {
    let lower = line.to_lowercase();
    if lower.starts_with("user:") || lower.starts_with("human:") || lower.starts_with("customer:") {
        Some("user")
    } else if lower.starts_with("assistant:") || lower.starts_with("agent:") || lower.starts_with("bot:") || lower.starts_with("ai:") {
        Some("agent")
    } else if lower.contains("] user:") || lower.contains("] human:") {
        Some("user")
    } else if lower.contains("] assistant:") || lower.contains("] agent:") {
        Some("agent")
    } else if lower.starts_with("speaker_a:") || lower.starts_with("speaker_1:") {
        Some("user")
    } else if lower.starts_with("speaker_b:") || lower.starts_with("speaker_2:") {
        Some("agent")
    } else {
        None
    }
}

// --- OpenAI-compatible tool-calling types ---

#[derive(Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<Message>,
    temperature: f64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<Tool>,
}

#[derive(Serialize, Clone)]
struct Message {
    role: String,
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<ToolCallOut>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
}

#[derive(Serialize, Clone)]
struct Tool {
    #[serde(rename = "type")]
    type_: String,
    function: ToolFunction,
}

#[derive(Serialize, Clone)]
struct ToolFunction {
    name: String,
    description: String,
    parameters: Value,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: ResponseMessage,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct ResponseMessage {
    content: Option<String>,
    tool_calls: Option<Vec<ToolCall>>,
}

#[derive(Deserialize, Clone)]
struct ToolCall {
    id: String,
    function: ToolCallFunction,
}

#[derive(Deserialize, Serialize, Clone)]
struct ToolCallFunction {
    name: String,
    arguments: String,
}

#[derive(Serialize, Clone)]
struct ToolCallOut {
    id: String,
    #[serde(rename = "type")]
    type_: String,
    function: ToolCallFunction,
}

// --- JSON arg helpers ---

fn opt_str(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(|v| v.as_str()).map(String::from)
}

fn opt_u8(args: &Value, key: &str, default: u8) -> u8 {
    args.get(key).and_then(|v| v.as_u64()).unwrap_or(default as u64) as u8
}

// --- Tool actions returned to caller ---

#[derive(Debug)]
pub enum MemoryAction {
    Recall { query: String },
    Event { event: String, date: Option<String>, time: Option<String>, note: Option<String>, tags: Option<String>, importance: u8, emotion: Option<String>, location: Option<String>, people: Option<String>, source: Option<String> },
    Thing { thing: String, desc: Option<String>, category: Option<String>, tags: Option<String>, importance: u8, emotion: Option<String>, source: Option<String>, confidence: u8, related: Option<String> },
    Person { name: String, role: Option<String>, relationship: Option<String>, note: Option<String>, tags: Option<String>, importance: u8, emotion: Option<String> },
    Place { name: String, desc: Option<String>, address: Option<String>, kind: Option<String>, note: Option<String>, tags: Option<String>, importance: u8, emotion: Option<String> },
    Alter { id: String, fields: Vec<(String, String)> },
    Forget { id: String },
}

// --- Tool definitions ---

fn memory_tools() -> Vec<Tool> {
    vec![
        Tool {
            type_: "function".into(),
            function: ToolFunction {
                name: "recall".into(),
                description: "Search existing memories before storing. Use this FIRST to check if a fact is already stored.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "Search query to find existing memories" }
                    },
                    "required": ["query"]
                }),
            },
        },
        Tool {
            type_: "function".into(),
            function: ToolFunction {
                name: "event".into(),
                description: "Store a new event (something that happened or will happen).".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "event": { "type": "string", "description": "What happened (max 200 chars)" },
                        "date": { "type": "string", "description": "YYYY-MM-DD if known" },
                        "time": { "type": "string", "description": "HH:MM if known" },
                        "note": { "type": "string", "description": "Additional detail (max 200 chars)" },
                        "tags": { "type": "string", "description": "Comma-separated keywords" },
                        "importance": { "type": "integer", "minimum": 0, "maximum": 10, "description": "0=trivial, 5=normal, 10=critical" },
                        "emotion": { "type": "string", "description": "Emotional valence: positive, negative, neutral, or free text" },
                        "location": { "type": "string", "description": "Where it happened" },
                        "people": { "type": "string", "description": "Who was involved, comma-separated" },
                        "source": { "type": "string", "description": "How you know: conversation, observed, told" }
                    },
                    "required": ["event"]
                }),
            },
        },
        Tool {
            type_: "function".into(),
            function: ToolFunction {
                name: "thing".into(),
                description: "Store a fact, preference, or piece of knowledge.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "thing": { "type": "string", "description": "The fact or knowledge (max 200 chars)" },
                        "desc": { "type": "string", "description": "Description (max 200 chars)" },
                        "category": { "type": "string", "description": "Category: preference, fact, skill, hobby, etc." },
                        "tags": { "type": "string", "description": "Comma-separated keywords" },
                        "importance": { "type": "integer", "minimum": 0, "maximum": 10 },
                        "emotion": { "type": "string" },
                        "source": { "type": "string" },
                        "confidence": { "type": "integer", "minimum": 0, "maximum": 10, "description": "0=guess, 10=absolute fact" },
                        "related": { "type": "string", "description": "Comma-separated related concepts" }
                    },
                    "required": ["thing"]
                }),
            },
        },
        Tool {
            type_: "function".into(),
            function: ToolFunction {
                name: "person".into(),
                description: "Store information about a person mentioned in conversation.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Person's name" },
                        "role": { "type": "string", "description": "Role or title" },
                        "relationship": { "type": "string", "description": "Relationship: friend, colleague, family, pet, etc." },
                        "note": { "type": "string", "description": "Key detail about this person" },
                        "tags": { "type": "string" },
                        "importance": { "type": "integer", "minimum": 0, "maximum": 10 },
                        "emotion": { "type": "string" }
                    },
                    "required": ["name"]
                }),
            },
        },
        Tool {
            type_: "function".into(),
            function: ToolFunction {
                name: "place".into(),
                description: "Store information about a location or place.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Place name" },
                        "desc": { "type": "string" },
                        "address": { "type": "string" },
                        "kind": { "type": "string", "description": "Type: city, building, room, outdoor, virtual" },
                        "note": { "type": "string" },
                        "tags": { "type": "string" },
                        "importance": { "type": "integer", "minimum": 0, "maximum": 10 },
                        "emotion": { "type": "string" }
                    },
                    "required": ["name"]
                }),
            },
        },
        Tool {
            type_: "function".into(),
            function: ToolFunction {
                name: "alter".into(),
                description: "Update an existing memory by its ID (e.g. E1, U2). Use after recall finds outdated info.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "id": { "type": "string", "description": "Memory ID from recall results (e.g. E1, U2)" },
                        "event": { "type": "string" },
                        "note": { "type": "string" },
                        "tags": { "type": "string" },
                        "importance": { "type": "integer" },
                        "emotion": { "type": "string" },
                        "location": { "type": "string" },
                        "people": { "type": "string" }
                    },
                    "required": ["id"]
                }),
            },
        },
        Tool {
            type_: "function".into(),
            function: ToolFunction {
                name: "forget".into(),
                description: "Delete a memory that is wrong or no longer relevant.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "id": { "type": "string", "description": "Memory ID to delete (e.g. E1, U2)" }
                    },
                    "required": ["id"]
                }),
            },
        },
    ]
}

// --- System prompt ---

const SYSTEM_PROMPT: &str = r#"You are a memory management agent. Given a conversation chunk, you must decide what facts are worth remembering.

Your workflow for EACH fact you identify:
1. FIRST call recall() to check if it already exists in memory
2. If it exists and info changed → call alter() to update it
3. If it exists and info is wrong → call forget() then store new
4. If it does NOT exist → call event(), thing(), person(), or place() to store it

Guidelines:
- Only store facts explicitly stated or strongly implied by the user
- Skip greetings, filler, and small talk unless they reveal a fact
- importance: 1-3 trivial, 4-6 normal, 7-8 significant, 9-10 critical
- confidence (things only): 0=guess, 10=absolute fact
- Keep fields under 200 chars
- Tags are comma-separated keywords
- Dates in YYYY-MM-DD format
- source: "conversation" for all facts from this conversation

Use the right memory type:
- event: something that happened or will happen (has temporal aspect)
- thing: a fact, preference, knowledge, or attribute
- person: information about a specific person (name, role, relationship)
- place: information about a location
"#;

// --- Agent loop ---

/// Process a conversation chunk through the LLM agent loop.
/// The LLM calls tools (recall, event, thing, etc.) and we execute them.
/// Returns the list of actions taken.
async fn process_chunk(
    config: &LlmConfig,
    client: &reqwest::Client,
    tools: &[Tool],
    chunk: &str,
    recall_fn: &dyn Fn(&str) -> String,
) -> Result<Vec<MemoryAction>, String> {
    let url = format!("{}/v1/chat/completions", config.base_url);

    let mut messages = vec![
        Message { role: "system".into(), content: Some(SYSTEM_PROMPT.into()), tool_calls: None, tool_call_id: None },
        Message { role: "user".into(), content: Some(format!("Process this conversation chunk and memorize the key facts:\n\n{}", chunk)), tool_calls: None, tool_call_id: None },
    ];

    let mut actions: Vec<MemoryAction> = Vec::new();
    let max_rounds = 10;

    for _ in 0..max_rounds {
        let request = ChatRequest {
            model: config.model.clone(),
            messages: messages.clone(),
            temperature: 0.1,
            tools: tools.to_vec(),
        };

        let mut req = client.post(&url).json(&request);
        if !config.api_key.is_empty() {
            req = req.header("Authorization", format!("Bearer {}", config.api_key));
        }

        let response = req.send().await.map_err(|e| format!("request failed: {}", e))?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(format!("API error {}: {}", status, body));
        }

        let chat_resp: ChatResponse = response.json().await.map_err(|e| format!("parse: {}", e))?;
        let choice = chat_resp.choices.first().ok_or("no choices")?;

        // If no tool calls, LLM is done
        let tool_calls = match &choice.message.tool_calls {
            Some(tc) if !tc.is_empty() => tc.clone(),
            _ => break,
        };

        // Add assistant message with tool calls to history
        messages.push(Message {
            role: "assistant".into(),
            content: choice.message.content.clone(),
            tool_calls: Some(tool_calls.iter().map(|tc| ToolCallOut {
                id: tc.id.clone(),
                type_: "function".into(),
                function: tc.function.clone(),
            }).collect()),
            tool_call_id: None,
        });

        // Execute each tool call
        for tc in &tool_calls {
            let args: Value = serde_json::from_str(&tc.function.arguments).unwrap_or_default();
            let result = match tc.function.name.as_str() {
                "recall" => {
                    let query = args["query"].as_str().unwrap_or("");
                    let result = recall_fn(query);
                    actions.push(MemoryAction::Recall { query: query.into() });
                    result
                }
                "event" => {
                    actions.push(MemoryAction::Event {
                        event: args["event"].as_str().unwrap_or("").into(),
                        date: opt_str(&args, "date"), time: opt_str(&args, "time"),
                        note: opt_str(&args, "note"), tags: opt_str(&args, "tags"),
                        importance: opt_u8(&args, "importance", 5),
                        emotion: opt_str(&args, "emotion"), location: opt_str(&args, "location"),
                        people: opt_str(&args, "people"), source: opt_str(&args, "source"),
                    });
                    "stored".into()
                }
                "thing" => {
                    actions.push(MemoryAction::Thing {
                        thing: args["thing"].as_str().unwrap_or("").into(),
                        desc: opt_str(&args, "desc"), category: opt_str(&args, "category"),
                        tags: opt_str(&args, "tags"), importance: opt_u8(&args, "importance", 5),
                        emotion: opt_str(&args, "emotion"), source: opt_str(&args, "source"),
                        confidence: opt_u8(&args, "confidence", 7), related: opt_str(&args, "related"),
                    });
                    "stored".into()
                }
                "person" => {
                    actions.push(MemoryAction::Person {
                        name: args["name"].as_str().unwrap_or("").into(),
                        role: opt_str(&args, "role"), relationship: opt_str(&args, "relationship"),
                        note: opt_str(&args, "note"), tags: opt_str(&args, "tags"),
                        importance: opt_u8(&args, "importance", 5), emotion: opt_str(&args, "emotion"),
                    });
                    "stored".into()
                }
                "place" => {
                    actions.push(MemoryAction::Place {
                        name: args["name"].as_str().unwrap_or("").into(),
                        desc: opt_str(&args, "desc"), address: opt_str(&args, "address"),
                        kind: opt_str(&args, "kind"), note: opt_str(&args, "note"),
                        tags: opt_str(&args, "tags"), importance: opt_u8(&args, "importance", 5),
                        emotion: opt_str(&args, "emotion"),
                    });
                    "stored".into()
                }
                "alter" => {
                    let id = args["id"].as_str().unwrap_or("").to_string();
                    let mut fields = Vec::new();
                    for key in &["event", "note", "tags", "importance", "emotion", "location", "people"] {
                        if let Some(v) = args.get(key).and_then(|v| v.as_str()) {
                            fields.push((key.to_string(), v.to_string()));
                        } else if let Some(v) = args.get(key).and_then(|v| v.as_u64()) {
                            fields.push((key.to_string(), v.to_string()));
                        }
                    }
                    actions.push(MemoryAction::Alter { id, fields });
                    "updated".into()
                }
                "forget" => {
                    let id = args["id"].as_str().unwrap_or("").to_string();
                    actions.push(MemoryAction::Forget { id });
                    "forgotten".into()
                }
                _ => "unknown tool".into(),
            };

            // Add tool result to conversation
            messages.push(Message {
                role: "tool".into(),
                content: Some(result),
                tool_calls: None,
                tool_call_id: Some(tc.id.clone()),
            });
        }

        // If finish_reason is "stop", done
        if choice.finish_reason.as_deref() == Some("stop") {
            break;
        }
    }

    Ok(actions)
}

/// Ingest a full conversation: split into chunks, run agent loop per chunk.
pub async fn ingest_conversation(
    config: &LlmConfig,
    conversation: &str,
    recall_fn: &dyn Fn(&str) -> String,
) -> Result<Vec<MemoryAction>, String> {
    if config.api_key.is_empty() && !config.base_url.contains("localhost") {
        return Err("no API key set. Use MEMORY39_LLM_KEY or provider-specific env var".into());
    }

    let chunks = split_chunks(conversation);
    if chunks.is_empty() {
        return Ok(Vec::new());
    }

    let client = reqwest::Client::new();
    let tools = memory_tools();

    let total = chunks.len();
    if total == 1 {
        eprintln!("  Processing 1 chunk...");
    } else {
        eprintln!("  Split into {} chunks", total);
    }

    let mut all_actions = Vec::new();
    for chunk in &chunks {
        eprint!("  Chunk {}/{}...", chunk.index + 1, total);
        match process_chunk(config, &client, &tools, &chunk.text, recall_fn).await {
            Ok(actions) => {
                let stores = actions.iter().filter(|a| !matches!(a, MemoryAction::Recall { .. })).count();
                eprintln!(" {} actions", stores);
                all_actions.extend(actions);
            }
            Err(e) => eprintln!(" error: {}", e),
        }
    }

    Ok(all_actions)
}
