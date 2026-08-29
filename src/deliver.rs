use crate::codex_ipc;
use crate::error::{Error, Result};
use crate::homes::Homes;
use crate::model::{Agent, Session};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use uuid::Uuid;

#[derive(Deserialize)]
struct PeerKey {
    #[serde(rename = "peerToken")]
    peer_token: String,
}

pub fn deliver_live(homes: &Homes, session: &Session, message: &str) -> Result<Vec<String>> {
    let mut delivered = Vec::new();
    match session.agent {
        Agent::Claude => {
            if let Some(socket) = &session.messaging_socket {
                match send_claude_uds(homes, session, socket, message) {
                    Ok(()) => delivered.push("claude-uds".into()),
                    Err(error) => delivered.push(format!("claude-uds-failed:{error}")),
                }
            }
            if delivered.is_empty()
                && let Some(target) = &session.tmux
            {
                match send_tmux(target, message) {
                    Ok(()) => delivered.push("claude-tmux".into()),
                    Err(error) => delivered.push(format!("claude-tmux-failed:{error}")),
                }
            }
        }
        Agent::Grok => match send_grok_single(session, message) {
            Ok(()) => delivered.push("grok-single".into()),
            Err(error) => delivered.push(format!("grok-single-failed:{error}")),
        },
        Agent::Codex => {
            let ipc = homes.codex.join("ipc").join("ipc.sock");
            if ipc.exists() {
                match codex_ipc::send_user_turn(&ipc, &session.session_id, message) {
                    Ok(()) => delivered.push("codex-ipc".into()),
                    Err(error) => delivered.push(format!("codex-ipc-failed:{error}")),
                }
            }
            if delivered
                .iter()
                .all(|item| item.starts_with("codex-ipc-failed"))
            {
                match send_codex_exec(session, message) {
                    Ok(()) => delivered.push("codex-exec".into()),
                    Err(error) => delivered.push(format!("codex-exec-failed:{error}")),
                }
            }
        }
        Agent::OpenCode => match send_opencode_run(session, message) {
            Ok(()) => delivered.push("opencode-run".into()),
            Err(error) => delivered.push(format!("opencode-run-failed:{error}")),
        },
        Agent::Cursor => {}
    }
    Ok(delivered)
}

fn program(var: &str, fallback: &str) -> String {
    std::env::var(var).unwrap_or_else(|_| fallback.into())
}

fn send_opencode_run(session: &Session, message: &str) -> Result<()> {
    let mut command = Command::new(program("MAGENTS_OPENCODE_BIN", "opencode"));
    command.arg("run").arg("--session").arg(&session.session_id);
    if let Some(cwd) = &session.cwd {
        command.arg("--dir").arg(cwd);
    }
    command
        .arg(message)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let child = command.spawn().map_err(|source| Error::Io {
        path: PathBuf::from("opencode"),
        source,
    })?;
    std::thread::spawn(move || {
        let mut child = child;
        let _ = child.wait();
    });
    Ok(())
}

fn send_grok_single(session: &Session, message: &str) -> Result<()> {
    let cwd = session.cwd.as_deref().unwrap_or(".");
    let child = Command::new(program("MAGENTS_GROK_BIN", "grok"))
        .args([
            "--cwd",
            cwd,
            "--resume",
            &session.session_id,
            "--always-approve",
            "--single",
            message,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|source| Error::Io {
            path: PathBuf::from("grok"),
            source,
        })?;
    std::thread::spawn(move || {
        let mut child = child;
        let _ = child.wait();
    });
    Ok(())
}

fn send_tmux(target: &str, message: &str) -> Result<()> {
    let tmux = program("MAGENTS_TMUX_BIN", "tmux");
    let status = Command::new(&tmux)
        .args(["send-keys", "-t", target, "-l", "--", message])
        .status()
        .map_err(|source| Error::Io {
            path: PathBuf::from("tmux"),
            source,
        })?;
    if !status.success() {
        return Err(Error::msg(format!("tmux send-keys failed for {target}")));
    }
    let status = Command::new(&tmux)
        .args(["send-keys", "-t", target, "Enter"])
        .status()
        .map_err(|source| Error::Io {
            path: PathBuf::from("tmux"),
            source,
        })?;
    if !status.success() {
        return Err(Error::msg(format!("tmux enter failed for {target}")));
    }
    Ok(())
}

fn send_codex_exec(session: &Session, message: &str) -> Result<()> {
    let mut command = Command::new(program("MAGENTS_CODEX_BIN", "codex"));
    command.arg("exec").arg("--skip-git-repo-check");
    if let Some(cwd) = &session.cwd {
        command.arg("-C").arg(cwd);
    }
    command
        .arg("resume")
        .arg(&session.session_id)
        .arg(message)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|source| Error::Io {
        path: PathBuf::from("codex"),
        source,
    })?;
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait().map_err(|source| Error::Io {
            path: PathBuf::from("codex"),
            source,
        })? {
            let mut stderr = String::new();
            if let Some(mut pipe) = child.stderr.take() {
                let _ = pipe.read_to_string(&mut stderr);
            }
            if !status.success() {
                let detail = stderr.trim();
                return Err(Error::msg(if detail.is_empty() {
                    format!("codex exec resume exited {status}")
                } else {
                    format!("codex exec resume: {detail}")
                }));
            }
            return Ok(());
        }
        if started.elapsed() > Duration::from_secs(8) {
            std::thread::spawn(move || {
                let mut child = child;
                let _ = child.wait();
            });
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
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
                return Err(Error::msg(
                    value
                        .get("data")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("uds error"),
                ));
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
    use super::{PeerKey, deliver_live};
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

    const ENV: &[&str] = &[
        "MAGENTS_GROK_BIN",
        "MAGENTS_CODEX_BIN",
        "MAGENTS_TMUX_BIN",
        "MAGENTS_OPENCODE_BIN",
    ];

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
    fn grok_single_uses_stub_and_cwd_fallback() {
        let _guard = test_env::lock(ENV);
        let (dir, homes) = isolated();
        let log = dir.path().join("grok.log");
        let stub = dir.path().join("grok");
        test_env::write_executable(
            &stub,
            &format!("printf '%s\\n' \"$@\" > '{}'", log.display()),
        );
        unsafe { std::env::set_var("MAGENTS_GROK_BIN", &stub) };
        let mut live = session(Agent::Grok, "grok-1");
        live.cwd = None;
        let delivered = deliver_live(&homes, &live, "ping from magents").unwrap();
        let args = wait_for(&log, Duration::from_secs(2));
        assert_eq!(delivered, vec!["grok-single".to_string()]);
        assert!(args.contains("--single"), "{args}");
        assert!(args.contains("grok-1"), "{args}");
        assert!(args.contains("ping from magents"), "{args}");
    }

    #[test]
    fn grok_single_reports_missing_binary() {
        let _guard = test_env::lock(ENV);
        let (dir, homes) = isolated();
        unsafe {
            std::env::set_var(
                "MAGENTS_GROK_BIN",
                dir.path().join("missing-grok").as_os_str(),
            );
        }
        let delivered = deliver_live(&homes, &session(Agent::Grok, "g"), "hi").unwrap();
        assert!(
            delivered
                .iter()
                .any(|item| item.starts_with("grok-single-failed:")),
            "{delivered:?}"
        );
    }

    #[test]
    fn tmux_fallback_when_claude_has_no_socket() {
        let _guard = test_env::lock(ENV);
        let (dir, homes) = isolated();
        let log = dir.path().join("tmux.log");
        let stub = dir.path().join("tmux");
        test_env::write_executable(
            &stub,
            &format!("printf '%s\\n' \"$@\" >> '{}'", log.display()),
        );
        unsafe { std::env::set_var("MAGENTS_TMUX_BIN", &stub) };
        let mut live = session(Agent::Claude, "c1");
        live.tmux = Some("magents:0.0".into());
        let delivered = deliver_live(&homes, &live, "typed").unwrap();
        assert_eq!(delivered, vec!["claude-tmux".to_string()]);
        let args = wait_for(&log, Duration::from_secs(2));
        assert!(args.contains("send-keys"), "{args}");
        assert!(args.contains("typed"), "{args}");
        assert!(args.contains("Enter"), "{args}");
    }

    #[test]
    fn tmux_send_keys_failure_is_reported() {
        let _guard = test_env::lock(ENV);
        let (dir, homes) = isolated();
        let stub = dir.path().join("tmux");
        test_env::write_executable(&stub, "exit 1");
        unsafe { std::env::set_var("MAGENTS_TMUX_BIN", &stub) };
        let mut live = session(Agent::Claude, "c1");
        live.tmux = Some("gone:0.0".into());
        let delivered = deliver_live(&homes, &live, "typed").unwrap();
        assert!(
            delivered
                .iter()
                .any(|item| item.starts_with("claude-tmux-failed:")),
            "{delivered:?}"
        );
    }

    #[test]
    fn tmux_enter_failure_is_reported() {
        let _guard = test_env::lock(ENV);
        let (dir, homes) = isolated();
        let stub = dir.path().join("tmux");
        test_env::write_executable(
            &stub,
            r#"
case " $* " in
  *" -l "*) exit 0 ;;
  *) exit 1 ;;
esac
"#,
        );
        unsafe { std::env::set_var("MAGENTS_TMUX_BIN", &stub) };
        let mut live = session(Agent::Claude, "c1");
        live.tmux = Some("pane:0.0".into());
        let delivered = deliver_live(&homes, &live, "typed").unwrap();
        assert!(
            delivered
                .iter()
                .any(|item| item.contains("tmux enter failed")),
            "{delivered:?}"
        );
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
        let delivered = deliver_live(&homes, &live, "typed").unwrap();
        assert!(
            delivered
                .iter()
                .any(|item| item.starts_with("claude-tmux-failed:")),
            "{delivered:?}"
        );
    }

    #[test]
    fn codex_exec_resume_uses_stub() {
        let _guard = test_env::lock(ENV);
        let (dir, homes) = isolated();
        let log = dir.path().join("codex.log");
        let stub = dir.path().join("codex");
        test_env::write_executable(
            &stub,
            &format!("printf '%s\\n' \"$@\" > '{}'", log.display()),
        );
        unsafe { std::env::set_var("MAGENTS_CODEX_BIN", &stub) };
        let delivered = deliver_live(&homes, &session(Agent::Codex, "thread-1"), "go").unwrap();
        assert_eq!(delivered, vec!["codex-exec".to_string()]);
        let args = wait_for(&log, Duration::from_secs(2));
        assert!(args.contains("exec"), "{args}");
        assert!(args.contains("resume"), "{args}");
        assert!(args.contains("thread-1"), "{args}");
        assert!(args.contains("-C"), "{args}");
    }

    #[test]
    fn codex_exec_failure_includes_stderr() {
        let _guard = test_env::lock(ENV);
        let (dir, homes) = isolated();
        let stub = dir.path().join("codex");
        test_env::write_executable(
            &stub,
            "echo paginated_threads is not supported yet >&2; exit 1",
        );
        unsafe { std::env::set_var("MAGENTS_CODEX_BIN", &stub) };
        let mut live = session(Agent::Codex, "thread-1");
        live.cwd = None;
        let delivered = deliver_live(&homes, &live, "go").unwrap();
        assert!(
            delivered
                .iter()
                .any(|item| item.contains("paginated_threads")),
            "{delivered:?}"
        );
    }

    #[test]
    fn codex_exec_failure_without_stderr() {
        let _guard = test_env::lock(ENV);
        let (dir, homes) = isolated();
        let stub = dir.path().join("codex");
        test_env::write_executable(&stub, "exit 1");
        unsafe { std::env::set_var("MAGENTS_CODEX_BIN", &stub) };
        let delivered = deliver_live(&homes, &session(Agent::Codex, "t"), "go").unwrap();
        assert!(
            delivered
                .iter()
                .any(|item| item.contains("codex exec resume exited")),
            "{delivered:?}"
        );
    }

    #[test]
    fn codex_ipc_success_and_failure_paths() {
        let _guard = test_env::lock(ENV);
        let world = crate::handoff_tests::World::new();
        let stub = world.homes.magents.join("codex-stub");
        test_env::write_executable(&stub, "exit 0");
        unsafe { std::env::set_var("MAGENTS_CODEX_BIN", &stub) };
        let billing = crate::discover::resolve(&world.homes, "codex:Billing").unwrap();
        let delivered = deliver_live(&world.homes, &billing, "via dummy sock").unwrap();
        assert!(
            delivered
                .iter()
                .any(|item| item.starts_with("codex-ipc-failed:")),
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
        let delivered = deliver_live(&homes, &session(Agent::Codex, "thread-1"), "ipc hi").unwrap();
        assert_eq!(delivered, vec!["codex-ipc".to_string()]);
        let _ = dir;
    }

    #[test]
    fn missing_codex_binary_is_reported() {
        let _guard = test_env::lock(ENV);
        let (dir, homes) = isolated();
        unsafe {
            std::env::set_var("MAGENTS_CODEX_BIN", dir.path().join("no-codex").as_os_str());
        }
        let delivered = deliver_live(&homes, &session(Agent::Codex, "t"), "go").unwrap();
        assert!(
            delivered
                .iter()
                .any(|item| item.starts_with("codex-exec-failed:")),
            "{delivered:?}"
        );
    }

    #[test]
    fn opencode_missing_binary_is_reported() {
        let _guard = test_env::lock(ENV);
        let (dir, homes) = isolated();
        unsafe {
            std::env::set_var(
                "MAGENTS_OPENCODE_BIN",
                dir.path().join("no-opencode").as_os_str(),
            );
        }
        let mut live = session(Agent::OpenCode, "ses");
        live.cwd = None;
        let delivered = deliver_live(&homes, &live, "go").unwrap();
        assert!(
            delivered
                .iter()
                .any(|item| item.starts_with("opencode-run-failed:")),
            "{delivered:?}"
        );
    }

    #[test]
    fn claude_uds_missing_socket_and_missing_token() {
        let (_dir, homes) = isolated();
        let mut live = session(Agent::Claude, "c1");
        live.messaging_socket = Some(PathBuf::from("/tmp/magents-no-such-socket.sock"));
        let delivered = deliver_live(&homes, &live, "hi").unwrap();
        assert!(
            delivered
                .iter()
                .any(|item| item.starts_with("claude-uds-failed:")),
            "{delivered:?}"
        );

        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("empty.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        drop(listener);
        live.messaging_socket = Some(socket);
        live.pid = None;
        let delivered = deliver_live(&homes, &live, "hi").unwrap();
        assert!(
            delivered.iter().any(|item| item.contains("peer token")),
            "{delivered:?}"
        );
    }

    #[test]
    fn claude_uds_capability_token_and_error_reply() {
        let (dir, homes) = isolated();
        let socket = dir.path().join("caps.sock");
        let listener = UnixListener::bind(&socket).unwrap();
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
            let _ = writeln!(stream, r#"{{"type":"error","data":"nope"}}"#);
            buf
        });
        let mut live = session(Agent::Claude, "c1");
        live.messaging_socket = Some(socket);
        live.pid = None;
        let delivered = deliver_live(&homes, &live, "from magents").unwrap();
        assert!(
            delivered
                .iter()
                .any(|item| item.contains("claude-uds-failed:nope")),
            "{delivered:?}"
        );
        let body = received.join().unwrap();
        assert!(body.contains("cap-token"));
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
}
