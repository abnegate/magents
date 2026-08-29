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

struct World {
    _dir: TempDir,
    homes: Homes,
}

impl World {
    fn new() -> Self {
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
        &homes
            .claude
            .join("projects")
            .join("tmp-dr")
            .join(format!("{CLAUDE_ID}.jsonl")),
        r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"run the 109 point matrix"}]}}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"starting verification"},{"type":"tool_use","name":"Bash"}]}}
{"type":"user","isMeta":true,"message":{"role":"user","content":[{"type":"text","text":"ignore me"}]}}
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
{"params":{"update":{"sessionUpdate":"turn_completed"}}}
"#,
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
        grok.last_user_request
            .as_deref()
            .unwrap_or("")
            .contains("dedicated databases")
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
    assert_eq!(
        opencode.last_assistant_action.as_deref(),
        Some("all checks passed")
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
    assert_eq!(
        transcript.last_user_request.as_deref(),
        Some("json fallback prompt about dedicated databases")
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
