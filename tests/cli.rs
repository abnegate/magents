use serde_json::Value;
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;

static ENV: Mutex<()> = Mutex::new(());

const CLAUDE_ID: &str = "11111111-1111-4111-8111-111111111111";
const CURSOR_ID: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const GROK_ID: &str = "01testgrok0000000000000000";

struct Harness {
    _dir: TempDir,
    root: std::path::PathBuf,
}

impl Harness {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        write_tree(&root);
        Self { _dir: dir, root }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_magents"));
        command
            .env("HOME", &self.root)
            .env("CLAUDE_CONFIG_DIR", self.root.join("claude"))
            .env("GROK_HOME", self.root.join("grok"))
            .env("CODEX_HOME", self.root.join("codex"))
            .env("CURSOR_HOME", self.root.join("cursor"))
            .env("CURSOR_APP_SUPPORT", self.root.join("cursor-app"))
            .env("OPENCODE_DATA", self.root.join("opencode"))
            .env("MAGENTS_HOME", self.root.join("magents"))
            .env("XDG_DATA_HOME", self.root.join("xdg-data"))
            .args(args);
        command
    }

    fn json(&self, args: &[&str]) -> Value {
        let output = self.command(args).output().unwrap();
        assert!(
            output.status.success(),
            "magents {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
}

fn write(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, body).unwrap();
}

fn write_tree(root: &Path) {
    let pid = std::process::id();
    write(
        &root.join("claude/sessions").join(format!("{pid}.json")),
        &format!(
            r#"{{"pid":{pid},"sessionId":"{CLAUDE_ID}","cwd":"/tmp/dr","entrypoint":"cli","name":"disaster-recovery","startedAt":{ts}}}"#,
            ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        ),
    );
    write(
        &root
            .join("claude/projects/tmp-dr")
            .join(format!("{CLAUDE_ID}.jsonl")),
        r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"run the 109 point matrix"}]}}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"starting verification"}]}}
"#,
    );
    write(
        &root.join("grok/active_sessions.json"),
        &format!(r#"[{{"session_id":"{GROK_ID}","pid":{pid}}}]"#),
    );
    write(
        &root
            .join("grok/sessions/%2Ftmp%2Fedge")
            .join(GROK_ID)
            .join("summary.json"),
        &format!(
            r#"{{"info":{{"id":"{GROK_ID}","cwd":"/tmp/edge"}},"generated_title":"Queue GC PRs","session_kind":"main","last_active_at":"2026-08-29T00:00:00Z"}}"#,
        ),
    );
    write(
        &root
            .join("grok/sessions/%2Ftmp%2Fedge")
            .join(GROK_ID)
            .join("updates.jsonl"),
        r#"{"params":{"update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"fix the dedicated databases leak"}}}}
{"params":{"update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"closing the fd"}}}}
{"params":{"update":{"sessionUpdate":"turn_completed"}}}
"#,
    );
    write(
        &root
            .join("cursor/projects/Users-tmp-cloud/agent-transcripts")
            .join(CURSOR_ID)
            .join(format!("{CURSOR_ID}.jsonl")),
        r#"{"role":"user","message":{"content":[{"type":"text","text":"<user_query>Pull the 109 point matrix from Claude</user_query>"}]}}
{"role":"assistant","message":{"content":[{"type":"text","text":"found the suite"},{"type":"tool_use","name":"Grep"}]}}
"#,
    );
    write(
        &root.join("opencode/storage/session/proj/ses_jsononly.json"),
        r#"{"id":"ses_jsononly","directory":"/tmp/json-only","title":"json fallback session","time":{"updated":1700000000000}}"#,
    );
    write(
        &root.join("opencode/storage/message/ses_jsononly/msg_1.json"),
        r#"{"role":"user"}"#,
    );
    write(
        &root.join("opencode/storage/part/msg_1/prt_1.json"),
        r#"{"type":"text","text":"json fallback prompt about dedicated databases"}"#,
    );
}

#[test]
fn cli_lists_reads_searches_and_mails_across_harnesses() {
    let _lock = ENV.lock().unwrap();
    let harness = Harness::new();

    let listed = harness.json(&["list", "-n", "20"]);
    let agents: Vec<_> = listed
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["agent"].as_str().unwrap().to_string())
        .collect();
    assert!(agents.contains(&"claude".into()));
    assert!(agents.contains(&"grok".into()));
    assert!(agents.contains(&"cursor".into()));
    assert!(agents.contains(&"opencode".into()));

    let claude = harness.json(&["get", "claude:disaster-recovery"]);
    assert_eq!(claude["session_id"], CLAUDE_ID);
    assert_eq!(claude["live"], true);

    let transcript = harness.json(&["read", "cursor:109 point", "-n", "10"]);
    assert!(
        transcript["last_user_request"]
            .as_str()
            .unwrap()
            .contains("109 point matrix")
    );
    assert_eq!(transcript["inert"], true);

    let grok = harness.json(&["read", "grok:Queue GC", "-n", "10"]);
    assert!(
        grok["last_user_request"]
            .as_str()
            .unwrap()
            .contains("dedicated databases")
    );

    let opencode = harness.json(&["read", "opencode:json fallback", "-n", "10"]);
    assert!(
        opencode["last_user_request"]
            .as_str()
            .unwrap()
            .contains("dedicated databases")
    );

    let hits = harness.json(&["search", "109 point"]);
    assert!(
        hits.as_array()
            .unwrap()
            .iter()
            .any(|hit| hit["session"]["agent"] == "claude")
    );
    assert!(
        hits.as_array()
            .unwrap()
            .iter()
            .any(|hit| hit["session"]["agent"] == "cursor")
    );

    let sent = harness.json(&[
        "send",
        "cursor:109 point",
        "dense handoff: the matrix is in cloud/docs",
    ]);
    assert_eq!(sent["queued"], true);
    assert!(sent["delivered"].as_array().unwrap().is_empty());

    let inbox = harness.json(&["inbox", "--agent", "cursor", "--session", CURSOR_ID]);
    assert_eq!(inbox.as_array().unwrap().len(), 1);
    assert!(
        inbox[0]["message"]
            .as_str()
            .unwrap()
            .contains("matrix is in cloud/docs")
    );

    let live = harness.json(&["list", "--live", "--agent", "claude", "-n", "5"]);
    assert!(
        live.as_array()
            .unwrap()
            .iter()
            .all(|row| row["live"] == true)
    );

    let search = harness.json(&[
        "search",
        "dedicated databases",
        "--agent",
        "grok",
        "-n",
        "5",
    ]);
    assert!(!search.as_array().unwrap().is_empty());

    let miss = harness
        .command(&["get", "no-such-session-xyz"])
        .output()
        .unwrap();
    assert!(!miss.status.success());
}

#[test]
fn cli_installs_cursor_and_opencode_mcp_config() {
    let _lock = ENV.lock().unwrap();
    let harness = Harness::new();
    let output = harness
        .command(&["install", "--cursor", "--opencode"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let cursor = fs::read_to_string(harness.root.join(".cursor/mcp.json")).unwrap();
    assert!(cursor.contains("magents"));
    let opencode = fs::read_to_string(harness.root.join(".config/opencode/opencode.json")).unwrap();
    assert!(opencode.contains("magents"));
    assert!(opencode.contains("\"type\": \"local\""));
}

#[test]
fn mcp_stdio_exits_when_stdin_closes() {
    let _lock = ENV.lock().unwrap();
    let harness = Harness::new();
    let mut child = harness
        .command(&["mcp"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdin.take());
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() > Duration::from_secs(3) => {
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
            Ok(None) => thread::sleep(Duration::from_millis(20)),
            Err(_) => break,
        }
    }
}
