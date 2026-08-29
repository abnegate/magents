use crate::error::{Error, Result};
use crate::homes::{Homes, named_process_alive, pid_alive};
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
    if filter.agent.is_none() || filter.agent == Some(Agent::Cursor) {
        sessions.extend(discover_cursor(homes)?);
    }
    if filter.agent.is_none() || filter.agent == Some(Agent::OpenCode) {
        sessions.extend(discover_opencode(homes)?);
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

fn discover_cursor(homes: &Homes) -> Result<Vec<Session>> {
    let headers = cursor_headers(homes);
    let workspaces = cursor_workspaces(homes);
    let live_app = named_process_alive("Cursor");
    let projects = homes.cursor.join("projects");
    if !projects.is_dir() {
        return Ok(Vec::new());
    }
    let mut sessions = Vec::new();
    for project in fs::read_dir(&projects).map_err(|source| Error::Io {
        path: projects.clone(),
        source,
    })? {
        let project = project.map_err(|source| Error::Io {
            path: projects.clone(),
            source,
        })?;
        let transcripts = project.path().join("agent-transcripts");
        if !transcripts.is_dir() {
            continue;
        }
        for entry in fs::read_dir(&transcripts).map_err(|source| Error::Io {
            path: transcripts.clone(),
            source,
        })? {
            let entry = entry.map_err(|source| Error::Io {
                path: transcripts.clone(),
                source,
            })?;
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            let Some(session_id) = dir.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if session_id == "subagents" {
                continue;
            }
            let jsonl = dir.join(format!("{session_id}.jsonl"));
            if !jsonl.is_file() {
                continue;
            }
            let header = headers.get(session_id);
            if header.is_some_and(|header| header.subagent) {
                continue;
            }
            let cwd = header
                .and_then(|header| header.workspace_id.as_deref())
                .and_then(|id| workspaces.get(id).cloned());
            let modified = jsonl
                .metadata()
                .ok()
                .and_then(|meta| meta.modified().ok())
                .map(DateTime::<Utc>::from);
            let last_activity_at = header
                .and_then(|header| header.last_activity_at)
                .or(modified);
            sessions.push(Session {
                agent: Agent::Cursor,
                session_id: session_id.to_string(),
                desktop_id: None,
                name: None,
                title: header
                    .and_then(|header| header.title.clone())
                    .or_else(|| cursor_title_from_transcript(&jsonl)),
                cwd,
                branch: None,
                live: live_app && recently_active(last_activity_at),
                archived: header.is_some_and(|header| header.archived),
                pid: None,
                model: None,
                last_activity_at,
                transcript_path: Some(jsonl),
                messaging_socket: None,
                origin: Some("cursor".into()),
                tmux: None,
            });
        }
    }
    Ok(sessions)
}

struct CursorHeader {
    title: Option<String>,
    archived: bool,
    subagent: bool,
    workspace_id: Option<String>,
    last_activity_at: Option<DateTime<Utc>>,
}

fn cursor_headers(homes: &Homes) -> HashMap<String, CursorHeader> {
    let db = homes
        .cursor_app
        .join("User")
        .join("globalStorage")
        .join("state.vscdb");
    let Ok(connection) =
        Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
    else {
        return HashMap::new();
    };
    let Ok(mut statement) = connection.prepare(
        "SELECT composerId, workspaceId, lastUpdatedAt, isArchived, isSubagent, value
         FROM composerHeaders",
    ) else {
        return HashMap::new();
    };
    let Ok(rows) = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<i64>>(2)?,
            row.get::<_, i64>(3).unwrap_or(0),
            row.get::<_, i64>(4).unwrap_or(0),
            row.get::<_, Option<String>>(5)?,
        ))
    }) else {
        return HashMap::new();
    };
    let mut headers = HashMap::new();
    for row in rows.flatten() {
        let (id, workspace_id, updated, archived, subagent, value) = row;
        let parsed = value
            .as_deref()
            .and_then(|raw| serde_json::from_str::<Value>(raw).ok());
        let title = parsed
            .as_ref()
            .and_then(|value| value.get("name").and_then(Value::as_str))
            .filter(|name| !name.is_empty())
            .map(ToOwned::to_owned);
        let workspace_id = parsed
            .as_ref()
            .and_then(|value| {
                value
                    .pointer("/workspaceIdentifier/id")
                    .and_then(Value::as_str)
            })
            .map(ToOwned::to_owned)
            .or(workspace_id);
        headers.insert(
            id,
            CursorHeader {
                title,
                archived: archived != 0,
                subagent: subagent != 0,
                workspace_id,
                last_activity_at: millis(updated),
            },
        );
    }
    headers
}

fn cursor_workspaces(homes: &Homes) -> HashMap<String, String> {
    let root = homes.cursor_app.join("User").join("workspaceStorage");
    let mut map = HashMap::new();
    let Ok(entries) = fs::read_dir(&root) else {
        return map;
    };
    for entry in entries.flatten() {
        let path = entry.path().join("workspace.json");
        let Ok(value) = read_json(&path) else {
            continue;
        };
        if let Some(cwd) = value
            .get("folder")
            .and_then(Value::as_str)
            .and_then(file_uri_path)
            && let Some(id) = entry.file_name().to_str()
        {
            map.insert(id.to_string(), cwd);
        }
    }
    map
}

fn cursor_title_from_transcript(path: &Path) -> Option<String> {
    let raw = fs::read_to_string(path).ok()?;
    let line = raw.lines().next()?;
    let value: Value = serde_json::from_str(line).ok()?;
    let text = value.pointer("/message/content").and_then(|content| {
        content.as_array().and_then(|items| {
            items
                .iter()
                .find_map(|item| item.get("text").and_then(Value::as_str))
        })
    })?;
    let text = cursor_user_text(text);
    if text.is_empty() {
        None
    } else {
        Some(text.chars().take(80).collect())
    }
}

pub fn cursor_user_text(text: &str) -> String {
    if let Some(start) = text.find("<user_query>") {
        let rest = &text[start + "<user_query>".len()..];
        let body = rest.split("</user_query>").next().unwrap_or(rest);
        return body.split_whitespace().collect::<Vec<_>>().join(" ");
    }
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn discover_opencode(homes: &Homes) -> Result<Vec<Session>> {
    let db = homes.opencode.join("opencode.db");
    if db.is_file() {
        return discover_opencode_sqlite(homes, &db);
    }
    discover_opencode_json(homes)
}

fn discover_opencode_sqlite(homes: &Homes, db: &Path) -> Result<Vec<Session>> {
    let connection = Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut statement = connection.prepare(
        "SELECT id, parent_id, directory, title, time_updated, time_archived
         FROM session",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, Option<i64>>(4)?,
            row.get::<_, Option<i64>>(5)?,
        ))
    })?;
    let live_app = named_process_alive("opencode");
    let mut sessions = Vec::new();
    for row in rows {
        let (id, parent_id, directory, title, updated, archived_at) = row?;
        if parent_id.is_some() {
            continue;
        }
        let last_activity_at = millis(updated);
        sessions.push(Session {
            agent: Agent::OpenCode,
            session_id: id,
            desktop_id: None,
            name: None,
            title: title.filter(|value| !value.is_empty()),
            cwd: directory.filter(|value| !value.is_empty()),
            branch: None,
            live: live_app && recently_active(last_activity_at),
            archived: archived_at.is_some(),
            pid: None,
            model: None,
            last_activity_at,
            transcript_path: Some(homes.opencode.join("opencode.db")),
            messaging_socket: None,
            origin: Some("opencode".into()),
            tmux: None,
        });
    }
    Ok(sessions)
}

fn discover_opencode_json(homes: &Homes) -> Result<Vec<Session>> {
    let root = homes.opencode.join("storage").join("session");
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let live_app = named_process_alive("opencode");
    let mut sessions = Vec::new();
    for entry in WalkDir::new(&root).into_iter().flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Ok(value) = read_json(path) else {
            continue;
        };
        if value
            .get("parentID")
            .or_else(|| value.get("parent_id"))
            .is_some()
        {
            continue;
        }
        let Some(id) = value.get("id").and_then(Value::as_str) else {
            continue;
        };
        let updated = value.pointer("/time/updated").and_then(Value::as_i64);
        let last_activity_at = millis(updated);
        sessions.push(Session {
            agent: Agent::OpenCode,
            session_id: id.to_string(),
            desktop_id: None,
            name: None,
            title: value
                .get("title")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            cwd: value
                .get("directory")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            branch: None,
            live: live_app && recently_active(last_activity_at),
            archived: value.pointer("/time/archived").is_some(),
            pid: None,
            model: None,
            last_activity_at,
            transcript_path: Some(path.to_path_buf()),
            messaging_socket: None,
            origin: Some("opencode".into()),
            tmux: None,
        });
    }
    Ok(sessions)
}

fn recently_active(time: Option<DateTime<Utc>>) -> bool {
    time.map(|time| (Utc::now() - time).num_minutes().abs() < 15)
        .unwrap_or(false)
}

fn file_uri_path(uri: &str) -> Option<String> {
    let rest = uri.strip_prefix("file://")?;
    Some(percent_decode(rest))
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let Ok(value) = u8::from_str_radix(
                std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap_or(""),
                16,
            )
        {
            output.push(value);
            index += 3;
            continue;
        }
        output.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&output).into_owned()
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
        let (agent, rest) = split_agent_ref("cursor:latest");
        assert_eq!(agent, Some(Agent::Cursor));
        assert_eq!(rest, "latest");
        let (agent, rest) = split_agent_ref("opencode:ses_abc");
        assert_eq!(agent, Some(Agent::OpenCode));
        assert_eq!(rest, "ses_abc");
        let (agent, rest) = split_agent_ref("disaster recovery");
        assert_eq!(agent, None);
        assert_eq!(rest, "disaster recovery");
    }

    #[test]
    fn extracts_cursor_user_query() {
        let raw = "<timestamp>now</timestamp>\n<user_query>\nFix the leak\n</user_query>";
        assert_eq!(super::cursor_user_text(raw), "Fix the leak");
    }

    #[test]
    fn decodes_file_uris() {
        assert_eq!(
            super::file_uri_path("file:///Users/jakebarnby/Local/cloud").as_deref(),
            Some("/Users/jakebarnby/Local/cloud")
        );
        assert_eq!(
            super::file_uri_path("file:///tmp/foo%20bar").as_deref(),
            Some("/tmp/foo bar")
        );
    }
}
