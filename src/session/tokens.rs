use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::instance::{Session, Tool};

// ---------------------------------------------------------------------------
// TokenData
// ---------------------------------------------------------------------------

/// Token/context data extracted from an AI tool's session files.
#[derive(Debug, Default, Clone)]
pub struct TokenData {
    pub context_used: Option<u64>,
    pub model: Option<String>,
    pub cost_usd: Option<f64>,
}

// ---------------------------------------------------------------------------
// Token cache — avoids re-parsing unchanged files
// ---------------------------------------------------------------------------

/// Maps file path → (mtime, parsed data). Passed through from the App.
pub type TokenCache = HashMap<PathBuf, (std::time::SystemTime, TokenData)>;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Try to extract token data for a session based on its tool type and project path.
pub fn extract_token_data(session: &Session, cache: &mut TokenCache) -> TokenData {
    match &session.tool {
        Tool::Claude => extract_claude(session, cache),
        Tool::Gemini => extract_gemini(session),
        Tool::Codex => extract_codex(session),
        Tool::OpenCode | Tool::Cursor | Tool::Aider | Tool::Vibe | Tool::Shell | Tool::Custom(_) => {
            TokenData::default()
        }
    }
}

/// Refresh token data on all sessions in-place (best-effort, errors are silently ignored).
pub fn refresh_all(sessions: &mut [Session], cache: &mut TokenCache) {
    for session in sessions.iter_mut() {
        let data = extract_token_data(session, cache);
        if data.context_used.is_some() {
            session.context_used = data.context_used;
        }
        if data.model.is_some() {
            session.model = data.model;
        }
        if data.cost_usd.is_some() {
            session.cost_usd = data.cost_usd;
        }
    }
}

// ---------------------------------------------------------------------------
// Claude Code
// ---------------------------------------------------------------------------

/// Extract token data from Claude Code session files.
///
/// Claude stores session JSONL files at:
/// `~/.claude/projects/<path-slug>/<session-uuid>.jsonl`
///
/// The path-slug is the project path with `/` replaced by `-` and leading `-`.
/// e.g. `/Users/naman/Documents/work-brain` → `-Users-naman-Documents-work-brain`
///
/// Each assistant message line contains `message.usage` with token counts
/// and `message.model` with the model name.
fn extract_claude(session: &Session, cache: &mut TokenCache) -> TokenData {
    let path = match find_claude_jsonl(session) {
        Some(p) => p,
        None => return TokenData::default(),
    };
    parse_claude_jsonl(&path, cache).unwrap_or_default()
}

/// Convert a project path to Claude's directory slug format.
/// `/Users/naman/Documents/work-brain` → `-Users-naman-Documents-work-brain`
fn path_to_claude_slug(path: &Path) -> String {
    let s = path.to_string_lossy();
    s.replace('/', "-")
}

/// Find the JSONL file that belongs to this agentick session's Claude instance.
///
/// Multiple Claude sessions can share the same project directory. We use file
/// **creation time** (birthtime) to match: the JSONL created closest after the
/// agentick session's `created_at` is the one spawned by this session.
///
/// Falls back to most-recently-created file if birthtime isn't available.
pub fn find_claude_jsonl(session: &Session) -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    let slug = path_to_claude_slug(&session.project_path);
    let project_dir = home.join(".claude").join("projects").join(&slug);

    if !project_dir.is_dir() {
        return None;
    }

    // Convert session created_at to SystemTime for comparison.
    let created_ts = session.created_at.timestamp();
    let session_created = std::time::UNIX_EPOCH
        + std::time::Duration::from_secs(created_ts.max(0) as u64);

    // Find the JSONL file whose creation time (birthtime) is closest to but
    // after the agentick session's created_at. This correctly isolates "the
    // Claude session that was spawned by this agentick session" even when
    // other Claude sessions are actively writing to the same project dir.
    let entries = fs::read_dir(&project_dir).ok()?;
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;

    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.ends_with(".jsonl") {
            if let Ok(meta) = entry.metadata() {
                // Prefer birthtime (created()), fall back to mtime.
                let ctime = meta.created()
                    .unwrap_or_else(|_| meta.modified().unwrap_or(std::time::UNIX_EPOCH));

                // Skip files created before this agentick session.
                if ctime < session_created {
                    continue;
                }

                // Pick the file created closest after session_created
                // (i.e., the earliest qualifying birthtime).
                if best.as_ref().map_or(true, |(t, _)| ctime < *t) {
                    best = Some((ctime, entry.path()));
                }
            }
        }
    }

    let (_, path) = best?;
    Some(path)
}

/// Max bytes to read from the tail of a JSONL file.
/// 256 KB to reliably capture the last assistant message even with extended thinking blocks.
const TAIL_BYTES: u64 = 256 * 1024;

/// Read at most the last `max_bytes` of a file.
///
/// If the file is smaller than `max_bytes`, reads the entire file.
/// The first line may be partial (we seeked mid-line) — callers must handle this.
fn read_tail(path: &Path, max_bytes: u64) -> Option<String> {
    let mut file = fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    if len > max_bytes {
        file.seek(SeekFrom::Start(len - max_bytes)).ok()?;
    }
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// Parse a Claude Code JSONL file's token usage from the last assistant message.
///
/// Uses two optimizations to avoid blocking the UI:
/// 1. **Tail-read**: Only reads the last 64 KB (not the full 87 MB file).
/// 2. **mtime cache**: Skips parsing entirely if the file hasn't changed.
///
/// Each `"type":"assistant"` line has:
/// ```json
/// {
///   "type": "assistant",
///   "message": {
///     "model": "claude-opus-4-6",
///     "usage": {
///       "input_tokens": 3,
///       "cache_creation_input_tokens": 13355,
///       "cache_read_input_tokens": 10426,
///       "output_tokens": 11
///     }
///   }
/// }
/// ```
fn parse_claude_jsonl(path: &Path, cache: &mut TokenCache) -> Option<TokenData> {
    let mtime = fs::metadata(path).ok()?.modified().ok()?;

    // Return cached data if file hasn't changed since last parse.
    if let Some((cached_mtime, cached_data)) = cache.get(path) {
        if *cached_mtime == mtime {
            return Some(cached_data.clone());
        }
    }

    // Read only the tail — we only need the LAST assistant message.
    let content = read_tail(path, TAIL_BYTES)?;

    // We want the LAST assistant message's usage — that reflects the current
    // context window usage (each turn re-sends the full conversation).
    let mut last_input: u64 = 0;
    let mut model = String::new();
    let mut found = false;

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // Skip partial first line from tail-seek (won't start with '{').
        if !line.starts_with('{') {
            continue;
        }
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let msg_type = v.get("type").and_then(|t| t.as_str());

        if msg_type != Some("assistant") {
            continue;
        }

        let msg = match v.get("message") {
            Some(m) => m,
            None => continue,
        };

        if let Some(usage) = msg.get("usage") {
            found = true;
            // Overwrite each time — we only care about the last turn's usage.
            last_input = 0;
            last_input += usage.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
            last_input += usage.get("cache_creation_input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
            last_input += usage.get("cache_read_input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
        }

        if let Some(m) = msg.get("model").and_then(|m| m.as_str()) {
            model = m.to_string();
        }
    }

    if !found {
        return None;
    }

    let data = TokenData {
        context_used: Some(last_input),
        model: if model.is_empty() { None } else { Some(model) },
        cost_usd: None,
    };

    cache.insert(path.to_path_buf(), (mtime, data.clone()));
    Some(data)
}

// ---------------------------------------------------------------------------
// LLM-powered session title generation
// ---------------------------------------------------------------------------

/// Extract the first user message from a Claude JSONL file as a placeholder title.
///
/// Reads from the start of the file (not tail) since the first message is at the top.
/// Returns the first 5 words, with ellipsis if truncated.
pub fn extract_first_user_message(jsonl_path: &Path) -> Option<String> {
    // Read the first 16 KB — the first user message is near the top.
    let mut file = fs::File::open(jsonl_path).ok()?;
    let mut buf = vec![0u8; 16 * 1024];
    let n = std::io::Read::read(&mut file, &mut buf).ok()?;
    buf.truncate(n);
    let content = String::from_utf8_lossy(&buf);

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || !line.starts_with('{') {
            continue;
        }
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        if v.get("type").and_then(|t| t.as_str()) != Some("user") {
            continue;
        }

        let text = v.get("message").and_then(|msg| {
            msg.get("content").and_then(|c| {
                c.as_str().map(|s| s.to_string()).or_else(|| {
                    c.as_array()
                        .and_then(|arr| arr.first())
                        .and_then(|item| item.get("text"))
                        .and_then(|t| t.as_str())
                        .map(|s| s.to_string())
                })
            })
        });

        if let Some(t) = text {
            let trimmed = t.trim();
            if trimmed.len() < 5 {
                continue; // skip trivial messages like "hi", "ok"
            }
            let words: Vec<&str> = trimmed.split_whitespace().collect();
            return if words.len() <= 5 {
                Some(words.join(" "))
            } else {
                Some(format!("{}...", words[..5].join(" ")))
            };
        }
    }

    None
}

/// Find the Claude JSONL file for a session and collect the last few
/// conversation turns as a condensed context string for LLM summarization.
pub fn collect_conversation_context(jsonl_path: &Path) -> Option<String> {
    let content = read_tail(jsonl_path, TAIL_BYTES)?;

    let mut user_messages: Vec<String> = Vec::new();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || !line.starts_with('{') {
            continue;
        }
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        if v.get("type").and_then(|t| t.as_str()) != Some("user") {
            continue;
        }

        if let Some(msg) = v.get("message") {
            // content can be a plain string or an array of {type, text} objects.
            let text = msg.get("content").and_then(|c| {
                c.as_str().map(|s| s.to_string()).or_else(|| {
                    c.as_array()
                        .and_then(|arr| arr.first())
                        .and_then(|item| item.get("text"))
                        .and_then(|t| t.as_str())
                        .map(|s| s.to_string())
                })
            });
            if let Some(t) = text {
                let truncated: String = t.chars().take(200).collect();
                user_messages.push(truncated);
            }
        }
    }

    // Keep only the last ~5 user messages.
    let recent: Vec<&String> = user_messages.iter().rev().take(5).collect::<Vec<_>>().into_iter().rev().collect();

    if recent.is_empty() {
        return None;
    }

    let context = recent
        .iter()
        .map(|m| format!("User: {}", m))
        .collect::<Vec<_>>()
        .join("\n");

    Some(context)
}

/// Shell out to `claude -p --model sonnet` to generate a 3-4 word summary title.
///
/// Returns `None` on error, timeout, or empty output.
pub fn generate_llm_summary(context: &str) -> Option<String> {
    use std::process::Command;

    let prompt = format!(
        "Summarize this coding conversation in exactly 3-4 words as a short title. \
         Lowercase, no punctuation, no quotes. Examples: 'fix auth token bug', \
         'add dark mode', 'refactor api layer'.\n\nConversation:\n{}",
        context
    );

    let output = Command::new("claude")
        .args(["-p", "--model", "sonnet", &prompt])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let summary = String::from_utf8_lossy(&output.stdout).trim().to_string();

    if summary.is_empty() {
        return None;
    }

    // Safety net: truncate to 50 chars.
    let truncated: String = summary.chars().take(50).collect();
    Some(truncated)
}

// ---------------------------------------------------------------------------
// Multi-tool title dispatch
// ---------------------------------------------------------------------------

/// Whether the given tool supports auto-generated session titles.
pub fn supports_auto_title(tool: &Tool) -> bool {
    matches!(tool, Tool::Claude | Tool::Gemini | Tool::Codex | Tool::Cursor | Tool::Vibe)
}

/// Extract the first user message for any supported tool as a placeholder title.
/// Returns first 5 words with ellipsis if truncated.
pub fn extract_first_user_message_for_tool(session: &Session) -> Option<String> {
    match &session.tool {
        Tool::Claude => {
            let path = find_claude_jsonl(session)?;
            extract_first_user_message(&path)
        }
        Tool::Gemini => {
            let path = find_gemini_logs_json(session)?;
            extract_first_user_message_gemini(&path)
        }
        Tool::Codex => extract_first_user_message_codex(session),
        Tool::Cursor => extract_cursor_chat_name(session),
        Tool::Vibe => {
            let path = find_vibe_messages_jsonl(session)?;
            extract_first_user_message_vibe(&path)
        }
        _ => None,
    }
}

/// Collect recent conversation context for LLM summary generation.
///
/// Returns `None` for Cursor — its SQLite meta already provides a good title,
/// so no LLM summarization is needed.
pub fn collect_context_for_tool(session: &Session) -> Option<String> {
    match &session.tool {
        Tool::Claude => {
            let path = find_claude_jsonl(session)?;
            collect_conversation_context(&path)
        }
        Tool::Gemini => {
            let path = find_gemini_logs_json(session)?;
            collect_conversation_context_gemini(&path)
        }
        Tool::Codex => collect_conversation_context_codex(session),
        Tool::Cursor => None, // title from SQLite meta is already good
        Tool::Vibe => {
            let path = find_vibe_messages_jsonl(session)?;
            collect_conversation_context_vibe(&path)
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Gemini CLI — title extraction
// ---------------------------------------------------------------------------

/// Find the Gemini logs.json file for this session.
///
/// Gemini stores conversation history at `~/.gemini/tmp/{project_name}/logs.json`.
/// The project name is resolved via `~/.gemini/projects.json` which maps absolute
/// paths to project names. Falls back to scanning all tmp subdirs.
///
/// `logs.json` is a root-level JSON array of message objects:
/// `[{"sessionId":"uuid","messageId":0,"type":"user","message":"...","timestamp":"..."}]`
fn find_gemini_logs_json(session: &Session) -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    let tmp_dir = home.join(".gemini").join("tmp");
    if !tmp_dir.is_dir() {
        return None;
    }

    // Try to resolve project name via ~/.gemini/projects.json first.
    let project_path_str = session.project_path.to_string_lossy().to_string();
    let mut dirs_to_try: Vec<PathBuf> = Vec::new();

    let projects_file = home.join(".gemini").join("projects.json");
    if let Ok(content) = fs::read_to_string(&projects_file) {
        if let Ok(v) = serde_json::from_str::<Value>(&content) {
            if let Some(projects) = v.get("projects").and_then(|p| p.as_object()) {
                // Look for our project path in the mapping.
                if let Some(name) = projects.get(&project_path_str).and_then(|n| n.as_str()) {
                    dirs_to_try.push(tmp_dir.join(name));
                }
            }
        }
    }

    // Also try the directory basename as project name.
    if let Some(basename) = session.project_path.file_name() {
        let name = basename.to_string_lossy().to_string();
        let p = tmp_dir.join(&name);
        if !dirs_to_try.contains(&p) {
            dirs_to_try.push(p);
        }
    }

    // Fallback: scan all subdirs of ~/.gemini/tmp/
    if let Ok(entries) = fs::read_dir(&tmp_dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() && !dirs_to_try.contains(&p) {
                dirs_to_try.push(p);
            }
        }
    }

    // Find the first dir that has a logs.json with content.
    for dir in dirs_to_try {
        let logs_path = dir.join("logs.json");
        if logs_path.is_file() {
            return Some(logs_path);
        }
    }

    None
}

/// Extract the first user message from a Gemini logs.json file.
///
/// Format: root-level JSON array, entries have `type: "user"` and `message` field.
fn extract_first_user_message_gemini(path: &Path) -> Option<String> {
    let content = fs::read_to_string(path).ok()?;
    let v: Value = serde_json::from_str(&content).ok()?;
    let messages = v.as_array()?;

    for msg in messages {
        if msg.get("type").and_then(|t| t.as_str()) != Some("user") {
            continue;
        }
        let text = match msg.get("message").and_then(|m| m.as_str()) {
            Some(t) => t,
            None => continue,
        };
        let trimmed = text.trim();
        if trimmed.len() < 5 {
            continue;
        }
        let words: Vec<&str> = trimmed.split_whitespace().collect();
        return if words.len() <= 5 {
            Some(words.join(" "))
        } else {
            Some(format!("{}...", words[..5].join(" ")))
        };
    }
    None
}

/// Collect recent conversation context from a Gemini logs.json file.
fn collect_conversation_context_gemini(path: &Path) -> Option<String> {
    let content = fs::read_to_string(path).ok()?;
    let v: Value = serde_json::from_str(&content).ok()?;
    let messages = v.as_array()?;

    let user_messages: Vec<String> = messages
        .iter()
        .filter(|m| m.get("type").and_then(|t| t.as_str()) == Some("user"))
        .filter_map(|m| m.get("message").and_then(|c| c.as_str()).map(|s| s.chars().take(200).collect()))
        .collect();

    let recent: Vec<&String> = user_messages.iter().rev().take(5).collect::<Vec<_>>().into_iter().rev().collect();
    if recent.is_empty() {
        return None;
    }

    Some(recent.iter().map(|m| format!("User: {}", m)).collect::<Vec<_>>().join("\n"))
}

// ---------------------------------------------------------------------------
// Codex CLI — title extraction
// ---------------------------------------------------------------------------

/// Max bytes to read from head of a file for first-message extraction.
const HEAD_BYTES: usize = 16 * 1024;

/// Extract the first user message from Codex's global history.jsonl.
///
/// Codex stores all sessions in `~/.codex/history.jsonl` with lines like:
/// `{"session_id":"uuid","ts":N,"text":"..."}`
///
/// We find the session_id whose first `ts` is closest after `session.created_at`.
fn extract_first_user_message_codex(session: &Session) -> Option<String> {
    let home = dirs::home_dir()?;
    let path = home.join(".codex").join("history.jsonl");
    if !path.is_file() {
        return None;
    }

    let created_ts = session.created_at.timestamp();

    // Read head of file to find session starts.
    let mut file = fs::File::open(&path).ok()?;
    let mut buf = vec![0u8; HEAD_BYTES];
    let n = std::io::Read::read(&mut file, &mut buf).ok()?;
    buf.truncate(n);
    let content = String::from_utf8_lossy(&buf);

    // Group first messages by session_id, find the one closest after created_at.
    let mut best_session_id: Option<String> = None;
    let mut best_ts: i64 = i64::MAX;
    let mut first_messages: HashMap<String, String> = HashMap::new();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || !line.starts_with('{') {
            continue;
        }
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let sid = match v.get("session_id").and_then(|s| s.as_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        let ts = v.get("ts").and_then(|t| t.as_i64()).unwrap_or(0);
        let text = v.get("text").and_then(|t| t.as_str()).unwrap_or("");

        if !first_messages.contains_key(&sid) {
            first_messages.insert(sid.clone(), text.to_string());
            // Is this session_id's first timestamp closest after our created_at?
            if ts >= created_ts && ts < best_ts {
                best_ts = ts;
                best_session_id = Some(sid);
            }
        }
    }

    let sid = best_session_id?;
    let text = first_messages.get(&sid)?;
    let trimmed = text.trim();
    if trimmed.len() < 5 {
        return None;
    }
    let words: Vec<&str> = trimmed.split_whitespace().collect();
    if words.len() <= 5 {
        Some(words.join(" "))
    } else {
        Some(format!("{}...", words[..5].join(" ")))
    }
}

/// Collect recent conversation context from Codex history.jsonl for a session.
fn collect_conversation_context_codex(session: &Session) -> Option<String> {
    let home = dirs::home_dir()?;
    let path = home.join(".codex").join("history.jsonl");
    if !path.is_file() {
        return None;
    }

    let created_ts = session.created_at.timestamp();

    // Read head to find the matching session_id.
    let mut file = fs::File::open(&path).ok()?;
    let mut buf = vec![0u8; HEAD_BYTES];
    let n = std::io::Read::read(&mut file, &mut buf).ok()?;
    buf.truncate(n);
    let head_content = String::from_utf8_lossy(&buf);

    let mut best_session_id: Option<String> = None;
    let mut best_ts: i64 = i64::MAX;
    let mut seen_sids: HashSet<String> = HashSet::new();

    for line in head_content.lines() {
        let line = line.trim();
        if line.is_empty() || !line.starts_with('{') {
            continue;
        }
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let sid = match v.get("session_id").and_then(|s| s.as_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        let ts = v.get("ts").and_then(|t| t.as_i64()).unwrap_or(0);

        if !seen_sids.contains(&sid) {
            seen_sids.insert(sid.clone());
            if ts >= created_ts && ts < best_ts {
                best_ts = ts;
                best_session_id = Some(sid);
            }
        }
    }

    let target_sid = best_session_id?;

    // Read tail for context from this session_id.
    let tail = read_tail(&path, TAIL_BYTES)?;
    let mut user_messages: Vec<String> = Vec::new();

    for line in tail.lines() {
        let line = line.trim();
        if line.is_empty() || !line.starts_with('{') {
            continue;
        }
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let sid = v.get("session_id").and_then(|s| s.as_str()).unwrap_or("");
        if sid != target_sid {
            continue;
        }
        if let Some(text) = v.get("text").and_then(|t| t.as_str()) {
            let truncated: String = text.chars().take(200).collect();
            user_messages.push(truncated);
        }
    }

    let recent: Vec<&String> = user_messages.iter().rev().take(5).collect::<Vec<_>>().into_iter().rev().collect();
    if recent.is_empty() {
        return None;
    }

    Some(recent.iter().map(|m| format!("User: {}", m)).collect::<Vec<_>>().join("\n"))
}

// ---------------------------------------------------------------------------
// Cursor — title extraction (via SQLite chat databases)
// ---------------------------------------------------------------------------

/// Extract the Cursor-generated chat name for this session from SQLite.
///
/// Cursor stores chats at `~/.cursor/chats/{md5(project_path)}/{chat_uuid}/store.db`.
/// Each db has a `meta` table with key `0` holding hex-encoded JSON:
/// `{"name":"Chat Title","createdAt":1769713014539,...}`
///
/// We match the chat whose `createdAt` is closest after `session.created_at`.
fn extract_cursor_chat_name(session: &Session) -> Option<String> {
    let home = dirs::home_dir()?;
    let project_hash = compute_md5_hex(&session.project_path.to_string_lossy());
    let project_dir = home.join(".cursor").join("chats").join(&project_hash);

    if !project_dir.is_dir() {
        return None;
    }

    let created_ms = session.created_at.timestamp_millis();

    let mut best: Option<(i64, String)> = None;

    let entries = fs::read_dir(&project_dir).ok()?;
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let db_path = dir.join("store.db");
        if !db_path.is_file() {
            continue;
        }

        // Shell out to sqlite3 to read the meta table.
        if let Some((name, created_at)) = read_cursor_chat_meta(&db_path) {
            // Pick the chat created closest after the agentick session.
            if created_at >= created_ms {
                if best.as_ref().map_or(true, |(t, _)| created_at < *t) {
                    best = Some((created_at, name));
                }
            }
        }
    }

    best.map(|(_, name)| name)
}

/// Read the `name` and `createdAt` from a Cursor chat store.db's meta table.
///
/// Uses `sqlite3` CLI to avoid a native dependency. The meta value is hex-encoded JSON.
fn read_cursor_chat_meta(db_path: &Path) -> Option<(String, i64)> {
    use std::process::Command;

    let output = Command::new("sqlite3")
        .args([
            db_path.to_str()?,
            "SELECT value FROM meta WHERE key='0';",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let hex_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if hex_str.is_empty() {
        return None;
    }

    let bytes = hex_decode(&hex_str)?;
    let json_str = String::from_utf8(bytes).ok()?;
    let v: Value = serde_json::from_str(&json_str).ok()?;

    let name = v.get("name").and_then(|n| n.as_str())?.to_string();
    let created_at = v.get("createdAt").and_then(|c| c.as_i64())?;

    if name.is_empty() {
        return None;
    }

    Some((name, created_at))
}

/// Compute MD5 hex digest of a string (used for Cursor project dir hashing).
fn compute_md5_hex(input: &str) -> String {
    use std::process::Command;

    // Use macOS `md5 -qs` for zero-dep md5.
    if let Ok(output) = Command::new("md5")
        .args(["-qs", input])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
    {
        if output.status.success() {
            return String::from_utf8_lossy(&output.stdout).trim().to_string();
        }
    }

    // Fallback: try `md5sum` (Linux).
    if let Ok(output) = Command::new("md5sum")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
    {
        if output.status.success() {
            let out = String::from_utf8_lossy(&output.stdout);
            return out.split_whitespace().next().unwrap_or("").to_string();
        }
    }

    String::new()
}

/// Decode a hex string to bytes.
fn hex_decode(hex: &str) -> Option<Vec<u8>> {
    if hex.len() % 2 != 0 {
        return None;
    }
    let mut bytes = Vec::with_capacity(hex.len() / 2);
    for chunk in hex.as_bytes().chunks(2) {
        let hi = hex_val(chunk[0])?;
        let lo = hex_val(chunk[1])?;
        bytes.push((hi << 4) | lo);
    }
    Some(bytes)
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Vibe — title extraction
// ---------------------------------------------------------------------------

/// Find the Vibe messages.jsonl file for this session.
///
/// Vibe stores logs at `~/.vibe/logs/session/session_*/messages.jsonl`.
/// Each session dir also has `meta.json` with:
/// - `environment.working_directory` — for project path matching
/// - `start_time` — ISO 8601 timestamp for time-based matching
///
/// We filter by working_directory, then pick the session whose `start_time`
/// is closest after `session.created_at`.
fn find_vibe_messages_jsonl(session: &Session) -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    let sessions_dir = home.join(".vibe").join("logs").join("session");
    if !sessions_dir.is_dir() {
        return None;
    }

    let created_ts = session.created_at.timestamp();
    let project_str = session.project_path.to_string_lossy().to_string();

    let mut best: Option<(i64, PathBuf)> = None;

    let entries = fs::read_dir(&sessions_dir).ok()?;
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let name = match dir.file_name() {
            Some(n) => n.to_string_lossy().to_string(),
            None => continue,
        };
        if !name.starts_with("session_") {
            continue;
        }

        let messages_path = dir.join("messages.jsonl");
        if !messages_path.is_file() {
            continue;
        }

        // Read meta.json for working directory and start_time.
        let meta_path = dir.join("meta.json");
        let meta_v = if meta_path.is_file() {
            fs::read_to_string(&meta_path)
                .ok()
                .and_then(|c| serde_json::from_str::<Value>(&c).ok())
        } else {
            None
        };

        // Filter by working directory if available.
        if let Some(ref mv) = meta_v {
            let wd = mv
                .get("environment")
                .and_then(|e| e.get("working_directory"))
                .and_then(|w| w.as_str())
                .unwrap_or("");
            if !wd.is_empty() && wd != project_str {
                continue;
            }
        }

        // Use start_time from meta.json for matching (more reliable than birthtime).
        let start_ts: i64 = meta_v
            .as_ref()
            .and_then(|mv| mv.get("start_time").and_then(|s| s.as_str()))
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.timestamp())
            .unwrap_or_else(|| {
                // Fallback to file birthtime.
                messages_path
                    .metadata()
                    .ok()
                    .and_then(|m| m.created().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0)
            });

        // Pick the session started closest after the agentick session was created.
        if start_ts < created_ts {
            continue;
        }
        if best.as_ref().map_or(true, |(t, _)| start_ts < *t) {
            best = Some((start_ts, messages_path));
        }
    }

    best.map(|(_, p)| p)
}

/// Extract the first user message from a Vibe messages.jsonl file.
///
/// Expected line format: `{"role":"user","content":"..."}`
fn extract_first_user_message_vibe(path: &Path) -> Option<String> {
    let mut file = fs::File::open(path).ok()?;
    let mut buf = vec![0u8; HEAD_BYTES];
    let n = std::io::Read::read(&mut file, &mut buf).ok()?;
    buf.truncate(n);
    let content = String::from_utf8_lossy(&buf);

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || !line.starts_with('{') {
            continue;
        }
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        if v.get("role").and_then(|r| r.as_str()) != Some("user") {
            continue;
        }

        let text = v.get("content").and_then(|c| c.as_str())?;
        let trimmed = text.trim();
        if trimmed.len() < 5 {
            continue;
        }
        let words: Vec<&str> = trimmed.split_whitespace().collect();
        return if words.len() <= 5 {
            Some(words.join(" "))
        } else {
            Some(format!("{}...", words[..5].join(" ")))
        };
    }
    None
}

/// Collect recent user messages from a Vibe messages.jsonl file.
fn collect_conversation_context_vibe(path: &Path) -> Option<String> {
    let content = read_tail(path, TAIL_BYTES)?;

    let mut user_messages: Vec<String> = Vec::new();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || !line.starts_with('{') {
            continue;
        }
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        if v.get("role").and_then(|r| r.as_str()) != Some("user") {
            continue;
        }

        if let Some(text) = v.get("content").and_then(|c| c.as_str()) {
            let truncated: String = text.chars().take(200).collect();
            user_messages.push(truncated);
        }
    }

    let recent: Vec<&String> = user_messages.iter().rev().take(5).collect::<Vec<_>>().into_iter().rev().collect();
    if recent.is_empty() {
        return None;
    }

    Some(recent.iter().map(|m| format!("User: {}", m)).collect::<Vec<_>>().join("\n"))
}

// ---------------------------------------------------------------------------
// Gemini CLI
// ---------------------------------------------------------------------------

/// Extract token data from Gemini CLI session files.
///
/// Looks for `session_*.json` in `~/.gemini/tmp/`.
fn extract_gemini(session: &Session) -> TokenData {
    let _ = session; // project_path isn't relevant for gemini's global store
    try_gemini_session().unwrap_or_default()
}

fn try_gemini_session() -> Option<TokenData> {
    let home = dirs::home_dir()?;
    let tmp_dir = home.join(".gemini").join("tmp");

    if !tmp_dir.is_dir() {
        return None;
    }

    let entries = fs::read_dir(&tmp_dir).ok()?;

    // Find the most recently modified session_*.json file.
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;

    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with("session_") && name_str.ends_with(".json") {
            if let Ok(meta) = entry.metadata() {
                let mtime = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
                if best.as_ref().map_or(true, |(t, _)| mtime > *t) {
                    best = Some((mtime, entry.path()));
                }
            }
        }
    }

    let (_, path) = best?;
    let content = fs::read_to_string(&path).ok()?;
    parse_gemini_session_json(&content)
}

/// Parse Gemini session JSON.
///
/// Expected shape:
/// ```json
/// {
///   "model": "gemini-2.5-pro",
///   "usage": { "input_tokens": 1234, "output_tokens": 567 }
/// }
/// ```
fn parse_gemini_session_json(content: &str) -> Option<TokenData> {
    let v: Value = serde_json::from_str(content).ok()?;

    let model = v
        .get("model")
        .and_then(|m| m.as_str())
        .map(|s| s.to_string());

    let context_used = v.get("usage").and_then(|u| {
        let input = u.get("input_tokens").and_then(|t| t.as_u64()).unwrap_or(0);
        let output = u.get("output_tokens").and_then(|t| t.as_u64()).unwrap_or(0);
        let total = input + output;
        if total > 0 {
            Some(total)
        } else {
            None
        }
    });

    if context_used.is_some() || model.is_some() {
        Some(TokenData {
            context_used,
            model,
            cost_usd: None, // Gemini CLI doesn't report cost
        })
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Codex CLI
// ---------------------------------------------------------------------------

/// Extract token data from Codex CLI rollout files.
///
/// Looks for JSONL files in `~/.codex/`.
fn extract_codex(session: &Session) -> TokenData {
    let _ = session;
    try_codex_rollout().unwrap_or_default()
}

fn try_codex_rollout() -> Option<TokenData> {
    let home = dirs::home_dir()?;
    let codex_dir = home.join(".codex");

    if !codex_dir.is_dir() {
        return None;
    }

    let entries = fs::read_dir(&codex_dir).ok()?;

    // Find the most recently modified .jsonl file.
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;

    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.ends_with(".jsonl") {
            if let Ok(meta) = entry.metadata() {
                let mtime = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
                if best.as_ref().map_or(true, |(t, _)| mtime > *t) {
                    best = Some((mtime, entry.path()));
                }
            }
        }
    }

    let (_, path) = best?;
    parse_codex_jsonl(&path)
}

/// Parse a Codex JSONL rollout file.
///
/// Each line may have `{"tokens_used":...,"model":"..."}`.
/// We take the last line with token data.
fn parse_codex_jsonl(path: &Path) -> Option<TokenData> {
    let content = fs::read_to_string(path).ok()?;

    let mut result = TokenData::default();
    let mut found = false;

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let has_data = v.get("tokens_used").is_some() || v.get("model").is_some();
        if has_data {
            found = true;
            if let Some(tokens) = v.get("tokens_used").and_then(|t| t.as_u64()) {
                result.context_used = Some(tokens);
            }
            if let Some(model) = v.get("model").and_then(|m| m.as_str()) {
                result.model = Some(model.to_string());
            }
        }
    }

    if found {
        Some(result)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::instance::{Session, Tool};
    use std::path::PathBuf;

    /// Helper to create a minimal session for testing.
    fn make_session(tool: Tool) -> Session {
        Session {
            id: "test-id-123".into(),
            title: "test".into(),
            project_path: PathBuf::from("/tmp/agentick-test"),
            command: tool.default_command().into(),
            tool,
            status: super::super::instance::Status::Active,
            tmux_name: "agentick_test".into(),
            created_at: chrono::Utc::now(),
            context_used: None,
            context_limit: 200_000,
            model: None,
            cost_usd: None,
            last_activity: None,
            forked_from: None,
            user_renamed: false,
        }
    }

    #[test]
    fn path_to_claude_slug_converts_slashes() {
        let p = PathBuf::from("/Users/naman/Documents/work-brain");
        assert_eq!(path_to_claude_slug(&p), "-Users-naman-Documents-work-brain");
    }

    #[test]
    fn path_to_claude_slug_root() {
        let p = PathBuf::from("/");
        assert_eq!(path_to_claude_slug(&p), "-");
    }

    #[test]
    fn parse_claude_jsonl_valid() {
        let content = r#"{"type":"user","message":{"role":"user"}}
{"type":"assistant","message":{"model":"claude-opus-4-6","usage":{"input_tokens":100,"cache_creation_input_tokens":500,"cache_read_input_tokens":200,"output_tokens":50}}}
{"type":"assistant","message":{"model":"claude-opus-4-6","usage":{"input_tokens":80,"cache_creation_input_tokens":0,"cache_read_input_tokens":700,"output_tokens":30}}}
"#;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        fs::write(&path, content).unwrap();

        let mut cache = TokenCache::new();
        let data = parse_claude_jsonl(&path, &mut cache).unwrap();
        // Uses LAST assistant message only (input tokens): input=80, cache_create=0, cache_read=700
        assert_eq!(data.context_used, Some(780));
        assert_eq!(data.model.as_deref(), Some("claude-opus-4-6"));
        assert_eq!(data.cost_usd, None);
    }

    #[test]
    fn parse_claude_jsonl_no_assistant_messages() {
        let content = r#"{"type":"user","message":{"role":"user"}}
{"type":"system","message":{}}
"#;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        fs::write(&path, content).unwrap();

        let mut cache = TokenCache::new();
        assert!(parse_claude_jsonl(&path, &mut cache).is_none());
    }

    #[test]
    fn parse_gemini_session_valid() {
        let json = r#"{
            "model": "gemini-2.5-pro",
            "usage": { "input_tokens": 1000, "output_tokens": 500 }
        }"#;

        let data = parse_gemini_session_json(json).unwrap();
        assert_eq!(data.context_used, Some(1500));
        assert_eq!(data.model.as_deref(), Some("gemini-2.5-pro"));
        assert_eq!(data.cost_usd, None);
    }

    #[test]
    fn parse_gemini_session_zero_tokens() {
        let json = r#"{
            "model": "gemini-2.5-flash",
            "usage": { "input_tokens": 0, "output_tokens": 0 }
        }"#;

        let data = parse_gemini_session_json(json).unwrap();
        assert_eq!(data.context_used, None);
        assert_eq!(data.model.as_deref(), Some("gemini-2.5-flash"));
    }

    #[test]
    fn parse_gemini_session_empty_returns_none() {
        let json = r#"{ "something": "else" }"#;
        assert!(parse_gemini_session_json(json).is_none());
    }

    #[test]
    fn parse_codex_jsonl_valid() {
        let content = r#"{"event":"start"}
{"tokens_used":1234,"model":"o3"}
{"tokens_used":5678,"model":"o4-mini"}
"#;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout.jsonl");
        fs::write(&path, content).unwrap();

        let data = parse_codex_jsonl(&path).unwrap();
        // Should take the last matching line.
        assert_eq!(data.context_used, Some(5678));
        assert_eq!(data.model.as_deref(), Some("o4-mini"));
    }

    #[test]
    fn parse_codex_jsonl_no_token_data() {
        let content = r#"{"event":"start"}
{"event":"end"}
"#;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout.jsonl");
        fs::write(&path, content).unwrap();

        assert!(parse_codex_jsonl(&path).is_none());
    }

    #[test]
    fn parse_claude_jsonl_extracts_last_model() {
        let content = r#"{"type":"assistant","message":{"model":"claude-sonnet-4-6","usage":{"input_tokens":10,"output_tokens":5}}}
{"type":"assistant","message":{"model":"claude-opus-4-6","usage":{"input_tokens":20,"output_tokens":10}}}
"#;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        fs::write(&path, content).unwrap();

        let mut cache = TokenCache::new();
        let data = parse_claude_jsonl(&path, &mut cache).unwrap();
        assert_eq!(data.model.as_deref(), Some("claude-opus-4-6"));
        // Last message only (input tokens): input=20
        assert_eq!(data.context_used, Some(20));
    }

    #[test]
    fn extract_unsupported_tools_return_defaults() {
        for tool in [
            Tool::OpenCode,
            Tool::Cursor,
            Tool::Aider,
            Tool::Vibe,
            Tool::Shell,
            Tool::Custom("foo".into()),
        ] {
            let session = make_session(tool);
            let mut cache = TokenCache::new();
            let data = extract_token_data(&session, &mut cache);
            assert!(data.context_used.is_none());
            assert!(data.model.is_none());
            assert!(data.cost_usd.is_none());
        }
    }

    #[test]
    fn refresh_all_updates_sessions() {
        // With no real session files on disk, refresh_all should be a no-op
        // but must not panic.
        let mut sessions = vec![
            make_session(Tool::Claude),
            make_session(Tool::Gemini),
            make_session(Tool::Codex),
            make_session(Tool::Shell),
        ];

        // Pre-set some values to verify they are NOT overwritten when
        // extraction returns None.
        sessions[0].model = Some("existing-model".into());
        sessions[0].cost_usd = Some(1.23);

        let mut cache = TokenCache::new();
        refresh_all(&mut sessions, &mut cache);

        // Existing values should be preserved when extraction returns None.
        assert_eq!(sessions[0].model.as_deref(), Some("existing-model"));
        assert!((sessions[0].cost_usd.unwrap() - 1.23).abs() < f64::EPSILON);
    }

    #[test]
    fn parse_claude_jsonl_caches_by_mtime() {
        let content = r#"{"type":"assistant","message":{"model":"claude-opus-4-6","usage":{"input_tokens":100,"output_tokens":50}}}
"#;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        fs::write(&path, content).unwrap();

        let mut cache = TokenCache::new();

        // First call parses the file and populates cache.
        let data1 = parse_claude_jsonl(&path, &mut cache).unwrap();
        assert_eq!(data1.context_used, Some(100));
        assert_eq!(cache.len(), 1);

        // Second call with same mtime should return cached data.
        let data2 = parse_claude_jsonl(&path, &mut cache).unwrap();
        assert_eq!(data2.context_used, Some(100));
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn read_tail_small_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("small.txt");
        fs::write(&path, "hello\nworld\n").unwrap();

        // File is smaller than max_bytes — should read entire file.
        let content = read_tail(&path, 1024).unwrap();
        assert_eq!(content, "hello\nworld\n");
    }

    #[test]
    fn read_tail_large_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("large.jsonl");

        // Write a file larger than our tail size.
        let padding = "x".repeat(200);
        let mut file_content = String::new();
        for _ in 0..100 {
            file_content.push_str(&format!("{}\n", padding));
        }
        file_content.push_str(r#"{"type":"assistant","message":{"model":"test","usage":{"input_tokens":42,"output_tokens":8}}}"#);
        file_content.push('\n');
        fs::write(&path, &file_content).unwrap();

        // Read only tail — should still find the last assistant message.
        let tail = read_tail(&path, 1024).unwrap();
        assert!(tail.contains("\"input_tokens\":42"));
    }

    #[test]
    fn collect_conversation_context_extracts_user_messages() {
        let content = r#"{"type":"user","message":{"role":"user","content":"implement the quick create feature for sessions"}}
{"type":"assistant","message":{"model":"claude-opus-4-6","usage":{"input_tokens":100,"output_tokens":50}}}
{"type":"user","message":{"role":"user","content":"yes"}}
{"type":"assistant","message":{"model":"claude-opus-4-6","usage":{"input_tokens":200,"output_tokens":80}}}
"#;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        fs::write(&path, content).unwrap();

        let context = collect_conversation_context(&path).unwrap();
        assert!(context.contains("implement the quick create feature for sessions"));
        assert!(context.contains("yes"));
    }

    #[test]
    fn supports_auto_title_correct_tools() {
        assert!(supports_auto_title(&Tool::Claude));
        assert!(supports_auto_title(&Tool::Gemini));
        assert!(supports_auto_title(&Tool::Codex));
        assert!(supports_auto_title(&Tool::Cursor));
        assert!(supports_auto_title(&Tool::Vibe));
        assert!(!supports_auto_title(&Tool::OpenCode));
        assert!(!supports_auto_title(&Tool::Aider));
        assert!(!supports_auto_title(&Tool::Shell));
        assert!(!supports_auto_title(&Tool::Custom("foo".into())));
    }

    #[test]
    fn extract_first_user_message_gemini_valid() {
        // Real Gemini logs.json format: root array, "type"/"message" fields.
        let json = r#"[{"sessionId":"abc","messageId":0,"type":"assistant","message":"Hello!"},{"sessionId":"abc","messageId":1,"type":"user","message":"fix the authentication bug in login flow","timestamp":"2025-06-27T09:00:00Z"},{"sessionId":"abc","messageId":2,"type":"user","message":"also add tests","timestamp":"2025-06-27T09:01:00Z"}]"#;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("logs.json");
        fs::write(&path, json).unwrap();

        let msg = extract_first_user_message_gemini(&path).unwrap();
        assert_eq!(msg, "fix the authentication bug in...");
    }

    #[test]
    fn extract_first_user_message_gemini_short() {
        let json = r#"[{"sessionId":"abc","messageId":0,"type":"user","message":"fix the bug","timestamp":"2025-06-27T09:00:00Z"}]"#;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("logs.json");
        fs::write(&path, json).unwrap();

        let msg = extract_first_user_message_gemini(&path).unwrap();
        assert_eq!(msg, "fix the bug");
    }

    #[test]
    fn extract_first_user_message_gemini_no_users() {
        let json = r#"[{"sessionId":"abc","messageId":0,"type":"assistant","message":"Hello!"}]"#;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("logs.json");
        fs::write(&path, json).unwrap();

        assert!(extract_first_user_message_gemini(&path).is_none());
    }

    #[test]
    fn collect_conversation_context_gemini_valid() {
        let json = r#"[{"sessionId":"abc","messageId":0,"type":"user","message":"first message","timestamp":"2025-06-27T09:00:00Z"},{"sessionId":"abc","messageId":1,"type":"assistant","message":"response"},{"sessionId":"abc","messageId":2,"type":"user","message":"second message","timestamp":"2025-06-27T09:01:00Z"}]"#;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("logs.json");
        fs::write(&path, json).unwrap();

        let ctx = collect_conversation_context_gemini(&path).unwrap();
        assert!(ctx.contains("User: first message"));
        assert!(ctx.contains("User: second message"));
    }

    #[test]
    fn extract_first_user_message_vibe_valid() {
        let content = r#"{"role":"assistant","content":"I'm ready to help."}
{"role":"user","content":"implement the dashboard view with charts"}
{"role":"assistant","content":"Sure, I'll implement that."}
"#;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("messages.jsonl");
        fs::write(&path, content).unwrap();

        let msg = extract_first_user_message_vibe(&path).unwrap();
        assert_eq!(msg, "implement the dashboard view with...");
    }

    #[test]
    fn extract_first_user_message_vibe_no_users() {
        let content = r#"{"role":"assistant","content":"Hello!"}
"#;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("messages.jsonl");
        fs::write(&path, content).unwrap();

        assert!(extract_first_user_message_vibe(&path).is_none());
    }

    #[test]
    fn collect_conversation_context_vibe_valid() {
        let content = r#"{"role":"user","content":"add dark mode support"}
{"role":"assistant","content":"I'll add theme support."}
{"role":"user","content":"also update the sidebar"}
"#;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("messages.jsonl");
        fs::write(&path, content).unwrap();

        let ctx = collect_conversation_context_vibe(&path).unwrap();
        assert!(ctx.contains("User: add dark mode support"));
        assert!(ctx.contains("User: also update the sidebar"));
    }

    #[test]
    fn hex_decode_valid() {
        assert_eq!(hex_decode("48656c6c6f"), Some(b"Hello".to_vec()));
        assert_eq!(hex_decode(""), Some(vec![]));
    }

    #[test]
    fn hex_decode_invalid() {
        assert!(hex_decode("4g").is_none()); // invalid hex char
        assert!(hex_decode("123").is_none()); // odd length
    }

    #[test]
    fn compute_md5_hex_known_value() {
        // md5("/Users/naman/Documents/work-brain") = "605df61177d4441b5d46bf808d2e8aab"
        let hash = compute_md5_hex("/Users/naman/Documents/work-brain");
        assert_eq!(hash, "605df61177d4441b5d46bf808d2e8aab");
    }

    // -- extract_first_user_message (Claude) ----------------------------------

    #[test]
    fn extract_first_user_message_valid() {
        let content = r#"{"type":"user","message":{"role":"user","content":"implement the quick create feature for sessions"}}
{"type":"assistant","message":{"model":"claude-opus-4-6","usage":{"input_tokens":100,"output_tokens":50}}}
"#;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        fs::write(&path, content).unwrap();

        let msg = extract_first_user_message(&path).unwrap();
        assert_eq!(msg, "implement the quick create feature...");
    }

    #[test]
    fn extract_first_user_message_short_message() {
        let content = r#"{"type":"user","message":{"role":"user","content":"fix bug"}}
"#;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        fs::write(&path, content).unwrap();

        let msg = extract_first_user_message(&path).unwrap();
        assert_eq!(msg, "fix bug");
    }

    #[test]
    fn extract_first_user_message_skips_trivial() {
        // Messages under 5 chars should be skipped (e.g. "hi", "ok")
        let content = r#"{"type":"user","message":{"role":"user","content":"hi"}}
{"type":"user","message":{"role":"user","content":"please implement the feature"}}
"#;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        fs::write(&path, content).unwrap();

        let msg = extract_first_user_message(&path).unwrap();
        assert_eq!(msg, "please implement the feature");
    }

    #[test]
    fn extract_first_user_message_no_users() {
        let content = r#"{"type":"assistant","message":{"model":"test","usage":{"input_tokens":1,"output_tokens":1}}}
"#;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        fs::write(&path, content).unwrap();

        assert!(extract_first_user_message(&path).is_none());
    }

    #[test]
    fn extract_first_user_message_content_array() {
        // Claude sometimes uses array content format
        let content = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"refactor the authentication module completely"}]}}
"#;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        fs::write(&path, content).unwrap();

        let msg = extract_first_user_message(&path).unwrap();
        assert_eq!(msg, "refactor the authentication module completely");
    }

    // -- parse_claude_jsonl edge cases ----------------------------------------

    #[test]
    fn parse_claude_jsonl_skips_partial_first_line() {
        // Simulate a tail-read that starts mid-line.
        let content = r#"partial json that doesn't start with {
{"type":"assistant","message":{"model":"claude-opus-4-6","usage":{"input_tokens":42,"output_tokens":8}}}
"#;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        fs::write(&path, content).unwrap();

        let mut cache = TokenCache::new();
        let data = parse_claude_jsonl(&path, &mut cache).unwrap();
        assert_eq!(data.context_used, Some(42));
    }

    #[test]
    fn parse_claude_jsonl_malformed_json_line_skipped() {
        let content = r#"{"type":"assistant","message":{"model":"claude-opus-4-6","usage":{"input_tokens":10,"output_tokens":5}}}
{this is not valid json}
{"type":"assistant","message":{"model":"claude-opus-4-6","usage":{"input_tokens":99,"output_tokens":1}}}
"#;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        fs::write(&path, content).unwrap();

        let mut cache = TokenCache::new();
        let data = parse_claude_jsonl(&path, &mut cache).unwrap();
        // Should use the last valid assistant message.
        assert_eq!(data.context_used, Some(99));
    }

    #[test]
    fn parse_claude_jsonl_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.jsonl");
        fs::write(&path, "").unwrap();

        let mut cache = TokenCache::new();
        assert!(parse_claude_jsonl(&path, &mut cache).is_none());
    }

    // -- parse_gemini_session_json edge cases ----------------------------------

    #[test]
    fn parse_gemini_session_missing_model() {
        let json = r#"{ "usage": { "input_tokens": 100, "output_tokens": 50 } }"#;
        // No "model" field — still returns Some because context_used is present.
        let data = parse_gemini_session_json(json).unwrap();
        assert!(data.model.is_none());
        assert_eq!(data.context_used, Some(150));
    }

    // -- TokenData default ----------------------------------------------------

    #[test]
    fn token_data_default_all_none() {
        let td = TokenData::default();
        assert!(td.context_used.is_none());
        assert!(td.model.is_none());
        assert!(td.cost_usd.is_none());
    }

    // -- hex_val edge cases ---------------------------------------------------

    #[test]
    fn hex_decode_uppercase() {
        assert_eq!(hex_decode("4F6B"), Some(b"Ok".to_vec()));
    }

    #[test]
    fn hex_decode_mixed_case() {
        assert_eq!(hex_decode("4f6B"), Some(b"Ok".to_vec()));
    }
}
