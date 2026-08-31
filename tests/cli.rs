use serde_json::Value;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;

static ENV: Mutex<()> = Mutex::new(());

const CLAUDE_ID: &str = "11111111-1111-4111-8111-111111111111";
const CODEX_SPAWN_ID: &str = "22222222-2222-4222-8222-222222222222";
const CURSOR_SPAWN_ID: &str = "33333333-3333-4333-8333-333333333333";
const CURSOR_ID: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const GROK_ID: &str = "01testgrok0000000000000000";
const OPENCODE_SPAWN_ID: &str = "ses_spawned_opencode";

const AGENT_STUB: &str = r#"
if [ "$1" = 'create-chat' ]; then
    printf '%s\n' '33333333-3333-4333-8333-333333333333'
    exit 0
fi
printf '%s\n' "$@" > "$MAGENTS_TEST_ARGS"
printf '%s\n' "$$" > "$MAGENTS_TEST_AGENT_PID"
printf '%s\n' "$PPID" > "$MAGENTS_TEST_SUPERVISOR_PID"
printf '%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s\n' \
    "${CLAUDE_CONFIG_DIR-}" "${CODEX_HOME-}" "${CURSOR_CONFIG_DIR-}" "${CURSOR_DATA_DIR-}" \
    "${GROK_HOME-}" "${XDG_DATA_HOME-}" "${XDG_CONFIG_HOME-}" "$MAGENTS_HOME" \
    "${CLAUDE_SESSION_ID-}" \
    "${CODEX_SESSION_ID-}" "${CURSOR_SESSION_ID-}" "${OPENCODE_SESSION_ID-}" \
    > "$MAGENTS_TEST_ENV"
cat > "$MAGENTS_TEST_STDIN"
case "${MAGENTS_TEST_MODE-}" in
    early) exit 9 ;;
    malformed) printf '%s\n' 'raw malformed startup SECRET-OUTPUT'; exit 0 ;;
    no-id) printf '%s\n' '{"type":"thread.started"}'; exit 0 ;;
    timeout) sleep 1; exit 0 ;;
    cancel) sleep 5; exit 0 ;;
    orphan)
        (sleep 5) &
        printf '%s\n' "$!" > "$MAGENTS_TEST_DESCENDANT_PID"
        exit 0
        ;;
esac
session=''
previous=''
next=''
for argument in "$@"; do
    if [ "$next" = 'session' ]; then session="$argument"; next=''; fi
    if [ "$argument" = '--session-id' ] || [ "$argument" = '--resume' ] || [ "$argument" = '--session' ]; then next='session'; fi
    if [ "$previous" = 'resume' ] && [ -z "$session" ]; then session="$argument"; fi
    previous="$argument"
done
case "$MAGENTS_TEST_AGENT" in
    claude|cursor) printf '{"type":"system","subtype":"init","session_id":"%s"}\n' "$session" ;;
    codex) printf '%s\n' '{"type":"thread.started","thread_id":"22222222-2222-4222-8222-222222222222"}' ;;
    grok) printf '{"method":"session/update","params":{"sessionId":"%s"}}\n' "$session" ;;
    opencode) printf '%s\n' '{"type":"step_start","sessionID":"ses_spawned_opencode"}' ;;
esac
index=0
while [ "$index" -lt 300 ]; do
    printf '%s\n' 'raw stdout SECRET-OUTPUT'
    printf '%s\n' 'raw stderr SECRET-TOKEN' >&2
    index=$((index + 1))
done
sleep "${MAGENTS_TEST_HOLD_SECONDS:-0.3}"
printf '%s\n' done > "$MAGENTS_TEST_DONE"
"#;

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
            .env("CURSOR_CONFIG_DIR", self.root.join("cursor-config"))
            .env("CURSOR_DATA_DIR", self.root.join("cursor"))
            .env("CURSOR_APP_SUPPORT", self.root.join("cursor-app"))
            .env("MAGENTS_HOME", self.root.join("magents"))
            .env("XDG_CONFIG_HOME", self.root.join("opencode-config"))
            .env("XDG_DATA_HOME", &self.root)
            .args(args);
        for key in [
            "CLAUDE_CODE_MESSAGING_SOCKET",
            "CLAUDE_PROJECT_DIR",
            "CLAUDE_SESSION_ID",
            "CODEX_SESSION_ID",
            "CODEX_THREAD_ID",
            "COMPOSER_SESSION_ID",
            "CURSOR_AGENT",
            "CURSOR_PROJECT_DIR",
            "CURSOR_SESSION_ID",
            "GROK_SESSION_ID",
            "OPENCODE_DIRECTORY",
            "OPENCODE_SERVER",
            "OPENCODE_SESSION",
            "OPENCODE_SESSION_ID",
        ] {
            command.env_remove(key);
        }
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

fn write_executable(path: &Path, body: &str) {
    write(path, &format!("#!/bin/sh\nset -eu\n{body}\n"));
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn output_with_stdin(command: &mut Command, input: &str) -> Output {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn wait_for(path: &Path, timeout: Duration) -> String {
    let started = Instant::now();
    loop {
        if let Ok(body) = fs::read_to_string(path)
            && !body.trim().is_empty()
        {
            return body;
        }
        assert!(
            started.elapsed() < timeout,
            "timed out waiting for {}",
            path.display()
        );
        thread::sleep(Duration::from_millis(20));
    }
}

fn process_exists(pid: &str) -> bool {
    Command::new("kill")
        .args(["-0", pid.trim()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn process_command(pid: u32) -> String {
    let output = Command::new("ps")
        .args(["-ww", "-o", "command=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap()
}

fn wait_for_exit(pid: &str, timeout: Duration) {
    let started = Instant::now();
    while process_exists(pid) {
        assert!(started.elapsed() < timeout, "process {pid} did not exit");
        thread::sleep(Duration::from_millis(20));
    }
}

fn configure_stub(
    command: &mut Command,
    harness: &Harness,
    stub: &Path,
    agent: &str,
    variable: &str,
    tag: &str,
) -> std::path::PathBuf {
    let artifacts = harness.root.join("artifacts").join(tag);
    fs::create_dir_all(&artifacts).unwrap();
    command
        .env(variable, stub)
        .env("MAGENTS_TEST_AGENT", agent)
        .env("MAGENTS_TEST_ARGS", artifacts.join("args"))
        .env("MAGENTS_TEST_STDIN", artifacts.join("stdin"))
        .env("MAGENTS_TEST_ENV", artifacts.join("environment"))
        .env("MAGENTS_TEST_AGENT_PID", artifacts.join("agent-pid"))
        .env(
            "MAGENTS_TEST_DESCENDANT_PID",
            artifacts.join("descendant-pid"),
        )
        .env(
            "MAGENTS_TEST_SUPERVISOR_PID",
            artifacts.join("supervisor-pid"),
        )
        .env("MAGENTS_TEST_DONE", artifacts.join("done"))
        .env_remove("MAGENTS_TEST_MODE");
    artifacts
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
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"starting verification of src/lib.rs"},{"type":"tool_use","name":"Read","input":{"file_path":"src/lib.rs"}}]}}
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
    write(
        &root.join("claude/projects/tmp-dr/memory/MEMORY.md"),
        "CLAUDE_MEMORY_NEEDLE dedicated databases runbook\n",
    );
    write(
        &root.join("codex/memories/MEMORY.md"),
        "CODEX_MEMORY_NEEDLE billing worker cache\n",
    );
    write(
        &root.join("grok/memory/MEMORY.md"),
        "GROK_MEMORY_NEEDLE edge queue notes\n",
    );
    write(
        &root.join("grok/memory/tmp-edge/MEMORY.md"),
        "GROK_WORKSPACE_MEMORY_NEEDLE workspace notes\nMEMORY_NEEDLE\n",
    );
}

#[test]
fn cli_lists_reads_searches_and_mails_across_harnesses() {
    let _lock = ENV.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
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
    assert_eq!(sent["delivered"], serde_json::json!(["cursor-cli-failed"]));

    let inbox = harness.json(&["inbox", "--agent", "cursor", "--session", CURSOR_ID]);
    assert_eq!(inbox["items"].as_array().unwrap().len(), 1);
    assert!(
        inbox["items"][0]["message"]
            .as_str()
            .unwrap()
            .contains("matrix is in cloud/docs")
    );
    assert_eq!(inbox["unread"], 1);

    let digest = harness.json(&["digest", "claude:disaster-recovery", "-n", "8"]);
    assert!(
        digest["last_user_request"]
            .as_str()
            .unwrap()
            .contains("109 point matrix")
    );
    let files = harness.json(&["files", "claude:disaster-recovery"]);
    assert!(
        files["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|path| path == "src/lib.rs")
    );

    let acked = harness.json(&[
        "ack",
        "--agent",
        "cursor",
        "--session",
        CURSOR_ID,
        "--through",
        inbox["items"][0]["id"].as_str().unwrap(),
    ]);
    assert_eq!(acked["unread"], 0);
    let unread = harness.json(&[
        "inbox",
        "--agent",
        "cursor",
        "--session",
        CURSOR_ID,
        "--unread",
    ]);
    assert!(unread["items"].as_array().unwrap().is_empty());

    let note_cwd = harness.root.join("scratch-cwd");
    fs::create_dir_all(&note_cwd).unwrap();
    let put = harness.json(&[
        "put-note",
        "--cwd",
        note_cwd.to_str().unwrap(),
        "shared scratch",
    ]);
    assert_eq!(put["content"], "shared scratch");
    let got = harness.json(&["get-note", "--cwd", note_cwd.to_str().unwrap()]);
    assert_eq!(got["content"], "shared scratch");

    let memory = harness.json(&[
        "read-memory",
        "--agent",
        "claude",
        "--project",
        "tmp-dr",
        "--file",
        "MEMORY.md",
    ]);
    assert!(
        memory["content"]
            .as_str()
            .unwrap()
            .contains("CLAUDE_MEMORY_NEEDLE")
    );

    let who = harness.json(&["whoami"]);
    assert!(who["agent"].is_null());

    let listed = harness.json(&["list", "--cwd", "/tmp/dr", "-n", "20"]);
    assert!(
        listed
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["session_id"] == CLAUDE_ID)
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

    let memories = harness.json(&["search-memories", "MEMORY_NEEDLE", "-n", "10"]);
    let memory_hits = memories.as_array().unwrap();
    assert!(memory_hits.iter().any(|hit| {
        hit["agent"] == "claude"
            && hit["project"] == "tmp-dr"
            && hit["snippet"]
                .as_str()
                .unwrap()
                .contains("CLAUDE_MEMORY_NEEDLE")
    }));
    assert!(memory_hits.iter().any(|hit| hit["agent"] == "codex"));
    assert!(
        memory_hits
            .iter()
            .any(|hit| { hit["agent"] == "grok" && hit["project"] == "global" })
    );
    assert!(memory_hits.iter().any(|hit| {
        hit["agent"] == "grok"
            && hit["project"] == "tmp-edge"
            && hit["snippet"]
                .as_str()
                .unwrap()
                .contains("GROK_WORKSPACE_MEMORY_NEEDLE")
    }));
    let claude = harness.json(&["search-memories", "MEMORY_NEEDLE", "--agent", "claude"]);
    assert_eq!(claude.as_array().unwrap().len(), 1);
    assert_eq!(claude[0]["agent"], "claude");
    assert_eq!(claude[0]["project"], "tmp-dr");
    let grok = harness.json(&["search-memories", "MEMORY_NEEDLE", "--agent", "grok"]);
    assert_eq!(grok.as_array().unwrap().len(), 2);
    let codex = harness.json(&["search-memories", "MEMORY_NEEDLE", "--agent", "codex"]);
    assert_eq!(codex.as_array().unwrap().len(), 1);
    assert_eq!(codex[0]["agent"], "codex");

    let miss = harness
        .command(&["get", "no-such-session-xyz"])
        .output()
        .unwrap();
    assert!(!miss.status.success());

    let handed = harness
        .command(&[
            "handoff",
            "cursor:109 point",
            "--reason",
            "switching windows",
        ])
        .env("GROK_SESSION_ID", GROK_ID)
        .output()
        .unwrap();
    assert!(
        handed.status.success(),
        "{}",
        String::from_utf8_lossy(&handed.stderr)
    );
    let body: Value = serde_json::from_slice(&handed.stdout).unwrap();
    assert_eq!(body["to"]["agent"], "cursor");
    assert_eq!(body["reason"], "switching windows");
}

#[test]
fn cli_search_memories_requires_query() {
    let _lock = ENV.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let harness = Harness::new();
    let missing = harness
        .command(&["search-memories", "--agent", "claude"])
        .output()
        .unwrap();
    assert!(!missing.status.success());
    let stderr = String::from_utf8_lossy(&missing.stderr);
    assert!(
        stderr.contains("<QUERY>") || stderr.contains("required arguments"),
        "{stderr}"
    );
    let empty = harness
        .command(&["search-memories", "   "])
        .output()
        .unwrap();
    assert!(!empty.status.success());
    let stderr = String::from_utf8_lossy(&empty.stderr);
    assert!(stderr.contains("query is required"), "{stderr}");
}

#[test]
fn cli_create_memory_round_trips_into_search() {
    let _lock = ENV.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let harness = Harness::new();
    let created = harness.json(&[
        "create-memory",
        "--agent",
        "claude",
        "--project",
        "tmp-dr",
        "--file",
        "dedicated-db-gaps.md",
        "CLI_CREATE_MEMORY_NEEDLE dedicated db gaps",
    ]);
    assert_eq!(created["created"], true);
    assert_eq!(created["agent"], "claude");
    assert_eq!(created["file"], "dedicated-db-gaps.md");
    assert_eq!(created["project"], "tmp-dr");
    let path = created["path"].as_str().unwrap();
    assert!(path.ends_with("claude/projects/tmp-dr/memory/dedicated-db-gaps.md"));
    assert_eq!(
        fs::read_to_string(path).unwrap(),
        "CLI_CREATE_MEMORY_NEEDLE dedicated db gaps"
    );

    let hits = harness.json(&[
        "search-memories",
        "CLI_CREATE_MEMORY_NEEDLE",
        "--agent",
        "claude",
    ]);
    assert_eq!(hits.as_array().unwrap().len(), 1);
    assert_eq!(hits[0]["file"], "dedicated-db-gaps.md");
    assert_eq!(hits[0]["path"], path);

    let overwrite = harness
        .command(&[
            "create-memory",
            "--agent",
            "claude",
            "--project",
            "tmp-dr",
            "--file",
            "dedicated-db-gaps.md",
            "should not overwrite",
        ])
        .output()
        .unwrap();
    assert!(!overwrite.status.success());
    assert!(
        String::from_utf8_lossy(&overwrite.stderr).contains("already exists"),
        "{}",
        String::from_utf8_lossy(&overwrite.stderr)
    );

    let cursor = harness
        .command(&[
            "create-memory",
            "--agent",
            "cursor",
            "--file",
            "note.md",
            "no store",
        ])
        .output()
        .unwrap();
    assert!(!cursor.status.success());
    assert!(
        String::from_utf8_lossy(&cursor.stderr).contains("no first-party memory store"),
        "{}",
        String::from_utf8_lossy(&cursor.stderr)
    );

    let escape = harness
        .command(&[
            "create-memory",
            "--agent",
            "codex",
            "--file",
            "../secret.md",
            "escape",
        ])
        .output()
        .unwrap();
    assert!(!escape.status.success());
    assert!(
        String::from_utf8_lossy(&escape.stderr).contains("markdown basename"),
        "{}",
        String::from_utf8_lossy(&escape.stderr)
    );

    let missing = harness
        .command(&["create-memory", "--agent", "codex"])
        .output()
        .unwrap();
    assert!(!missing.status.success());
}

#[test]
fn cli_spawns_all_harnesses_and_routes_later_messages() {
    let _lock = ENV.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let harness = Harness::new();
    let stub = harness.root.join("agent-stub");
    write_executable(&stub, AGENT_STUB);
    let cwd = fs::canonicalize(&harness.root).unwrap();
    let cwd_text = cwd.to_str().unwrap();
    let cases = [
        ("claude", "MAGENTS_CLAUDE_BIN", "claude-print"),
        ("codex", "MAGENTS_CODEX_BIN", "codex-exec"),
        ("cursor", "MAGENTS_CURSOR_BIN", "cursor-agent"),
        ("grok", "MAGENTS_GROK_BIN", "grok-stream"),
        ("opencode", "MAGENTS_OPENCODE_BIN", "opencode-run"),
    ];

    for (agent, variable, origin) in cases {
        let prompt = format!("independent {agent} task SECRET-PROMPT");
        let mut command = harness.command(&["spawn", agent, "--cwd", cwd_text]);
        let artifacts = configure_stub(
            &mut command,
            &harness,
            &stub,
            agent,
            variable,
            &format!("spawn-{agent}"),
        );
        assert!(
            !command
                .get_args()
                .any(|argument| argument.to_string_lossy().contains(&prompt))
        );
        let output = output_with_stdin(&mut command, &prompt);
        assert!(
            output.status.success(),
            "{agent}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let body: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(body["accepted"], true);
        assert_eq!(body["status"], "starting");
        assert_eq!(body["session"]["agent"], agent);
        assert_eq!(body["session"]["cwd"], cwd_text);
        assert_eq!(body["session"]["live"], false);
        assert_eq!(body["session"]["origin"], origin);
        assert!(body.get("mail_id").is_none());
        let session_id = body["session"]["session_id"].as_str().unwrap();
        assert!(!session_id.is_empty());
        match agent {
            "codex" => assert_eq!(session_id, CODEX_SPAWN_ID),
            "cursor" => assert_eq!(session_id, CURSOR_SPAWN_ID),
            "opencode" => assert_eq!(session_id, OPENCODE_SPAWN_ID),
            _ => {}
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        for private in [
            &prompt,
            "SECRET-TOKEN",
            "SECRET-OUTPUT",
            "raw stdout",
            "raw stderr",
        ] {
            assert!(!stdout.contains(private), "{agent} stdout leaked {private}");
            assert!(!stderr.contains(private), "{agent} stderr leaked {private}");
        }

        let args = wait_for(&artifacts.join("args"), Duration::from_secs(2));
        let args = args.lines().collect::<Vec<_>>();
        let expected = match agent {
            "claude" => vec![
                "-p",
                "--verbose",
                "--output-format",
                "stream-json",
                "--session-id",
                session_id,
            ],
            "codex" => vec!["exec", "--json", "-C", cwd_text, "-"],
            "cursor" => vec![
                "-p",
                "--output-format",
                "stream-json",
                "--resume",
                session_id,
                "--workspace",
                cwd_text,
            ],
            "grok" => vec![
                "--cwd",
                cwd_text,
                "--session-id",
                session_id,
                "--output-format",
                "streaming-json",
                "--prompt-file",
                "/dev/stdin",
            ],
            "opencode" => vec!["run", "--format", "json", "--dir", cwd_text],
            _ => unreachable!(),
        };
        assert_eq!(args, expected, "{agent} argv changed");
        assert!(!args.iter().any(|argument| argument.contains(&prompt)));

        let child_prompt = wait_for(&artifacts.join("stdin"), Duration::from_secs(2));
        assert_eq!(child_prompt, prompt);
        assert!(!child_prompt.contains("<magents-reply-to"));
        let environment = wait_for(&artifacts.join("environment"), Duration::from_secs(2));
        let environment = environment.trim_end().split('|').collect::<Vec<_>>();
        assert_eq!(environment.len(), 12, "{agent}: {environment:?}");
        assert_eq!(
            environment[7],
            harness.root.join("magents").to_str().unwrap()
        );
        let expected_homes = match agent {
            "claude" => vec![(0, harness.root.join("claude"))],
            "codex" => vec![(1, harness.root.join("codex"))],
            "cursor" => vec![
                (2, harness.root.join("cursor-config")),
                (3, harness.root.join("cursor")),
            ],
            "grok" => vec![(4, harness.root.join("grok"))],
            "opencode" => vec![
                (5, harness.root.clone()),
                (6, harness.root.join("opencode-config")),
            ],
            _ => unreachable!(),
        };
        for index in 0..7 {
            let expected = expected_homes
                .iter()
                .find(|(target, _)| *target == index)
                .map(|(_, path)| path.to_str().unwrap())
                .unwrap_or("");
            assert_eq!(environment[index], expected, "{agent}: {environment:?}");
        }
        assert!(environment[8..].iter().all(|value| value.is_empty()));

        let resolved = harness.json(&["get", &format!("{agent}:{session_id}")]);
        assert_eq!(resolved["session_id"], session_id);
        assert_eq!(resolved["cwd"], cwd_text);
        assert_eq!(resolved["live"], false);
        let inbox = harness.json(&["inbox", "--agent", agent, "--session", session_id]);
        assert!(inbox["items"].as_array().unwrap().is_empty());

        assert!(!artifacts.join("done").exists());
        let agent_pid = wait_for(&artifacts.join("agent-pid"), Duration::from_secs(2));
        let supervisor_pid = wait_for(&artifacts.join("supervisor-pid"), Duration::from_secs(2));
        wait_for(&artifacts.join("done"), Duration::from_secs(3));
        wait_for_exit(&agent_pid, Duration::from_secs(3));
        wait_for_exit(&supervisor_pid, Duration::from_secs(3));
    }

    let registry = fs::read_dir(harness.root.join("magents/spawns"))
        .unwrap()
        .flatten()
        .filter_map(|entry| fs::read_to_string(entry.path()).ok())
        .collect::<String>();
    for private in ["SECRET-PROMPT", "SECRET-TOKEN", "SECRET-OUTPUT"] {
        assert!(!registry.contains(private));
    }

    let message = "follow up through exact ID SECRET-MESSAGE";
    let mut command = harness.command(&["send", &format!("codex:{CODEX_SPAWN_ID}"), message]);
    let artifacts = configure_stub(
        &mut command,
        &harness,
        &stub,
        "codex",
        "MAGENTS_CODEX_BIN",
        "resume-codex",
    );
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let sent: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(sent["queued"], true);
    assert_eq!(sent["to"]["session_id"], CODEX_SPAWN_ID);
    assert_eq!(sent["to"]["cwd"], cwd_text);
    assert_eq!(sent["delivered"], serde_json::json!(["codex-exec"]));
    assert!(!String::from_utf8_lossy(&output.stdout).contains(message));
    let resume_args = wait_for(&artifacts.join("args"), Duration::from_secs(2));
    assert_eq!(
        resume_args.lines().collect::<Vec<_>>(),
        vec![
            "exec",
            "--json",
            "-C",
            cwd_text,
            "resume",
            CODEX_SPAWN_ID,
            "-"
        ]
    );
    assert_eq!(
        wait_for(&artifacts.join("stdin"), Duration::from_secs(2)),
        message
    );
    let inbox = harness.json(&["inbox", "--agent", "codex", "--session", CODEX_SPAWN_ID]);
    assert_eq!(inbox["items"].as_array().unwrap().len(), 1);
    assert_eq!(inbox["items"][0]["message"], message);
    wait_for(&artifacts.join("done"), Duration::from_secs(3));
}

#[test]
fn mcp_spawn_survives_server_parent_exit() {
    let _lock = ENV.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let harness = Harness::new();
    let stub = harness.root.join("agent-stub");
    write_executable(&stub, AGENT_STUB);
    let cwd = fs::canonicalize(&harness.root).unwrap();
    let prompt = "MCP structured prompt SECRET-MCP-PROMPT";
    let mut command = harness.command(&["mcp"]);
    let artifacts = configure_stub(
        &mut command,
        &harness,
        &stub,
        "codex",
        "MAGENTS_CODEX_BIN",
        "mcp-parent-exit",
    );
    command
        .env("MAGENTS_TEST_HOLD_SECONDS", "2")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    let stdout = child.stdout.take().unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    let reader = thread::spawn(move || {
        let mut lines = Vec::new();
        for line in BufReader::new(stdout).lines() {
            let line = line.unwrap();
            if let Ok(value) = serde_json::from_str::<Value>(&line)
                && value["id"] == 2
            {
                sender.send(value).unwrap();
            }
            lines.push(line);
        }
        lines
    });
    let mut stdin = child.stdin.take().unwrap();
    for request in [
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "cli-test", "version": "1"}
            }
        }),
        serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        }),
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "spawn_session",
                "arguments": {
                    "agent": "codex",
                    "message": prompt,
                    "cwd": cwd
                }
            }
        }),
    ] {
        writeln!(stdin, "{request}").unwrap();
    }
    stdin.flush().unwrap();

    let response = receiver.recv_timeout(Duration::from_secs(3)).unwrap();
    let report: Value =
        serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(report["accepted"], true);
    assert_eq!(report["status"], "starting");
    assert_eq!(report["session"]["agent"], "codex");
    assert_eq!(report["session"]["live"], false);
    assert!(report.get("mail_id").is_none());
    assert!(!response.to_string().contains(prompt));

    drop(stdin);
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let _lines = reader.join().unwrap();

    assert_eq!(
        wait_for(&artifacts.join("stdin"), Duration::from_secs(2)),
        prompt
    );
    let provider = wait_for(&artifacts.join("args"), Duration::from_secs(2));
    assert!(!provider.contains(prompt));
    let supervisor_pid = wait_for(&artifacts.join("supervisor-pid"), Duration::from_secs(2));
    let provider_pid = wait_for(&artifacts.join("agent-pid"), Duration::from_secs(2));
    assert!(process_exists(&supervisor_pid));
    assert!(process_exists(&provider_pid));
    assert!(!artifacts.join("done").exists());
    wait_for(&artifacts.join("done"), Duration::from_secs(3));
    wait_for_exit(&supervisor_pid, Duration::from_secs(3));
    wait_for_exit(&provider_pid, Duration::from_secs(3));
}

#[test]
fn cli_spawn_adds_reply_route_only_for_known_caller() {
    let _lock = ENV.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let harness = Harness::new();
    let stub = harness.root.join("agent-stub");
    write_executable(&stub, AGENT_STUB);
    let cwd = fs::canonicalize(&harness.root).unwrap();
    let cwd_text = cwd.to_str().unwrap();
    let prompt = "reply after completing SECRET-ROUTE-TASK";
    let mut command = harness.command(&["spawn", "opencode", "--cwd", cwd_text]);
    let artifacts = configure_stub(
        &mut command,
        &harness,
        &stub,
        "opencode",
        "MAGENTS_OPENCODE_BIN",
        "reply-route",
    );
    command.env("CODEX_SESSION_ID", "known-caller");
    let output = output_with_stdin(&mut command, prompt);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let routed = wait_for(&artifacts.join("stdin"), Duration::from_secs(2));
    assert!(routed.contains("<magents-reply-to agent=\"codex\" session=\"known-caller\">"));
    assert!(routed.contains("magents send_message to codex:known-caller"));
    assert!(routed.ends_with(prompt));
    let environment = wait_for(&artifacts.join("environment"), Duration::from_secs(2));
    assert_eq!(environment.trim_end().split('|').nth(9), Some(""));
    for private in [prompt, "known-caller", "SECRET-TOKEN", "SECRET-OUTPUT"] {
        assert!(!String::from_utf8_lossy(&output.stdout).contains(private));
        assert!(!String::from_utf8_lossy(&output.stderr).contains(private));
    }
    wait_for(&artifacts.join("done"), Duration::from_secs(3));
}

#[test]
fn cli_spawn_keeps_prompt_out_of_outer_and_provider_argv() {
    let _lock = ENV.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let harness = Harness::new();
    let stub = harness.root.join("agent-stub");
    write_executable(&stub, AGENT_STUB);
    let cwd = fs::canonicalize(&harness.root).unwrap();
    let prompt = "outer argv must not contain SECRET-OUTER-PROMPT";
    let mut command = harness.command(&["spawn", "codex", "--cwd", cwd.to_str().unwrap()]);
    let artifacts = configure_stub(
        &mut command,
        &harness,
        &stub,
        "codex",
        "MAGENTS_CODEX_BIN",
        "outer-argv",
    );
    command
        .env("MAGENTS_TEST_MODE", "timeout")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = command.spawn().unwrap();
    let pid = child.id();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(prompt.as_bytes())
        .unwrap();

    assert_eq!(
        wait_for(&artifacts.join("stdin"), Duration::from_secs(2)),
        prompt
    );
    let outer = process_command(pid);
    assert!(outer.contains("spawn codex"), "unexpected argv: {outer}");
    assert!(!outer.contains(prompt), "outer argv leaked prompt: {outer}");
    let provider = wait_for(&artifacts.join("args"), Duration::from_secs(2));
    assert!(!provider.contains(prompt));

    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!combined.contains(prompt));
}

#[test]
fn cli_spawn_failures_are_bounded_sanitized_and_hidden() {
    let _lock = ENV.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let harness = Harness::new();
    let stub = harness.root.join("agent-stub");
    write_executable(&stub, AGENT_STUB);
    let cwd = fs::canonicalize(&harness.root).unwrap();
    let cwd_text = cwd.to_str().unwrap();
    let secret = "SECRET-FAILURE-PROMPT";

    for mode in ["early", "malformed", "no-id", "timeout"] {
        let mut command = harness.command(&["spawn", "codex", "--cwd", cwd_text]);
        let _artifacts = configure_stub(
            &mut command,
            &harness,
            &stub,
            "codex",
            "MAGENTS_CODEX_BIN",
            &format!("failure-{mode}"),
        );
        command.env("MAGENTS_TEST_MODE", mode);
        if mode == "timeout" {
            command.env("MAGENTS_STARTUP_TIMEOUT_MS", "20");
        }
        let output = output_with_stdin(&mut command, secret);
        assert!(!output.status.success(), "{mode} unexpectedly succeeded");
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(combined.contains("startup"), "{mode}: {combined}");
        for private in [secret, "SECRET-OUTPUT", "SECRET-TOKEN", "raw malformed"] {
            assert!(
                !combined.contains(private),
                "{mode} leaked {private}: {combined}"
            );
        }
    }

    let mut missing = harness.command(&["spawn", "codex", "--cwd", cwd_text]);
    missing.env(
        "MAGENTS_CODEX_BIN",
        harness.root.join("missing-agent-binary"),
    );
    let missing = output_with_stdin(&mut missing, secret);
    assert!(!missing.status.success());
    let missing_error = String::from_utf8_lossy(&missing.stderr);
    assert!(missing_error.contains("codex startup failed"));
    assert!(!missing_error.contains(secret));
    assert!(!missing_error.contains("missing-agent-binary"));

    let mut unknown = harness.command(&["spawn", "not-an-agent", "--cwd", cwd_text]);
    let unknown = output_with_stdin(&mut unknown, secret);
    assert!(!unknown.status.success());
    assert!(String::from_utf8_lossy(&unknown.stderr).contains("unknown agent: not-an-agent"));
    assert!(!String::from_utf8_lossy(&unknown.stderr).contains(secret));

    let mut invalid_cwd = harness.command(&[
        "spawn",
        "codex",
        "--cwd",
        harness.root.join("missing-cwd").to_str().unwrap(),
    ]);
    let invalid_cwd = output_with_stdin(&mut invalid_cwd, secret);
    assert!(!invalid_cwd.status.success());
    assert!(!String::from_utf8_lossy(&invalid_cwd.stderr).contains(secret));

    let mut empty = harness.command(&["spawn", "codex", "--cwd", cwd_text]);
    let empty = output_with_stdin(&mut empty, " \n\t");
    assert!(!empty.status.success());
    assert!(String::from_utf8_lossy(&empty.stderr).contains("prompt must not be empty"));

    let help = harness.command(&["--help"]).output().unwrap();
    assert!(help.status.success());
    let help = String::from_utf8_lossy(&help.stdout);
    assert!(help.contains("spawn"));
    assert!(!help.contains("__supervise"));
    let version = harness.command(&["--version"]).output().unwrap();
    assert!(version.status.success());
    assert!(String::from_utf8_lossy(&version.stdout).contains("magents"));
    let spawn_help = harness.command(&["spawn", "--help"]).output().unwrap();
    assert!(spawn_help.status.success());
    let spawn_help = String::from_utf8_lossy(&spawn_help.stdout);
    assert!(spawn_help.contains("<AGENT>"));
    assert!(spawn_help.contains("--prompt-file <PATH>"));
    assert!(spawn_help.contains("--cwd"));
    assert!(!spawn_help.contains("<MESSAGE>"));
    assert!(!spawn_help.contains("__supervise"));
}

#[test]
fn cli_spawn_handshake_timeout_reaps_supervisor_and_provider() {
    let _lock = ENV.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let harness = Harness::new();
    let stub = harness.root.join("agent-stub");
    write_executable(&stub, AGENT_STUB);
    let cwd = fs::canonicalize(&harness.root).unwrap();
    let prompt = "cancel silently SECRET-CANCELLATION-PROMPT";
    let mut command = harness.command(&["spawn", "codex", "--cwd", cwd.to_str().unwrap()]);
    let artifacts = configure_stub(
        &mut command,
        &harness,
        &stub,
        "codex",
        "MAGENTS_CODEX_BIN",
        "handshake-timeout",
    );
    command
        .env("MAGENTS_TEST_MODE", "cancel")
        .env("MAGENTS_STARTUP_TIMEOUT_MS", "5000")
        .env("MAGENTS_HANDSHAKE_TIMEOUT_MS", "800");
    let started = Instant::now();

    let output = output_with_stdin(&mut command, prompt);

    assert!(!output.status.success());
    assert!(started.elapsed() < Duration::from_secs(3));
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(combined.contains("timed out"), "{combined}");
    for private in [prompt, "SECRET-OUTPUT", "SECRET-TOKEN"] {
        assert!(!combined.contains(private), "leaked {private}: {combined}");
    }
    let supervisor = wait_for(&artifacts.join("supervisor-pid"), Duration::from_secs(1));
    let provider = wait_for(&artifacts.join("agent-pid"), Duration::from_secs(1));
    wait_for_exit(&supervisor, Duration::from_secs(1));
    wait_for_exit(&provider, Duration::from_secs(1));
}

#[test]
fn cli_spawn_failed_leader_reaps_pipe_holding_group() {
    let _lock = ENV.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let harness = Harness::new();
    let stub = harness.root.join("agent-stub");
    write_executable(&stub, AGENT_STUB);
    let cwd = fs::canonicalize(&harness.root).unwrap();
    let prompt = "fail with inherited pipes SECRET-ORPHAN-PROMPT";
    let mut command = harness.command(&["spawn", "codex", "--cwd", cwd.to_str().unwrap()]);
    let artifacts = configure_stub(
        &mut command,
        &harness,
        &stub,
        "codex",
        "MAGENTS_CODEX_BIN",
        "failed-leader",
    );
    command
        .env("MAGENTS_TEST_MODE", "orphan")
        .env("MAGENTS_STARTUP_TIMEOUT_MS", "500")
        .env("MAGENTS_HANDSHAKE_TIMEOUT_MS", "2000");
    let started = Instant::now();

    let output = output_with_stdin(&mut command, prompt);

    assert!(!output.status.success());
    assert!(started.elapsed() < Duration::from_secs(3));
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(combined.contains("startup"), "{combined}");
    for private in [prompt, "SECRET-OUTPUT", "SECRET-TOKEN"] {
        assert!(!combined.contains(private), "leaked {private}: {combined}");
    }
    let supervisor = wait_for(&artifacts.join("supervisor-pid"), Duration::from_secs(1));
    let provider = wait_for(&artifacts.join("agent-pid"), Duration::from_secs(1));
    let descendant = wait_for(&artifacts.join("descendant-pid"), Duration::from_secs(1));
    wait_for_exit(&supervisor, Duration::from_secs(1));
    wait_for_exit(&provider, Duration::from_secs(1));
    wait_for_exit(&descendant, Duration::from_secs(1));
}

#[test]
fn cli_installs_cursor_and_opencode_mcp_config() {
    let _lock = ENV.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
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
    let _lock = ENV.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
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
