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
        if let Some((matches, snippet)) = scan_file(path, &needle) {
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
}
