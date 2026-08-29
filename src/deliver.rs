use crate::error::{Error, Result};
use crate::homes::Homes;
use crate::model::{Agent, Session};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

pub fn deliver_live(homes: &Homes, session: &Session, message: &str) -> Result<Vec<String>> {
    let mut delivered = Vec::new();
    if session.agent == Agent::Claude
        && let Some(socket) = &session.messaging_socket
    {
        match send_claude_uds(homes, socket, message) {
            Ok(()) => delivered.push("claude-uds".into()),
            Err(error) => delivered.push(format!("claude-uds-failed:{error}")),
        }
    }
    Ok(delivered)
}

fn send_claude_uds(homes: &Homes, socket: &Path, message: &str) -> Result<()> {
    let token = claude_token(homes, socket)?;
    let payload = json!({
        "type": "text",
        "data": format!("<cross-session-message source=\"magents\">\n{message}\n</cross-session-message>"),
        "ts": chrono::Utc::now().to_rfc3339(),
        "meta": { "authToken": token }
    });
    let mut stream = UnixStream::connect(socket).map_err(|source| Error::Io {
        path: socket.to_path_buf(),
        source,
    })?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
    stream.set_write_timeout(Some(Duration::from_secs(5))).ok();
    writeln!(stream, "{payload}").map_err(|source| Error::Io {
        path: socket.to_path_buf(),
        source,
    })?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).map_err(|source| Error::Io {
        path: socket.to_path_buf(),
        source,
    })?;
    if line.trim().is_empty() {
        return Ok(());
    }
    let value: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
    if value.get("type").and_then(Value::as_str) == Some("error") {
        return Err(Error::msg(
            value
                .get("data")
                .and_then(Value::as_str)
                .unwrap_or("uds error"),
        ));
    }
    Ok(())
}

fn claude_token(homes: &Homes, socket: &Path) -> Result<String> {
    let digest = Sha256::digest(socket.to_string_lossy().as_bytes());
    let hex = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let capability = homes
        .claude
        .join("messaging-capabilities")
        .join(format!("{hex}.json"));
    if capability.is_file() {
        let raw = fs::read_to_string(&capability).map_err(|source| Error::Io {
            path: capability.clone(),
            source,
        })?;
        let value: Value = serde_json::from_str(&raw)?;
        if let Some(token) = value.get("authToken").and_then(Value::as_str) {
            return Ok(token.to_string());
        }
    }
    Err(Error::msg(
        "no Claude UDS auth token for this socket; message queued in mailbox only",
    ))
}
