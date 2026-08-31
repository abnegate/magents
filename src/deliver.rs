use crate::codex_ipc;
use crate::error::{Error, Result};
use crate::homes::Homes;
use crate::model::{Agent, Session};
use crate::runtime;
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;
use uuid::Uuid;

#[derive(Deserialize)]
struct PeerKey {
    #[serde(rename = "peerToken")]
    peer_token: String,
}

pub fn deliver_live(homes: &Homes, session: &Session, message: &str) -> Result<Vec<String>> {
    deliver_with(homes, session, message, runtime::resume)
}

fn deliver_with<F>(
    homes: &Homes,
    session: &Session,
    message: &str,
    mut resume: F,
) -> Result<Vec<String>>
where
    F: FnMut(&Homes, &Session, &str) -> Result<String>,
{
    let mut delivered = Vec::new();
    match session.agent {
        Agent::Claude => {
            let mut succeeded = false;
            if let Some(socket) = &session.messaging_socket {
                match send_claude_uds(homes, session, socket, message) {
                    Ok(()) => {
                        delivered.push("claude-uds".into());
                        succeeded = true;
                    }
                    Err(_) => delivered.push("claude-uds-failed".into()),
                }
            }
            if !succeeded && let Some(target) = session.tmux.as_deref() {
                match send_tmux(target, message) {
                    Ok(()) => {
                        delivered.push("claude-tmux".into());
                        succeeded = true;
                    }
                    Err(_) => delivered.push("claude-tmux-failed".into()),
                }
            }
            if !succeeded {
                record_resume(
                    homes,
                    session,
                    message,
                    "claude-cli",
                    &mut delivered,
                    &mut resume,
                );
            }
        }
        Agent::Grok => record_resume(
            homes,
            session,
            message,
            "grok-single",
            &mut delivered,
            &mut resume,
        ),
        Agent::Codex => {
            let mut succeeded = false;
            let ipc = homes.codex.join("ipc").join("ipc.sock");
            if ipc.exists() {
                match codex_ipc::send_user_turn(&ipc, &session.session_id, message) {
                    Ok(()) => {
                        delivered.push("codex-ipc".into());
                        succeeded = true;
                    }
                    Err(_) => delivered.push("codex-ipc-failed".into()),
                }
            }
            if !succeeded {
                record_resume(
                    homes,
                    session,
                    message,
                    "codex-exec",
                    &mut delivered,
                    &mut resume,
                );
            }
        }
        Agent::OpenCode => record_resume(
            homes,
            session,
            message,
            "opencode-run",
            &mut delivered,
            &mut resume,
        ),
        Agent::Cursor => record_resume(
            homes,
            session,
            message,
            "cursor-cli",
            &mut delivered,
            &mut resume,
        ),
    }
    Ok(delivered)
}

fn record_resume<F>(
    homes: &Homes,
    session: &Session,
    message: &str,
    route: &str,
    delivered: &mut Vec<String>,
    resume: &mut F,
) where
    F: FnMut(&Homes, &Session, &str) -> Result<String>,
{
    match resume(homes, session, message) {
        Ok(marker) => delivered.push(marker),
        Err(_) => delivered.push(format!("{route}-failed")),
    }
}

fn program(var: &str, fallback: &str) -> String {
    std::env::var(var).unwrap_or_else(|_| fallback.into())
}

fn send_tmux(target: &str, message: &str) -> Result<()> {
    let tmux = program("MAGENTS_TMUX_BIN", "tmux");
    let buffer = format!("magents-{}", Uuid::new_v4().simple());
    if let Err(error) = load_tmux_buffer(&tmux, &buffer, message) {
        delete_tmux_buffer(&tmux, &buffer);
        return Err(error);
    }
    if let Err(error) = run_tmux(
        &tmux,
        ["paste-buffer", "-b", &buffer, "-t", target, "-d"],
        "paste-buffer",
    ) {
        delete_tmux_buffer(&tmux, &buffer);
        return Err(error);
    }
    run_tmux(&tmux, ["send-keys", "-t", target, "Enter"], "send-keys")
}

fn load_tmux_buffer(tmux: &str, buffer: &str, message: &str) -> Result<()> {
    let mut child = Command::new(tmux)
        .args(["load-buffer", "-b", buffer, "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(tmux_io)?;
    let mut input = child
        .stdin
        .take()
        .ok_or_else(|| Error::msg("tmux load-buffer stdin unavailable"))?;
    if let Err(source) = input.write_all(message.as_bytes()) {
        drop(input);
        let _ = child.wait();
        return Err(tmux_io(source));
    }
    drop(input);
    let status = child.wait().map_err(tmux_io)?;
    if !status.success() {
        return Err(Error::msg("tmux load-buffer failed"));
    }
    Ok(())
}

fn run_tmux<const N: usize>(tmux: &str, arguments: [&str; N], operation: &str) -> Result<()> {
    let status = Command::new(tmux)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(tmux_io)?;
    if !status.success() {
        return Err(Error::msg(format!("tmux {operation} failed")));
    }
    Ok(())
}

fn delete_tmux_buffer(tmux: &str, buffer: &str) {
    let _ = Command::new(tmux)
        .args(["delete-buffer", "-b", buffer])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn tmux_io(source: std::io::Error) -> Error {
    Error::Io {
        path: PathBuf::from("tmux"),
        source,
    }
}

fn send_claude_uds(homes: &Homes, session: &Session, socket: &Path, message: &str) -> Result<()> {
    if !socket.exists() {
        return Err(Error::msg(format!("socket missing: {}", socket.display())));
    }
    let token = claude_peer_token(homes, session.pid, socket)?;
    let from = "uds:/tmp/cc-socks/magents.sock";
    let from_name = "magents";
    let wrapped = format!(
        "<cross-session-message from=\"{from}\" from-name=\"{from_name}\" from-mode=\"bypass\">\n{message}\n</cross-session-message>"
    );
    let msg_id = format!("cc-msg-{}", Uuid::new_v4().simple());
    let auth = json!({ "type": "auth", "token": token });
    let frame = json!({
        "type": "user",
        "message": { "role": "user", "content": wrapped },
        "from": from,
        "priority": "now",
        "msgV": 1,
        "msg_id": msg_id,
    });
    let mut stream = UnixStream::connect(socket).map_err(|source| Error::Io {
        path: socket.to_path_buf(),
        source,
    })?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
    stream.set_write_timeout(Some(Duration::from_secs(5))).ok();
    write!(stream, "{auth}\n{frame}\n").map_err(|source| Error::Io {
        path: socket.to_path_buf(),
        source,
    })?;
    let _ = stream.shutdown(std::net::Shutdown::Write);
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    match reader.read_line(&mut line) {
        Ok(0) | Err(_) => Ok(()),
        Ok(_) => {
            if line.trim().is_empty() {
                return Ok(());
            }
            let value: serde_json::Value =
                serde_json::from_str(&line).unwrap_or(serde_json::Value::Null);
            if value.get("type").and_then(serde_json::Value::as_str) == Some("error") {
                return Err(Error::msg("Claude UDS rejected message"));
            }
            Ok(())
        }
    }
}

fn claude_peer_token(homes: &Homes, pid: Option<u32>, socket: &Path) -> Result<String> {
    if let Some(token) = token_from_capability(homes, socket) {
        return Ok(token);
    }
    if let Some(pid) = pid
        && let Some(token) = token_from_pid_key(homes, pid)
    {
        return Ok(token);
    }
    if let Some(token) = token_from_socket_hash_key(homes, socket) {
        return Ok(token);
    }
    Err(Error::msg(
        "no Claude peer token for this session (.key / capability file missing)",
    ))
}

fn token_from_capability(homes: &Homes, socket: &Path) -> Option<String> {
    let digest = Sha256::digest(socket.to_string_lossy().as_bytes());
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    let path = homes
        .claude
        .join("messaging-capabilities")
        .join(format!("{hex}.json"));
    let raw = fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
    value
        .get("authToken")
        .or_else(|| value.get("peerToken"))
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned)
}

fn token_from_pid_key(homes: &Homes, pid: u32) -> Option<String> {
    let dir = homes.claude.join("sessions");
    let prefix = format!("{pid}.");
    let entries = fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(&prefix) && name.ends_with(".key") {
            return read_peer_token(&entry.path());
        }
    }
    None
}

fn token_from_socket_hash_key(homes: &Homes, socket: &Path) -> Option<String> {
    let real = fs::canonicalize(socket).unwrap_or_else(|_| socket.to_path_buf());
    let digest = Sha256::digest(real.to_string_lossy().as_bytes());
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    let pid = socket
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("");
    let path = homes
        .claude
        .join("sessions")
        .join(format!("{pid}.{hex}.key"));
    read_peer_token(&path)
}

fn read_peer_token(path: &PathBuf) -> Option<String> {
    let raw = fs::read_to_string(path).ok()?;
    serde_json::from_str::<PeerKey>(&raw)
        .ok()
        .map(|key| key.peer_token)
}

#[cfg(test)]
mod tests {
    use super::{PeerKey, deliver_live, deliver_with};
    use crate::error::Error;
    use crate::homes::Homes;
    use crate::model::{Agent, Session};
    use crate::test_env;
    use std::fs;
    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;
    use std::path::{Path, PathBuf};
    use std::thread;
    use std::time::Duration;
    use tempfile::TempDir;

    const ENV: &[&str] = &["MAGENTS_TMUX_BIN"];

    fn session(agent: Agent, id: &str) -> Session {
        Session {
            agent,
            session_id: id.into(),
            desktop_id: None,
            name: None,
            title: None,
            cwd: Some("/tmp/work".into()),
            branch: None,
            live: true,
            archived: false,
            pid: Some(std::process::id()),
            model: None,
            last_activity_at: None,
            transcript_path: None,
            messaging_socket: None,
            origin: None,
            tmux: None,
        }
    }

    fn isolated() -> (TempDir, Homes) {
        let dir = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(dir.path());
        (dir, homes)
    }

    fn wait_for(path: &Path, timeout: Duration) -> String {
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

    fn wait_listener(path: &Path) {
        let started = std::time::Instant::now();
        while !path.exists() {
            if started.elapsed() > Duration::from_secs(2) {
                panic!("socket {} never appeared", path.display());
            }
            thread::sleep(Duration::from_millis(10));
        }
        thread::sleep(Duration::from_millis(20));
    }

    #[test]
    fn parses_peer_key() {
        let key: PeerKey =
            serde_json::from_str(r#"{"peerToken":"abc","procStart":"now"}"#).unwrap();
        assert_eq!(key.peer_token, "abc");
    }

    #[test]
    fn supervised_routes_preserve_targets_and_markers() {
        let (_dir, homes) = isolated();
        for (agent, id, marker) in [
            (Agent::Claude, "claude-id", "claude-cli"),
            (Agent::Codex, "codex-id", "codex-exec"),
            (Agent::Cursor, "cursor-id", "cursor-cli"),
            (Agent::Grok, "grok-id", "grok-single"),
            (Agent::OpenCode, "opencode-id", "opencode-run"),
        ] {
            let live = session(agent, id);
            let delivered = deliver_with(
                &homes,
                &live,
                "private prompt",
                |actual_homes, actual, message| {
                    assert_eq!(actual_homes.magents, homes.magents);
                    assert_eq!(actual.agent, agent);
                    assert_eq!(actual.session_id, id);
                    assert_eq!(actual.cwd.as_deref(), Some("/tmp/work"));
                    assert_eq!(message, "private prompt");
                    Ok(marker.to_string())
                },
            )
            .unwrap();
            assert_eq!(delivered, vec![marker.to_string()]);
        }
    }

    #[test]
    fn supervised_failure_retains_route_without_private_output() {
        let (_dir, homes) = isolated();
        let live = session(Agent::Cursor, "cursor-id");
        let delivered = deliver_with(&homes, &live, "private prompt", |_, _, _| {
            Err(Error::msg(
                "child output contains private prompt and uds-secret-token",
            ))
        })
        .unwrap();
        assert_eq!(delivered, vec!["cursor-cli-failed".to_string()]);
    }

    #[test]
    fn tmux_fallback_keeps_message_out_of_argv() {
        let _guard = test_env::lock(ENV);
        let (dir, homes) = isolated();
        let log = dir.path().join("tmux.log");
        let input = dir.path().join("tmux.stdin");
        let stub = dir.path().join("tmux");
        test_env::write_executable(
            &stub,
            &format!(
                r#"
{{
  printf 'call'
  for argument in "$@"; do
    printf '\t%s' "$argument"
  done
  printf '\n'
}} >> '{}'
if [ "$1" = 'load-buffer' ]; then
  cat > '{}'
fi
"#,
                log.display(),
                input.display(),
            ),
        );
        unsafe { std::env::set_var("MAGENTS_TMUX_BIN", &stub) };
        let mut live = session(Agent::Claude, "c1");
        live.tmux = Some("magents:0.0".into());
        let message = "private prompt with spaces and $shell";
        let delivered = deliver_live(&homes, &live, message).unwrap();
        assert_eq!(delivered, vec!["claude-tmux".to_string()]);
        let args = wait_for(&log, Duration::from_secs(2));
        let calls = args
            .lines()
            .map(|line| line.split('\t').collect::<Vec<_>>())
            .collect::<Vec<_>>();
        assert_eq!(calls.len(), 3, "{calls:?}");
        let buffer = calls[0][3];
        assert!(buffer.starts_with("magents-"), "{buffer}");
        assert_eq!(buffer.len(), "magents-".len() + 32, "{buffer}");
        assert!(
            buffer["magents-".len()..]
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
            "{buffer}"
        );
        assert_eq!(calls[0], ["call", "load-buffer", "-b", buffer, "-"]);
        assert_eq!(
            calls[1],
            [
                "call",
                "paste-buffer",
                "-b",
                buffer,
                "-t",
                "magents:0.0",
                "-d",
            ]
        );
        assert_eq!(
            calls[2],
            ["call", "send-keys", "-t", "magents:0.0", "Enter"]
        );
        assert!(!args.contains(message), "{args}");
        assert_eq!(wait_for(&input, Duration::from_secs(2)), message);
    }

    #[test]
    fn failed_claude_uds_falls_through_to_tmux_and_stops() {
        let _guard = test_env::lock(ENV);
        let (dir, homes) = isolated();
        let log = dir.path().join("tmux.log");
        let stub = dir.path().join("tmux");
        test_env::write_executable(
            &stub,
            &format!(
                r#"
printf '%s\n' "$@" >> '{}'
if [ "$1" = 'load-buffer' ]; then
  cat >/dev/null
fi
"#,
                log.display(),
            ),
        );
        unsafe { std::env::set_var("MAGENTS_TMUX_BIN", &stub) };
        let mut live = session(Agent::Claude, "c1");
        live.messaging_socket = Some(dir.path().join("missing.sock"));
        live.tmux = Some("magents:0.0".into());
        let delivered = deliver_with(
            &homes,
            &live,
            "typed",
            |_, _, _| -> crate::error::Result<String> {
                panic!("supervisor resume must not run after tmux succeeds")
            },
        )
        .unwrap();
        assert_eq!(delivered.len(), 2, "{delivered:?}");
        assert_eq!(delivered[0], "claude-uds-failed");
        assert_eq!(delivered[1], "claude-tmux");
        let args = wait_for(&log, Duration::from_secs(2));
        assert!(!args.contains("typed"), "{args}");
    }

    #[test]
    fn tmux_partial_failure_cleans_buffer_and_falls_back_privately() {
        let _guard = test_env::lock(ENV);
        let (dir, homes) = isolated();
        let log = dir.path().join("tmux.log");
        let input = dir.path().join("tmux.stdin");
        let stub = dir.path().join("tmux");
        test_env::write_executable(
            &stub,
            &format!(
                r#"
{{
  printf 'call'
  for argument in "$@"; do
    printf '\t%s' "$argument"
  done
  printf '\n'
}} >> '{}'
case "$1" in
  load-buffer)
    cat > '{}'
    exit 0
    ;;
  paste-buffer|delete-buffer)
    exit 1
    ;;
  *)
    exit 99
    ;;
esac
"#,
                log.display(),
                input.display(),
            ),
        );
        unsafe { std::env::set_var("MAGENTS_TMUX_BIN", &stub) };
        let mut live = session(Agent::Claude, "c1");
        live.tmux = Some("gone:0.0".into());
        let message = "private fallback prompt";
        let delivered = deliver_with(&homes, &live, message, |_, actual, actual_message| {
            assert_eq!(actual.session_id, "c1");
            assert_eq!(actual_message, message);
            Ok("claude-cli".to_string())
        })
        .unwrap();
        assert_eq!(
            delivered,
            ["claude-tmux-failed".to_string(), "claude-cli".to_string()]
        );
        let args = wait_for(&log, Duration::from_secs(2));
        let calls = args
            .lines()
            .map(|line| line.split('\t').collect::<Vec<_>>())
            .collect::<Vec<_>>();
        assert_eq!(calls.len(), 3, "{calls:?}");
        let buffer = calls[0][3];
        assert_eq!(calls[0], ["call", "load-buffer", "-b", buffer, "-"]);
        assert_eq!(
            calls[1],
            ["call", "paste-buffer", "-b", buffer, "-t", "gone:0.0", "-d",]
        );
        assert_eq!(calls[2], ["call", "delete-buffer", "-b", buffer]);
        assert!(!args.contains(message), "{args}");
        assert_eq!(wait_for(&input, Duration::from_secs(2)), message);
    }

    #[test]
    fn tmux_enter_failure_is_reported() {
        let _guard = test_env::lock(ENV);
        let (dir, homes) = isolated();
        let stub = dir.path().join("tmux");
        test_env::write_executable(
            &stub,
            r#"
if [ "$1" = 'load-buffer' ]; then
  cat >/dev/null
  exit 0
fi
if [ "$1" = 'paste-buffer' ]; then
  exit 0
fi
exit 1
"#,
        );
        unsafe { std::env::set_var("MAGENTS_TMUX_BIN", &stub) };
        let mut live = session(Agent::Claude, "c1");
        live.tmux = Some("pane:0.0".into());
        let delivered = deliver_with(&homes, &live, "typed", |_, _, _| {
            Err(Error::msg("private child output"))
        })
        .unwrap();
        assert!(delivered.iter().any(|item| item == "claude-tmux-failed"));
    }

    #[test]
    fn missing_tmux_binary_is_reported() {
        let _guard = test_env::lock(ENV);
        let (dir, homes) = isolated();
        unsafe {
            std::env::set_var("MAGENTS_TMUX_BIN", dir.path().join("no-tmux").as_os_str());
        }
        let mut live = session(Agent::Claude, "c1");
        live.tmux = Some("pane:0.0".into());
        let delivered = deliver_with(&homes, &live, "typed", |_, _, _| {
            Err(Error::msg("private child output"))
        })
        .unwrap();
        assert!(
            delivered.iter().any(|item| item == "claude-tmux-failed"),
            "{delivered:?}"
        );
    }

    #[test]
    fn codex_ipc_success_and_failure_paths() {
        let world = crate::handoff_tests::World::new();
        let billing = crate::discover::resolve(&world.homes, "codex:Billing").unwrap();
        let delivered = deliver_with(
            &world.homes,
            &billing,
            "via dummy sock",
            |_, actual, message| {
                assert_eq!(actual.session_id, billing.session_id);
                assert_eq!(message, "via dummy sock");
                Ok("codex-exec".to_string())
            },
        )
        .unwrap();
        assert!(
            delivered.iter().any(|item| item == "codex-ipc-failed"),
            "{delivered:?}"
        );
        assert!(
            delivered.iter().any(|item| item == "codex-exec"),
            "{delivered:?}"
        );

        let (dir, homes) = isolated();
        let socket = homes.codex.join("ipc").join("ipc.sock");
        fs::create_dir_all(socket.parent().unwrap()).unwrap();
        thread::spawn({
            let socket = socket.clone();
            move || {
                let listener = UnixListener::bind(&socket).unwrap();
                let (mut stream, _) = listener.accept().unwrap();
                stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
                let mut buffer = Vec::new();
                for _ in 0..2 {
                    while buffer.len() < 4 {
                        let mut chunk = [0u8; 8192];
                        let read = stream.read(&mut chunk).unwrap();
                        buffer.extend_from_slice(&chunk[..read]);
                    }
                    let length = u32::from_le_bytes(buffer[..4].try_into().unwrap()) as usize;
                    let total = 4 + length;
                    while buffer.len() < total {
                        let mut chunk = [0u8; 8192];
                        let read = stream.read(&mut chunk).unwrap();
                        buffer.extend_from_slice(&chunk[..read]);
                    }
                    let request: serde_json::Value =
                        serde_json::from_slice(&buffer[4..total]).unwrap();
                    buffer.drain(..total);
                    let id = request["requestId"].clone();
                    let payload = serde_json::json!({
                        "type": "response",
                        "requestId": id,
                        "result": { "clientId": "c1", "turn": { "status": "in_progress" } }
                    });
                    let body = serde_json::to_vec(&payload).unwrap();
                    let mut frame = Vec::new();
                    frame.extend_from_slice(&(body.len() as u32).to_le_bytes());
                    frame.extend_from_slice(&body);
                    stream.write_all(&frame).unwrap();
                }
            }
        });
        wait_listener(&socket);
        let delivered = deliver_with(
            &homes,
            &session(Agent::Codex, "thread-1"),
            "ipc hi",
            |_, _, _| -> crate::error::Result<String> {
                panic!("supervisor resume must not run after Codex IPC succeeds")
            },
        )
        .unwrap();
        assert_eq!(delivered, vec!["codex-ipc".to_string()]);
        let _ = dir;
    }

    #[test]
    fn claude_uds_missing_socket_and_missing_token() {
        let (_dir, homes) = isolated();
        let mut live = session(Agent::Claude, "c1");
        live.messaging_socket = Some(PathBuf::from("/tmp/magents-no-such-socket.sock"));
        let delivered = deliver_with(&homes, &live, "hi", |_, _, _| {
            Err(Error::msg("resume unavailable"))
        })
        .unwrap();
        assert!(
            delivered.iter().any(|item| item == "claude-uds-failed"),
            "{delivered:?}"
        );

        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("empty.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        drop(listener);
        live.messaging_socket = Some(socket);
        live.pid = None;
        let delivered = deliver_with(&homes, &live, "hi", |_, _, _| {
            Err(Error::msg("resume unavailable"))
        })
        .unwrap();
        assert_eq!(
            delivered,
            vec![
                "claude-uds-failed".to_string(),
                "claude-cli-failed".to_string(),
            ]
        );
    }

    #[test]
    fn claude_uds_failure_falls_through_tmux_then_cli_privately() {
        let _guard = test_env::lock(ENV);
        let (dir, homes) = isolated();
        let socket = dir.path().join("caps.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let tmux = dir.path().join("tmux");
        test_env::write_executable(&tmux, "exit 1");
        unsafe { std::env::set_var("MAGENTS_TMUX_BIN", &tmux) };
        let digest = {
            use sha2::{Digest, Sha256};
            let bytes = Sha256::digest(socket.to_string_lossy().as_bytes());
            bytes
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        };
        fs::create_dir_all(homes.claude.join("messaging-capabilities")).unwrap();
        fs::write(
            homes
                .claude
                .join("messaging-capabilities")
                .join(format!("{digest}.json")),
            r#"{"authToken":"cap-token"}"#,
        )
        .unwrap();
        let received = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
            let mut buf = String::new();
            let _ = stream.read_to_string(&mut buf);
            let _ = writeln!(
                stream,
                r#"{{"type":"error","data":"private reply cap-token from magents"}}"#
            );
            buf
        });
        let mut live = session(Agent::Claude, "c1");
        live.messaging_socket = Some(socket);
        live.pid = None;
        live.tmux = Some("pane:0.0".into());
        let delivered = deliver_with(&homes, &live, "from magents", |_, actual, message| {
            assert_eq!(actual.session_id, "c1");
            assert_eq!(message, "from magents");
            Ok("claude-cli".to_string())
        })
        .unwrap();
        assert_eq!(
            delivered,
            vec![
                "claude-uds-failed".to_string(),
                "claude-tmux-failed".to_string(),
                "claude-cli".to_string(),
            ]
        );
        let evidence = delivered.join("\n");
        assert!(!evidence.contains("cap-token"), "{delivered:?}");
        assert!(!evidence.contains("from magents"), "{delivered:?}");
        assert!(!evidence.contains("private reply"), "{delivered:?}");
        let body = received.join().unwrap();
        assert!(body.contains("cap-token"));
        assert!(body.contains("from magents"));
    }

    #[test]
    fn claude_uds_hash_key_and_empty_ack() {
        let (dir, homes) = isolated();
        let socket = dir.path().join("4242.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let real = fs::canonicalize(&socket).unwrap();
        let digest = {
            use sha2::{Digest, Sha256};
            let bytes = Sha256::digest(real.to_string_lossy().as_bytes());
            bytes
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        };
        fs::create_dir_all(homes.claude.join("sessions")).unwrap();
        fs::write(
            homes
                .claude
                .join("sessions")
                .join(format!("4242.{digest}.key")),
            r#"{"peerToken":"hash-token","procStart":"now"}"#,
        )
        .unwrap();
        let received = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
            let mut buf = String::new();
            let _ = stream.read_to_string(&mut buf);
            let _ = writeln!(stream, "   ");
            buf
        });
        let mut live = session(Agent::Claude, "c1");
        live.messaging_socket = Some(socket);
        live.pid = None;
        let delivered = deliver_live(&homes, &live, "hash path").unwrap();
        assert_eq!(delivered, vec!["claude-uds".to_string()]);
        let body = received.join().unwrap();
        assert!(body.contains("hash-token"));
    }

    #[test]
    fn claude_uds_non_error_ack_is_ok() {
        let (dir, homes) = isolated();
        let socket = dir.path().join("ok.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        fs::create_dir_all(homes.claude.join("sessions")).unwrap();
        fs::write(
            homes
                .claude
                .join("sessions")
                .join(format!("{}.deadbeef.key", std::process::id())),
            r#"{"peerToken":"pid-token","procStart":"now"}"#,
        )
        .unwrap();
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
            let mut buf = String::new();
            let _ = stream.read_to_string(&mut buf);
            let _ = writeln!(stream, r#"{{"type":"ok"}}"#);
        });
        let mut live = session(Agent::Claude, "c1");
        live.messaging_socket = Some(socket);
        let delivered = deliver_live(&homes, &live, "ack").unwrap();
        assert_eq!(delivered, vec!["claude-uds".to_string()]);
    }

    #[test]
    fn claude_uds_connects_to_non_socket_file() {
        let (dir, homes) = isolated();
        fs::create_dir_all(homes.claude.join("sessions")).unwrap();
        fs::write(
            homes
                .claude
                .join("sessions")
                .join(format!("{}.key", std::process::id())),
            r#"{"peerToken":"x","procStart":"now"}"#,
        )
        .unwrap();
        let path = dir.path().join("not-a-socket");
        fs::write(&path, "regular-file").unwrap();
        let mut live = session(Agent::Claude, "c1");
        live.messaging_socket = Some(path);
        let delivered = deliver_with(&homes, &live, "hi", |_, _, _| {
            Err(Error::msg("resume unavailable"))
        })
        .unwrap();
        assert!(delivered.iter().any(|item| item == "claude-uds-failed"));
    }

    #[test]
    fn tmux_load_buffer_stdin_write_failure() {
        let _guard = test_env::lock(ENV);
        let (dir, homes) = isolated();
        let stub = dir.path().join("tmux");
        test_env::write_executable(
            &stub,
            r#"
if [ "$1" = 'load-buffer' ]; then
  exec <&-
  exit 1
fi
exit 0
"#,
        );
        unsafe { std::env::set_var("MAGENTS_TMUX_BIN", &stub) };
        let mut live = session(Agent::Claude, "c1");
        live.tmux = Some("magents:0.0".into());
        live.messaging_socket = None;
        let delivered = deliver_with(&homes, &live, "typed-secret", |_, _, _| {
            Err(Error::msg("resume unavailable"))
        })
        .unwrap();
        assert!(
            delivered.iter().any(|item| item.contains("tmux")
                || item.contains("failed")
                || item.contains("cli")),
            "{delivered:?}"
        );
    }

    #[test]
    fn claude_uds_write_fails_when_peer_disconnects() {
        let (dir, homes) = isolated();
        fs::create_dir_all(homes.claude.join("sessions")).unwrap();
        fs::write(
            homes
                .claude
                .join("sessions")
                .join(format!("{}.key", std::process::id())),
            r#"{"peerToken":"x","procStart":"now"}"#,
        )
        .unwrap();
        let socket = dir.path().join("drop.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            drop(stream);
        });
        wait_listener(&socket);
        let mut live = session(Agent::Claude, "c1");
        live.messaging_socket = Some(socket);
        let delivered = deliver_with(&homes, &live, "hi", |_, _, _| {
            Err(Error::msg("resume unavailable"))
        })
        .unwrap();
        assert!(delivered.iter().any(|item| item == "claude-uds-failed"));
    }

    #[test]
    fn pid_key_lookup_and_wait_timeout() {
        let (dir, homes) = isolated();
        fs::create_dir_all(homes.claude.join("sessions")).unwrap();
        fs::write(
            homes.claude.join("sessions").join("9.session.key"),
            r#"{"peerToken":"pid-key","procStart":"now"}"#,
        )
        .unwrap();
        assert_eq!(
            super::token_from_pid_key(&homes, 9).as_deref(),
            Some("pid-key")
        );
        assert!(super::token_from_pid_key(&homes, 8).is_none());
        assert!(wait_for(&dir.path().join("missing.log"), Duration::from_millis(40)).is_empty());
    }
}
