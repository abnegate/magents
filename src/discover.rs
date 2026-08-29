use crate::error::{Error, Result};
use crate::homes::{Homes, pid_alive};
use crate::model::{Agent, Session};
use chrono::{DateTime, TimeZone, Utc};
use regex::Regex;
use rusqlite::Connection;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use walkdir::WalkDir;

#[derive(Clone, Debug)]
pub struct ListFilter {
    pub agent: Option<Agent>,
    pub query: Option<String>,
    pub live_only: bool,
    pub include_archived: bool,
    pub limit: usize,
}

impl Default for ListFilter {
    fn default() -> Self {
        Self {
            agent: None,
            query: None,
            live_only: false,
            include_archived: false,
            limit: 20,
        }
    }
}

pub fn list_sessions(homes: &Homes, filter: &ListFilter) -> Result<Vec<Session>> {
    let mut sessions = Vec::new();
    if filter.agent.is_none() || filter.agent == Some(Agent::Claude) {
        sessions.extend(discover_claude(homes)?);
    }
    if filter.agent.is_none() || filter.agent == Some(Agent::Grok) {
        sessions.extend(discover_grok(homes)?);
    }
    if filter.agent.is_none() || filter.agent == Some(Agent::Codex) {
        sessions.extend(discover_codex(homes)?);
    }
    let needle = filter
        .query
        .as_deref()
        .map(|query| query.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|query| !query.is_empty())
        .map(|query| query.to_ascii_lowercase());
    sessions.retain(|session| {
        if filter.live_only && !session.live {
            return false;
        }
        if !filter.include_archived && session.archived {
            return false;
        }
        if let Some(needle) = &needle
            && !session.haystack().contains(needle)
        {
            return false;
        }
        true
    });
    sessions.sort_by(|left, right| {
        right
            .live
            .cmp(&left.live)
            .then_with(|| right.activity_ms().cmp(&left.activity_ms()))
            .then_with(|| left.session_id.cmp(&right.session_id))
    });
    sessions
        .dedup_by(|left, right| left.agent == right.agent && left.session_id == right.session_id);
    if filter.limit > 0 && sessions.len() > filter.limit {
        sessions.truncate(filter.limit);
    }
    Ok(sessions)
}

pub fn resolve(homes: &Homes, reference: &str) -> Result<Session> {
    let reference = reference.trim();
    if reference.is_empty() {
        return Err(Error::msg("session reference is required"));
    }
    let (agent, rest) = split_agent_ref(reference);
    let filter = ListFilter {
        agent,
        query: None,
        live_only: false,
        include_archived: true,
        limit: 0,
    };
    let sessions = list_sessions(homes, &filter)?;
    if rest.eq_ignore_ascii_case("latest") || rest.eq_ignore_ascii_case("self") {
        return sessions
            .into_iter()
            .next()
            .ok_or_else(|| Error::NotFound(reference.to_string()));
    }
    let exact: Vec<Session> = sessions
        .iter()
        .filter(|session| {
            session.session_id.eq_ignore_ascii_case(rest)
                || session
                    .desktop_id
                    .as_deref()
                    .is_some_and(|id| id.eq_ignore_ascii_case(rest))
                || session
                    .name
                    .as_deref()
                    .is_some_and(|name| name.eq_ignore_ascii_case(rest))
                || session.pid.map(|pid| pid.to_string()) == Some(rest.to_string())
        })
        .cloned()
        .collect();
    match exact.len() {
        1 => return Ok(exact.into_iter().next().unwrap()),
        n if n > 1 => {
            return Err(Error::Ambiguous {
                reference: reference.to_string(),
                matches: exact.iter().map(Session::label).collect(),
            });
        }
        _ => {}
    }
    let needle = rest.to_ascii_lowercase();
    let matches: Vec<Session> = sessions
        .into_iter()
        .filter(|session| session.haystack().contains(&needle))
        .collect();
    match matches.len() {
        1 => Ok(matches.into_iter().next().unwrap()),
        0 => Err(Error::NotFound(reference.to_string())),
        _ => Err(Error::Ambiguous {
            reference: reference.to_string(),
            matches: matches.iter().map(Session::label).collect(),
        }),
    }
}

fn split_agent_ref(reference: &str) -> (Option<Agent>, &str) {
    if let Some((head, tail)) = reference.split_once(':')
        && let Some(agent) = Agent::parse(head)
    {
        return (Some(agent), tail);
    }
    (None, reference)
}

fn uuid_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$")
            .expect("uuid regex")
    })
}

pub fn claude_transcript_index(claude_home: &Path) -> HashMap<String, PathBuf> {
    let mut index = HashMap::new();
    let projects = claude_home.join("projects");
    if !projects.is_dir() {
        return index;
    }
    for entry in WalkDir::new(&projects).max_depth(2).into_iter().flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
            continue;
        }
        if let Some(stem) = path.file_stem().and_then(|stem| stem.to_str())
            && uuid_re().is_match(stem)
        {
            index.insert(stem.to_string(), path.to_path_buf());
        }
    }
    index
}

fn discover_claude(homes: &Homes) -> Result<Vec<Session>> {
    let index = claude_transcript_index(&homes.claude);
    let mut by_id: HashMap<String, Session> = HashMap::new();

    let live_dir = homes.claude.join("sessions");
    if live_dir.is_dir() {
        for entry in fs::read_dir(&live_dir).map_err(|source| Error::Io {
            path: live_dir.clone(),
            source,
        })? {
            let entry = entry.map_err(|source| Error::Io {
                path: live_dir.clone(),
                source,
            })?;
            let path = entry.path();
            let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            if !stem.chars().all(|ch| ch.is_ascii_digit()) {
                continue;
            }
            let Ok(value) = read_json(&path) else {
                continue;
            };
            let pid = value.get("pid").and_then(Value::as_u64).unwrap_or(0) as u32;
            let Some(session_id) = value.get("sessionId").and_then(Value::as_str) else {
                continue;
            };
            if !pid_alive(pid) {
                continue;
            }
            let messaging_socket = value
                .get("messagingSocketPath")
                .and_then(Value::as_str)
                .filter(|path| !path.is_empty())
                .map(PathBuf::from)
                .or_else(|| {
                    let fallback = PathBuf::from(format!("/tmp/cc-socks/{pid}.sock"));
                    fallback.exists().then_some(fallback)
                });
            by_id.insert(
                session_id.to_string(),
                Session {
                    agent: Agent::Claude,
                    session_id: session_id.to_string(),
                    desktop_id: None,
                    name: value
                        .get("name")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned),
                    title: None,
                    cwd: value
                        .get("cwd")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned),
                    branch: None,
                    live: true,
                    archived: false,
                    pid: Some(pid),
                    model: None,
                    last_activity_at: millis(value.get("startedAt").and_then(Value::as_i64)),
                    transcript_path: index.get(session_id).cloned(),
                    messaging_socket,
                    origin: value
                        .get("entrypoint")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned),
                    tmux: value
                        .get("tmux")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned),
                },
            );
        }
    }

    if homes.claude_desktop.is_dir() {
        for entry in WalkDir::new(&homes.claude_desktop)
            .into_iter()
            .flatten()
            .filter(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.starts_with("local_") && name.ends_with(".json"))
            })
        {
            let Ok(value) = read_json(entry.path()) else {
                continue;
            };
            let Some(desktop_id) = value.get("sessionId").and_then(Value::as_str) else {
                continue;
            };
            let cli_id = value
                .get("cliSessionId")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            let archived = value
                .get("isArchived")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let title = value
                .get("title")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            let cwd = value
                .get("cwd")
                .or_else(|| value.get("originCwd"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            let branch = value
                .get("branch")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            let model = value
                .get("model")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            let last_activity_at = millis(
                value
                    .get("lastActivityAt")
                    .or_else(|| value.get("lastFocusedAt"))
                    .and_then(Value::as_i64),
            );
            if let Some(session_id) = &cli_id {
                if let Some(existing) = by_id.get_mut(session_id) {
                    existing.desktop_id = Some(desktop_id.to_string());
                    if existing.title.is_none() {
                        existing.title = title.clone();
                    }
                    if existing.cwd.is_none() {
                        existing.cwd = cwd.clone();
                    }
                    existing.branch = branch.clone();
                    existing.model = model.clone();
                    existing.archived = archived;
                    if existing.last_activity_at.is_none() {
                        existing.last_activity_at = last_activity_at;
                    }
                    continue;
                }
                by_id.insert(
                    session_id.clone(),
                    Session {
                        agent: Agent::Claude,
                        session_id: session_id.clone(),
                        desktop_id: Some(desktop_id.to_string()),
                        name: None,
                        title,
                        cwd,
                        branch,
                        live: false,
                        archived,
                        pid: None,
                        model,
                        last_activity_at,
                        transcript_path: index.get(session_id).cloned(),
                        messaging_socket: None,
                        origin: Some("claude-desktop".into()),
                        tmux: None,
                    },
                );
            }
        }
    }

    Ok(by_id.into_values().collect())
}

#[derive(Deserialize)]
struct GrokSummary {
    info: GrokInfo,
    #[serde(default)]
    session_summary: Option<String>,
    #[serde(default)]
    generated_title: Option<String>,
    #[serde(default)]
    current_model_id: Option<String>,
    #[serde(default)]
    last_active_at: Option<DateTime<Utc>>,
    #[serde(default)]
    session_kind: Option<String>,
}

#[derive(Deserialize)]
struct GrokInfo {
    id: String,
    #[serde(default)]
    cwd: Option<String>,
}

#[derive(Deserialize)]
struct GrokActive {
    session_id: String,
    pid: u32,
}

fn discover_grok(homes: &Homes) -> Result<Vec<Session>> {
    let mut live: HashMap<String, u32> = HashMap::new();
    let active_path = homes.grok.join("active_sessions.json");
    if let Ok(raw) = fs::read_to_string(&active_path)
        && let Ok(rows) = serde_json::from_str::<Vec<GrokActive>>(&raw)
    {
        for row in rows {
            if pid_alive(row.pid) {
                live.insert(row.session_id, row.pid);
            }
        }
    }
    let root = homes.grok.join("sessions");
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut sessions = Vec::new();
    for entry in WalkDir::new(&root).into_iter().flatten() {
        if entry.file_name() != "summary.json" {
            continue;
        }
        let path = entry.path();
        let Ok(raw) = fs::read_to_string(path) else {
            continue;
        };
        let Ok(summary) = serde_json::from_str::<GrokSummary>(&raw) else {
            continue;
        };
        if summary.session_kind.as_deref() == Some("subagent") {
            continue;
        }
        let pid = live.get(&summary.info.id).copied();
        let transcript = path
            .parent()
            .map(|parent| parent.join("updates.jsonl"))
            .filter(|file| file.is_file());
        sessions.push(Session {
            agent: Agent::Grok,
            session_id: summary.info.id,
            desktop_id: None,
            name: None,
            title: summary.generated_title.or(summary.session_summary),
            cwd: summary.info.cwd,
            branch: None,
            live: pid.is_some(),
            archived: false,
            pid,
            model: summary.current_model_id,
            last_activity_at: summary.last_active_at,
            transcript_path: transcript,
            messaging_socket: None,
            origin: Some(if pid.is_some() {
                "tui".into()
            } else {
                "grok".into()
            }),
            tmux: None,
        });
    }
    Ok(sessions)
}

fn discover_codex(homes: &Homes) -> Result<Vec<Session>> {
    let db_path = newest_state_db(&homes.codex);
    let Some(db_path) = db_path else {
        return Ok(Vec::new());
    };
    let connection =
        Connection::open_with_flags(&db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut statement = connection.prepare(
        "SELECT id, title, cwd, git_branch, model, archived, updated_at_ms, rollout_path, source
         FROM threads",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, i64>(5).unwrap_or(0),
            row.get::<_, Option<i64>>(6)?,
            row.get::<_, Option<String>>(7)?,
            row.get::<_, Option<String>>(8)?,
        ))
    })?;
    let ipc_live = homes.codex.join("ipc").join("ipc.sock").exists();
    let mut sessions = Vec::new();
    for row in rows {
        let (id, title, cwd, branch, model, archived, updated_ms, rollout, source) = row?;
        let last_activity_at = millis(updated_ms);
        let recent = last_activity_at
            .map(|time| (Utc::now() - time).num_minutes().abs() < 10)
            .unwrap_or(false);
        let transcript = rollout
            .map(PathBuf::from)
            .filter(|path| path.is_file())
            .or_else(|| fallback_rollout(&homes.codex, &id));
        sessions.push(Session {
            agent: Agent::Codex,
            session_id: id,
            desktop_id: None,
            name: None,
            title: title.filter(|value| !value.is_empty()),
            cwd,
            branch,
            live: recent && ipc_live && archived == 0,
            archived: archived != 0,
            pid: None,
            model,
            last_activity_at,
            transcript_path: transcript,
            messaging_socket: None,
            origin: Some(codex_origin(source.as_deref())),
            tmux: None,
        });
    }
    Ok(sessions)
}

fn codex_origin(source: Option<&str>) -> String {
    let raw = source.unwrap_or("");
    if raw.contains("vscode") || raw.contains("Codex Desktop") {
        "desktop".into()
    } else if raw.contains("cli") || raw.contains("codex-tui") {
        "cli".into()
    } else if raw.contains("subagent") {
        "subagent".into()
    } else {
        "codex".into()
    }
}

fn newest_state_db(codex_home: &Path) -> Option<PathBuf> {
    let mut best: Option<(u64, PathBuf)> = None;
    let Ok(entries) = fs::read_dir(codex_home) else {
        return None;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if let Some(rest) = name.strip_prefix("state_")
            && let Some(num) = rest.strip_suffix(".sqlite")
            && let Ok(index) = num.parse::<u64>()
            && best
                .as_ref()
                .is_none_or(|(best_index, _)| index > *best_index)
        {
            best = Some((index, entry.path()));
        }
    }
    best.map(|(_, path)| path)
}

fn fallback_rollout(codex_home: &Path, session_id: &str) -> Option<PathBuf> {
    let sessions = codex_home.join("sessions");
    if !sessions.is_dir() {
        return None;
    }
    WalkDir::new(sessions)
        .into_iter()
        .flatten()
        .map(|entry| entry.into_path())
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.contains(session_id) && name.ends_with(".jsonl"))
        })
}

fn read_json(path: &Path) -> Result<Value> {
    let raw = fs::read_to_string(path).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(serde_json::from_str(&raw)?)
}

fn millis(value: Option<i64>) -> Option<DateTime<Utc>> {
    let value = value?;
    let millis = if value.abs() < 1_000_000_000_000 {
        value * 1000
    } else {
        value
    };
    Utc.timestamp_millis_opt(millis).single()
}

#[cfg(test)]
mod tests {
    use super::split_agent_ref;
    use crate::model::Agent;

    #[test]
    fn parses_agent_prefixed_refs() {
        let (agent, rest) = split_agent_ref("grok:latest");
        assert_eq!(agent, Some(Agent::Grok));
        assert_eq!(rest, "latest");
        let (agent, rest) = split_agent_ref("disaster recovery");
        assert_eq!(agent, None);
        assert_eq!(rest, "disaster recovery");
    }
}
