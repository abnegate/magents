use crate::error::{Error, Result};
use crate::homes::{Homes, named_process_alive, pid_alive};
use crate::model::{Agent, Caller, Identity, Session};
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
    pub cwd: Option<String>,
    pub branch: Option<String>,
}

impl Default for ListFilter {
    fn default() -> Self {
        Self {
            agent: None,
            query: None,
            live_only: false,
            include_archived: false,
            limit: 20,
            cwd: None,
            branch: None,
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
    if filter.agent.is_none() || filter.agent == Some(Agent::Gemini) {
        sessions.extend(discover_gemini(homes)?);
    }
    if filter.agent.is_none() || filter.agent == Some(Agent::Copilot) {
        sessions.extend(discover_copilot(homes)?);
    }
    merge_spawned(homes, &mut sessions, filter.agent)?;
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
        if let Some(cwd) = &filter.cwd
            && !cwd_matches(session.cwd.as_deref(), cwd)
        {
            return false;
        }
        if let Some(branch) = &filter.branch
            && session.branch.as_deref() != Some(branch.as_str())
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

fn merge_spawned(homes: &Homes, sessions: &mut Vec<Session>, agent: Option<Agent>) -> Result<()> {
    let mut spawned: HashMap<(Agent, String), Session> = crate::spawn::sessions(homes)?
        .into_iter()
        .filter(|session| agent.is_none() || agent == Some(session.agent))
        .map(|session| ((session.agent, session.session_id.clone()), session))
        .collect();

    for session in sessions.iter_mut() {
        let key = (session.agent, session.session_id.clone());
        let Some(registry) = spawned.remove(&key) else {
            continue;
        };
        if session.cwd.is_none() {
            session.cwd = registry.cwd;
        }
    }
    sessions.extend(spawned.into_values());
    Ok(())
}

pub fn resolve(homes: &Homes, reference: &str) -> Result<Session> {
    let reference = reference.trim();
    if reference.is_empty() {
        return Err(Error::msg("session reference is required"));
    }
    let (agent, rest) = split_agent_ref(reference);
    let filter = ListFilter {
        agent,
        include_archived: true,
        limit: 0,
        ..ListFilter::default()
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

pub fn identify(homes: &Homes) -> Identity {
    let caller = Caller::from_env();
    let env_cwd = env_project_dir();

    if let (Some(agent), Some(session_id)) = (caller.agent, caller.session_id.as_deref())
        && !session_id.is_empty()
        && let Ok(session) = resolve(homes, &format!("{agent}:{session_id}"))
    {
        return identity_from_session(session);
    }

    let live = list_sessions(
        homes,
        &ListFilter {
            agent: caller.agent,
            live_only: true,
            include_archived: false,
            limit: 0,
            ..ListFilter::default()
        },
    )
    .unwrap_or_default();

    if let Ok(socket) = std::env::var("CLAUDE_CODE_MESSAGING_SOCKET") {
        let path = PathBuf::from(&socket);
        let matches: Vec<Session> = live
            .iter()
            .filter(|session| session.messaging_socket.as_deref() == Some(path.as_path()))
            .cloned()
            .collect();
        if matches.len() == 1 {
            return identity_from_session(matches.into_iter().next().unwrap());
        }
    }

    if let (Some(agent), Some(cwd)) = (caller.agent, env_cwd.as_deref()) {
        let matches: Vec<Session> = live
            .iter()
            .filter(|session| session.agent == agent && cwd_matches(session.cwd.as_deref(), cwd))
            .cloned()
            .collect();
        if matches.len() == 1 {
            return identity_from_session(matches.into_iter().next().unwrap());
        }
    }

    if let Some(agent) = caller.agent {
        let matches: Vec<Session> = live
            .into_iter()
            .filter(|session| session.agent == agent)
            .collect();
        if matches.len() == 1 {
            return identity_from_session(matches.into_iter().next().unwrap());
        }
    }

    Identity {
        agent: caller.agent,
        session_id: caller.session_id,
        cwd: env_cwd,
        branch: None,
        session: None,
    }
}

fn identity_from_session(session: Session) -> Identity {
    Identity {
        agent: Some(session.agent),
        session_id: Some(session.session_id.clone()),
        cwd: session.cwd.clone(),
        branch: session.branch.clone(),
        session: Some(session),
    }
}

fn env_project_dir() -> Option<String> {
    for key in [
        "CLAUDE_PROJECT_DIR",
        "CURSOR_PROJECT_DIR",
        "OPENCODE_DIRECTORY",
    ] {
        if let Ok(value) = std::env::var(key)
            && !value.trim().is_empty()
        {
            return Some(value);
        }
    }
    std::env::current_dir()
        .ok()
        .map(|path| path.to_string_lossy().into_owned())
}

pub(crate) fn cwd_matches(session_cwd: Option<&str>, wanted: &str) -> bool {
    let Some(session_cwd) = session_cwd else {
        return false;
    };
    let wanted = normalize_cwd(wanted);
    let session = normalize_cwd(session_cwd);
    session == wanted
        || session.starts_with(&format!("{wanted}/"))
        || wanted.starts_with(&format!("{session}/"))
}

fn normalize_cwd(value: &str) -> String {
    let trimmed = value.trim().trim_end_matches('/');
    std::fs::canonicalize(trimmed)
        .unwrap_or_else(|_| PathBuf::from(trimmed))
        .to_string_lossy()
        .into_owned()
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

fn discover_gemini(homes: &Homes) -> Result<Vec<Session>> {
    let root = homes.gemini.join("tmp");
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let live_app = named_process_alive("gemini");
    let mut by_id = HashMap::new();
    for entry in WalkDir::new(&root).into_iter().flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy();
        if name == "logs.json" {
            continue;
        }
        let ext = path.extension().and_then(|ext| ext.to_str());
        if ext != Some("json") && ext != Some("jsonl") {
            continue;
        }
        if !path
            .components()
            .any(|component| component.as_os_str() == "chats")
        {
            continue;
        }
        let Some(parsed) = parse_gemini_session(path) else {
            continue;
        };
        if !crate::model::valid_session_id(Agent::Gemini, &parsed.id) {
            continue;
        }
        let session = Session {
            agent: Agent::Gemini,
            session_id: parsed.id.clone(),
            desktop_id: None,
            name: None,
            title: parsed.title,
            cwd: parsed.cwd,
            branch: None,
            live: live_app && recently_active(parsed.last_activity_at),
            archived: false,
            pid: None,
            model: parsed.model,
            last_activity_at: parsed.last_activity_at,
            transcript_path: Some(path.to_path_buf()),
            messaging_socket: None,
            origin: Some("gemini".into()),
            tmux: None,
        };
        by_id
            .entry(parsed.id)
            .and_modify(|existing: &mut Session| {
                if session.activity_ms() >= existing.activity_ms() {
                    *existing = session.clone();
                }
            })
            .or_insert(session);
    }
    Ok(by_id.into_values().collect())
}

struct GeminiParsed {
    id: String,
    title: Option<String>,
    cwd: Option<String>,
    model: Option<String>,
    last_activity_at: Option<DateTime<Utc>>,
}

fn parse_gemini_session(path: &Path) -> Option<GeminiParsed> {
    let raw = fs::read_to_string(path).ok()?;
    if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") {
        return parse_gemini_jsonl(&raw);
    }
    if let Ok(value) = serde_json::from_str::<Value>(&raw) {
        return parse_gemini_object(&value);
    }
    parse_gemini_jsonl(&raw)
}

fn parse_gemini_object(value: &Value) -> Option<GeminiParsed> {
    let id = value
        .get("sessionId")
        .or_else(|| value.get("session_id"))
        .or_else(|| value.get("id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())?
        .to_string();
    let title = gemini_title_from_messages(value.get("messages")).or_else(|| {
        value
            .get("title")
            .and_then(Value::as_str)
            .filter(|title| !title.is_empty())
            .map(ToOwned::to_owned)
    });
    Some(GeminiParsed {
        id,
        title,
        cwd: value
            .get("cwd")
            .or_else(|| value.get("projectPath"))
            .and_then(Value::as_str)
            .filter(|cwd| !cwd.is_empty())
            .map(ToOwned::to_owned),
        model: value
            .get("model")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        last_activity_at: parse_gemini_time(
            value
                .get("lastUpdated")
                .or_else(|| value.get("lastUpdatedAt"))
                .or_else(|| value.get("startTime")),
        ),
    })
}

fn parse_gemini_jsonl(raw: &str) -> Option<GeminiParsed> {
    let mut id = None;
    let mut cwd = None;
    let mut model = None;
    let mut last_activity_at = None;
    let mut title = None;
    for line in raw.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if id.is_none() {
            id = value
                .get("sessionId")
                .or_else(|| value.get("session_id"))
                .or_else(|| value.get("id"))
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned);
        }
        if cwd.is_none() {
            cwd = value
                .get("cwd")
                .or_else(|| value.get("projectPath"))
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned);
        }
        if model.is_none() {
            model = value
                .get("model")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
        }
        last_activity_at = parse_gemini_time(
            value
                .get("lastUpdated")
                .or_else(|| value.get("timestamp"))
                .or_else(|| value.get("startTime")),
        )
        .or(last_activity_at);
        if title.is_none() {
            title = gemini_title_from_messages(value.get("messages"))
                .or_else(|| gemini_message_title(&value));
        }
    }
    Some(GeminiParsed {
        id: id?,
        title,
        cwd,
        model,
        last_activity_at,
    })
}

fn gemini_title_from_messages(messages: Option<&Value>) -> Option<String> {
    let items = messages.and_then(Value::as_array)?;
    items.iter().find_map(gemini_message_title)
}

fn gemini_message_title(value: &Value) -> Option<String> {
    let kind = value
        .get("type")
        .or_else(|| value.get("role"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if kind != "user" {
        return None;
    }
    let text = gemini_message_text(value)?;
    Some(text.chars().take(80).collect())
}

fn gemini_message_text(value: &Value) -> Option<String> {
    if let Some(text) = value.get("content").and_then(Value::as_str) {
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        return (!text.is_empty()).then_some(text);
    }
    if let Some(text) = value.get("message").and_then(Value::as_str) {
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        return (!text.is_empty()).then_some(text);
    }
    if let Some(text) = value
        .get("content")
        .and_then(|content| content.get("text"))
        .and_then(Value::as_str)
    {
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        return (!text.is_empty()).then_some(text);
    }
    None
}

fn parse_gemini_time(value: Option<&Value>) -> Option<DateTime<Utc>> {
    let value = value?;
    if let Some(millis_value) = value.as_i64() {
        return millis(Some(millis_value));
    }
    DateTime::parse_from_rfc3339(value.as_str()?)
        .ok()
        .map(|time| time.with_timezone(&Utc))
}

fn discover_copilot(homes: &Homes) -> Result<Vec<Session>> {
    let root = homes.copilot.join("session-state");
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let store = copilot_store(&homes.copilot);
    let live_app = named_process_alive("copilot");
    let mut sessions = Vec::new();
    let entries = fs::read_dir(&root).map_err(|source| Error::Io {
        path: root.clone(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| Error::Io {
            path: root.clone(),
            source,
        })?;
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let Some(session_id) = dir.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !crate::model::valid_session_id(Agent::Copilot, session_id) {
            continue;
        }
        let events = dir.join("events.jsonl");
        if !events.is_file() {
            continue;
        }
        let workspace = read_simple_yaml(&dir.join("workspace.yaml"));
        if workspace
            .get("client_name")
            .is_some_and(|name| name.contains("autopilot"))
        {
            continue;
        }
        let meta = store.get(session_id);
        let cwd = workspace
            .get("cwd")
            .cloned()
            .or_else(|| meta.and_then(|row| row.cwd.clone()));
        let title = workspace
            .get("name")
            .filter(|name| !name.is_empty())
            .cloned()
            .or_else(|| meta.and_then(|row| row.summary.clone()))
            .or_else(|| copilot_title_from_events(&events));
        let last_activity_at = workspace
            .get("updated_at")
            .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
            .map(|time| time.with_timezone(&Utc))
            .or_else(|| meta.and_then(|row| row.updated_at))
            .or_else(|| {
                events
                    .metadata()
                    .ok()
                    .and_then(|meta| meta.modified().ok())
                    .map(DateTime::<Utc>::from)
            });
        sessions.push(Session {
            agent: Agent::Copilot,
            session_id: session_id.to_string(),
            desktop_id: None,
            name: None,
            title,
            cwd,
            branch: meta.and_then(|row| row.branch.clone()),
            live: live_app && recently_active(last_activity_at),
            archived: false,
            pid: None,
            model: None,
            last_activity_at,
            transcript_path: Some(events),
            messaging_socket: None,
            origin: Some("copilot".into()),
            tmux: None,
        });
    }
    Ok(sessions)
}

struct CopilotStoreRow {
    cwd: Option<String>,
    branch: Option<String>,
    summary: Option<String>,
    updated_at: Option<DateTime<Utc>>,
}

fn copilot_store(home: &Path) -> HashMap<String, CopilotStoreRow> {
    let db = home.join("session-store.db");
    let Ok(connection) =
        Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
    else {
        return HashMap::new();
    };
    let Ok(mut statement) =
        connection.prepare("SELECT id, cwd, branch, summary, updated_at FROM sessions")
    else {
        return HashMap::new();
    };
    let Ok(rows) = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, Option<String>>(4)?,
        ))
    }) else {
        return HashMap::new();
    };
    let mut map = HashMap::new();
    for row in rows.flatten() {
        let (id, cwd, branch, summary, updated) = row;
        map.insert(
            id,
            CopilotStoreRow {
                cwd: cwd.filter(|value| !value.is_empty()),
                branch: branch.filter(|value| !value.is_empty()),
                summary: summary.filter(|value| !value.is_empty()),
                updated_at: updated.and_then(|value| {
                    DateTime::parse_from_rfc3339(&value)
                        .ok()
                        .map(|time| time.with_timezone(&Utc))
                }),
            },
        );
    }
    map
}

fn read_simple_yaml(path: &Path) -> HashMap<String, String> {
    let Ok(raw) = fs::read_to_string(path) else {
        return HashMap::new();
    };
    let mut map = HashMap::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        map.insert(key.trim().to_string(), value.trim().to_string());
    }
    map
}

fn copilot_title_from_events(path: &Path) -> Option<String> {
    let raw = fs::read_to_string(path).ok()?;
    for line in raw.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if value.get("type").and_then(Value::as_str) != Some("user.message") {
            continue;
        }
        let text = value
            .pointer("/data/content")
            .and_then(Value::as_str)
            .unwrap_or("")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if !text.is_empty() {
            return Some(text.chars().take(80).collect());
        }
    }
    None
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
    use super::{ListFilter, cwd_matches, list_sessions, resolve, split_agent_ref};
    use crate::homes::Homes;
    use crate::model::Agent;
    use crate::spawn::Transport;
    use crate::test_env;
    use serde_json::json;
    use std::fs;

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
        let (agent, rest) = split_agent_ref("gemini:latest");
        assert_eq!(agent, Some(Agent::Gemini));
        assert_eq!(rest, "latest");
        let (agent, rest) = split_agent_ref("copilot:abc");
        assert_eq!(agent, Some(Agent::Copilot));
        assert_eq!(rest, "abc");
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
    fn recently_active_window() {
        use chrono::{Duration, Utc};
        assert!(!super::recently_active(None));
        assert!(super::recently_active(Some(Utc::now())));
        assert!(!super::recently_active(Some(
            Utc::now() - Duration::minutes(30)
        )));
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

    #[test]
    fn registry_only_session_is_immediately_addressable() {
        let directory = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(directory.path());
        let session = crate::spawn::record(
            &homes,
            Agent::Codex,
            "spawned-codex",
            directory.path(),
            Transport::CodexExec,
        )
        .unwrap();

        let resolved = resolve(&homes, "codex:spawned-codex").unwrap();
        assert_eq!(resolved.session_id, session.session_id);
        assert_eq!(resolved.cwd, session.cwd);
        assert!(!resolved.live);
        assert_eq!(resolved.origin.as_deref(), Some("codex-exec"));

        let live = list_sessions(
            &homes,
            &ListFilter {
                agent: Some(Agent::Codex),
                live_only: true,
                include_archived: true,
                limit: 0,
                query: None,
                ..ListFilter::default()
            },
        )
        .unwrap();
        assert!(live.is_empty());
    }

    #[test]
    fn legacy_opencode_data_override_drives_discovery() {
        const KEYS: &[&str] = &["HOME", "MAGENTS_HOME", "OPENCODE_DATA", "XDG_DATA_HOME"];
        let _guard = test_env::lock(KEYS);
        let directory = tempfile::tempdir().unwrap();
        let legacy = directory.path().join("legacy").join("opencode");
        let session = legacy
            .join("storage")
            .join("session")
            .join("project")
            .join("ses_legacy.json");
        fs::create_dir_all(session.parent().unwrap()).unwrap();
        fs::write(
            &session,
            json!({
                "id": "ses_legacy",
                "directory": directory.path(),
                "title": "legacy override"
            })
            .to_string(),
        )
        .unwrap();
        unsafe {
            std::env::set_var("HOME", directory.path());
            std::env::set_var("MAGENTS_HOME", directory.path().join("magents"));
            std::env::set_var("OPENCODE_DATA", &legacy);
            std::env::set_var("XDG_DATA_HOME", directory.path().join("xdg"));
        }

        let homes = Homes::from_env();
        let sessions = list_sessions(
            &homes,
            &ListFilter {
                agent: Some(Agent::OpenCode),
                include_archived: true,
                limit: 0,
                ..ListFilter::default()
            },
        )
        .unwrap();

        assert_eq!(homes.opencode, legacy);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, "ses_legacy");
        assert_eq!(
            sessions[0].transcript_path.as_deref(),
            Some(session.as_path())
        );
    }

    #[test]
    fn native_cursor_data_wins_and_registry_only_fills_missing_cwd() {
        let directory = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(directory.path());
        assert_ne!(homes.cursor_config, homes.cursor);
        assert_ne!(homes.cursor_app, homes.cursor);
        let session_id = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
        let transcript = homes
            .cursor
            .join("projects")
            .join("workspace")
            .join("agent-transcripts")
            .join(session_id)
            .join(format!("{session_id}.jsonl"));
        fs::create_dir_all(transcript.parent().unwrap()).unwrap();
        fs::write(
            &transcript,
            json!({
                "role": "user",
                "message": {"content": [{"type": "text", "text": "native title"}]}
            })
            .to_string(),
        )
        .unwrap();
        for (root, ignored_id) in [
            (&homes.cursor_config, "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"),
            (&homes.cursor_app, "cccccccc-cccc-4ccc-8ccc-cccccccccccc"),
        ] {
            let ignored = root
                .join("projects")
                .join("workspace")
                .join("agent-transcripts")
                .join(ignored_id)
                .join(format!("{ignored_id}.jsonl"));
            fs::create_dir_all(ignored.parent().unwrap()).unwrap();
            fs::write(
                ignored,
                json!({
                    "role": "user",
                    "message": {"content": [{"type": "text", "text": "ignored title"}]}
                })
                .to_string(),
            )
            .unwrap();
        }
        crate::spawn::record(
            &homes,
            Agent::Cursor,
            session_id,
            directory.path(),
            Transport::CursorAgent,
        )
        .unwrap();

        let sessions = list_sessions(
            &homes,
            &ListFilter {
                agent: Some(Agent::Cursor),
                include_archived: true,
                limit: 0,
                ..ListFilter::default()
            },
        )
        .unwrap();
        assert_eq!(sessions.len(), 1);
        let session = &sessions[0];
        assert_eq!(session.title.as_deref(), Some("native title"));
        assert_eq!(session.cwd.as_deref(), directory.path().to_str());
        assert_eq!(session.origin.as_deref(), Some("cursor"));
        assert_eq!(
            session.transcript_path.as_deref(),
            Some(transcript.as_path())
        );
    }

    #[test]
    fn cwd_matches_canonical_and_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("repo").join("crate");
        fs::create_dir_all(&nested).unwrap();
        let root = dir.path().join("repo");
        assert!(cwd_matches(
            Some(nested.to_str().unwrap()),
            root.to_str().unwrap()
        ));
        assert!(cwd_matches(
            Some(root.to_str().unwrap()),
            nested.to_str().unwrap()
        ));
        assert!(!cwd_matches(None, root.to_str().unwrap()));
        assert!(!cwd_matches(Some("/no/such/cwd"), "/also/missing"));
    }

    #[test]
    fn identify_socket_unique_live_and_discovery_errors() {
        use super::identify;
        use crate::handoff_tests::World;
        use std::os::unix::fs::PermissionsExt;
        use std::process::{Command, Stdio};

        const KEYS: &[&str] = &[
            "GROK_SESSION_ID",
            "CLAUDE_CODE_MESSAGING_SOCKET",
            "CLAUDE_PROJECT_DIR",
            "CLAUDE_SESSION_ID",
            "CURSOR_SESSION_ID",
            "CURSOR_PROJECT_DIR",
            "CURSOR_AGENT",
            "COMPOSER_SESSION_ID",
            "OPENCODE_SESSION_ID",
            "OPENCODE_DIRECTORY",
            "OPENCODE_SERVER",
            "OPENCODE_SESSION",
            "CODEX_HOME",
            "CODEX_THREAD_ID",
            "CODEX_SESSION_ID",
        ];
        let _guard = test_env::lock(KEYS);
        for key in KEYS {
            unsafe { std::env::remove_var(key) };
        }

        let world = World::new();
        let pid = std::process::id();
        let socket = world.homes.claude.join("identify.sock");
        fs::write(
            world
                .homes
                .claude
                .join("sessions")
                .join(format!("{pid}.json")),
            json!({
                "pid": pid,
                "sessionId": "11111111-1111-4111-8111-111111111111",
                "cwd": "/tmp/dr",
                "messagingSocketPath": socket,
            })
            .to_string(),
        )
        .unwrap();
        unsafe {
            std::env::set_var("CLAUDE_CODE_MESSAGING_SOCKET", &socket);
        }
        let who = identify(&world.homes);
        assert_eq!(
            who.session_id.as_deref(),
            Some("11111111-1111-4111-8111-111111111111")
        );

        unsafe {
            std::env::remove_var("CLAUDE_CODE_MESSAGING_SOCKET");
            std::env::set_var("CLAUDE_PROJECT_DIR", "/tmp/unrelated-identify-cwd");
        }
        let who = identify(&world.homes);
        assert_eq!(
            who.session_id.as_deref(),
            Some("11111111-1111-4111-8111-111111111111")
        );
        unsafe { std::env::remove_var("CLAUDE_PROJECT_DIR") };

        fs::write(
            world.homes.claude.join("sessions").join("000001.json"),
            json!({ "pid": pid, "cwd": "/tmp/no-id" }).to_string(),
        )
        .unwrap();

        let mut child = Command::new("sleep")
            .arg("8")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let extra = "33333333-3333-4333-8333-333333333333";
        fs::write(
            world
                .homes
                .claude
                .join("sessions")
                .join(format!("{}.json", child.id())),
            json!({ "pid": child.id(), "sessionId": extra }).to_string(),
        )
        .unwrap();
        fs::write(
            world.homes.claude_desktop.join("local_merge.json"),
            json!({
                "sessionId": "desktop-merge",
                "cliSessionId": extra,
                "cwd": "/tmp/merged",
                "lastActivityAt": chrono::Utc::now().timestamp_millis()
            })
            .to_string(),
        )
        .unwrap();
        let sessions = list_sessions(
            &world.homes,
            &ListFilter {
                agent: Some(Agent::Claude),
                include_archived: true,
                limit: 0,
                ..ListFilter::default()
            },
        )
        .unwrap();
        assert!(
            sessions.iter().any(|session| session.session_id == extra
                && session.cwd.as_deref() == Some("/tmp/merged"))
        );
        let _ = child.kill();
        let _ = child.wait();

        let storage = world
            .homes
            .opencode
            .join("storage")
            .join("session")
            .join("proj");
        fs::create_dir_all(&storage).unwrap();
        fs::write(storage.join("bad.json"), "not-json").unwrap();
        fs::write(storage.join("noid.json"), "{}").unwrap();
        let _ = list_sessions(
            &world.homes,
            &ListFilter {
                agent: Some(Agent::OpenCode),
                include_archived: true,
                limit: 0,
                ..ListFilter::default()
            },
        );

        let sessions_dir = world.homes.claude.join("sessions");
        let mut permissions = fs::metadata(&sessions_dir).unwrap().permissions();
        permissions.set_mode(0o000);
        fs::set_permissions(&sessions_dir, permissions).unwrap();
        assert!(list_sessions(&world.homes, &ListFilter::default()).is_err());
        let mut permissions = fs::metadata(&sessions_dir).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&sessions_dir, permissions).unwrap();

        let projects = world.homes.cursor.join("projects");
        if let Some(transcripts) = fs::read_dir(&projects).ok().and_then(|entries| {
            entries.flatten().find_map(|entry| {
                let path = entry.path().join("agent-transcripts");
                path.is_dir().then_some(path)
            })
        }) {
            let mut permissions = fs::metadata(&transcripts).unwrap().permissions();
            permissions.set_mode(0o000);
            fs::set_permissions(&transcripts, permissions).unwrap();
            assert!(
                list_sessions(
                    &world.homes,
                    &ListFilter {
                        agent: Some(Agent::Cursor),
                        include_archived: true,
                        limit: 0,
                        ..ListFilter::default()
                    }
                )
                .is_err()
            );
            let mut permissions = fs::metadata(&transcripts).unwrap().permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&transcripts, permissions).unwrap();
        }

        let isolated = Homes::isolated(tempfile::tempdir().unwrap().path());
        fs::create_dir_all(isolated.cursor.join("projects")).unwrap();
        let mut permissions = fs::metadata(isolated.cursor.join("projects"))
            .unwrap()
            .permissions();
        permissions.set_mode(0o000);
        fs::set_permissions(isolated.cursor.join("projects"), permissions).unwrap();
        assert!(
            list_sessions(
                &isolated,
                &ListFilter {
                    agent: Some(Agent::Cursor),
                    include_archived: true,
                    limit: 0,
                    ..ListFilter::default()
                }
            )
            .is_err()
        );
        let mut permissions = fs::metadata(isolated.cursor.join("projects"))
            .unwrap()
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(isolated.cursor.join("projects"), permissions).unwrap();

        let transcripts = isolated
            .cursor
            .join("projects")
            .join("ws")
            .join("agent-transcripts");
        fs::create_dir_all(&transcripts).unwrap();
        fs::write(transcripts.join("not-a-session.txt"), "skip").unwrap();
        fs::create_dir_all(transcripts.join("missing-jsonl")).unwrap();
        let _ = list_sessions(
            &isolated,
            &ListFilter {
                agent: Some(Agent::Cursor),
                include_archived: true,
                limit: 0,
                ..ListFilter::default()
            },
        );

        let json_home = Homes::isolated(tempfile::tempdir().unwrap().path());
        let storage = json_home
            .opencode
            .join("storage")
            .join("session")
            .join("proj");
        fs::create_dir_all(&storage).unwrap();
        fs::write(storage.join("bad.json"), "not-json").unwrap();
        fs::write(storage.join("noid.json"), "{}").unwrap();
        let _ = list_sessions(
            &json_home,
            &ListFilter {
                agent: Some(Agent::OpenCode),
                include_archived: true,
                limit: 0,
                ..ListFilter::default()
            },
        );

        let grok = Homes::isolated(tempfile::tempdir().unwrap().path());
        let summary = grok.grok.join("sessions").join("s1").join("summary.json");
        fs::create_dir_all(summary.parent().unwrap()).unwrap();
        fs::write(&summary, "not-json").unwrap();
        let mut permissions = fs::metadata(&summary).unwrap().permissions();
        permissions.set_mode(0o000);
        fs::set_permissions(&summary, permissions).unwrap();
        let _ = list_sessions(
            &grok,
            &ListFilter {
                agent: Some(Agent::Grok),
                include_archived: true,
                limit: 0,
                ..ListFilter::default()
            },
        );
        let mut permissions = fs::metadata(&summary).unwrap().permissions();
        permissions.set_mode(0o644);
        fs::set_permissions(&summary, permissions).unwrap();

        let empty_db = Homes::isolated(tempfile::tempdir().unwrap().path());
        fs::create_dir_all(&empty_db.opencode).unwrap();
        rusqlite::Connection::open(empty_db.opencode.join("opencode.db")).unwrap();
        assert!(
            list_sessions(
                &empty_db,
                &ListFilter {
                    agent: Some(Agent::OpenCode),
                    include_archived: true,
                    limit: 0,
                    ..ListFilter::default()
                }
            )
            .is_err()
        );

        use std::os::unix::ffi::OsStrExt;
        let weird = isolated
            .cursor
            .join("projects")
            .join("ws")
            .join("agent-transcripts")
            .join(std::ffi::OsStr::from_bytes(&[0xff, 0xfe]));
        let _ = fs::create_dir(&weird);
        let sessions_dir = isolated.claude.join("sessions");
        fs::create_dir_all(&sessions_dir).unwrap();
        let _ = fs::write(
            sessions_dir.join(std::ffi::OsStr::from_bytes(&[0xff, 0xfe])),
            "{}",
        );
        let _ = list_sessions(
            &isolated,
            &ListFilter {
                agent: Some(Agent::Cursor),
                include_archived: true,
                limit: 0,
                ..ListFilter::default()
            },
        );
        let _ = list_sessions(
            &isolated,
            &ListFilter {
                agent: Some(Agent::Claude),
                include_archived: true,
                limit: 0,
                ..ListFilter::default()
            },
        );

        let workspace = isolated
            .cursor_app
            .join("User")
            .join("workspaceStorage")
            .join("ws1");
        fs::create_dir_all(&workspace).unwrap();
        fs::write(workspace.join("workspace.json"), "not-json").unwrap();
        let _ = list_sessions(
            &isolated,
            &ListFilter {
                agent: Some(Agent::Cursor),
                include_archived: true,
                limit: 0,
                ..ListFilter::default()
            },
        );

        fs::create_dir_all(&world.homes.claude_desktop).unwrap();
        let desktop = world.homes.claude_desktop.join("local_locked.json");
        fs::write(&desktop, r#"{"sessionId":"locked"}"#).unwrap();
        let mut permissions = fs::metadata(&desktop).unwrap().permissions();
        permissions.set_mode(0o000);
        fs::set_permissions(&desktop, permissions).unwrap();
        let _ = list_sessions(
            &world.homes,
            &ListFilter {
                agent: Some(Agent::Claude),
                include_archived: true,
                limit: 0,
                ..ListFilter::default()
            },
        );
        let mut permissions = fs::metadata(&desktop).unwrap().permissions();
        permissions.set_mode(0o644);
        fs::set_permissions(&desktop, permissions).unwrap();
    }

    #[test]
    fn discovers_gemini_json_and_jsonl_and_skips_logs() {
        let directory = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(directory.path());
        let chats = homes.gemini.join("tmp").join("abc123hash").join("chats");
        fs::create_dir_all(&chats).unwrap();
        fs::write(
            chats.join("session-2026-01-12T02-48-bddd4c16.json"),
            json!({
                "sessionId": "bddd4c16-a97d-4639-b5cc-e276075dda32",
                "projectHash": "abc123hash",
                "startTime": "2026-01-12T02:49:11.948Z",
                "lastUpdated": "2026-01-12T02:52:11.722Z",
                "messages": [
                    {"type": "info", "content": "login"},
                    {"type": "user", "content": "run the gemini matrix"}
                ]
            })
            .to_string(),
        )
        .unwrap();
        fs::write(
            chats.join("session-jsonl.jsonl"),
            r#"{"sessionId":"55555555-5555-4555-8555-555555555555","cwd":"/tmp/gemini","model":"gemini-2.5-pro"}
{"type":"user","content":"jsonl user turn"}
{"type":"gemini","content":"jsonl assistant"}
"#,
        )
        .unwrap();
        fs::write(
            homes
                .gemini
                .join("tmp")
                .join("abc123hash")
                .join("logs.json"),
            json!([{"sessionId": "should-not-list", "type": "user", "message": "nope"}])
                .to_string(),
        )
        .unwrap();
        fs::write(chats.join("not-json.txt"), "ignore").unwrap();
        fs::write(chats.join("broken.json"), "not-json").unwrap();
        let checkpoints = homes
            .gemini
            .join("tmp")
            .join("abc123hash")
            .join("checkpoints");
        fs::create_dir_all(&checkpoints).unwrap();
        fs::write(
            checkpoints.join("outside-chats.json"),
            json!({
                "sessionId": "cccccccc-cccc-4ccc-8ccc-cccccccccccc",
                "messages": [{"type": "user", "content": "not under chats"}]
            })
            .to_string(),
        )
        .unwrap();
        fs::write(
            chats.join("invalid-id.json"),
            json!({
                "sessionId": "-leading-dash",
                "messages": [{"type": "user", "content": "bad id"}]
            })
            .to_string(),
        )
        .unwrap();
        fs::write(
            chats.join("title-only.json"),
            json!({
                "id": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
                "title": "from title field",
                "projectPath": "/tmp/from-project",
                "lastUpdated": 1_700_000_000_000i64
            })
            .to_string(),
        )
        .unwrap();
        fs::write(
            chats.join("older-dup.json"),
            json!({
                "sessionId": "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
                "lastUpdated": "2026-01-01T00:00:00Z",
                "messages": [{"type": "user", "content": "older dup"}]
            })
            .to_string(),
        )
        .unwrap();
        fs::write(
            chats.join("newer-dup.json"),
            json!({
                "sessionId": "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
                "lastUpdated": "2026-03-01T00:00:00Z",
                "messages": [{"type": "user", "content": "newer dup"}]
            })
            .to_string(),
        )
        .unwrap();
        fs::write(
            chats.join("nested-and-empty.jsonl"),
            r#"{"session_id":"dddddddd-dddd-4ddd-8ddd-dddddddddddd"}
{"type":"user","content":"   "}
{"type":"user","content":{"text":"from nested text"}}
not-json
"#,
        )
        .unwrap();
        fs::write(
            chats.join("message-key.jsonl"),
            r#"{"id":"ffffffff-ffff-4fff-8fff-ffffffffffff"}
{"role":"user","message":"from message key"}
{"type":"user","content":{"text":"   "}}
{"type":"user"}
{"type":"user","content":{"other":true}}
"#,
        )
        .unwrap();
        fs::write(
            chats.join("object-as-jsonl.json"),
            r#"{"sessionId":"eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee"}
{"type":"user","content":"json object file is jsonl"}
"#,
        )
        .unwrap();

        let sessions = list_sessions(
            &homes,
            &ListFilter {
                agent: Some(Agent::Gemini),
                include_archived: true,
                limit: 0,
                ..ListFilter::default()
            },
        )
        .unwrap();
        assert_eq!(sessions.len(), 7);
        let json = sessions
            .iter()
            .find(|session| session.session_id == "bddd4c16-a97d-4639-b5cc-e276075dda32")
            .unwrap();
        assert_eq!(json.title.as_deref(), Some("run the gemini matrix"));
        assert_eq!(json.origin.as_deref(), Some("gemini"));
        assert!(!json.live);
        let jsonl = sessions
            .iter()
            .find(|session| session.session_id == "55555555-5555-4555-8555-555555555555")
            .unwrap();
        assert_eq!(jsonl.cwd.as_deref(), Some("/tmp/gemini"));
        assert_eq!(jsonl.model.as_deref(), Some("gemini-2.5-pro"));
        assert_eq!(jsonl.title.as_deref(), Some("jsonl user turn"));
        let titled = sessions
            .iter()
            .find(|session| session.session_id == "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa")
            .unwrap();
        assert_eq!(titled.title.as_deref(), Some("from title field"));
        assert_eq!(titled.cwd.as_deref(), Some("/tmp/from-project"));
        assert!(titled.last_activity_at.is_some());
        let dup = sessions
            .iter()
            .find(|session| session.session_id == "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb")
            .unwrap();
        assert_eq!(dup.title.as_deref(), Some("newer dup"));
        let nested = sessions
            .iter()
            .find(|session| session.session_id == "dddddddd-dddd-4ddd-8ddd-dddddddddddd")
            .unwrap();
        assert_eq!(nested.title.as_deref(), Some("from nested text"));
        let message_key = sessions
            .iter()
            .find(|session| session.session_id == "ffffffff-ffff-4fff-8fff-ffffffffffff")
            .unwrap();
        assert_eq!(message_key.title.as_deref(), Some("from message key"));
        let recovered = sessions
            .iter()
            .find(|session| session.session_id == "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee")
            .unwrap();
        assert_eq!(
            recovered.title.as_deref(),
            Some("json object file is jsonl")
        );
        assert!(
            !sessions
                .iter()
                .any(|session| session.session_id.contains("leading")
                    || session.session_id.starts_with("ccc"))
        );
    }

    #[test]
    fn discovers_copilot_cli_and_skips_autopilot() {
        let directory = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(directory.path());
        let cli_id = "7167c730-5662-49bd-9d80-8a206275968e";
        let cloud_id = "066af29d-a95a-4389-9220-87ba5438c2bf";
        let cli = homes.copilot.join("session-state").join(cli_id);
        fs::create_dir_all(&cli).unwrap();
        fs::write(
            cli.join("workspace.yaml"),
            "id: 7167c730-5662-49bd-9d80-8a206275968e\ncwd: /tmp/copilot-cli\nclient_name: github/cli\nname: Return JSON {}\nupdated_at: 2026-08-29T07:51:40.622Z\n",
        )
        .unwrap();
        fs::write(
            cli.join("events.jsonl"),
            r#"{"type":"session.start","data":{"sessionId":"7167c730-5662-49bd-9d80-8a206275968e","context":{"cwd":"/tmp/copilot-cli"}}}
{"type":"user.message","data":{"content":"Return JSON {}"}}
{"type":"assistant.message","data":{"content":"{}","toolRequests":[{"name":"view"}]}}
"#,
        )
        .unwrap();
        let cloud = homes.copilot.join("session-state").join(cloud_id);
        fs::create_dir_all(&cloud).unwrap();
        fs::write(
            cloud.join("workspace.yaml"),
            "id: 066af29d-a95a-4389-9220-87ba5438c2bf\ncwd: /\nclient_name: github/autopilot\n",
        )
        .unwrap();
        fs::write(
            cloud.join("events.jsonl"),
            r#"{"type":"user.message","data":{"content":"cloud should stay hidden"}}
"#,
        )
        .unwrap();
        fs::write(homes.copilot.join("session-state").join("not-a-dir"), "x").unwrap();
        fs::create_dir_all(homes.copilot.join("session-state").join("-badid")).unwrap();
        fs::write(
            homes
                .copilot
                .join("session-state")
                .join("-badid")
                .join("events.jsonl"),
            r#"{"type":"user.message","data":{"content":"invalid id"}}
"#,
        )
        .unwrap();
        fs::create_dir_all(
            homes
                .copilot
                .join("session-state")
                .join("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"),
        )
        .unwrap();
        let events_only = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
        let events_dir = homes.copilot.join("session-state").join(events_only);
        fs::create_dir_all(&events_dir).unwrap();
        fs::write(
            events_dir.join("workspace.yaml"),
            "# comment\n\ncwd: /tmp/from-events\nclient_name: github/cli\nnot-a-pair\n",
        )
        .unwrap();
        fs::write(
            events_dir.join("events.jsonl"),
            r#"not-json
{"type":"assistant.message","data":{"content":"skip"}}
{"type":"user.message","data":{"content":"   "}}
{"type":"user.message","data":{"content":"title from events.jsonl"}}
"#,
        )
        .unwrap();
        let db = homes.copilot.join("session-store.db");
        let connection = rusqlite::Connection::open(&db).unwrap();
        connection
            .execute(
                "CREATE TABLE sessions (
                    id TEXT PRIMARY KEY,
                    cwd TEXT,
                    repository TEXT,
                    host_type TEXT,
                    branch TEXT,
                    summary TEXT,
                    created_at TEXT,
                    updated_at TEXT
                )",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO sessions (id, cwd, branch, summary, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![
                    cli_id,
                    "/tmp/copilot-cli",
                    "main",
                    "Return JSON {}",
                    "2026-08-29T07:51:40.622Z"
                ],
            )
            .unwrap();

        let sessions = list_sessions(
            &homes,
            &ListFilter {
                agent: Some(Agent::Copilot),
                include_archived: true,
                limit: 0,
                ..ListFilter::default()
            },
        )
        .unwrap();
        assert_eq!(sessions.len(), 2);
        let cli = sessions
            .iter()
            .find(|session| session.session_id == cli_id)
            .unwrap();
        assert_eq!(cli.cwd.as_deref(), Some("/tmp/copilot-cli"));
        assert_eq!(cli.branch.as_deref(), Some("main"));
        assert_eq!(cli.title.as_deref(), Some("Return JSON {}"));
        assert_eq!(cli.origin.as_deref(), Some("copilot"));
        assert!(!cli.live);
        let from_events = sessions
            .iter()
            .find(|session| session.session_id == events_only)
            .unwrap();
        assert_eq!(from_events.cwd.as_deref(), Some("/tmp/from-events"));
        assert_eq!(
            from_events.title.as_deref(),
            Some("title from events.jsonl")
        );
        assert!(from_events.last_activity_at.is_some());

        let store_miss = Homes::isolated(tempfile::tempdir().unwrap().path());
        let miss_id = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
        let miss_dir = store_miss.copilot.join("session-state").join(miss_id);
        fs::create_dir_all(&miss_dir).unwrap();
        fs::write(
            miss_dir.join("events.jsonl"),
            r#"{"type":"user.message","data":{"content":"no store row"}}
"#,
        )
        .unwrap();
        rusqlite::Connection::open(store_miss.copilot.join("session-store.db")).unwrap();
        let query_fail = Homes::isolated(tempfile::tempdir().unwrap().path());
        let fail_id = "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee";
        let fail_dir = query_fail.copilot.join("session-state").join(fail_id);
        fs::create_dir_all(&fail_dir).unwrap();
        fs::write(
            fail_dir.join("events.jsonl"),
            r#"{"type":"user.message","data":{"content":"store query fails"}}
"#,
        )
        .unwrap();
        let fail_db =
            rusqlite::Connection::open(query_fail.copilot.join("session-store.db")).unwrap();
        fail_db
            .execute_batch(
                "CREATE TABLE sessions (id TEXT);
                 CREATE TRIGGER sessions_block BEFORE SELECT ON sessions BEGIN
                   SELECT RAISE(FAIL, 'blocked');
                 END;",
            )
            .ok();
        let failed_store = list_sessions(
            &query_fail,
            &ListFilter {
                agent: Some(Agent::Copilot),
                include_archived: true,
                limit: 0,
                ..ListFilter::default()
            },
        )
        .unwrap();
        assert!(
            failed_store
                .iter()
                .any(|session| session.session_id == fail_id)
        );
        let untitled = "dddddddd-dddd-4ddd-8ddd-dddddddddddd";
        let untitled_dir = store_miss.copilot.join("session-state").join(untitled);
        fs::create_dir_all(&untitled_dir).unwrap();
        fs::write(
            untitled_dir.join("events.jsonl"),
            r#"{"type":"assistant.message","data":{"content":"no user turn"}}
{"type":"user.message","data":{}}
"#,
        )
        .unwrap();
        let listed = list_sessions(
            &store_miss,
            &ListFilter {
                agent: Some(Agent::Copilot),
                include_archived: true,
                limit: 0,
                ..ListFilter::default()
            },
        )
        .unwrap();
        assert_eq!(listed.len(), 2);
        assert!(listed.iter().any(|session| session.session_id == miss_id
            && session.title.as_deref() == Some("no store row")));
        assert!(
            listed
                .iter()
                .any(|session| session.session_id == untitled && session.title.is_none())
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let blocked = Homes::isolated(tempfile::tempdir().unwrap().path());
            let root = blocked.copilot.join("session-state");
            fs::create_dir_all(&root).unwrap();
            let mut permissions = fs::metadata(&root).unwrap().permissions();
            permissions.set_mode(0o000);
            fs::set_permissions(&root, permissions).unwrap();
            assert!(
                list_sessions(
                    &blocked,
                    &ListFilter {
                        agent: Some(Agent::Copilot),
                        include_archived: true,
                        limit: 0,
                        ..ListFilter::default()
                    },
                )
                .is_err()
            );
            let mut permissions = fs::metadata(&root).unwrap().permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&root, permissions).unwrap();
        }
    }
}
