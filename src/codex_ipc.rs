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
    use super::{encode_frame, send_user_turn};
    use serde_json::{Value, json};
    use std::io::{Read, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};
    use std::thread;
    use std::time::Duration;

    fn read_value(stream: &mut UnixStream, buffer: &mut Vec<u8>) -> Value {
        stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
        while buffer.len() < 4 {
            let mut chunk = [0u8; 8192];
            let read = stream.read(&mut chunk).unwrap();
            assert!(read > 0, "client closed");
            buffer.extend_from_slice(&chunk[..read]);
        }
        let length = u32::from_le_bytes(buffer[..4].try_into().unwrap()) as usize;
        let total = 4 + length;
        while buffer.len() < total {
            let mut chunk = [0u8; 8192];
            let read = stream.read(&mut chunk).unwrap();
            assert!(read > 0, "client closed mid-frame");
            buffer.extend_from_slice(&chunk[..read]);
        }
        let payload = buffer[4..total].to_vec();
        buffer.drain(..total);
        serde_json::from_slice(&payload).unwrap()
    }

    fn write_value(stream: &mut UnixStream, value: &Value) {
        let frame = encode_frame(value).unwrap();
        stream.write_all(&frame).unwrap();
        stream.flush().ok();
    }

    fn serve(socket: PathBuf, handler: impl Fn(&Value) -> Vec<Value> + Send + 'static) {
        thread::spawn(move || {
            let listener = UnixListener::bind(&socket).unwrap();
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = Vec::new();
            loop {
                let Ok(request) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    read_value(&mut stream, &mut buffer)
                })) else {
                    break;
                };
                for response in handler(&request) {
                    write_value(&mut stream, &response);
                }
            }
        });
        thread::sleep(Duration::from_millis(50));
    }

    fn temp_socket() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("ipc.sock");
        (dir, socket)
    }

    fn echo_id(request: &Value) -> Value {
        request["requestId"].clone()
    }

    #[test]
    fn frames_are_little_endian_length_prefixed() {
        let value = json!({"type": "request", "method": "initialize"});
        let frame = encode_frame(&value).unwrap();
        let length = u32::from_le_bytes(frame[..4].try_into().unwrap()) as usize;
        assert_eq!(length, frame.len() - 4);
        let parsed: serde_json::Value = serde_json::from_slice(&frame[4..]).unwrap();
        assert_eq!(parsed["method"], "initialize");
    }

    #[test]
    fn send_user_turn_speaks_initialize_and_start_turn() {
        let (_dir, socket) = temp_socket();
        serve(socket.clone(), |request| {
            let id = echo_id(request);
            match request["method"].as_str() {
                Some("initialize") => vec![
                    json!({
                        "type": "client-discovery-request",
                        "requestId": "disc-1"
                    }),
                    json!({"type": "broadcast", "method": "noise"}),
                    json!({
                        "type": "request",
                        "requestId": "inbound-1",
                        "method": "ping"
                    }),
                    json!({"type": "ignored"}),
                    json!({
                        "type": "response",
                        "requestId": id,
                        "result": { "clientId": "client-99" }
                    }),
                ],
                Some("thread-follower-start-turn") => vec![json!({
                    "type": "response",
                    "requestId": id,
                    "result": { "turn": { "status": "in_progress" } }
                })],
                _ => Vec::new(),
            }
        });
        send_user_turn(&socket, "thread-1", "hello from magents").unwrap();
    }

    #[test]
    fn send_user_turn_failed_status_is_error() {
        let (_dir, socket) = temp_socket();
        serve(socket.clone(), |request| {
            let id = echo_id(request);
            match request["method"].as_str() {
                Some("initialize") => vec![json!({
                    "type": "response",
                    "requestId": id,
                    "result": {}
                })],
                Some("thread-follower-start-turn") => vec![json!({
                    "type": "response",
                    "requestId": id,
                    "result": { "turn": { "status": "failed" } }
                })],
                _ => Vec::new(),
            }
        });
        let error = send_user_turn(&socket, "thread-1", "nope").unwrap_err();
        assert!(error.to_string().contains("codex turn failed"));
    }

    #[test]
    fn send_user_turn_rpc_error_is_error() {
        let (_dir, socket) = temp_socket();
        serve(socket.clone(), |request| {
            let id = echo_id(request);
            vec![json!({
                "type": "response",
                "requestId": id,
                "resultType": "error",
                "error": "nope"
            })]
        });
        let error = send_user_turn(&socket, "thread-1", "nope").unwrap_err();
        assert!(error.to_string().contains("codex ipc initialize error"));
    }

    #[test]
    fn send_user_turn_rejects_missing_socket() {
        let error = send_user_turn(Path::new("/tmp/magents-no-ipc.sock"), "t", "m").unwrap_err();
        assert!(error.to_string().contains("failed to read"));
    }

    #[test]
    fn send_user_turn_rejects_zero_length_frame() {
        let (_dir, socket) = temp_socket();
        thread::spawn({
            let socket = socket.clone();
            move || {
                let listener = UnixListener::bind(&socket).unwrap();
                let (mut stream, _) = listener.accept().unwrap();
                let mut sink = [0u8; 32];
                let _ = stream.read(&mut sink);
                stream.write_all(&0u32.to_le_bytes()).unwrap();
            }
        });
        thread::sleep(Duration::from_millis(50));
        let error = send_user_turn(&socket, "t", "m").unwrap_err();
        assert!(error.to_string().contains("invalid frame length"));
    }

    #[test]
    fn send_user_turn_closed_socket() {
        let (_dir, socket) = temp_socket();
        thread::spawn({
            let socket = socket.clone();
            move || {
                let listener = UnixListener::bind(&socket).unwrap();
                let (mut stream, _) = listener.accept().unwrap();
                let mut sink = [0u8; 4096];
                let _ = stream.read(&mut sink);
                drop(stream);
            }
        });
        thread::sleep(Duration::from_millis(50));
        let error = send_user_turn(&socket, "t", "m").unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains("socket closed")
                || message.contains("failed to read")
                || message.contains("timeout"),
            "{message}"
        );
    }

    #[test]
    fn send_user_turn_handles_discovery_without_ids_and_timeout() {
        let (_dir, socket) = temp_socket();
        serve(socket.clone(), |request| {
            let id = echo_id(request);
            match request["method"].as_str() {
                Some("initialize") => vec![
                    json!({ "type": "client-discovery-request" }),
                    json!({ "type": "request", "method": "ping" }),
                    json!({
                        "type": "response",
                        "requestId": id,
                        "result": { "clientId": "c-timeout" }
                    }),
                ],
                Some("thread-follower-start-turn") => vec![json!({
                    "type": "response",
                    "requestId": id,
                    "result": { "turn": { "status": "in_progress" } }
                })],
                _ => Vec::new(),
            }
        });
        send_user_turn(&socket, "thread-1", "ok").unwrap();

        let (_dir, silent) = temp_socket();
        thread::spawn({
            let silent = silent.clone();
            move || {
                let listener = UnixListener::bind(&silent).unwrap();
                let (stream, _) = listener.accept().unwrap();
                thread::sleep(Duration::from_secs(6));
                drop(stream);
            }
        });
        thread::sleep(Duration::from_millis(50));
        let error = send_user_turn(&silent, "t", "m").unwrap_err();
        assert!(
            error.to_string().contains("timeout")
                || error.to_string().contains("socket closed")
                || error.to_string().contains("failed to read"),
            "{error}"
        );
    }

    #[test]
    fn encode_frame_rejects_giant_payload() {
        let huge = "x".repeat((8 * 1024 * 1024) + 1);
        let error = encode_frame(&json!(huge)).unwrap_err();
        assert!(error.to_string().contains("too large"));
    }
}
