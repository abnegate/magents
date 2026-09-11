use super::text::{
    cap_turn, clean_turn, keep_turn, mcp_server, skill_name_from_path, slash_commands,
};
use crate::model::{Agent, Session};
use crate::transcript::{human_prompts, jsonl, read_session};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

const SUBAGENT_KINDS: &[&str] = &["subagent", "subagent_resume", "subagent_fork"];
const HEADLESS_ORIGINS: &[&str] = &[
    "claude-print",
    "codex-exec",
    "copilot-json",
    "cursor-agent",
    "gemini-stream",
    "grok-stream",
    "opencode-run",
    "headless",
];

#[derive(Clone, Copy, Debug)]
pub enum Skip {
    NoTranscript,
    Subagent,
    Headless,
    ExcludedCwd,
    OutsideCwd,
    OlderThanWindow,
    NoHumanTurns,
    SmokeTest,
    Unreadable,
    NotInList,
}

impl Skip {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoTranscript => "no_transcript",
            Self::Subagent => "subagent",
            Self::Headless => "headless",
            Self::ExcludedCwd => "excluded_cwd",
            Self::OutsideCwd => "outside_cwd",
            Self::OlderThanWindow => "older_than_window",
            Self::NoHumanTurns => "no_human_turns",
            Self::SmokeTest => "smoke_test",
            Self::Unreadable => "unreadable",
            Self::NotInList => "not_in_session_id_list",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompactSession {
    pub index: usize,
    pub agent: String,
    pub id: String,
    pub session_id: String,
    pub cwd: String,
    pub git_root: String,
    pub branch: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub human_turns: usize,
    pub smoke_test: bool,
    pub repeated_single_turn: bool,
    pub turns: Vec<String>,
    pub slash_commands: BTreeMap<String, usize>,
    pub skills_loaded: BTreeMap<String, usize>,
    pub skill_load_paths: BTreeMap<String, String>,
    pub mcp_servers_used: BTreeMap<String, usize>,
    pub tools: BTreeMap<String, usize>,
    pub top_dirs_touched: BTreeMap<String, usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
}

pub struct ScanFilter<'a> {
    pub include_headless: bool,
    pub include_subagents: bool,
    pub cwd: &'a [String],
    pub exclude_cwd: &'a [String],
    pub drop_patterns: &'a [regex::Regex],
    pub min_turns: usize,
    pub session_ids: &'a [String],
    pub cutoff: Option<chrono::DateTime<chrono::Utc>>,
}

pub fn compact(
    session: &Session,
    filter: &ScanFilter<'_>,
) -> std::result::Result<CompactSession, Skip> {
    let ref_id = format!("{}:{}", session.agent.as_str(), session.session_id);
    if !filter.session_ids.is_empty()
        && !filter
            .session_ids
            .iter()
            .any(|id| id == &session.session_id || id == &ref_id)
    {
        return Err(Skip::NotInList);
    }
    let kind = grok_session_kind(session).or_else(|| session.origin.clone());
    if is_subagent(session, kind.as_deref()) && !filter.include_subagents {
        return Err(Skip::Subagent);
    }
    if is_headless(session, kind.as_deref()) && !filter.include_headless {
        return Err(Skip::Headless);
    }
    let cwd = session.cwd.clone().unwrap_or_default();
    if filter.exclude_cwd.iter().any(|base| under(&cwd, base)) || cwd.contains("grok-e2e") {
        return Err(Skip::ExcludedCwd);
    }
    if !filter.cwd.is_empty() && !filter.cwd.iter().any(|base| under(&cwd, base)) {
        return Err(Skip::OutsideCwd);
    }
    if let Some(cutoff) = filter.cutoff {
        match session.last_activity_at {
            Some(time) if time >= cutoff => {}
            _ => return Err(Skip::OlderThanWindow),
        }
    }
    let Some(path) = session.transcript_path.as_deref() else {
        return Err(Skip::NoTranscript);
    };
    let transcript = read_session(session, 0).map_err(|_| Skip::Unreadable)?;
    let mut turns = Vec::new();
    let mut slash: BTreeMap<String, usize> = BTreeMap::new();
    let mut tools: BTreeMap<String, usize> = BTreeMap::new();
    for turn in &transcript.turns {
        if turn.role == "user" {
            for prompt in human_prompts(&turn.text) {
                if filter
                    .drop_patterns
                    .iter()
                    .any(|pattern| pattern.is_match(&prompt))
                {
                    continue;
                }
                if prompt.is_empty() {
                    continue;
                }
                if !keep_turn(&prompt) {
                    continue;
                }
                let cleaned = clean_turn(&prompt);
                for name in slash_commands(&cleaned) {
                    *slash.entry(name).or_default() += 1;
                }
                turns.push(cap_turn(&cleaned));
            }
        } else {
            for name in &turn.tools {
                *tools.entry(name.clone()).or_default() += 1;
            }
        }
    }
    let mut skills: BTreeMap<String, usize> = BTreeMap::new();
    let mut skill_paths: BTreeMap<String, String> = BTreeMap::new();
    let mut mcp: BTreeMap<String, usize> = BTreeMap::new();
    let mut paths: BTreeMap<String, usize> = BTreeMap::new();
    scan_trace(
        path,
        &mut tools,
        &mut skills,
        &mut skill_paths,
        &mut mcp,
        &mut paths,
    );
    if turns.len() < filter.min_turns {
        return Err(Skip::NoHumanTurns);
    }
    let real_tools: usize = tools
        .iter()
        .filter(|(name, _)| name.as_str() != "send_feedback")
        .map(|(_, n)| *n)
        .sum();
    let smoke = real_tools == 0 && turns.iter().all(|turn| turn.split_whitespace().count() < 3);
    if smoke {
        return Err(Skip::SmokeTest);
    }
    let git = git_root(&cwd)
        .map(|path| path.display().to_string())
        .unwrap_or_default();
    let dirs = top_dirs(
        &paths,
        if git.is_empty() {
            cwd.as_str()
        } else {
            git.as_str()
        },
    );
    let tools = top_counts(tools, 25);
    Ok(CompactSession {
        index: 0,
        agent: session.agent.as_str().to_string(),
        id: ref_id,
        session_id: session.session_id.clone(),
        cwd,
        git_root: git,
        branch: session.branch.clone().unwrap_or_default(),
        title: session.label(),
        session_kind: kind,
        updated_at: session.last_activity_at.map(|time| time.to_rfc3339()),
        model: session.model.clone(),
        human_turns: turns.len(),
        smoke_test: false,
        repeated_single_turn: false,
        turns,
        slash_commands: slash,
        skills_loaded: skills,
        skill_load_paths: skill_paths,
        mcp_servers_used: mcp,
        tools,
        top_dirs_touched: dirs,
        origin: session.origin.clone(),
    })
}

fn is_subagent(session: &Session, kind: Option<&str>) -> bool {
    session.origin.as_deref() == Some("subagent")
        || kind.is_some_and(|kind| SUBAGENT_KINDS.contains(&kind))
}

fn is_headless(session: &Session, kind: Option<&str>) -> bool {
    kind == Some("headless")
        || session
            .origin
            .as_deref()
            .is_some_and(|origin| HEADLESS_ORIGINS.contains(&origin))
}

fn grok_session_kind(session: &Session) -> Option<String> {
    if session.agent != Agent::Grok {
        return None;
    }
    let path = session
        .transcript_path
        .as_ref()?
        .parent()?
        .join("summary.json");
    let raw = std::fs::read_to_string(path).ok()?;
    let value: Value = serde_json::from_str(&raw).ok()?;
    value
        .get("session_kind")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

pub fn git_root(cwd: &str) -> Option<PathBuf> {
    if cwd.is_empty() {
        return None;
    }
    let mut dir = PathBuf::from(cwd);
    loop {
        if dir.join(".git").exists() {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

pub fn under(path: &str, base: &str) -> bool {
    if path.is_empty() || base.is_empty() {
        return false;
    }
    let path = norm(path);
    let base = norm(base);
    path == base || path.starts_with(&(base + "/"))
}

fn norm(path: &str) -> String {
    let replaced = path.replace('\\', "/");
    let mut out = PathBuf::new();
    for component in Path::new(&replaced).components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out.to_string_lossy().replace('\\', "/")
}

fn scan_trace(
    path: &Path,
    tools: &mut BTreeMap<String, usize>,
    skills: &mut BTreeMap<String, usize>,
    skill_paths: &mut BTreeMap<String, String>,
    mcp: &mut BTreeMap<String, usize>,
    paths: &mut BTreeMap<String, usize>,
) {
    for value in load_trace(path) {
        walk(&value, tools, skills, skill_paths, mcp, paths, false);
    }
}

fn load_trace(path: &Path) -> Vec<Value> {
    if let Ok(records) = jsonl(path)
        && !records.is_empty()
    {
        return records;
    }
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .into_iter()
        .collect()
}

fn walk(
    value: &Value,
    tools: &mut BTreeMap<String, usize>,
    skills: &mut BTreeMap<String, usize>,
    skill_paths: &mut BTreeMap<String, String>,
    mcp: &mut BTreeMap<String, usize>,
    paths: &mut BTreeMap<String, usize>,
    in_tool: bool,
) {
    match value {
        Value::Object(map) => {
            let typed = map.get("type").and_then(Value::as_str).unwrap_or("");
            let toolish = in_tool
                || typed == "tool_use"
                || typed.contains("tool")
                || map.contains_key("toolCall")
                || map.contains_key("tool_name");
            if let Some(name) = map.get("tool_name").and_then(Value::as_str) {
                add_tool(name, tools, mcp);
            }
            if toolish && let Some(name) = map.get("name").and_then(Value::as_str) {
                add_tool(name, tools, mcp);
            }
            for key in ["path", "file_path", "target_file", "target", "filename"] {
                if let Some(path) = map.get(key).and_then(Value::as_str) {
                    add_path(path, skills, skill_paths, paths);
                }
            }
            for child in map.values() {
                walk(child, tools, skills, skill_paths, mcp, paths, toolish);
            }
        }
        Value::Array(items) => {
            for item in items {
                walk(item, tools, skills, skill_paths, mcp, paths, in_tool);
            }
        }
        Value::String(text) => {
            if let Some(name) = skill_name_from_path(text) {
                *skills.entry(name.clone()).or_default() += 1;
            }
        }
        _ => {}
    }
}

fn add_tool(name: &str, tools: &mut BTreeMap<String, usize>, mcp: &mut BTreeMap<String, usize>) {
    if name.is_empty() {
        return;
    }
    *tools.entry(name.to_string()).or_default() += 1;
    if let Some(server) = mcp_server(name) {
        *mcp.entry(server).or_default() += 1;
    }
}

fn add_path(
    path: &str,
    skills: &mut BTreeMap<String, usize>,
    skill_paths: &mut BTreeMap<String, String>,
    paths: &mut BTreeMap<String, usize>,
) {
    if let Some(name) = skill_name_from_path(path) {
        *skills.entry(name.clone()).or_default() += 1;
        skill_paths.entry(name).or_insert_with(|| path.to_string());
        return;
    }
    if path.contains('/') {
        *paths.entry(path.to_string()).or_default() += 1;
    }
}

fn top_dirs(paths: &BTreeMap<String, usize>, base: &str) -> BTreeMap<String, usize> {
    let mut dirs: BTreeMap<String, usize> = BTreeMap::new();
    let base_norm = norm(base);
    for (path, count) in paths {
        let path_norm = norm(path);
        let rel = path_norm
            .strip_prefix(&base_norm)
            .map(|rest| rest.trim_start_matches('/').to_string())
            .filter(|_| under(path, base))
            .unwrap_or_else(|| path_norm.clone());
        let parts: Vec<&str> = rel
            .split(['/', '\\'])
            .filter(|part| !part.is_empty())
            .collect();
        let key = if parts.len() > 2 {
            format!("{}/{}", parts[0], parts[1])
        } else {
            parts.first().copied().unwrap_or(rel.as_str()).to_string()
        };
        *dirs.entry(key).or_default() += *count;
    }
    top_counts(dirs, 10)
}

fn top_counts(map: BTreeMap<String, usize>, limit: usize) -> BTreeMap<String, usize> {
    let mut rows: Vec<(String, usize)> = map.into_iter().collect();
    rows.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    rows.into_iter().take(limit).collect()
}

pub fn filename(index: usize, agent: &str, session_id: &str) -> String {
    let safe: String = session_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '-'
            }
        })
        .collect();
    format!("{index:04}-{agent}-{safe}.json")
}

#[cfg(test)]
mod tests {
    use super::{ScanFilter, Skip, compact, filename, git_root, under};
    use crate::model::{Agent, Session};
    use chrono::{TimeZone, Utc};
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn skip_labels() {
        assert_eq!(Skip::NoTranscript.as_str(), "no_transcript");
        assert_eq!(Skip::Subagent.as_str(), "subagent");
        assert_eq!(Skip::Headless.as_str(), "headless");
        assert_eq!(Skip::ExcludedCwd.as_str(), "excluded_cwd");
        assert_eq!(Skip::OutsideCwd.as_str(), "outside_cwd");
        assert_eq!(Skip::OlderThanWindow.as_str(), "older_than_window");
        assert_eq!(Skip::NoHumanTurns.as_str(), "no_human_turns");
        assert_eq!(Skip::SmokeTest.as_str(), "smoke_test");
        assert_eq!(Skip::Unreadable.as_str(), "unreadable");
        assert_eq!(Skip::NotInList.as_str(), "not_in_session_id_list");
    }

    #[test]
    fn under_and_filename() {
        assert!(under("/Users/test/app/src", "/Users/test/app"));
        assert!(under("/Users/test/app", "/Users/test/app"));
        assert!(!under("/Users/test/other", "/Users/test/app"));
        assert!(!under("", "/tmp"));
        assert!(under("/a/b/../c", "/a"));
        assert_eq!(filename(3, "grok", "01ab/c"), "0003-grok-01ab-c.json");
        let mut paths = std::collections::BTreeMap::new();
        paths.insert("/a/b/src/lib.rs".into(), 1);
        let dirs = super::top_dirs(&paths, "/a/b/");
        assert!(dirs.contains_key("src"), "{dirs:?}");
        paths.insert("/Users/me/app/../app/src/main.rs".into(), 9);
        paths.insert("/Users/me/app/zzz/other.rs".into(), 1);
        let ranked = super::top_dirs(&paths, "/Users/me/app");
        assert!(ranked.contains_key("src"), "{ranked:?}");
        assert!(git_root("").is_none());
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".git")).unwrap();
        assert_eq!(
            git_root(dir.path().to_str().unwrap()).as_deref(),
            Some(dir.path())
        );
    }

    fn session(path: Option<std::path::PathBuf>, cwd: &str) -> Session {
        Session {
            agent: Agent::Grok,
            session_id: "01scan".into(),
            desktop_id: None,
            name: None,
            title: Some("scan".into()),
            cwd: Some(cwd.into()),
            branch: None,
            live: false,
            archived: false,
            pid: None,
            model: None,
            last_activity_at: Some(Utc::now()),
            transcript_path: path,
            messaging_socket: None,
            origin: Some("tui".into()),
            tmux: None,
        }
    }

    fn filter<'a>(
        cwd: &'a [String],
        exclude: &'a [String],
        drops: &'a [regex::Regex],
        ids: &'a [String],
        cutoff: Option<chrono::DateTime<Utc>>,
    ) -> ScanFilter<'a> {
        ScanFilter {
            include_headless: false,
            include_subagents: false,
            cwd,
            exclude_cwd: exclude,
            drop_patterns: drops,
            min_turns: 1,
            session_ids: ids,
            cutoff,
        }
    }

    #[test]
    fn compact_covers_skip_reasons_and_trace_metadata() {
        let dir = tempdir().unwrap();
        let jsonl = dir.path().join("chat.jsonl");
        fs::write(
            &jsonl,
            concat!(
                r#"{"params":{"update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"please /review the crate"}}}}"#,
                "\n",
                r#"{"params":{"update":{"sessionUpdate":"tool_call","type":"tool_use","name":"read_file","file_path":"/Users/test/.grok/skills/review/SKILL.md"}}}"#,
                "\n",
                r#"{"params":{"update":{"sessionUpdate":"tool_call","tool_name":"magents__list_sessions","path":"/Users/test/app/src/lib.rs"}}}"#,
                "\n",
                r#"{"params":{"update":{"sessionUpdate":"turn_completed"}}}"#,
                "\n"
            ),
        )
        .unwrap();
        let empty: [String; 0] = [];
        let none: [regex::Regex; 0] = [];
        let kept = compact(
            &session(Some(jsonl.clone()), "/Users/test/app"),
            &filter(&empty, &empty, &none, &empty, None),
        )
        .unwrap();
        assert!(kept.skills_loaded.contains_key("review"));
        assert!(kept.mcp_servers_used.contains_key("magents"));

        let mut old = session(Some(jsonl.clone()), "/Users/test/app");
        old.last_activity_at = Some(Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap());
        let cutoff = Some(Utc::now());
        assert!(matches!(
            compact(&old, &filter(&empty, &empty, &none, &empty, cutoff)),
            Err(Skip::OlderThanWindow)
        ));
        old.last_activity_at = None;
        assert!(matches!(
            compact(&old, &filter(&empty, &empty, &none, &empty, cutoff)),
            Err(Skip::OlderThanWindow)
        ));

        let mut missing = session(None, "/Users/test/app");
        assert!(matches!(
            compact(&missing, &filter(&empty, &empty, &none, &empty, None)),
            Err(Skip::NoTranscript)
        ));
        missing.cwd = Some("/tmp/grok-e2e/work".into());
        missing.transcript_path = Some(jsonl.clone());
        assert!(matches!(
            compact(&missing, &filter(&empty, &empty, &none, &empty, None)),
            Err(Skip::ExcludedCwd)
        ));

        let drop = [regex::Regex::new("please /review").unwrap()];
        assert!(matches!(
            compact(
                &session(Some(jsonl.clone()), "/Users/test/app"),
                &filter(&empty, &empty, &drop, &empty, None)
            ),
            Err(Skip::NoHumanTurns)
        ));

        let ids = ["other".into()];
        assert!(matches!(
            compact(
                &session(Some(jsonl.clone()), "/Users/test/app"),
                &filter(&empty, &empty, &none, &ids, None)
            ),
            Err(Skip::NotInList)
        ));

        let pretty = dir.path().join("one.json");
        fs::write(
            &pretty,
            "{\n  \"messages\": [{\"type\": \"user\", \"content\": \"read /Users/x/skills/demo/SKILL.md now\"}, {\"type\": \"tool_use\", \"name\": \"\", \"tool_name\": \"\"}]\n}\n",
        )
        .unwrap();
        let mut gemini = session(Some(pretty), "/Users/test/app");
        gemini.agent = Agent::Gemini;
        let _ = compact(&gemini, &filter(&empty, &empty, &none, &empty, None));

        let mut sub = session(Some(jsonl.clone()), "/Users/test/app");
        sub.origin = Some("subagent".into());
        assert!(matches!(
            compact(&sub, &filter(&empty, &empty, &none, &empty, None)),
            Err(Skip::Subagent)
        ));
        sub.origin = Some("grok-stream".into());
        assert!(matches!(
            compact(&sub, &filter(&empty, &empty, &none, &empty, None)),
            Err(Skip::Headless)
        ));
        sub.origin = Some("tui".into());
        sub.transcript_path = Some(dir.path().join("missing.jsonl"));
        assert!(matches!(
            compact(&sub, &filter(&empty, &empty, &none, &empty, None)),
            Err(Skip::Unreadable)
        ));
    }

    #[test]
    fn compact_strips_claude_chrome_and_closing_xml_slash_false_positives() {
        let dir = tempdir().unwrap();
        let jsonl = dir.path().join("chrome.jsonl");
        fs::write(
            &jsonl,
            concat!(
                r#"{"params":{"update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"<local-command-stdout>\nThe app was quit while the command was running.\n</local-command-stdout>"}}}}"#,
                "\n",
                r#"{"params":{"update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"<command-name>/shepherd</command-name></summary></task-id> please land this pull request"}}}}"#,
                "\n",
                r#"{"params":{"update":{"sessionUpdate":"tool_call","toolCall":{"name":"read_file"}}}}"#,
                "\n",
                r#"{"params":{"update":{"sessionUpdate":"turn_completed"}}}"#,
                "\n"
            ),
        )
        .unwrap();
        let empty: [String; 0] = [];
        let none: [regex::Regex; 0] = [];
        let kept = compact(
            &session(Some(jsonl), "/Users/test/app"),
            &filter(&empty, &empty, &none, &empty, None),
        )
        .unwrap();
        assert_eq!(kept.human_turns, 1);
        assert!(!kept.turns.iter().any(|turn| turn.contains("app was quit")));
        assert!(kept.slash_commands.contains_key("shepherd"));
        assert!(!kept.slash_commands.contains_key("summary"));
        assert!(!kept.slash_commands.contains_key("task-id"));
    }
}
