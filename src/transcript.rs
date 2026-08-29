use crate::discover::{ListFilter, list_sessions, resolve};
use crate::error::{Error, Result};
use crate::homes::Homes;
use crate::model::{Agent, SearchHit, Session, Transcript, Turn};
use serde_json::Value;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

pub fn read_transcript(homes: &Homes, reference: &str, limit: usize) -> Result<Transcript> {
    let session = resolve(homes, reference)?;
    read_session(&session, limit)
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

fn scan_file(path: &Path, needle: &str) -> Option<(usize, String)> {
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

fn extract_snippet(line: &str, needle: &str) -> String {
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
    }
}
