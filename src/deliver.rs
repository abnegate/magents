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

fn send_opencode_run(session: &Session, message: &str) -> Result<()> {
    let mut command = Command::new("opencode");
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
    let child = Command::new("grok")
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
    let status = Command::new("tmux")
        .args(["send-keys", "-t", target, "-l", "--", message])
        .status()
        .map_err(|source| Error::Io {
            path: PathBuf::from("tmux"),
            source,
        })?;
    if !status.success() {
        return Err(Error::msg(format!("tmux send-keys failed for {target}")));
    }
    let status = Command::new("tmux")
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
    let mut command = Command::new("codex");
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
    use super::PeerKey;

    #[test]
    fn parses_peer_key() {
        let key: PeerKey =
            serde_json::from_str(r#"{"peerToken":"abc","procStart":"now"}"#).unwrap();
        assert_eq!(key.peer_token, "abc");
    }
}
