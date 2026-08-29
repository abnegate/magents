use crate::deliver::deliver_live;
use crate::discover::{ListFilter, list_sessions, resolve};
use crate::error::Error;
use crate::homes::Homes;
use crate::mailbox::{self, compose};
use crate::model::{Agent, Caller};
use crate::transcript::{read_transcript, search_transcripts};
use chrono::Utc;
use rusqlite::Connection;
use serde_json::json;
use std::fs;
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::Duration;
use tempfile::TempDir;

const CLAUDE_ID: &str = "11111111-1111-4111-8111-111111111111";
const CURSOR_ID: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const CURSOR_SUB_ID: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
const GROK_ID: &str = "01testgrok0000000000000000";
const CODEX_ID: &str = "01testcodex000000000000000";
const OPENCODE_ID: &str = "ses_testopencode0001";
const OPENCODE_CHILD: &str = "ses_testchild0000001";

pub(crate) struct World {
    _dir: TempDir,
    pub(crate) homes: Homes,
}

impl World {
    pub(crate) fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(dir.path());
        write_claude(&homes);
        write_grok(&homes);
        write_codex(&homes);
        write_cursor(&homes);
        write_opencode_sqlite(&homes);
        Self { _dir: dir, homes }
    }
}

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

fn pid() -> u32 {
    std::process::id()
}

fn write(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, body).unwrap();
}

fn write_claude(homes: &Homes) {
    write(&homes.claude.join("sessions").join("not-a-pid.json"), "{}");
    write(&homes.claude.join("sessions").join("12345.notjson"), "skip");
    write(
        &homes.claude.join("sessions").join("999999.json"),
        &json!({
            "pid": 999999u32,
            "sessionId": "dead-claude",
            "cwd": "/tmp/dead"
        })
        .to_string(),
    );
    write(
        &homes.claude.join("sessions").join("888888.json"),
        "not-json",
    );
    write(
        &homes
            .claude
            .join("sessions")
            .join(format!("{}.json", pid())),
        &serde_json::to_string(&json!({
            "pid": pid(),
            "sessionId": CLAUDE_ID,
            "cwd": "/tmp/dr",
            "entrypoint": "cli",
            "name": "disaster-recovery",
            "startedAt": now_ms(),
        }))
        .unwrap(),
    );
    write(
        &homes.claude_desktop.join("local_live.json"),
        &json!({
            "sessionId": "desktop-live",
            "cliSessionId": CLAUDE_ID,
            "title": "Disaster recovery desktop",
            "cwd": "/tmp/dr",
            "branch": "fix/dr",
            "model": "opus",
            "lastActivityAt": now_ms(),
            "isArchived": false
        })
        .to_string(),
    );
    write(
        &homes.claude_desktop.join("local_old.json"),
        &json!({
            "sessionId": "desktop-old",
            "cliSessionId": "22222222-2222-4222-8222-222222222222",
            "title": "Archived desktop chat",
            "originCwd": "/tmp/old",
            "lastFocusedAt": 1_700_000_000,
            "isArchived": true
        })
        .to_string(),
    );
    write(&homes.claude_desktop.join("local_bad.json"), "not-json");
    write(
        &homes.claude_desktop.join("local_noid.json"),
        &json!({"title": "missing id"}).to_string(),
    );
    write(
        &homes
            .claude
            .join("projects")
            .join("tmp-old")
            .join("22222222-2222-4222-8222-222222222222.jsonl"),
        r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"old desktop prompt"}]}}
"#,
    );
    write(
        &homes
            .claude
            .join("projects")
            .join("tmp-dr")
            .join(format!("{CLAUDE_ID}.jsonl")),
        r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"run the 109 point matrix"}]}}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"starting verification"},{"type":"tool_use","name":"Bash"}]}}
{"type":"user","isMeta":true,"message":{"role":"user","content":[{"type":"text","text":"ignore me"}]}}
{"type":"progress"}
{"type":"assistant","message":{"role":"assistant","content":[]}}
"#,
    );
}

fn write_grok(homes: &Homes) {
    write(
        &homes.grok.join("active_sessions.json"),
        &serde_json::to_string(&json!([{ "session_id": GROK_ID, "pid": pid() }])).unwrap(),
    );
    let session_dir = homes
        .grok
        .join("sessions")
        .join("%2Ftmp%2Fedge")
        .join(GROK_ID);
    write(
        &session_dir.join("summary.json"),
        &serde_json::to_string(&json!({
            "info": { "id": GROK_ID, "cwd": "/tmp/edge" },
            "generated_title": "Queue GC PRs",
            "current_model_id": "grok-4.6",
            "last_active_at": Utc::now().to_rfc3339(),
            "session_kind": "main"
        }))
        .unwrap(),
    );
    write(
        &session_dir.join("updates.jsonl"),
        r#"{"params":{"update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"fix the dedicated databases leak"}}}}
{"params":{"update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"closing the fd"}}}}
{"params":{"update":{"sessionUpdate":"tool_call","name":"shell"}}}
{"params":{"update":{"sessionUpdate":"noise"}}}
{"params":{"update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"second turn"}}}}
{"params":{"update":{"sessionUpdate":"tool_call","toolCall":{"name":"grep"}}}}
{"params":{"update":{"sessionUpdate":"turn_completed"}}}
"#,
    );
    let sub = homes
        .grok
        .join("sessions")
        .join("%2Ftmp%2Fedge")
        .join("01subagentgrok00000000000");
    write(
        &sub.join("summary.json"),
        &json!({
            "info": { "id": "01subagentgrok00000000000", "cwd": "/tmp/edge" },
            "session_kind": "subagent",
            "generated_title": "hidden"
        })
        .to_string(),
    );
    write(&sub.join("summary.bad"), "nope");
    write(
        &homes
            .grok
            .join("sessions")
            .join("%2Ftmp%2Fother")
            .join("01othergrok0000000000000")
            .join("summary.json"),
        "not-json",
    );
    let idle = homes
        .grok
        .join("sessions")
        .join("%2Ftmp%2Fidle")
        .join("01idlegrok00000000000000");
    write(
        &idle.join("summary.json"),
        &json!({
            "info": { "id": "01idlegrok00000000000000", "cwd": "/tmp/idle" },
            "session_summary": "Idle grok chat",
            "session_kind": "main"
        })
        .to_string(),
    );
}

fn write_codex(homes: &Homes) {
    let db = homes.codex.join("state_1.sqlite");
    fs::create_dir_all(homes.codex.join("ipc")).unwrap();
    fs::write(homes.codex.join("ipc").join("ipc.sock"), "").unwrap();
    let rollout = homes.codex.join("sessions").join("rollout.jsonl");
    write(
        &rollout,
        r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"fix billing worker memory"}]}}
{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"patching the worker"}]}}
{"type":"event_msg","payload":{"type":"token_count"}}
{"type":"response_item"}
{"type":"response_item","payload":{"type":"reasoning"}}
{"type":"response_item","payload":{"type":"message"}}
{"type":"response_item","payload":{"type":"message","role":"assistant","content":[]}}
"#,
    );
    let connection = Connection::open(&db).unwrap();
    connection
        .execute(
            "CREATE TABLE threads (
                id TEXT PRIMARY KEY,
                title TEXT,
                cwd TEXT,
                git_branch TEXT,
                model TEXT,
                archived INTEGER,
                updated_at_ms INTEGER,
                rollout_path TEXT,
                source TEXT
            )",
            [],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO threads (id, title, cwd, git_branch, model, archived, updated_at_ms, rollout_path, source)
             VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6, ?7, 'vscode')",
            rusqlite::params![
                CODEX_ID,
                "Billing memory leak",
                "/tmp/cloud",
                "fix/billing",
                "gpt-5.6-sol",
                now_ms(),
                rollout.to_str().unwrap()
            ],
        )
        .unwrap();
    let older = homes.codex.join("state_0.sqlite");
    Connection::open(&older)
        .unwrap()
        .execute("CREATE TABLE threads (id TEXT PRIMARY KEY)", [])
        .unwrap();
    connection
        .execute(
            "INSERT INTO threads (id, title, cwd, git_branch, model, archived, updated_at_ms, rollout_path, source)
             VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6, NULL, 'cli')",
            rusqlite::params![
                "01archivedcodex00000000000",
                "Old cli thread",
                "/tmp/old-codex",
                "main",
                "gpt",
                now_ms() - 3_600_000
            ],
        )
        .unwrap();
    let fallback = homes
        .codex
        .join("sessions")
        .join("rollout-01fallbackcodex000000000.jsonl");
    write(
        &fallback,
        r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"fallback rollout"}]}}
"#,
    );
    connection
        .execute(
            "INSERT INTO threads (id, title, cwd, git_branch, model, archived, updated_at_ms, rollout_path, source)
             VALUES (?1, '', ?2, NULL, NULL, 0, ?3, NULL, 'other')",
            rusqlite::params!["01fallbackcodex000000000", "/tmp/fb", now_ms()],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO threads (id, title, cwd, git_branch, model, archived, updated_at_ms, rollout_path, source)
             VALUES (?1, 'sub', NULL, NULL, NULL, 0, ?2, NULL, 'codex-subagent')",
            rusqlite::params!["01subagentcodex000000000", now_ms()],
        )
        .unwrap();
}

fn write_cursor(homes: &Homes) {
    let jsonl = homes
        .cursor
        .join("projects")
        .join("Users-tmp-cloud")
        .join("agent-transcripts")
        .join(CURSOR_ID)
        .join(format!("{CURSOR_ID}.jsonl"));
    write(
        &jsonl,
        r#"{"role":"user","message":{"content":[{"type":"text","text":"<timestamp>now</timestamp>\n<user_query>\nPull the 109 point matrix from Claude\n</user_query>"}]}}
{"role":"assistant","message":{"content":[{"type":"text","text":"searching transcripts"},{"type":"tool_use","name":"Grep"}]}}
{"role":"assistant","message":{"content":[{"type":"text","text":"found the suite"}]}}
"#,
    );
    let sub = homes
        .cursor
        .join("projects")
        .join("Users-tmp-cloud")
        .join("agent-transcripts")
        .join(CURSOR_SUB_ID)
        .join(format!("{CURSOR_SUB_ID}.jsonl"));
    write(
        &sub,
        r#"{"role":"user","message":{"content":[{"type":"text","text":"<user_query>subagent only</user_query>"}]}}
"#,
    );
    write(
        &homes
            .cursor_app
            .join("User")
            .join("workspaceStorage")
            .join("ws-cloud")
            .join("workspace.json"),
        r#"{"folder":"file:///tmp/cloud"}
"#,
    );
    let db = homes
        .cursor_app
        .join("User")
        .join("globalStorage")
        .join("state.vscdb");
    fs::create_dir_all(db.parent().unwrap()).unwrap();
    let connection = Connection::open(&db).unwrap();
    connection
        .execute(
            "CREATE TABLE composerHeaders (
                composerId TEXT PRIMARY KEY,
                workspaceId TEXT,
                lastUpdatedAt INTEGER,
                isArchived INTEGER,
                isSubagent INTEGER,
                value TEXT
            )",
            [],
        )
        .unwrap();
    let value = json!({
        "name": "Test rounds analysis",
        "workspaceIdentifier": { "id": "ws-cloud" }
    });
    connection
        .execute(
            "INSERT INTO composerHeaders VALUES (?1, ?2, ?3, 0, 0, ?4)",
            rusqlite::params![CURSOR_ID, "ws-cloud", now_ms(), value.to_string()],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO composerHeaders VALUES (?1, ?2, ?3, 0, 1, ?4)",
            rusqlite::params![
                CURSOR_SUB_ID,
                "ws-cloud",
                now_ms(),
                json!({"name": "hidden subagent"}).to_string()
            ],
        )
        .unwrap();
}

fn write_opencode_sqlite(homes: &Homes) {
    let db = homes.opencode.join("opencode.db");
    fs::create_dir_all(&homes.opencode).unwrap();
    let connection = Connection::open(&db).unwrap();
    connection
        .execute(
            "CREATE TABLE session (
                id TEXT PRIMARY KEY,
                project_id TEXT,
                parent_id TEXT,
                directory TEXT,
                title TEXT,
                time_updated INTEGER,
                time_archived INTEGER
            )",
            [],
        )
        .unwrap();
    connection
        .execute(
            "CREATE TABLE message (
                id TEXT PRIMARY KEY,
                session_id TEXT,
                time_created INTEGER,
                time_updated INTEGER,
                data TEXT
            )",
            [],
        )
        .unwrap();
    connection
        .execute(
            "CREATE TABLE part (
                id TEXT PRIMARY KEY,
                message_id TEXT,
                session_id TEXT,
                time_created INTEGER,
                time_updated INTEGER,
                data TEXT
            )",
            [],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO session VALUES (?1, 'proj', NULL, '/tmp/zone', 'zone-dev checks', ?2, NULL)",
            rusqlite::params![OPENCODE_ID, now_ms()],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO session VALUES (?1, 'proj', ?2, '/tmp/zone', 'child', ?3, NULL)",
            rusqlite::params![OPENCODE_CHILD, OPENCODE_ID, now_ms()],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO message VALUES ('msg_user', ?1, 1, 1, ?2)",
            rusqlite::params![OPENCODE_ID, json!({"role":"user"}).to_string()],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO part VALUES ('prt_user', 'msg_user', ?1, 1, 1, ?2)",
            rusqlite::params![
                OPENCODE_ID,
                json!({"type":"text","text":"Do all the zone-dev --simple check tasks complete?"})
                    .to_string()
            ],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO message VALUES ('msg_asst', ?1, 2, 2, ?2)",
            rusqlite::params![OPENCODE_ID, json!({"role":"assistant"}).to_string()],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO part VALUES ('prt_asst', 'msg_asst', ?1, 2, 2, ?2)",
            rusqlite::params![
                OPENCODE_ID,
                json!({"type":"text","text":"all checks passed"}).to_string()
            ],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO part VALUES ('prt_tool', 'msg_asst', ?1, 2, 3, ?2)",
            rusqlite::params![
                OPENCODE_ID,
                json!({"type":"tool_call","name":"bash"}).to_string()
            ],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO part VALUES ('prt_tool2', 'msg_asst', ?1, 2, 4, ?2)",
            rusqlite::params![OPENCODE_ID, json!({"type":"tool_use"}).to_string()],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO part VALUES ('prt_other', 'msg_asst', ?1, 2, 5, ?2)",
            rusqlite::params![
                OPENCODE_ID,
                json!({"type":"markdown","text":"more"}).to_string()
            ],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO session VALUES (?1, 'proj', NULL, '/tmp/zone', 'archived zone', ?2, ?2)",
            rusqlite::params!["ses_archived_zone", now_ms()],
        )
        .unwrap();
}

fn write_opencode_json_only(root: &Path) {
    let session = root
        .join("storage")
        .join("session")
        .join("proj")
        .join("ses_jsononly.json");
    write(
        &session,
        &json!({
            "id": "ses_jsononly",
            "directory": "/tmp/json-only",
            "title": "json fallback session",
            "time": { "updated": now_ms() }
        })
        .to_string(),
    );
    write(
        &root
            .join("storage")
            .join("message")
            .join("ses_jsononly")
            .join("msg_1.json"),
        &json!({"role":"user"}).to_string(),
    );
    write(
        &root
            .join("storage")
            .join("part")
            .join("msg_1")
            .join("prt_1.json"),
        &json!({"type":"text","text":"json fallback prompt about dedicated databases"}).to_string(),
    );
    write(
        &root
            .join("storage")
            .join("part")
            .join("msg_1")
            .join("prt_2.json"),
        &json!({"type":"text","text":"second json chunk"}).to_string(),
    );
    write(
        &root
            .join("storage")
            .join("part")
            .join("msg_1")
            .join("prt_3.json"),
        &json!({"type":"tool_call","name":"grep"}).to_string(),
    );
    write(
        &root
            .join("storage")
            .join("part")
            .join("msg_1")
            .join("prt_bad.json"),
        "not-json",
    );
    write(
        &root
            .join("storage")
            .join("message")
            .join("ses_jsononly")
            .join("msg_empty.json"),
        &json!({"role":"assistant"}).to_string(),
    );
}

fn all_agents() -> ListFilter {
    ListFilter {
        agent: None,
        query: None,
        live_only: false,
        include_archived: false,
        limit: 0,
    }
}

#[test]
fn lists_every_harness_and_skips_subagents() {
    let world = World::new();
    let sessions = list_sessions(&world.homes, &all_agents()).unwrap();
    let ids: Vec<_> = sessions
        .iter()
        .map(|session| (session.agent, session.session_id.as_str()))
        .collect();
    assert!(ids.contains(&(Agent::Claude, CLAUDE_ID)));
    assert!(ids.contains(&(Agent::Grok, GROK_ID)));
    assert!(ids.contains(&(Agent::Codex, CODEX_ID)));
    assert!(ids.contains(&(Agent::Cursor, CURSOR_ID)));
    assert!(ids.contains(&(Agent::OpenCode, OPENCODE_ID)));
    assert!(!ids.iter().any(|(_, id)| *id == CURSOR_SUB_ID));
    assert!(!ids.iter().any(|(_, id)| *id == OPENCODE_CHILD));
}

#[test]
fn resolves_by_title_prefix_and_agent() {
    let world = World::new();
    let claude = resolve(&world.homes, "claude:disaster-recovery").unwrap();
    assert_eq!(claude.session_id, CLAUDE_ID);
    assert!(claude.live);
    let cursor = resolve(&world.homes, "cursor:Test rounds").unwrap();
    assert_eq!(cursor.cwd.as_deref(), Some("/tmp/cloud"));
    let miss = resolve(&world.homes, "claude:does-not-exist").unwrap_err();
    assert!(matches!(miss, Error::NotFound(_)));
}

#[test]
fn reads_compact_handoff_for_each_agent() {
    let world = World::new();
    let claude = read_transcript(&world.homes, "claude:disaster-recovery", 20).unwrap();
    assert_eq!(
        claude.last_user_request.as_deref(),
        Some("run the 109 point matrix")
    );
    assert!(
        claude
            .last_assistant_action
            .as_ref()
            .unwrap()
            .contains("starting")
    );
    assert!(
        claude
            .turns
            .iter()
            .any(|turn| turn.tools.contains(&"Bash".into()))
    );
    assert!(claude.inert);

    let grok = read_transcript(&world.homes, "grok:Queue GC", 20).unwrap();
    assert!(
        grok.turns
            .iter()
            .any(|turn| turn.text.contains("dedicated databases"))
    );
    assert!(
        grok.last_user_request
            .as_deref()
            .unwrap()
            .contains("second turn")
    );
    assert!(
        grok.turns
            .iter()
            .any(|turn| turn.tools.contains(&"shell".into()))
    );

    let codex = read_transcript(&world.homes, "codex:Billing", 20).unwrap();
    assert_eq!(
        codex.last_user_request.as_deref(),
        Some("fix billing worker memory")
    );

    let cursor = read_transcript(&world.homes, "cursor:Test rounds", 20).unwrap();
    assert_eq!(
        cursor.last_user_request.as_deref(),
        Some("Pull the 109 point matrix from Claude")
    );
    assert!(
        cursor
            .turns
            .iter()
            .any(|turn| turn.tools.contains(&"Grep".into()))
    );

    let opencode = read_transcript(&world.homes, "opencode:zone-dev", 20).unwrap();
    assert!(
        opencode
            .last_user_request
            .as_deref()
            .unwrap()
            .contains("zone-dev --simple check")
    );
    assert!(
        opencode
            .last_assistant_action
            .as_deref()
            .unwrap()
            .contains("all checks passed")
    );
}

#[test]
fn searches_across_harnesses() {
    let world = World::new();
    let hits = search_transcripts(&world.homes, "109 point", None, false, 10).unwrap();
    let agents: Vec<_> = hits.iter().map(|hit| hit.session.agent).collect();
    assert!(agents.contains(&Agent::Claude));
    assert!(agents.contains(&Agent::Cursor));
    let billing = search_transcripts(
        &world.homes,
        "billing worker",
        Some(Agent::Codex),
        false,
        10,
    )
    .unwrap();
    assert_eq!(billing.len(), 1);
    let zone =
        search_transcripts(&world.homes, "zone-dev", Some(Agent::OpenCode), false, 10).unwrap();
    assert_eq!(zone.len(), 1);
    let limited = search_transcripts(&world.homes, "109 point", None, false, 1).unwrap();
    assert_eq!(limited.len(), 1);
}

#[test]
fn mailbox_roundtrip_and_cursor_has_no_live_inject() {
    let world = World::new();
    let session = resolve(&world.homes, "cursor:Test rounds").unwrap();
    let delivered = deliver_live(&world.homes, &session, "handoff").unwrap();
    assert!(delivered.is_empty());
    let caller = Caller {
        agent: Some(Agent::Grok),
        session_id: Some(GROK_ID.into()),
    };
    let mail = compose(
        &caller,
        Agent::Cursor,
        session.session_id.clone(),
        "dense context: failing query in billing.rs".into(),
        delivered,
    );
    mailbox::post(&world.homes, &mail).unwrap();
    let inbox = mailbox::inbox(
        &world.homes,
        &Caller {
            agent: Some(Agent::Cursor),
            session_id: Some(session.session_id.clone()),
        },
        None,
        None,
    )
    .unwrap();
    assert_eq!(inbox.len(), 1);
    assert!(inbox[0].message.contains("billing.rs"));
}

#[test]
fn reads_opencode_json_fallback_after_storage_layout_fix() {
    let dir = tempfile::tempdir().unwrap();
    let homes = Homes::isolated(dir.path());
    write_opencode_json_only(&homes.opencode);
    let session = resolve(&homes, "opencode:json fallback").unwrap();
    let transcript = read_transcript(&homes, "opencode:json fallback", 20).unwrap();
    assert_eq!(session.session_id, "ses_jsononly");
    assert!(
        transcript
            .last_user_request
            .as_deref()
            .unwrap()
            .contains("json fallback prompt about dedicated databases")
    );
    assert!(
        transcript
            .turns
            .iter()
            .any(|turn| turn.tools.contains(&"grep".into()))
    );
}

#[test]
fn claude_uds_inject_writes_auth_and_user_frames() {
    let dir = tempfile::tempdir().unwrap();
    let homes = Homes::isolated(dir.path());
    let sock_dir = dir.path().join("socks");
    fs::create_dir_all(&sock_dir).unwrap();
    let socket = sock_dir.join("4242.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let received = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
        let mut buf = String::new();
        std::io::Read::read_to_string(&mut stream, &mut buf).ok();
        buf
    });
    write(
        &homes
            .claude
            .join("sessions")
            .join(format!("{}.json", pid())),
        &json!({
            "pid": pid(),
            "sessionId": CLAUDE_ID,
            "cwd": "/tmp/dr",
            "entrypoint": "cli",
            "name": "uds-target",
            "messagingSocketPath": socket,
            "startedAt": now_ms()
        })
        .to_string(),
    );
    write(
        &homes
            .claude
            .join("sessions")
            .join(format!("{}.deadbeef.key", pid())),
        r#"{"peerToken":"test-token","procStart":"now"}"#,
    );
    write(
        &homes
            .claude
            .join("projects")
            .join("tmp-dr")
            .join(format!("{CLAUDE_ID}.jsonl")),
        r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"hi"}]}}
"#,
    );
    let session = resolve(&homes, "claude:uds-target").unwrap();
    let delivered = deliver_live(&homes, &session, "from magents").unwrap();
    assert_eq!(delivered, vec!["claude-uds".to_string()]);
    let body = received.join().unwrap();
    assert!(body.contains("\"type\":\"auth\""));
    assert!(body.contains("test-token"));
    assert!(body.contains("\"type\":\"user\""));
    assert!(body.contains("from magents"));
}

#[test]
fn opencode_run_is_invoked_with_session_and_dir() {
    let dir = tempfile::tempdir().unwrap();
    let homes = Homes::isolated(dir.path());
    write_opencode_sqlite(&homes);
    let bin = dir.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let stub = bin.join("opencode");
    let log = dir.path().join("opencode-args.txt");
    let _guard = crate::test_env::lock(&["MAGENTS_OPENCODE_BIN"]);
    write(
        &stub,
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$0\" \"$@\" > '{}'\n",
            log.display()
        ),
    );
    Command::new("chmod")
        .args(["+x", stub.to_str().unwrap()])
        .status()
        .unwrap();
    let session = resolve(&homes, "opencode:zone-dev").unwrap();
    unsafe { std::env::set_var("MAGENTS_OPENCODE_BIN", &stub) };
    let delivered = deliver_live(&homes, &session, "continue the zone-dev checks").unwrap();
    let args = wait_for_file(&log, Duration::from_secs(2));
    unsafe { std::env::remove_var("MAGENTS_OPENCODE_BIN") };
    assert_eq!(delivered, vec!["opencode-run".to_string()]);
    assert!(args.contains("run"), "opencode stub args were {args:?}");
    assert!(args.contains("--session"));
    assert!(args.contains(OPENCODE_ID));
    assert!(args.contains("/tmp/zone"));
}

#[test]
fn filters_limits_latest_ambiguous_and_desktop() {
    let world = World::new();
    let defaulted = list_sessions(&world.homes, &ListFilter::default()).unwrap();
    assert!(!defaulted.is_empty());
    assert!(defaulted.len() <= 20);

    let live = list_sessions(
        &world.homes,
        &ListFilter {
            agent: Some(Agent::Claude),
            query: Some("disaster".into()),
            live_only: true,
            include_archived: false,
            limit: 1,
        },
    )
    .unwrap();
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].session_id, CLAUDE_ID);
    assert_eq!(live[0].desktop_id.as_deref(), Some("desktop-live"));
    assert_eq!(live[0].branch.as_deref(), Some("fix/dr"));

    let archived = list_sessions(
        &world.homes,
        &ListFilter {
            agent: Some(Agent::Claude),
            query: None,
            live_only: false,
            include_archived: true,
            limit: 0,
        },
    )
    .unwrap();
    assert!(
        archived
            .iter()
            .any(|session| session.session_id == "22222222-2222-4222-8222-222222222222")
    );

    let missed = list_sessions(
        &world.homes,
        &ListFilter {
            agent: Some(Agent::Grok),
            query: Some("no-such-haystack-term".into()),
            live_only: false,
            include_archived: false,
            limit: 20,
        },
    )
    .unwrap();
    assert!(missed.is_empty());

    let truncated = list_sessions(
        &world.homes,
        &ListFilter {
            agent: None,
            query: None,
            live_only: false,
            include_archived: true,
            limit: 1,
        },
    )
    .unwrap();
    assert_eq!(truncated.len(), 1);

    let latest = resolve(&world.homes, "claude:latest").unwrap();
    assert_eq!(latest.session_id, CLAUDE_ID);
    let by_pid = resolve(&world.homes, &format!("claude:{}", pid())).unwrap();
    assert_eq!(by_pid.session_id, CLAUDE_ID);
    let twins = resolve(&world.homes, &pid().to_string()).unwrap_err();
    assert!(matches!(twins, Error::Ambiguous { .. }));
    let empty = resolve(&world.homes, "  ").unwrap_err();
    assert!(empty.to_string().contains("required"));
    let none = resolve(&world.homes, "grok:latest-missing-zzzz").unwrap_err();
    assert!(matches!(none, Error::NotFound(_)));

    let ambiguous = resolve(&world.homes, "tmp").unwrap_err();
    assert!(matches!(ambiguous, Error::Ambiguous { .. }));

    let no_latest = resolve(
        &Homes::isolated(tempfile::tempdir().unwrap().path()),
        "latest",
    )
    .unwrap_err();
    assert!(matches!(no_latest, Error::NotFound(_)));
}

#[test]
fn cursor_headers_without_table() {
    let dir = tempfile::tempdir().unwrap();
    let homes = Homes::isolated(dir.path());
    let db = homes
        .cursor_app
        .join("User")
        .join("globalStorage")
        .join("state.vscdb");
    fs::create_dir_all(db.parent().unwrap()).unwrap();
    Connection::open(&db)
        .unwrap()
        .execute("CREATE TABLE other (id TEXT)", [])
        .unwrap();
    write(
        &homes
            .cursor
            .join("projects")
            .join("x")
            .join("agent-transcripts")
            .join("eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee")
            .join("eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee.jsonl"),
        r#"{"role":"user","message":{"content":[{"type":"text","text":"orphan cursor"}]}}
"#,
    );
    let sessions = list_sessions(
        &homes,
        &ListFilter {
            agent: Some(Agent::Cursor),
            query: None,
            live_only: false,
            include_archived: true,
            limit: 0,
        },
    )
    .unwrap();
    assert_eq!(sessions.len(), 1);
}

#[test]
fn fallback_rollout_without_sessions_dir() {
    let dir = tempfile::tempdir().unwrap();
    let homes = Homes::isolated(dir.path());
    let db = homes.codex.join("state_3.sqlite");
    fs::create_dir_all(&homes.codex).unwrap();
    let connection = Connection::open(&db).unwrap();
    connection
        .execute(
            "CREATE TABLE threads (
                id TEXT PRIMARY KEY,
                title TEXT,
                cwd TEXT,
                git_branch TEXT,
                model TEXT,
                archived INTEGER,
                updated_at_ms INTEGER,
                rollout_path TEXT,
                source TEXT
            )",
            [],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO threads VALUES ('01norollout', 'x', NULL, NULL, NULL, 0, 1, NULL, 'cli')",
            [],
        )
        .unwrap();
    let sessions = list_sessions(
        &homes,
        &ListFilter {
            agent: Some(Agent::Codex),
            query: None,
            live_only: false,
            include_archived: true,
            limit: 0,
        },
    )
    .unwrap();
    assert_eq!(sessions.len(), 1);
    assert!(sessions[0].transcript_path.is_none());
}

#[test]
fn empty_homes_and_cursor_without_db() {
    let dir = tempfile::tempdir().unwrap();
    let homes = Homes::isolated(dir.path());
    assert!(list_sessions(&homes, &all_agents()).unwrap().is_empty());
    assert!(crate::discover::claude_transcript_index(&homes.claude).is_empty());

    write(
        &homes
            .cursor
            .join("projects")
            .join("plain")
            .join("agent-transcripts")
            .join("cccccccc-cccc-4ccc-8ccc-cccccccccccc")
            .join("cccccccc-cccc-4ccc-8ccc-cccccccccccc.jsonl"),
        r#"{"role":"user","message":{"content":[{"type":"text","text":"no composer header title here"}]}}
{"role":"system","message":{"content":[{"type":"text","text":"skip"}]}}
{"role":"assistant","message":{"content":[]}}
"#,
    );
    write(
        &homes
            .cursor
            .join("projects")
            .join("blank")
            .join("agent-transcripts")
            .join("dddddddd-dddd-4ddd-8ddd-dddddddddddd")
            .join("dddddddd-dddd-4ddd-8ddd-dddddddddddd.jsonl"),
        r#"{"role":"user","message":{"content":[{"type":"text","text":"   "}]}}
"#,
    );
    write(
        &homes.cursor.join("projects").join("file-not-dir"),
        "not a directory",
    );
    write(
        &homes
            .cursor
            .join("projects")
            .join("plain")
            .join("agent-transcripts")
            .join("subagents")
            .join("x.jsonl"),
        "skip",
    );
    let sessions = list_sessions(
        &homes,
        &ListFilter {
            agent: Some(Agent::Cursor),
            query: None,
            live_only: false,
            include_archived: true,
            limit: 0,
        },
    )
    .unwrap();
    assert!(sessions.iter().any(|session| {
        session
            .title
            .as_deref()
            .unwrap_or("")
            .contains("no composer header")
    }));
    assert!(
        sessions
            .iter()
            .any(|session| session.session_id.starts_with("dddddddd") && session.title.is_none())
    );
}

#[test]
fn opencode_json_parent_and_archived() {
    let dir = tempfile::tempdir().unwrap();
    let homes = Homes::isolated(dir.path());
    write_opencode_json_only(&homes.opencode);
    write(
        &homes
            .opencode
            .join("storage")
            .join("session")
            .join("proj")
            .join("ses_child.json"),
        &json!({
            "id": "ses_child",
            "parentID": "ses_jsononly",
            "title": "child"
        })
        .to_string(),
    );
    write(
        &homes
            .opencode
            .join("storage")
            .join("session")
            .join("proj")
            .join("ses_arch.json"),
        &json!({
            "id": "ses_arch",
            "title": "archived json",
            "time": { "updated": now_ms(), "archived": now_ms() }
        })
        .to_string(),
    );
    let sessions = list_sessions(
        &homes,
        &ListFilter {
            agent: Some(Agent::OpenCode),
            query: None,
            live_only: false,
            include_archived: true,
            limit: 0,
        },
    )
    .unwrap();
    assert!(
        sessions
            .iter()
            .any(|session| session.session_id == "ses_jsononly")
    );
    assert!(sessions.iter().any(|session| session.archived));
    assert!(
        !sessions
            .iter()
            .any(|session| session.session_id == "ses_child")
    );
}

fn wait_for_file(path: &Path, timeout: Duration) -> String {
    let started = std::time::Instant::now();
    loop {
        if let Ok(body) = fs::read_to_string(path)
            && !body.trim().is_empty()
        {
            return body;
        }
        if started.elapsed() > timeout {
            return String::new();
        }
        thread::sleep(Duration::from_millis(20));
    }
}
