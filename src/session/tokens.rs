use std::collections::HashMap;
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
        Tool::OpenCode | Tool::Cursor | Tool::Aider | Tool::Shell | Tool::Custom(_) => {
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
    try_claude_project_jsonl(session, cache).unwrap_or_default()
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
fn try_claude_project_jsonl(session: &Session, cache: &mut TokenCache) -> Option<TokenData> {
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
    parse_claude_jsonl(&path, cache)
}

/// Max bytes to read from the tail of a JSONL file.
/// 64 KB is plenty — the last assistant message is typically ~200-500 bytes.
const TAIL_BYTES: u64 = 64 * 1024;

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
    let mut last_output: u64 = 0;
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

        if v.get("type").and_then(|t| t.as_str()) != Some("assistant") {
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
            last_output = usage.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
        }

        if let Some(m) = msg.get("model").and_then(|m| m.as_str()) {
            model = m.to_string();
        }
    }

    if !found {
        return None;
    }

    let data = TokenData {
        context_used: Some(last_input + last_output),
        model: if model.is_empty() { None } else { Some(model) },
        cost_usd: None,
    };

    cache.insert(path.to_path_buf(), (mtime, data.clone()));
    Some(data)
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
        let content = r#"{"type":"human","message":{"role":"user"}}
{"type":"assistant","message":{"model":"claude-opus-4-6","usage":{"input_tokens":100,"cache_creation_input_tokens":500,"cache_read_input_tokens":200,"output_tokens":50}}}
{"type":"assistant","message":{"model":"claude-opus-4-6","usage":{"input_tokens":80,"cache_creation_input_tokens":0,"cache_read_input_tokens":700,"output_tokens":30}}}
"#;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        fs::write(&path, content).unwrap();

        let mut cache = TokenCache::new();
        let data = parse_claude_jsonl(&path, &mut cache).unwrap();
        // Uses LAST assistant message only: input=80, cache_create=0, cache_read=700, output=30
        assert_eq!(data.context_used, Some(810));
        assert_eq!(data.model.as_deref(), Some("claude-opus-4-6"));
        assert_eq!(data.cost_usd, None);
    }

    #[test]
    fn parse_claude_jsonl_no_assistant_messages() {
        let content = r#"{"type":"human","message":{"role":"user"}}
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
        // Last message only: input=20, output=10
        assert_eq!(data.context_used, Some(30));
    }

    #[test]
    fn extract_unsupported_tools_return_defaults() {
        for tool in [
            Tool::OpenCode,
            Tool::Cursor,
            Tool::Aider,
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
        assert_eq!(data1.context_used, Some(150));
        assert_eq!(cache.len(), 1);

        // Second call with same mtime should return cached data.
        let data2 = parse_claude_jsonl(&path, &mut cache).unwrap();
        assert_eq!(data2.context_used, Some(150));
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
}
