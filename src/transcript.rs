use crate::discover::{ListFilter, list_sessions, resolve};
use crate::error::{Error, Result};
use crate::homes::Homes;
use crate::model::{Agent, Digest, FilesTouched, SearchHit, Session, Transcript, Turn};
use serde_json::Value;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

pub fn read_transcript(homes: &Homes, reference: &str, limit: usize) -> Result<Transcript> {
    let session = resolve(homes, reference)?;
    read_session(&session, limit)
}

pub fn session_digest(homes: &Homes, reference: &str, limit: usize) -> Result<Digest> {
    let transcript = read_transcript(homes, reference, if limit == 0 { 12 } else { limit })?;
    let turns = transcript
        .turns
        .into_iter()
        .map(|mut turn| {
            turn.text = clip(&turn.text, 280);
            turn
        })
        .collect();
    Ok(Digest {
        session: transcript.session.clone(),
        last_user_request: transcript.last_user_request,
        last_assistant_action: transcript.last_assistant_action,
        cwd: transcript.session.cwd.clone(),
        branch: transcript.session.branch.clone(),
        turns,
        inert: true,
    })
}

pub fn files_touched(homes: &Homes, reference: &str) -> Result<FilesTouched> {
    let session = resolve(homes, reference)?;
    let mut files = std::collections::BTreeSet::new();
    if let Some(path) = session.transcript_path.as_deref() {
        collect_files(session.agent, path, &session.session_id, &mut files);
    }
    Ok(FilesTouched {
        session,
        files: files.into_iter().collect(),
        inert: true,
    })
}

fn collect_files(
    agent: Agent,
    path: &Path,
    session_id: &str,
    files: &mut std::collections::BTreeSet<String>,
) {
    if agent == Agent::OpenCode && path.extension().and_then(|ext| ext.to_str()) == Some("db") {
        if let Ok(turns) = read_opencode_sqlite(path, session_id) {
            for turn in turns {
                collect_paths_from_text(&turn.text, files);
            }
        }
        return;
    }
    if let Ok(records) = jsonl(path) {
        for value in records {
            collect_paths_from_value(&value, files);
        }
    } else if let Ok(raw) = std::fs::read_to_string(path)
        && let Ok(value) = serde_json::from_str::<Value>(&raw)
    {
        collect_paths_from_value(&value, files);
    }
}

fn collect_paths_from_value(value: &Value, files: &mut std::collections::BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let key = key.to_ascii_lowercase();
                if matches!(
                    key.as_str(),
                    "path" | "file_path" | "file" | "target" | "target_file" | "filename"
                ) && let Some(path) = child.as_str()
                    && looks_like_path(path)
                {
                    files.insert(path.to_string());
                }
                collect_paths_from_value(child, files);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_paths_from_value(item, files);
            }
        }
        Value::String(text) => collect_paths_from_text(text, files),
        _ => {}
    }
}

fn collect_paths_from_text(text: &str, files: &mut std::collections::BTreeSet<String>) {
    for token in text.split_whitespace() {
        let token = token.trim_matches(|ch: char| {
            matches!(ch, ',' | ';' | ')' | '(' | '"' | '\'' | '`' | '[' | ']')
        });
        if looks_like_path(token) && token.contains('/') && token.contains('.') {
            files.insert(token.to_string());
        }
    }
}

fn looks_like_path(value: &str) -> bool {
    let value = value.trim();
    !value.is_empty()
        && value.len() <= 512
        && !value.contains("://")
        && !value.contains('\n')
        && (value.starts_with('/')
            || value.starts_with("./")
            || value.starts_with("../")
            || (value.contains('/') && value.split('/').any(|part| part.contains('.'))))
}

pub fn read_session(session: &Session, limit: usize) -> Result<Transcript> {
    let path = session
        .transcript_path
        .as_deref()
        .ok_or_else(|| Error::msg("no transcript found for that session"))?;
    let turns = match session.agent {
        Agent::Claude => read_claude(path)?,
        Agent::Grok => read_grok(path)?,
        Agent::Codex => read_codex(path)?,
        Agent::Cursor => read_cursor(path)?,
        Agent::OpenCode => read_opencode(session, path)?,
        Agent::Gemini => read_gemini(path)?,
        Agent::Copilot => read_copilot(path)?,
    };
    Ok(compact(session.clone(), turns, limit))
}

pub fn search_transcripts(
    homes: &Homes,
    query: &str,
    agent: Option<Agent>,
    include_archived: bool,
    limit: usize,
) -> Result<Vec<SearchHit>> {
    let query = query.trim();
    if query.is_empty() {
        return Err(Error::msg("query is required"));
    }
    let needle = query.to_ascii_lowercase();
    let sessions = list_sessions(
        homes,
        &ListFilter {
            agent,
            query: None,
            live_only: false,
            include_archived,
            limit: 120,
            ..ListFilter::default()
        },
    )?;
    let mut hits = Vec::new();
    for session in sessions {
        let Some(path) = session.transcript_path.as_deref() else {
            continue;
        };
        let hit = if session.agent == Agent::OpenCode
            && path.extension().and_then(|ext| ext.to_str()) == Some("db")
        {
            scan_opencode_db(path, &session.session_id, &needle)
        } else {
            scan_file(path, &needle)
        };
        if let Some((matches, snippet)) = hit {
            hits.push(SearchHit {
                session,
                matches,
                snippet,
            });
        }
        if hits.len() >= limit {
            break;
        }
    }
    Ok(hits)
}

fn compact(session: Session, turns: Vec<Turn>, limit: usize) -> Transcript {
    let last_user_request = turns
        .iter()
        .rev()
        .find(|turn| turn.role == "user" && !turn.text.is_empty())
        .map(|turn| clip(&turn.text, 400));
    let last_assistant_action = turns
        .iter()
        .rev()
        .find(|turn| turn.role == "assistant" && (!turn.text.is_empty() || !turn.tools.is_empty()))
        .map(|turn| {
            if turn.text.is_empty() {
                format!("called {}", turn.tools.join(", "))
            } else {
                clip(&turn.text, 400)
            }
        });
    let turn_count = turns.len();
    let window: Vec<Turn> = if limit == 0 || turns.len() <= limit {
        turns
    } else {
        turns[turns.len() - limit..].to_vec()
    };
    Transcript {
        session,
        turn_count,
        returned_turns: window.len(),
        last_user_request,
        last_assistant_action,
        turns: window,
        inert: true,
    }
}

fn read_cursor(path: &Path) -> Result<Vec<Turn>> {
    let mut turns = Vec::new();
    for value in jsonl(path)? {
        let Some(role) = value.get("role").and_then(Value::as_str) else {
            continue;
        };
        if role != "user" && role != "assistant" {
            continue;
        }
        let (text, tools) = content_parts(value.pointer("/message/content"));
        let text = if role == "user" {
            unwrap_cursor_user(&text)
        } else {
            text
        };
        if text.is_empty() && tools.is_empty() {
            continue;
        }
        turns.push(Turn {
            role: role.to_string(),
            text,
            tools,
        });
    }
    Ok(turns)
}

fn read_opencode(session: &crate::model::Session, path: &Path) -> Result<Vec<Turn>> {
    if path.extension().and_then(|ext| ext.to_str()) == Some("db") {
        return read_opencode_sqlite(path, &session.session_id);
    }
    read_opencode_json_tree(path, &session.session_id)
}

fn read_opencode_sqlite(path: &Path, session_id: &str) -> Result<Vec<Turn>> {
    let connection =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut statement = connection.prepare(
        "SELECT m.data, p.data
         FROM message m
         LEFT JOIN part p ON p.message_id = m.id
         WHERE m.session_id = ?1
         ORDER BY m.time_created, p.time_created",
    )?;
    let rows = statement.query_map([session_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
    })?;
    let mut turns = Vec::new();
    let mut current_role = String::new();
    let mut text = String::new();
    let mut tools = Vec::new();
    let flush =
        |role: &mut String, text: &mut String, tools: &mut Vec<String>, turns: &mut Vec<Turn>| {
            if role.is_empty() {
                return;
            }
            if text.is_empty() && tools.is_empty() {
                role.clear();
                return;
            }
            turns.push(Turn {
                role: std::mem::take(role),
                text: std::mem::take(text),
                tools: std::mem::take(tools),
            });
        };
    for row in rows {
        let (message_raw, part_raw) = row?;
        let message: Value = serde_json::from_str(&message_raw)?;
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("assistant")
            .to_string();
        if role != current_role {
            flush(&mut current_role, &mut text, &mut tools, &mut turns);
            current_role = role;
        }
        if let Some(part_raw) = part_raw {
            let part: Value = serde_json::from_str(&part_raw)?;
            match part.get("type").and_then(Value::as_str) {
                Some("text") => {
                    if let Some(chunk) = part.get("text").and_then(Value::as_str) {
                        if !text.is_empty() {
                            text.push('\n');
                        }
                        text.push_str(chunk);
                    }
                }
                Some(kind) if kind.contains("tool") => {
                    if let Some(name) = part
                        .get("name")
                        .or_else(|| part.pointer("/tool/name"))
                        .or_else(|| part.get("tool"))
                        .and_then(Value::as_str)
                    {
                        tools.push(name.to_string());
                    } else {
                        tools.push(kind.to_string());
                    }
                }
                _ => {
                    if let Some(chunk) = part.get("text").and_then(Value::as_str) {
                        if !text.is_empty() {
                            text.push('\n');
                        }
                        text.push_str(chunk);
                    }
                }
            }
        }
    }
    flush(&mut current_role, &mut text, &mut tools, &mut turns);
    Ok(turns)
}

fn read_opencode_json_tree(path: &Path, session_id: &str) -> Result<Vec<Turn>> {
    let Some(storage) = path
        .parent()
        .and_then(|parent| parent.parent())
        .and_then(|parent| parent.parent())
    else {
        return Ok(Vec::new());
    };
    let messages = storage.join("message").join(session_id);
    if !messages.is_dir() {
        return Ok(Vec::new());
    }
    let mut files: Vec<_> = std::fs::read_dir(&messages)
        .map_err(|source| Error::Io {
            path: messages.clone(),
            source,
        })?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("json"))
        .collect();
    files.sort();
    let mut turns = Vec::new();
    for file in files {
        let value: Value =
            serde_json::from_str(&std::fs::read_to_string(&file).map_err(|source| Error::Io {
                path: file.clone(),
                source,
            })?)?;
        let role = value
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("assistant")
            .to_string();
        let parts_dir = file.file_stem().map(|stem| storage.join("part").join(stem));
        let mut text = String::new();
        let mut tools = Vec::new();
        if let Some(parts_dir) = parts_dir.filter(|dir| dir.is_dir())
            && let Ok(entries) = std::fs::read_dir(parts_dir)
        {
            for entry in entries.flatten() {
                let Ok(part) = serde_json::from_str::<Value>(
                    &std::fs::read_to_string(entry.path()).unwrap_or_default(),
                ) else {
                    continue;
                };
                if let Some(chunk) = part.get("text").and_then(Value::as_str) {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(chunk);
                }
                if part
                    .get("type")
                    .and_then(Value::as_str)
                    .is_some_and(|kind| kind.contains("tool"))
                    && let Some(name) = part.get("name").and_then(Value::as_str)
                {
                    tools.push(name.to_string());
                }
            }
        }
        if text.is_empty() && tools.is_empty() {
            continue;
        }
        turns.push(Turn { role, text, tools });
    }
    Ok(turns)
}

fn scan_opencode_db(path: &Path, session_id: &str, needle: &str) -> Option<(usize, String)> {
    let connection =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .ok()?;
    let mut statement = connection
        .prepare("SELECT data FROM part WHERE session_id = ?1")
        .ok()?;
    let rows = statement
        .query_map([session_id], |row| row.get::<_, String>(0))
        .ok()?;
    let mut matches = 0usize;
    let mut snippet = None;
    for row in rows.flatten() {
        let lowered = row.to_ascii_lowercase();
        if !lowered.contains(needle) {
            continue;
        }
        matches += 1;
        if snippet.is_none() {
            snippet = Some(extract_snippet(&row, needle));
        }
    }
    snippet.map(|snippet| (matches, snippet))
}

fn unwrap_cursor_user(text: &str) -> String {
    if let Some(start) = text.find("<user_query>") {
        let rest = &text[start + "<user_query>".len()..];
        let body = rest.split("</user_query>").next().unwrap_or(rest);
        return body.split_whitespace().collect::<Vec<_>>().join(" ");
    }
    text.to_string()
}

fn read_gemini(path: &Path) -> Result<Vec<Turn>> {
    if let Ok(raw) = std::fs::read_to_string(path)
        && let Ok(value) = serde_json::from_str::<Value>(&raw)
    {
        return Ok(gemini_turns_from_messages(value.get("messages")));
    }
    let mut turns = Vec::new();
    for value in jsonl(path)? {
        if let Some(turn) = gemini_turn(&value) {
            turns.push(turn);
        }
    }
    Ok(turns)
}

fn gemini_turns_from_messages(messages: Option<&Value>) -> Vec<Turn> {
    let Some(items) = messages.and_then(Value::as_array) else {
        return Vec::new();
    };
    items.iter().filter_map(gemini_turn).collect()
}

fn gemini_turn(value: &Value) -> Option<Turn> {
    let kind = value
        .get("type")
        .or_else(|| value.get("role"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let role = match kind {
        "user" => "user",
        "gemini" | "model" | "assistant" => "assistant",
        _ => return None,
    };
    let text = gemini_text(value);
    let tools = gemini_tools(value);
    if text.is_empty() && tools.is_empty() {
        return None;
    }
    Some(Turn {
        role: role.to_string(),
        text,
        tools,
    })
}

fn gemini_text(value: &Value) -> String {
    if let Some(text) = value.get("content").and_then(Value::as_str) {
        return text.split_whitespace().collect::<Vec<_>>().join(" ");
    }
    if let Some(text) = value.get("message").and_then(Value::as_str) {
        return text.split_whitespace().collect::<Vec<_>>().join(" ");
    }
    if let Some(text) = value
        .get("content")
        .and_then(|content| content.get("text"))
        .and_then(Value::as_str)
    {
        return text.split_whitespace().collect::<Vec<_>>().join(" ");
    }
    let (text, _) = content_parts(value.get("content"));
    text
}

fn gemini_tools(value: &Value) -> Vec<String> {
    let mut tools = Vec::new();
    if let Some(name) = value
        .get("tool")
        .or_else(|| value.get("toolName"))
        .or_else(|| value.pointer("/tool_call/name"))
        .and_then(Value::as_str)
    {
        tools.push(name.to_string());
    }
    if let Some(items) = value.get("toolCalls").and_then(Value::as_array) {
        for item in items {
            if let Some(name) = item.get("name").and_then(Value::as_str) {
                tools.push(name.to_string());
            }
        }
    }
    tools
}

fn read_copilot(path: &Path) -> Result<Vec<Turn>> {
    let mut turns = Vec::new();
    for value in jsonl(path)? {
        let kind = value.get("type").and_then(Value::as_str).unwrap_or("");
        let data = value.get("data").unwrap_or(&Value::Null);
        match kind {
            "user.message" => {
                let text = data
                    .get("content")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                if !text.is_empty() {
                    turns.push(Turn {
                        role: "user".into(),
                        text,
                        tools: Vec::new(),
                    });
                }
            }
            "assistant.message" => {
                let text = data
                    .get("content")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                let mut tools = Vec::new();
                if let Some(requests) = data.get("toolRequests").and_then(Value::as_array) {
                    for request in requests {
                        if let Some(name) = request.get("name").and_then(Value::as_str) {
                            tools.push(name.to_string());
                        }
                    }
                }
                if !text.is_empty() || !tools.is_empty() {
                    turns.push(Turn {
                        role: "assistant".into(),
                        text,
                        tools,
                    });
                }
            }
            _ => {}
        }
    }
    Ok(turns)
}

fn read_claude(path: &Path) -> Result<Vec<Turn>> {
    let mut turns = Vec::new();
    for value in jsonl(path)? {
        let kind = value.get("type").and_then(Value::as_str).unwrap_or("");
        if kind != "user" && kind != "assistant" {
            continue;
        }
        if value.get("isMeta").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let message = value.get("message").unwrap_or(&value);
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or(kind)
            .to_string();
        let (text, tools) = content_parts(message.get("content"));
        if text.is_empty() && tools.is_empty() {
            continue;
        }
        turns.push(Turn { role, text, tools });
    }
    Ok(turns)
}

fn read_codex(path: &Path) -> Result<Vec<Turn>> {
    let mut turns = Vec::new();
    for value in jsonl(path)? {
        if value.get("type").and_then(Value::as_str) != Some("response_item") {
            continue;
        }
        let Some(payload) = value.get("payload") else {
            continue;
        };
        if payload.get("type").and_then(Value::as_str) != Some("message") {
            continue;
        }
        let Some(role) = payload.get("role").and_then(Value::as_str) else {
            continue;
        };
        let (text, tools) = content_parts(payload.get("content"));
        if text.is_empty() && tools.is_empty() {
            continue;
        }
        turns.push(Turn {
            role: role.to_string(),
            text,
            tools,
        });
    }
    Ok(turns)
}

fn read_grok(path: &Path) -> Result<Vec<Turn>> {
    let mut turns = Vec::new();
    let mut user = String::new();
    let mut assistant = String::new();
    let mut tools = Vec::new();
    let flush = |user: &mut String,
                 assistant: &mut String,
                 tools: &mut Vec<String>,
                 turns: &mut Vec<Turn>| {
        if !user.is_empty() {
            turns.push(Turn {
                role: "user".into(),
                text: std::mem::take(user),
                tools: Vec::new(),
            });
        }
        if !assistant.is_empty() || !tools.is_empty() {
            turns.push(Turn {
                role: "assistant".into(),
                text: std::mem::take(assistant),
                tools: std::mem::take(tools),
            });
        }
    };
    for value in jsonl(path)? {
        let update = value
            .pointer("/params/update")
            .cloned()
            .unwrap_or(Value::Null);
        let kind = update
            .get("sessionUpdate")
            .and_then(Value::as_str)
            .unwrap_or("");
        match kind {
            "user_message_chunk" => {
                if !assistant.is_empty() || !tools.is_empty() {
                    flush(&mut String::new(), &mut assistant, &mut tools, &mut turns);
                }
                if let Some(text) = chunk_text(&update) {
                    user.push_str(&text);
                }
            }
            "agent_message_chunk" => {
                if let Some(text) = chunk_text(&update) {
                    assistant.push_str(&text);
                }
            }
            "tool_call" => {
                if let Some(name) = update
                    .pointer("/toolCall/name")
                    .and_then(Value::as_str)
                    .or_else(|| update.get("name").and_then(Value::as_str))
                {
                    tools.push(name.to_string());
                }
            }
            "turn_completed" => {
                flush(&mut user, &mut assistant, &mut tools, &mut turns);
            }
            _ => {}
        }
    }
    flush(&mut user, &mut assistant, &mut tools, &mut turns);
    Ok(turns)
}

fn chunk_text(update: &Value) -> Option<String> {
    update
        .pointer("/content/text")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn content_parts(content: Option<&Value>) -> (String, Vec<String>) {
    let mut texts = Vec::new();
    let mut tools = Vec::new();
    match content {
        Some(Value::String(text)) => texts.push(text.clone()),
        Some(Value::Array(items)) => {
            for item in items {
                match item.get("type").and_then(Value::as_str) {
                    Some("text" | "input_text" | "output_text") => {
                        if let Some(text) = item.get("text").and_then(Value::as_str)
                            && !text.trim().is_empty()
                        {
                            texts.push(text.to_string());
                        }
                    }
                    Some("tool_use") => {
                        if let Some(name) = item.get("name").and_then(Value::as_str) {
                            tools.push(name.to_string());
                        }
                    }
                    _ => {
                        if let Some(text) = item.get("text").and_then(Value::as_str) {
                            texts.push(text.to_string());
                        }
                    }
                }
            }
        }
        _ => {}
    }
    (texts.join("\n"), tools)
}

pub(crate) fn scan_file(path: &Path, needle: &str) -> Option<(usize, String)> {
    let file = File::open(path).ok()?;
    let mut matches = 0usize;
    let mut snippet = None;
    for line in BufReader::new(file)
        .lines()
        .map_while(std::result::Result::ok)
    {
        let lowered = line.to_ascii_lowercase();
        if !lowered.contains(needle) {
            continue;
        }
        matches += 1;
        if snippet.is_none() {
            snippet = Some(extract_snippet(&line, needle));
        }
    }
    snippet.map(|snippet| (matches, snippet))
}

pub(crate) fn extract_snippet(line: &str, needle: &str) -> String {
    let lowered = line.to_ascii_lowercase();
    let index = lowered.find(needle).unwrap_or(0);
    let start = index.saturating_sub(160);
    let end = (index + needle.len() + 160).min(line.len());
    let mut snippet = line[start..end].replace('\n', " ");
    if start > 0 {
        snippet.insert_str(0, "...");
    }
    if end < line.len() {
        snippet.push_str("...");
    }
    clip(&snippet, 400)
}

fn jsonl(path: &Path) -> Result<Vec<Value>> {
    let file = File::open(path).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut records = Vec::new();
    for line in BufReader::new(file).lines() {
        let line = line.map_err(|source| Error::Io {
            path: path.to_path_buf(),
            source,
        })?;
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<Value>(&line) {
            records.push(value);
        }
    }
    Ok(records)
}

fn clip(text: &str, limit: usize) -> String {
    let collapsed: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= limit {
        collapsed
    } else {
        collapsed.chars().take(limit).collect::<String>() + "..."
    }
}

#[cfg(test)]
mod tests {
    use super::{clip, content_parts};
    use serde_json::json;

    #[test]
    fn extracts_codex_user_text() {
        let content = json!([{ "type": "input_text", "text": "hello" }]);
        let (text, tools) = content_parts(Some(&content));
        assert_eq!(text, "hello");
        assert!(tools.is_empty());
    }

    #[test]
    fn clips_whitespace() {
        assert_eq!(clip(" a   b ", 10), "a b");
    }

    #[test]
    fn unwraps_cursor_user_query() {
        let raw =
            "<timestamp>Sunday</timestamp>\n<user_query>\nPull the 109 point matrix\n</user_query>";
        assert_eq!(super::unwrap_cursor_user(raw), "Pull the 109 point matrix");
        assert_eq!(super::unwrap_cursor_user("plain"), "plain");
    }

    #[test]
    fn clips_long_text_and_snippets() {
        let long = "word ".repeat(200);
        let clipped = clip(&long, 20);
        assert!(clipped.ends_with("..."));
        assert!(clipped.chars().count() > 20);
        let line = format!("{}NEEDLE{}", "a".repeat(200), "b".repeat(200));
        let snippet = super::extract_snippet(&line, "needle");
        assert!(snippet.starts_with("..."));
        assert!(snippet.ends_with("..."));
    }

    #[test]
    fn digest_and_files_from_tool_input() {
        use super::{files_touched, looks_like_path, session_digest};
        use crate::handoff_tests::World;

        let world = World::new();
        let digest = session_digest(&world.homes, "claude:disaster-recovery", 0).unwrap();
        assert!(digest.inert);
        assert!(digest.turns.iter().any(|turn| turn.text.len() <= 283));
        let files = files_touched(&world.homes, "claude:disaster-recovery").unwrap();
        assert!(files.files.iter().any(|path| path == "src/lib.rs"));
        let gemini = files_touched(&world.homes, "gemini:latest").unwrap();
        assert!(gemini.files.iter().any(|path| path == "src/gemini.rs"));
        let copilot = files_touched(&world.homes, "copilot:latest").unwrap();
        assert!(copilot.files.iter().any(|path| path == "src/copilot.rs"));
        assert!(looks_like_path("src/lib.rs"));
        assert!(looks_like_path("/tmp/x.rs"));
        assert!(!looks_like_path("https://example.com/x.rs"));
        assert!(!looks_like_path("plain"));
    }

    #[test]
    fn reads_gemini_and_copilot_layout_variants() {
        let dir = tempfile::tempdir().unwrap();
        let pretty = dir.path().join("pretty.json");
        std::fs::write(
            &pretty,
            r#"{
  "messages": [
    {"type": "user", "content": "inspect src/pretty_gemini.rs"},
    {"role": "model", "message": "model via message"},
    {"type": "assistant", "content": {"text": "nested assistant"}},
    {"type": "gemini", "content": [{"type": "text", "text": "parts"}], "tool": "Read", "toolCalls": [{"name": "Write"}]},
    {"type": "info", "content": "skip"},
    {"type": "user", "content": "   "},
    {"type": "gemini", "tool_call": {"name": "Bash"}}
  ]
}"#,
        )
        .unwrap();
        let turns = super::read_gemini(&pretty).unwrap();
        assert!(turns.iter().any(|turn| turn.role == "user"));
        assert!(
            turns
                .iter()
                .any(|turn| turn.role == "assistant" && turn.text.contains("model via message"))
        );
        assert!(
            turns
                .iter()
                .any(|turn| turn.text.contains("nested assistant"))
        );
        assert!(turns.iter().any(
            |turn| turn.tools.contains(&"Read".into()) && turn.tools.contains(&"Write".into())
        ));
        assert!(
            turns
                .iter()
                .any(|turn| turn.tools.contains(&"Bash".into()) && turn.text.is_empty())
        );
        assert!(super::read_gemini(&dir.path().join("missing.json")).is_err());
        let no_messages = dir.path().join("empty-object.json");
        std::fs::write(&no_messages, "{}").unwrap();
        assert!(super::read_gemini(&no_messages).unwrap().is_empty());
        let jsonl = dir.path().join("journal.jsonl");
        std::fs::write(
            &jsonl,
            r#"{"sessionId":"x"}
{"type":"user","content":"jsonl user"}
{"type":"gemini","content":"jsonl assistant","toolName":"Grep"}
"#,
        )
        .unwrap();
        let jsonl_turns = super::read_gemini(&jsonl).unwrap();
        assert_eq!(jsonl_turns.len(), 2);
        assert_eq!(jsonl_turns[1].tools, vec!["Grep".to_string()]);

        let events = dir.path().join("events.jsonl");
        std::fs::write(
            &events,
            r#"{"type":"session.start","data":{}}
{"type":"user.message","data":{"content":"   "}}
{"type":"user.message","data":{"content":"copilot user"}}
{"type":"assistant.message","data":{"content":"","toolRequests":[{"name":"view"},{"id":"skip"}]}}
{"type":"assistant.message","data":{"content":"plain assistant"}}
"#,
        )
        .unwrap();
        let copilot = super::read_copilot(&events).unwrap();
        assert_eq!(copilot.len(), 3);
        assert_eq!(copilot[0].role, "user");
        assert_eq!(copilot[1].tools, vec!["view".to_string()]);
        assert!(copilot[1].text.is_empty());
        assert_eq!(copilot[2].text, "plain assistant");
    }

    #[test]
    fn content_parts_variants() {
        let (text, tools) = content_parts(Some(&json!("just a string")));
        assert_eq!(text, "just a string");
        assert!(tools.is_empty());
        let (text, tools) = content_parts(Some(&json!([
            {"type": "other", "text": "side"},
            {"type": "tool_use", "name": "Bash"}
        ])));
        assert!(text.contains("side"));
        assert_eq!(tools, vec!["Bash".to_string()]);
        let (text, tools) = content_parts(None);
        assert!(text.is_empty());
        assert!(tools.is_empty());
    }

    #[test]
    fn search_and_compact_edge_cases() {
        use crate::discover::list_sessions;
        use crate::homes::Homes;
        use crate::model::{Agent, Session, Turn};

        let empty = super::search_transcripts(
            &Homes::isolated(tempfile::tempdir().unwrap().path()),
            "   ",
            None,
            false,
            10,
        )
        .unwrap_err();
        assert!(empty.to_string().contains("query is required"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let blocked = Homes::isolated(tempfile::tempdir().unwrap().path());
            let projects = blocked.cursor.join("projects");
            std::fs::create_dir_all(&projects).unwrap();
            let mut permissions = std::fs::metadata(&projects).unwrap().permissions();
            permissions.set_mode(0o000);
            std::fs::set_permissions(&projects, permissions).unwrap();
            assert!(super::search_transcripts(&blocked, "needle", None, false, 10).is_err());
            let mut permissions = std::fs::metadata(&projects).unwrap().permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&projects, permissions).unwrap();
        }

        let no_path = super::read_session(
            &Session {
                agent: Agent::Cursor,
                session_id: "x".into(),
                desktop_id: None,
                name: None,
                title: None,
                cwd: None,
                branch: None,
                live: false,
                archived: false,
                pid: None,
                model: None,
                last_activity_at: None,
                transcript_path: None,
                messaging_socket: None,
                origin: None,
                tmux: None,
            },
            10,
        )
        .unwrap_err();
        assert!(no_path.to_string().contains("no transcript"));

        let compact = super::compact(
            Session {
                agent: Agent::Grok,
                session_id: "x".into(),
                desktop_id: None,
                name: None,
                title: None,
                cwd: None,
                branch: None,
                live: false,
                archived: false,
                pid: None,
                model: None,
                last_activity_at: None,
                transcript_path: None,
                messaging_socket: None,
                origin: None,
                tmux: None,
            },
            vec![
                Turn {
                    role: "user".into(),
                    text: "one".into(),
                    tools: vec![],
                },
                Turn {
                    role: "assistant".into(),
                    text: String::new(),
                    tools: vec!["shell".into()],
                },
                Turn {
                    role: "user".into(),
                    text: "two".into(),
                    tools: vec![],
                },
            ],
            1,
        );
        assert_eq!(compact.returned_turns, 1);
        assert_eq!(compact.last_user_request.as_deref(), Some("two"));
        assert_eq!(
            compact.last_assistant_action.as_deref(),
            Some("called shell")
        );

        let dir = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(dir.path());
        let _ = list_sessions(&homes, &crate::discover::ListFilter::default());
        let jsonl = dir.path().join("empty.jsonl");
        std::fs::write(&jsonl, "\nnot-json\n").unwrap();
        assert!(super::jsonl(&jsonl).unwrap().is_empty());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let blocked = dir.path().join("blocked.jsonl");
            std::fs::write(&blocked, "{}\n").unwrap();
            let mut permissions = std::fs::metadata(&blocked).unwrap().permissions();
            permissions.set_mode(0o000);
            std::fs::set_permissions(&blocked, permissions).unwrap();
            assert!(super::jsonl(&blocked).is_err());
            let mut permissions = std::fs::metadata(&blocked).unwrap().permissions();
            permissions.set_mode(0o644);
            std::fs::set_permissions(&blocked, permissions).unwrap();
            let invalid = dir.path().join("invalid-utf8.jsonl");
            std::fs::write(&invalid, [0xff, 0x80, b'\n']).unwrap();
            assert!(super::jsonl(&invalid).is_err());
        }
        assert!(super::scan_file(&jsonl, "needle").is_none());
        assert!(
            super::read_opencode_json_tree(&dir.path().join("nope.json"), "x")
                .unwrap()
                .is_empty()
        );

        let world = crate::handoff_tests::World::new();
        let opencode = super::files_touched(&world.homes, "opencode:ses_testopencode0001").unwrap();
        assert!(opencode.inert);

        let json = dir.path().join("blob.json");
        std::fs::write(
            &json,
            r#"{"file_path":"src/main.rs","items":[{"target_file":"./lib.rs"}]}"#,
        )
        .unwrap();
        let mut files = std::collections::BTreeSet::new();
        super::collect_files(Agent::Claude, &json, "x", &mut files);
        assert!(files.contains("src/main.rs"));
        assert!(files.contains("./lib.rs"));

        let cursor = dir.path().join("cursor.jsonl");
        std::fs::write(
            &cursor,
            r#"{"role":"system","message":{"content":[{"type":"text","text":"skip"}]}}
{"role":"assistant","message":{"content":[]}}
{"role":"user","message":{"content":[{"type":"text","text":"hello"}]}}
"#,
        )
        .unwrap();
        let turns = super::read_cursor(&cursor).unwrap();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].role, "user");
        let no_role = dir.path().join("norole.jsonl");
        std::fs::write(&no_role, "{\"message\":{}}\n").unwrap();
        assert!(super::read_cursor(&no_role).unwrap().is_empty());
        assert!(
            super::read_opencode_json_tree(std::path::Path::new("nope.json"), "x")
                .unwrap()
                .is_empty()
        );
        let garbage = dir.path().join("garbage.db");
        std::fs::write(&garbage, "not sqlite").unwrap();
        let mut files = std::collections::BTreeSet::new();
        super::collect_files(Agent::OpenCode, &garbage, "x", &mut files);
        assert!(files.is_empty());

        let storage = dir.path().join("storage");
        let session_json = storage.join("session").join("proj").join("ses_tree.json");
        std::fs::create_dir_all(session_json.parent().unwrap()).unwrap();
        std::fs::write(&session_json, "{}").unwrap();
        let messages = storage.join("message").join("ses_tree");
        std::fs::create_dir_all(&messages).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&messages).unwrap().permissions();
            permissions.set_mode(0o000);
            std::fs::set_permissions(&messages, permissions).unwrap();
            assert!(super::read_opencode_json_tree(&session_json, "ses_tree").is_err());
            let mut permissions = std::fs::metadata(&messages).unwrap().permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&messages, permissions).unwrap();
            let blocked = messages.join("one.json");
            std::fs::write(&blocked, r#"{"role":"user"}"#).unwrap();
            let mut permissions = std::fs::metadata(&blocked).unwrap().permissions();
            permissions.set_mode(0o000);
            std::fs::set_permissions(&blocked, permissions).unwrap();
            assert!(super::read_opencode_json_tree(&session_json, "ses_tree").is_err());
            let mut permissions = std::fs::metadata(&blocked).unwrap().permissions();
            permissions.set_mode(0o644);
            std::fs::set_permissions(&blocked, permissions).unwrap();
        }

        let db = dir.path().join("parts.db");
        let connection = rusqlite::Connection::open(&db).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE message (id TEXT, session_id TEXT, time_created INTEGER, data TEXT);
                 CREATE TABLE part (id TEXT, message_id TEXT, time_created INTEGER, data TEXT);
                 INSERT INTO message VALUES ('m0', 'ses_x', 1, '{\"role\":\"user\"}');
                 INSERT INTO message VALUES ('m1', 'ses_x', 2, '{\"role\":\"assistant\"}');
                 INSERT INTO part VALUES ('p1', 'm1', 1, '{\"type\":\"text\",\"text\":\"one\"}');
                 INSERT INTO part VALUES ('p2', 'm1', 2, '{\"type\":\"text\",\"text\":\"two\"}');
                 INSERT INTO part VALUES ('p3', 'm1', 3, '{\"type\":\"note\",\"text\":\"three\"}');
                 INSERT INTO part VALUES ('p4', 'm1', 4, '{\"type\":\"tool_use\"}');
                 INSERT INTO part VALUES ('p5', 'm1', 5, '{\"type\":\"text\"}');
                 INSERT INTO part VALUES ('p6', 'm1', 6, '{\"type\":\"note\"}');",
            )
            .unwrap();
        let turns = super::read_opencode_sqlite(&db, "ses_x").unwrap();
        assert!(turns.iter().any(|turn| turn.text.contains("one")));
        assert!(turns.iter().any(|turn| turn.text.contains("two")));
    }
}
