use crate::error::{Error, Result};
use serde::Deserialize;
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use uuid::Uuid;

const MAX_FRAME: u32 = 8 * 1024 * 1024;
const START_TURN_VERSION: u64 = 2;

#[derive(Deserialize)]
struct IpcResponse {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default, rename = "requestId")]
    request_id: Option<String>,
    #[serde(default, rename = "resultType")]
    result_type: Option<String>,
    #[serde(default)]
    error: Option<Value>,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    method: Option<String>,
}

pub fn send_user_turn(socket: &Path, conversation_id: &str, message: &str) -> Result<()> {
    let stream = UnixStream::connect(socket).map_err(|source| Error::Io {
        path: socket.to_path_buf(),
        source,
    })?;
    stream.set_read_timeout(Some(Duration::from_secs(15))).ok();
    stream.set_write_timeout(Some(Duration::from_secs(5))).ok();
    let mut client = Client {
        stream,
        buffer: Vec::new(),
        client_id: "initializing-client".into(),
        socket: socket.to_path_buf(),
    };
    let init = client.request(
        "initialize",
        0,
        json!({ "clientType": "magents" }),
        Duration::from_secs(5),
    )?;
    if let Some(id) = init
        .get("clientId")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
    {
        client.client_id = id;
    }
    let started = client.request(
        "thread-follower-start-turn",
        START_TURN_VERSION,
        json!({
            "conversationId": conversation_id,
            "hostId": "local",
            "turnStart": {
                "request": {
                    "threadId": conversation_id,
                    "input": [{ "type": "text", "text": message }],
                },
                "context": { "inheritThreadSettings": true },
            }
        }),
        Duration::from_secs(20),
    )?;
    let status = started
        .pointer("/result/turn/status")
        .and_then(Value::as_str)
        .or_else(|| started.pointer("/turn/status").and_then(Value::as_str));
    if status == Some("failed") {
        return Err(Error::msg(format!("codex turn failed: {started}")));
    }
    Ok(())
}

struct Client {
    stream: UnixStream,
    buffer: Vec<u8>,
    client_id: String,
    socket: PathBuf,
}

impl Client {
    fn request(
        &mut self,
        method: &str,
        version: u64,
        params: Value,
        timeout: Duration,
    ) -> Result<Value> {
        let request_id = Uuid::new_v4().to_string();
        self.write_json(&json!({
            "type": "request",
            "requestId": request_id,
            "sourceClientId": self.client_id,
            "version": version,
            "method": method,
            "params": params,
            "timeoutMs": timeout.as_millis() as u64,
        }))?;
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(Error::msg(format!(
                    "codex ipc timeout waiting for {method}"
                )));
            }
            self.stream.set_read_timeout(Some(remaining)).ok();
            let value = self.read_json()?;
            let parsed: IpcResponse = serde_json::from_value(value.clone())?;
            match parsed.kind.as_str() {
                "client-discovery-request" => {
                    if let Some(id) = parsed.request_id {
                        self.write_json(&json!({
                            "type": "client-discovery-response",
                            "requestId": id,
                            "response": { "canHandle": false },
                        }))?;
                    }
                }
                "response" if parsed.request_id.as_deref() == Some(request_id.as_str()) => {
                    if parsed.result_type.as_deref() == Some("error") {
                        return Err(Error::msg(format!(
                            "codex ipc {method} error: {}",
                            parsed.error.unwrap_or(Value::Null)
                        )));
                    }
                    return Ok(parsed.result.unwrap_or(Value::Null));
                }
                "broadcast" => {}
                "request" => {
                    if let Some(id) = parsed.request_id {
                        self.write_json(&json!({
                            "type": "response",
                            "requestId": id,
                            "resultType": "error",
                            "error": "no-handler-for-request",
                            "method": parsed.method,
                        }))?;
                    }
                }
                _ => {}
            }
        }
    }

    fn write_json(&mut self, value: &Value) -> Result<()> {
        write_frame(&mut self.stream, value, &self.socket)
    }

    fn read_json(&mut self) -> Result<Value> {
        read_frame(&mut self.stream, &mut self.buffer, &self.socket)
    }
}

pub fn encode_frame(value: &Value) -> Result<Vec<u8>> {
    let payload = serde_json::to_vec(value)?;
    if payload.len() > MAX_FRAME as usize {
        return Err(Error::msg("codex ipc frame too large"));
    }
    let mut frame = Vec::with_capacity(4 + payload.len());
    frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    frame.extend_from_slice(&payload);
    Ok(frame)
}

fn write_frame(stream: &mut UnixStream, value: &Value, socket: &Path) -> Result<()> {
    let frame = encode_frame(value)?;
    stream.write_all(&frame).map_err(|source| Error::Io {
        path: socket.to_path_buf(),
        source,
    })
}

fn read_frame(stream: &mut UnixStream, buffer: &mut Vec<u8>, socket: &Path) -> Result<Value> {
    while buffer.len() < 4 {
        read_more(stream, buffer, socket)?;
    }
    let length = u32::from_le_bytes(buffer[..4].try_into().unwrap());
    if length == 0 || length > MAX_FRAME {
        return Err(Error::msg(format!(
            "codex ipc invalid frame length {length}"
        )));
    }
    let total = 4 + length as usize;
    while buffer.len() < total {
        read_more(stream, buffer, socket)?;
    }
    let payload = buffer[4..total].to_vec();
    buffer.drain(..total);
    Ok(serde_json::from_slice(&payload)?)
}

fn read_more(stream: &mut UnixStream, buffer: &mut Vec<u8>, socket: &Path) -> Result<()> {
    let mut chunk = [0u8; 8192];
    let read = stream.read(&mut chunk).map_err(|source| Error::Io {
        path: socket.to_path_buf(),
        source,
    })?;
    if read == 0 {
        return Err(Error::msg("codex ipc socket closed"));
    }
    buffer.extend_from_slice(&chunk[..read]);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::encode_frame;
    use serde_json::json;

    #[test]
    fn frames_are_little_endian_length_prefixed() {
        let value = json!({"type": "request", "method": "initialize"});
        let frame = encode_frame(&value).unwrap();
        let length = u32::from_le_bytes(frame[..4].try_into().unwrap()) as usize;
        assert_eq!(length, frame.len() - 4);
        let parsed: serde_json::Value = serde_json::from_slice(&frame[4..]).unwrap();
        assert_eq!(parsed["method"], "initialize");
    }
}
