use crate::error::{Error, Result};
use crate::homes::Homes;
use crate::model::{Agent, Session, valid_session_id};
use crate::spawn::{Report, Status, Transport};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::cell::Cell;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};
use uuid::Uuid;

const DEFAULT_STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
const CLEANUP_GRACE: Duration = Duration::from_millis(100);
const MAX_CAPTURE: usize = 4096;
const MAX_REPLY: u64 = 64 * 1024;
const MAX_START_LINE: usize = 64 * 1024;
const PROTOCOL_VERSION: u8 = 1;

const IDENTITY_ENV: &[&str] = &[
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
];

const HOME_ENV: &[&str] = &[
    "CLAUDE_CONFIG_DIR",
    "CODEX_HOME",
    "CURSOR_APP_SUPPORT",
    "CURSOR_CONFIG_DIR",
    "CURSOR_DATA_DIR",
    "CURSOR_HOME",
    "GROK_HOME",
    "MAGENTS_HOME",
    "OPENCODE_DATA",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
];

#[derive(Debug, Deserialize, Serialize)]
struct Request {
    message: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct Reply {
    accepted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<Status>,
    #[serde(skip_serializing_if = "Option::is_none")]
    session: Option<Session>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(tag = "control", rename_all = "snake_case")]
enum SupervisorControl {
    Provider {
        version: u8,
        supervisor: u32,
        provider: u32,
        group: u32,
    },
    Error {
        version: u8,
    },
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "control", rename_all = "snake_case")]
enum ParentControl {
    Accept { version: u8 },
    Cancel { version: u8 },
}

#[derive(Clone, Copy, Debug)]
struct Provider {
    pid: u32,
    group: u32,
}

enum ParentSignal {
    Accept,
    Cancel,
}

enum Exchange {
    Control(Result<SupervisorControl>),
    Reply(Box<Result<Reply>>),
}

enum Output {
    Started(String),
    Closed,
}

struct Launch {
    child: Child,
    group: u32,
    output: Receiver<Output>,
}

impl Launch {
    fn provider(&self) -> Provider {
        Provider {
            pid: self.child.id(),
            group: self.group,
        }
    }

    fn wait(mut self) {
        let _ = self.child.wait();
        finish_group(self.group);
    }

    fn terminate(mut self) {
        terminate_owned_child(&mut self.child, self.group);
    }
}

pub(crate) fn start(homes: &Homes, agent: Agent, prompt: &str, cwd: &Path) -> Result<Report> {
    request_supervisor(homes, agent, prompt, cwd, None)
}

pub(crate) fn resume(homes: &Homes, session: &Session, message: &str) -> Result<String> {
    if message.trim().is_empty() {
        return Err(Error::msg("message must not be empty"));
    }
    if !valid_session_id(session.agent, &session.session_id) {
        return Err(Error::msg("session has an invalid id"));
    }
    let cwd = session
        .cwd
        .as_deref()
        .ok_or_else(|| Error::msg("session has no working directory"))?;
    let cwd = fs::canonicalize(cwd)
        .map_err(|_| Error::msg("session working directory is unavailable"))?;
    if !cwd.is_dir() {
        return Err(Error::msg("session working directory is unavailable"));
    }
    let report = request_supervisor(
        homes,
        session.agent,
        message,
        &cwd,
        Some(&session.session_id),
    )?;
    if report.session.agent != session.agent || report.session.session_id != session.session_id {
        return Err(Error::msg("agent resumed a different session"));
    }
    Ok(resume_marker(session.agent).to_string())
}

pub fn supervise(homes: &Homes, agent: Agent, cwd: &Path, session_id: Option<&str>) -> Result<()> {
    let request = read_request()?;
    let parent = watch_parent();
    let control_sent = Cell::new(false);
    let mut reporter = |provider: Provider| {
        write_control(&SupervisorControl::Provider {
            version: PROTOCOL_VERSION,
            supervisor: std::process::id(),
            provider: provider.pid,
            group: provider.group,
        })?;
        control_sent.set(true);
        Ok(())
    };
    let pending = match prepare_request_with(
        homes,
        agent,
        cwd,
        session_id,
        &request.message,
        Some(&mut reporter),
    ) {
        Ok(pending) => pending,
        Err(_) => return report_supervisor_error(agent, control_sent.get()),
    };
    match complete_request(homes, agent, cwd, pending, Some(&parent)) {
        Ok((session, launch)) => {
            let reply = write_reply(&Reply {
                accepted: true,
                status: Some(Status::Starting),
                session: Some(session),
                error: None,
            });
            settle_accepted(launch, reply, &parent)
        }
        Err(_) => report_supervisor_error(agent, true),
    }
}

fn request_supervisor(
    homes: &Homes,
    agent: Agent,
    message: &str,
    cwd: &Path,
    session_id: Option<&str>,
) -> Result<Report> {
    let executable = std::env::var_os("MAGENTS_SUPERVISOR_BIN")
        .map(PathBuf::from)
        .map(Ok)
        .unwrap_or_else(std::env::current_exe)
        .map_err(|_| Error::msg("failed to locate magents supervisor"))?;
    let mut command = Command::new(executable);
    command
        .arg("__supervise")
        .arg(agent.as_str())
        .arg("--cwd")
        .arg(cwd)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(session_id) = session_id {
        command.arg("--session").arg(session_id);
    }
    isolate_environment(&mut command, homes, agent);
    isolate_supervisor(&mut command);
    let mut child = command
        .spawn()
        .map_err(|_| Error::msg("failed to start magents supervisor"))?;
    let group = child.id();
    let Some(stdin) = child.stdin.take() else {
        terminate_child(&mut child, group);
        return Err(Error::msg("magents supervisor input unavailable"));
    };
    let Some(stdout) = child.stdout.take() else {
        terminate_child(&mut child, group);
        return Err(Error::msg("magents supervisor output unavailable"));
    };
    let request = serde_json::to_vec(&Request {
        message: message.to_string(),
    })?;
    let mut stdin = stdin;
    if stdin
        .write_all(&request)
        .and_then(|_| stdin.write_all(b"\n"))
        .and_then(|_| stdin.flush())
        .is_err()
    {
        terminate_child(&mut child, group);
        return Err(Error::msg("failed to send request to magents supervisor"));
    }
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || exchange_supervisor(stdout, sender));
    let deadline = Instant::now() + handshake_timeout();
    let mut providers = Vec::new();
    let mut error_control = false;
    let reply = loop {
        match receive_exchange(&receiver, deadline) {
            Ok(Exchange::Control(Ok(control))) => match validate_control(control, group) {
                Ok(Some(provider)) if !error_control => providers.push(provider),
                Ok(None) if providers.is_empty() && !error_control => error_control = true,
                Ok(_) => {
                    cancel_supervisor(&mut child, group, &mut stdin, &providers);
                    return Err(Error::msg(
                        "magents supervisor returned an invalid control sequence",
                    ));
                }
                Err(error) => {
                    cancel_supervisor(&mut child, group, &mut stdin, &providers);
                    return Err(error);
                }
            },
            Ok(Exchange::Control(Err(error))) => {
                cancel_supervisor(&mut child, group, &mut stdin, &providers);
                return Err(error);
            }
            Ok(Exchange::Reply(reply)) => match *reply {
                Ok(reply) => break reply,
                Err(error) => {
                    cancel_supervisor(&mut child, group, &mut stdin, &providers);
                    return Err(error);
                }
            },
            Err(error) => {
                cancel_supervisor(&mut child, group, &mut stdin, &providers);
                return Err(error);
            }
        }
    };
    if providers.is_empty() && !error_control {
        cancel_supervisor(&mut child, group, &mut stdin, &providers);
        return Err(Error::msg(
            "magents supervisor returned a reply without control",
        ));
    }
    if reply.accepted && providers.is_empty() {
        cancel_supervisor(&mut child, group, &mut stdin, &providers);
        return Err(Error::msg(
            "magents supervisor accepted startup without provider ownership",
        ));
    }
    let session = match validate_reply(reply, agent, cwd, session_id) {
        Ok(session) => session,
        Err(error) => {
            cancel_supervisor(&mut child, group, &mut stdin, &providers);
            return Err(error);
        }
    };
    if write_parent_control(
        &mut stdin,
        &ParentControl::Accept {
            version: PROTOCOL_VERSION,
        },
    )
    .is_err()
    {
        cancel_supervisor(&mut child, group, &mut stdin, &providers);
        return Err(Error::msg(
            "failed to accept magents supervisor startup response",
        ));
    }
    drop(stdin);
    if let Some(provider) = providers.first() {
        let _ = crate::spawn::write_live(homes, &session, child.id(), provider.pid, provider.group);
    }
    thread::spawn(move || {
        let mut child = child;
        let _ = child.wait();
    });
    Ok(Report {
        accepted: true,
        status: Status::Starting,
        session,
    })
}

fn exchange_supervisor(stdout: ChildStdout, sender: mpsc::Sender<Exchange>) {
    let mut reader = BufReader::new(stdout);
    let mut controlled = false;
    loop {
        let name = if controlled {
            "startup response"
        } else {
            "control"
        };
        let raw = match read_frame(&mut reader, name) {
            Ok(raw) => raw,
            Err(error) => {
                let exchange = if controlled {
                    Exchange::Reply(Box::new(Err(error)))
                } else {
                    Exchange::Control(Err(error))
                };
                let _ = sender.send(exchange);
                return;
            }
        };
        let value = match serde_json::from_slice::<Value>(&raw) {
            Ok(value) => value,
            Err(_) => {
                let exchange = if controlled {
                    Exchange::Reply(Box::new(Err(Error::msg(
                        "magents supervisor returned an invalid startup response",
                    ))))
                } else {
                    Exchange::Control(Err(Error::msg(
                        "magents supervisor returned an invalid control message",
                    )))
                };
                let _ = sender.send(exchange);
                return;
            }
        };
        if value.get("control").is_some() {
            let control = serde_json::from_value(value)
                .map_err(|_| Error::msg("magents supervisor returned an invalid control message"));
            let valid = control.is_ok();
            if sender.send(Exchange::Control(control)).is_err() || !valid {
                return;
            }
            controlled = true;
            continue;
        }
        let reply = serde_json::from_value(value)
            .map_err(|_| Error::msg("magents supervisor returned an invalid startup response"));
        let _ = sender.send(Exchange::Reply(Box::new(reply)));
        return;
    }
}

fn read_frame(reader: &mut impl BufRead, name: &str) -> Result<Vec<u8>> {
    let mut raw = Vec::new();
    let mut limited = reader.take(MAX_REPLY);
    if limited.read_until(b'\n', &mut raw).is_err() || raw.last() != Some(&b'\n') {
        return Err(Error::msg(format!("magents supervisor returned no {name}")));
    }
    Ok(raw)
}

fn receive_exchange(receiver: &Receiver<Exchange>, deadline: Instant) -> Result<Exchange> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(Error::msg("magents supervisor startup response timed out"));
    }
    receiver
        .recv_timeout(remaining)
        .map_err(|_| Error::msg("magents supervisor startup response timed out"))
}

fn validate_control(control: SupervisorControl, supervisor: u32) -> Result<Option<Provider>> {
    match control {
        SupervisorControl::Provider {
            version,
            supervisor: reported,
            provider,
            group,
        } if version == PROTOCOL_VERSION
            && reported == supervisor
            && provider > 0
            && group > 0
            && provider != supervisor =>
        {
            Ok(Some(Provider {
                pid: provider,
                group,
            }))
        }
        SupervisorControl::Error { version } if version == PROTOCOL_VERSION => Ok(None),
        _ => Err(Error::msg(
            "magents supervisor returned mismatched provider ownership",
        )),
    }
}

fn write_control(control: &SupervisorControl) -> Result<()> {
    write_standard_output(control)
}

fn write_parent_control(stdin: &mut ChildStdin, control: &ParentControl) -> Result<()> {
    let mut raw = serde_json::to_vec(control)?;
    raw.push(b'\n');
    stdin
        .write_all(&raw)
        .and_then(|_| stdin.flush())
        .map_err(|_| Error::msg("failed to control magents supervisor"))
}

fn watch_parent() -> Receiver<ParentSignal> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut raw = Vec::new();
        let stdin = std::io::stdin();
        let mut stdin = stdin.lock().take(MAX_REPLY);
        let signal = if stdin.read_until(b'\n', &mut raw).is_ok() && raw.last() == Some(&b'\n') {
            match serde_json::from_slice::<ParentControl>(&raw) {
                Ok(ParentControl::Accept { version }) if version == PROTOCOL_VERSION => {
                    ParentSignal::Accept
                }
                Ok(ParentControl::Cancel { version }) if version == PROTOCOL_VERSION => {
                    ParentSignal::Cancel
                }
                _ => ParentSignal::Cancel,
            }
        } else {
            ParentSignal::Cancel
        };
        let _ = sender.send(signal);
    });
    receiver
}

fn validate_reply(
    reply: Reply,
    agent: Agent,
    cwd: &Path,
    expected: Option<&str>,
) -> Result<Session> {
    if !reply.accepted {
        return Err(Error::msg(format!("{} startup failed", agent.as_str())));
    }
    let session = reply
        .session
        .ok_or_else(|| Error::msg("magents supervisor omitted startup metadata"))?;
    if reply.status != Some(Status::Starting)
        || session.agent != agent
        || session.cwd.as_deref() != cwd.to_str()
        || !valid_session_id(agent, &session.session_id)
        || expected.is_some_and(|expected| session.session_id != expected)
    {
        return Err(Error::msg(
            "magents supervisor returned mismatched startup metadata",
        ));
    }
    Ok(session)
}

fn read_request() -> Result<Request> {
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|_| Error::msg("failed to read supervisor request"))?;
    let request = serde_json::from_str::<Request>(&line)
        .map_err(|_| Error::msg("invalid supervisor request"))?;
    if request.message.trim().is_empty() {
        return Err(Error::msg("supervisor message must not be empty"));
    }
    Ok(request)
}

fn write_reply(reply: &Reply) -> Result<()> {
    write_standard_output(reply)
}

fn write_standard_output(value: &impl Serialize) -> Result<()> {
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    let mut raw = serde_json::to_vec(value)?;
    raw.push(b'\n');
    stdout.write_all(&raw).map_err(|source| Error::Io {
        path: PathBuf::from("stdout"),
        source,
    })?;
    stdout.flush().map_err(|source| Error::Io {
        path: PathBuf::from("stdout"),
        source,
    })
}

fn report_supervisor_error(agent: Agent, control_sent: bool) -> Result<()> {
    let control = if control_sent {
        Ok(())
    } else {
        write_control(&SupervisorControl::Error {
            version: PROTOCOL_VERSION,
        })
    };
    let reply = write_reply(&Reply {
        accepted: false,
        status: None,
        session: None,
        error: Some(format!("{} startup failed", agent.as_str())),
    });
    close_standard_input();
    close_standard_output();
    control?;
    reply
}

fn settle_accepted(
    launch: Launch,
    reply: Result<()>,
    parent: &Receiver<ParentSignal>,
) -> Result<()> {
    if let Err(error) = reply {
        launch.terminate();
        return Err(error);
    }
    match parent.recv_timeout(handshake_timeout()) {
        Ok(ParentSignal::Accept) => {
            launch.wait();
            Ok(())
        }
        Ok(ParentSignal::Cancel) | Err(_) => {
            launch.terminate();
            Err(Error::msg("magents supervisor startup was not accepted"))
        }
    }
}

type Pending = (Launch, Option<String>, Option<String>, Transport);
type Reporter<'a> = Option<&'a mut dyn FnMut(Provider) -> Result<()>>;

#[cfg(test)]
fn supervise_request(
    homes: &Homes,
    agent: Agent,
    cwd: &Path,
    session_id: Option<&str>,
    message: &str,
) -> Result<(Session, Launch)> {
    let pending = prepare_request(homes, agent, cwd, session_id, message)?;
    complete_request(homes, agent, cwd, pending, None)
}

#[cfg(test)]
fn prepare_request(
    homes: &Homes,
    agent: Agent,
    cwd: &Path,
    session_id: Option<&str>,
    message: &str,
) -> Result<Pending> {
    prepare_request_with(homes, agent, cwd, session_id, message, None)
}

fn prepare_request_with(
    homes: &Homes,
    agent: Agent,
    cwd: &Path,
    session_id: Option<&str>,
    message: &str,
    mut reporter: Reporter<'_>,
) -> Result<Pending> {
    if !cwd.is_absolute() || !cwd.is_dir() {
        return Err(Error::msg("working directory is unavailable"));
    }
    if let Some(session_id) = session_id
        && !valid_session_id(agent, session_id)
    {
        return Err(Error::msg("session has an invalid id"));
    }
    let resumed = session_id.map(ToOwned::to_owned);
    let (launch, expected, transport) = match session_id {
        Some(session_id) => launch_resume(homes, agent, cwd, session_id, message, &mut reporter)?,
        None => launch_new(homes, agent, cwd, message, &mut reporter)?,
    };
    Ok((launch, expected, resumed, transport))
}

fn complete_request(
    homes: &Homes,
    agent: Agent,
    cwd: &Path,
    pending: Pending,
    parent: Option<&Receiver<ParentSignal>>,
) -> Result<(Session, Launch)> {
    let (launch, expected, resumed, transport) = pending;
    let (started_id, launch) = await_start(launch, expected.as_deref(), parent)?;
    let session = if resumed.is_some() {
        resumed_session(agent, &started_id, cwd, transport)
    } else {
        match crate::spawn::record(homes, agent, &started_id, cwd, transport) {
            Ok(session) => session,
            Err(error) => {
                launch.terminate();
                return Err(error);
            }
        }
    };
    Ok((session, launch))
}

fn launch_new(
    homes: &Homes,
    agent: Agent,
    cwd: &Path,
    message: &str,
    reporter: &mut Reporter<'_>,
) -> Result<(Launch, Option<String>, Transport)> {
    match agent {
        Agent::Claude => {
            let session_id = Uuid::new_v4().to_string();
            let mut command = agent_command(homes, agent, cwd);
            command
                .arg("-p")
                .arg("--verbose")
                .arg("--output-format")
                .arg("stream-json")
                .arg("--session-id")
                .arg(&session_id);
            Ok((
                launch(command, agent, message, reporter)?,
                Some(session_id),
                Transport::ClaudePrint,
            ))
        }
        Agent::Codex => {
            let mut command = agent_command(homes, agent, cwd);
            command
                .arg("exec")
                .arg("--json")
                .arg("-C")
                .arg(cwd)
                .arg("-");
            Ok((
                launch(command, agent, message, reporter)?,
                None,
                Transport::CodexExec,
            ))
        }
        Agent::Cursor => {
            let session_id = create_cursor_chat(homes, cwd, reporter)?;
            let mut command = agent_command(homes, agent, cwd);
            command
                .arg("-p")
                .arg("--output-format")
                .arg("stream-json")
                .arg("--resume")
                .arg(&session_id)
                .arg("--workspace")
                .arg(cwd);
            Ok((
                launch(command, agent, message, reporter)?,
                Some(session_id),
                Transport::CursorAgent,
            ))
        }
        Agent::Grok => {
            let session_id = Uuid::new_v4().to_string();
            let mut command = agent_command(homes, agent, cwd);
            command
                .arg("--cwd")
                .arg(cwd)
                .arg("--session-id")
                .arg(&session_id)
                .arg("--output-format")
                .arg("streaming-json")
                .arg("--prompt-file")
                .arg("/dev/stdin");
            Ok((
                launch(command, agent, message, reporter)?,
                Some(session_id),
                Transport::GrokStream,
            ))
        }
        Agent::OpenCode => {
            let mut command = agent_command(homes, agent, cwd);
            command
                .arg("run")
                .arg("--format")
                .arg("json")
                .arg("--dir")
                .arg(cwd);
            Ok((
                launch(command, agent, message, reporter)?,
                None,
                Transport::OpenCodeRun,
            ))
        }
    }
}

fn launch_resume(
    homes: &Homes,
    agent: Agent,
    cwd: &Path,
    session_id: &str,
    message: &str,
    reporter: &mut Reporter<'_>,
) -> Result<(Launch, Option<String>, Transport)> {
    let mut command = agent_command(homes, agent, cwd);
    let transport = match agent {
        Agent::Claude => {
            command
                .arg("-p")
                .arg("--verbose")
                .arg("--output-format")
                .arg("stream-json")
                .arg("--resume")
                .arg(session_id);
            Transport::ClaudePrint
        }
        Agent::Codex => {
            command
                .arg("exec")
                .arg("--json")
                .arg("-C")
                .arg(cwd)
                .arg("resume")
                .arg(session_id)
                .arg("-");
            Transport::CodexExec
        }
        Agent::Cursor => {
            command
                .arg("-p")
                .arg("--output-format")
                .arg("stream-json")
                .arg("--resume")
                .arg(session_id)
                .arg("--workspace")
                .arg(cwd);
            Transport::CursorAgent
        }
        Agent::Grok => {
            command
                .arg("--cwd")
                .arg(cwd)
                .arg("--resume")
                .arg(session_id)
                .arg("--output-format")
                .arg("streaming-json")
                .arg("--prompt-file")
                .arg("/dev/stdin");
            Transport::GrokStream
        }
        Agent::OpenCode => {
            command
                .arg("run")
                .arg("--format")
                .arg("json")
                .arg("--dir")
                .arg(cwd)
                .arg("--session")
                .arg(session_id);
            Transport::OpenCodeRun
        }
    };
    Ok((
        launch(command, agent, message, reporter)?,
        Some(session_id.to_string()),
        transport,
    ))
}

fn agent_command(homes: &Homes, agent: Agent, cwd: &Path) -> Command {
    let (variable, fallback) = match agent {
        Agent::Claude => ("MAGENTS_CLAUDE_BIN", "claude"),
        Agent::Codex => ("MAGENTS_CODEX_BIN", "codex"),
        Agent::Cursor => ("MAGENTS_CURSOR_BIN", "cursor-agent"),
        Agent::Grok => ("MAGENTS_GROK_BIN", "grok"),
        Agent::OpenCode => ("MAGENTS_OPENCODE_BIN", "opencode"),
    };
    let mut command = Command::new(std::env::var_os(variable).unwrap_or_else(|| fallback.into()));
    command.current_dir(cwd);
    isolate_environment(&mut command, homes, agent);
    command
}

fn isolate_environment(command: &mut Command, homes: &Homes, agent: Agent) {
    for key in IDENTITY_ENV.iter().chain(HOME_ENV) {
        command.env_remove(key);
    }
    command.env("MAGENTS_HOME", &homes.magents);
    match agent {
        Agent::Claude => {
            command.env("CLAUDE_CONFIG_DIR", &homes.claude);
        }
        Agent::Codex => {
            command.env("CODEX_HOME", &homes.codex);
        }
        Agent::Cursor => {
            command
                .env("CURSOR_CONFIG_DIR", &homes.cursor_config)
                .env("CURSOR_DATA_DIR", &homes.cursor);
        }
        Agent::Grok => {
            command.env("GROK_HOME", &homes.grok);
        }
        Agent::OpenCode => {
            command.env("XDG_DATA_HOME", homes.opencode_data_home());
            if let Some(config) = homes.opencode_config.parent() {
                command.env("XDG_CONFIG_HOME", config);
            }
        }
    }
}

fn create_cursor_chat(homes: &Homes, cwd: &Path, reporter: &mut Reporter<'_>) -> Result<String> {
    let mut command = agent_command(homes, Agent::Cursor, cwd);
    command
        .arg("create-chat")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    isolate_provider(&mut command);
    let mut child = command
        .spawn()
        .map_err(|_| Error::msg("failed to start cursor chat creation"))?;
    let group = child.id();
    if let Some(reporter) = reporter.as_deref_mut()
        && let Err(error) = reporter(Provider {
            pid: child.id(),
            group,
        })
    {
        terminate_owned_child(&mut child, group);
        return Err(error);
    }
    let Some(stdout) = child.stdout.take() else {
        terminate_owned_child(&mut child, group);
        return Err(Error::msg("cursor chat creation output unavailable"));
    };
    let Some(stderr) = child.stderr.take() else {
        terminate_owned_child(&mut child, group);
        return Err(Error::msg("cursor chat creation error stream unavailable"));
    };
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let _ = sender.send(drain_bounded(stdout));
    });
    thread::spawn(move || drain(stderr));
    if wait_child(&mut child, startup_timeout()).is_err() {
        terminate_owned_child(&mut child, group);
        return Err(Error::msg("cursor chat creation failed"));
    }
    let status = child
        .wait()
        .map_err(|_| Error::msg("cursor chat creation failed"))?;
    finish_group(group);
    let output = receiver
        .recv_timeout(CLEANUP_GRACE)
        .map_err(|_| Error::msg("cursor chat creation failed"))?;
    if !status.success() {
        return Err(Error::msg("cursor chat creation failed"));
    }
    let mut lines = output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    let id = lines
        .next()
        .filter(|id| valid_session_id(Agent::Cursor, id))
        .ok_or_else(|| Error::msg("cursor chat creation returned an invalid id"))?;
    if lines.next().is_some() {
        return Err(Error::msg("cursor chat creation returned an invalid id"));
    }
    Ok(id.to_string())
}

fn launch(
    mut command: Command,
    agent: Agent,
    message: &str,
    reporter: &mut Reporter<'_>,
) -> Result<Launch> {
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    isolate_provider(&mut command);
    let mut child = command
        .spawn()
        .map_err(|_| Error::msg(format!("failed to start {}", agent.as_str())))?;
    let group = child.id();
    let Some(mut stdin) = child.stdin.take() else {
        terminate_owned_child(&mut child, group);
        return Err(Error::msg(format!("{} input unavailable", agent.as_str())));
    };
    let Some(stdout) = child.stdout.take() else {
        terminate_owned_child(&mut child, group);
        return Err(Error::msg(format!("{} output unavailable", agent.as_str())));
    };
    let Some(stderr) = child.stderr.take() else {
        terminate_owned_child(&mut child, group);
        return Err(Error::msg(format!(
            "{} error stream unavailable",
            agent.as_str()
        )));
    };
    let message = message.as_bytes().to_vec();
    thread::spawn(move || {
        let _ = stdin.write_all(&message);
        let _ = stdin.flush();
    });
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || watch_start(stdout, agent, sender));
    thread::spawn(move || drain(stderr));
    let launch = Launch {
        child,
        group,
        output: receiver,
    };
    if let Some(reporter) = reporter.as_deref_mut()
        && let Err(error) = reporter(launch.provider())
    {
        launch.terminate();
        return Err(error);
    }
    Ok(launch)
}

fn await_start(
    launch: Launch,
    expected: Option<&str>,
    parent: Option<&Receiver<ParentSignal>>,
) -> Result<(String, Launch)> {
    let deadline = Instant::now() + startup_timeout();
    loop {
        if parent.is_some_and(parent_cancelled) {
            launch.terminate();
            return Err(Error::msg("agent startup cancelled"));
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            launch.terminate();
            return Err(Error::msg("agent startup timed out"));
        }
        match launch
            .output
            .recv_timeout(remaining.min(Duration::from_millis(10)))
        {
            Ok(Output::Started(session_id)) => {
                if expected.is_some_and(|expected| expected != session_id) {
                    launch.terminate();
                    return Err(Error::msg("agent returned a different session id"));
                }
                return Ok((session_id, launch));
            }
            Ok(Output::Closed) | Err(RecvTimeoutError::Disconnected) => {
                launch.terminate();
                return Err(Error::msg("agent exited before startup"));
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
    }
}

fn parent_cancelled(parent: &Receiver<ParentSignal>) -> bool {
    match parent.try_recv() {
        Ok(ParentSignal::Accept | ParentSignal::Cancel) | Err(TryRecvError::Disconnected) => true,
        Err(TryRecvError::Empty) => false,
    }
}

fn parse_start(agent: Agent, line: &str) -> Option<String> {
    let value = serde_json::from_str::<Value>(line).ok()?;
    let id = match agent {
        Agent::Claude | Agent::Cursor => {
            if value.get("type")?.as_str()? != "system" || value.get("subtype")?.as_str()? != "init"
            {
                return None;
            }
            value.get("session_id")?.as_str()?
        }
        Agent::Codex => {
            if value.get("type")?.as_str()? != "thread.started" {
                return None;
            }
            value.get("thread_id")?.as_str()?
        }
        Agent::Grok => {
            if value.get("method")?.as_str()? != "session/update" {
                return None;
            }
            value.pointer("/params/sessionId")?.as_str()?
        }
        Agent::OpenCode => {
            if value.get("type")?.as_str()? != "step_start" {
                return None;
            }
            value.get("sessionID")?.as_str()?
        }
    };
    valid_session_id(agent, id).then(|| id.to_string())
}

fn watch_start(mut reader: impl Read, agent: Agent, sender: mpsc::Sender<Output>) {
    let mut line = Vec::new();
    let mut overflow = false;
    let mut buffer = [0_u8; 8192];
    while let Ok(read) = reader.read(&mut buffer) {
        if read == 0 {
            break;
        }
        for byte in &buffer[..read] {
            if *byte == b'\n' {
                if !overflow
                    && let Ok(line) = std::str::from_utf8(&line)
                    && let Some(session_id) = parse_start(agent, line)
                {
                    let _ = sender.send(Output::Started(session_id));
                    drain(reader);
                    let _ = sender.send(Output::Closed);
                    return;
                }
                line.clear();
                overflow = false;
            } else if line.len() < MAX_START_LINE {
                line.push(*byte);
            } else {
                overflow = true;
            }
        }
    }
    let _ = sender.send(Output::Closed);
}

fn wait_child(child: &mut Child, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        if child
            .try_wait()
            .map_err(|_| Error::msg("agent process status unavailable"))?
            .is_some()
        {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(Error::msg("agent process timed out"));
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn drain(mut reader: impl Read) {
    let _ = std::io::copy(&mut reader, &mut std::io::sink());
}

fn drain_bounded(mut reader: impl Read) -> String {
    let mut kept = Vec::new();
    let mut buffer = [0_u8; 8192];
    while let Ok(read) = reader.read(&mut buffer) {
        if read == 0 {
            break;
        }
        let remaining = MAX_CAPTURE.saturating_sub(kept.len());
        kept.extend_from_slice(&buffer[..read.min(remaining)]);
    }
    String::from_utf8(kept).unwrap_or_default()
}

fn isolate_supervisor(command: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;

        // SAFETY: setsid is async-signal-safe and this closure does not allocate.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
}

fn isolate_provider(command: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;

        // SAFETY: setpgid is async-signal-safe and this closure does not allocate.
        unsafe {
            command.pre_exec(|| {
                if libc::setpgid(0, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
}

fn cancel_supervisor(
    child: &mut Child,
    supervisor: u32,
    stdin: &mut ChildStdin,
    providers: &[Provider],
) {
    let _ = write_parent_control(
        stdin,
        &ParentControl::Cancel {
            version: PROTOCOL_VERSION,
        },
    );
    if wait_child(child, CLEANUP_GRACE.saturating_mul(2)).is_ok() {
        let _ = child.wait();
        for provider in providers {
            terminate_provider(*provider, supervisor, false);
        }
        return;
    }
    for provider in providers {
        terminate_provider(*provider, supervisor, true);
    }
    if wait_child(child, CLEANUP_GRACE).is_ok() {
        let _ = child.wait();
        for provider in providers {
            terminate_provider(*provider, supervisor, false);
        }
        return;
    }
    terminate_child(child, supervisor);
    for provider in providers {
        terminate_provider(*provider, supervisor, false);
    }
}

fn terminate_provider(provider: Provider, supervisor: u32, trusted: bool) {
    #[cfg(unix)]
    {
        if trusted && supervisor_session_alive(supervisor) {
            signal_group(provider.group, libc::SIGTERM);
            let deadline = Instant::now() + CLEANUP_GRACE;
            while group_exists(provider.group) && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
            }
            if group_exists(provider.group) {
                signal_group(provider.group, libc::SIGKILL);
            }
            return;
        }
        if !provider_owned(provider, supervisor) {
            return;
        }
        signal_group(provider.group, libc::SIGTERM);
        let deadline = Instant::now() + CLEANUP_GRACE;
        while provider_owned(provider, supervisor) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        if provider_owned(provider, supervisor) {
            signal_group(provider.group, libc::SIGKILL);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = provider;
        let _ = supervisor;
        let _ = trusted;
    }
}

#[cfg(unix)]
fn supervisor_session_alive(supervisor: u32) -> bool {
    let Ok(supervisor) = i32::try_from(supervisor) else {
        return false;
    };
    unsafe { libc::getpgid(supervisor) == supervisor && libc::getsid(supervisor) == supervisor }
}

#[cfg(unix)]
fn provider_owned(provider: Provider, supervisor: u32) -> bool {
    let Ok(pid) = i32::try_from(provider.pid) else {
        return false;
    };
    let Ok(group) = i32::try_from(provider.group) else {
        return false;
    };
    let Ok(supervisor) = i32::try_from(supervisor) else {
        return false;
    };
    unsafe { libc::getpgid(pid) == group && libc::getsid(pid) == supervisor }
}

fn terminate_owned_child(child: &mut Child, group: u32) {
    #[cfg(unix)]
    {
        signal_group(group, libc::SIGTERM);
        if wait_child(child, CLEANUP_GRACE).is_ok() {
            let _ = child.wait();
        }
        if group_exists(group) {
            signal_group(group, libc::SIGKILL);
        }
        let _ = child.wait();
    }
    #[cfg(not(unix))]
    {
        let _ = group;
        let _ = child.kill();
        let _ = child.wait();
    }
}

fn terminate_child(child: &mut Child, group: u32) {
    #[cfg(unix)]
    {
        if child.try_wait().is_ok_and(|status| status.is_some()) {
            let _ = child.wait();
            return;
        }
        signal_group(group, libc::SIGTERM);
        let _ = wait_child(child, CLEANUP_GRACE);
        signal_group(group, libc::SIGKILL);
        let _ = child.wait();
    }
    #[cfg(not(unix))]
    {
        let _ = group;
        let _ = child.kill();
        let _ = child.wait();
    }
}

fn finish_group(group: u32) {
    #[cfg(unix)]
    {
        if signal_group(group, libc::SIGTERM) {
            thread::sleep(CLEANUP_GRACE);
            signal_group(group, libc::SIGKILL);
        }
    }
    #[cfg(not(unix))]
    let _ = group;
}

#[cfg(unix)]
fn signal_group(group: u32, signal: libc::c_int) -> bool {
    i32::try_from(group)
        .ok()
        .is_some_and(|group| unsafe { libc::kill(-group, signal) == 0 })
}

#[cfg(unix)]
fn group_exists(group: u32) -> bool {
    signal_group(group, 0)
}

fn close_standard_input() {
    #[cfg(unix)]
    unsafe {
        libc::close(libc::STDIN_FILENO);
    }
}

fn close_standard_output() {
    #[cfg(unix)]
    unsafe {
        libc::close(libc::STDOUT_FILENO);
    }
}

fn startup_timeout() -> Duration {
    std::env::var("MAGENTS_STARTUP_TIMEOUT_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_STARTUP_TIMEOUT)
}

fn handshake_timeout() -> Duration {
    std::env::var("MAGENTS_HANDSHAKE_TIMEOUT_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .map(Duration::from_millis)
        .unwrap_or_else(|| startup_timeout().saturating_add(CLEANUP_GRACE.saturating_mul(2)))
}

fn resume_marker(agent: Agent) -> &'static str {
    match agent {
        Agent::Claude => "claude-cli",
        Agent::Codex => "codex-exec",
        Agent::Cursor => "cursor-cli",
        Agent::Grok => "grok-single",
        Agent::OpenCode => "opencode-run",
    }
}

fn resumed_session(agent: Agent, session_id: &str, cwd: &Path, transport: Transport) -> Session {
    Session {
        agent,
        session_id: session_id.to_string(),
        desktop_id: None,
        name: None,
        title: None,
        cwd: Some(cwd.to_string_lossy().into_owned()),
        branch: None,
        live: false,
        archived: false,
        pid: None,
        model: None,
        last_activity_at: None,
        transcript_path: None,
        messaging_socket: None,
        origin: Some(transport.as_str().to_string()),
        tmux: None,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_START_LINE, Output, PROTOCOL_VERSION, ParentSignal, Provider, Reply, SupervisorControl,
        isolate_provider, parse_start, receive_exchange, request_supervisor, resume, resume_marker,
        resumed_session, settle_accepted, supervise_request, supervisor_session_alive,
        terminate_provider, validate_control, watch_start,
    };
    use crate::error::Error;
    use crate::homes::{Homes, pid_alive};
    use crate::model::Agent;
    use crate::spawn::{Status, Transport, records};
    use crate::test_env;
    use std::fs;
    use std::io::Cursor;
    use std::process::{Command, Stdio};
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration, Instant};

    const ENV: &[&str] = &[
        "CLAUDE_CONFIG_DIR",
        "CLAUDE_SESSION_ID",
        "CODEX_HOME",
        "CODEX_THREAD_ID",
        "CURSOR_CONFIG_DIR",
        "CURSOR_DATA_DIR",
        "CURSOR_SESSION_ID",
        "GROK_HOME",
        "GROK_SESSION_ID",
        "MAGENTS_CLAUDE_BIN",
        "MAGENTS_CODEX_BIN",
        "MAGENTS_CURSOR_BIN",
        "MAGENTS_GROK_BIN",
        "MAGENTS_HANDSHAKE_TIMEOUT_MS",
        "MAGENTS_OPENCODE_BIN",
        "MAGENTS_SUPERVISOR_BIN",
        "MAGENTS_STARTUP_TIMEOUT_MS",
        "MAGENTS_TEST_AGENT",
        "MAGENTS_TEST_ARGS",
        "MAGENTS_TEST_CONTROL",
        "MAGENTS_TEST_ENV",
        "MAGENTS_TEST_PROCESS",
        "MAGENTS_TEST_STDIN",
        "MAGENTS_TEST_REPLY",
        "OPENCODE_SESSION_ID",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
    ];

    const SCRIPT: &str = r#"
if [ "$1" = 'create-chat' ]; then
    printf '%s\n' '33333333-3333-4333-8333-333333333333'
    exit 0
fi
session=''
next=''
for argument in "$@"; do
    if [ "$next" = 'session' ]; then session="$argument"; next=''; fi
    if [ "$argument" = '--session-id' ] || [ "$argument" = '--resume' ]; then next='session'; fi
done
printf '%s\n' "$@" > "$MAGENTS_TEST_ARGS"
printf '%s|%s|%s|%s|%s|%s|%s|%s\n' "$CLAUDE_CONFIG_DIR" "$CODEX_HOME" "$CURSOR_CONFIG_DIR" "$CURSOR_DATA_DIR" "$GROK_HOME" "$XDG_DATA_HOME" "$XDG_CONFIG_HOME" "$MAGENTS_HOME" > "$MAGENTS_TEST_ENV"
cat > "$MAGENTS_TEST_STDIN"
case "$MAGENTS_TEST_AGENT" in
    claude|cursor) printf '{"type":"system","subtype":"init","session_id":"%s"}\n' "$session" ;;
    codex) printf '%s\n' '{"type":"thread.started","thread_id":"22222222-2222-4222-8222-222222222222"}' ;;
    grok) printf '{"method":"session/update","params":{"sessionId":"%s"}}\n' "$session" ;;
    opencode) printf '%s\n' '{"type":"step_start","sessionID":"ses_fixture_444"}' ;;
esac
"#;

    const SUPERVISOR_SCRIPT: &str = r#"
IFS= read -r request
(sleep 5) &
provider=$!
if [ -n "${MAGENTS_TEST_PROCESS-}" ]; then
    printf '%s:%s\n' "$$" "$provider" > "$MAGENTS_TEST_PROCESS"
fi
case "$MAGENTS_TEST_CONTROL" in
    auto)
        group=$(ps -o pgid= -p "$provider" | tr -d ' ')
        printf '{"control":"provider","version":1,"supervisor":%s,"provider":%s,"group":%s}\n' "$$" "$provider" "$group"
        ;;
    close) exit 0 ;;
    truncated) printf '%s' '{}' ; exit 0 ;;
    error-then-provider)
        printf '{"control":"error","version":1}\n'
        group=$(ps -o pgid= -p "$provider" | tr -d ' ')
        printf '{"control":"provider","version":1,"supervisor":%s,"provider":%s,"group":%s}\n' "$$" "$provider" "$group"
        ;;
    *) printf '%s' "$MAGENTS_TEST_CONTROL" ;;
esac
printf '%s' "$MAGENTS_TEST_REPLY"
if ! IFS= read -r parent; then
    kill "$provider" 2>/dev/null || true
    wait "$provider" 2>/dev/null || true
    exit 0
fi
case "$parent" in
    *'"control":"accept"'*) wait "$provider" ;;
    *)
        kill "$provider" 2>/dev/null || true
        wait "$provider" 2>/dev/null || true
        ;;
esac
"#;

    #[test]
    fn launches_and_parses_all_current_creation_paths() {
        let _guard = test_env::lock(ENV);
        let directory = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(directory.path());
        let cwd = fs::canonicalize(directory.path()).unwrap();
        let binary = directory.path().join("agent");
        let args = directory.path().join("args");
        let stdin = directory.path().join("stdin");
        let environment = directory.path().join("environment");
        test_env::write_executable(&binary, SCRIPT);
        unsafe {
            std::env::set_var("MAGENTS_TEST_ARGS", &args);
            std::env::set_var("MAGENTS_TEST_STDIN", &stdin);
            std::env::set_var("MAGENTS_TEST_ENV", &environment);
            std::env::set_var("CLAUDE_SESSION_ID", "foreign-claude");
            std::env::set_var("CODEX_THREAD_ID", "foreign-codex");
            std::env::set_var("CURSOR_SESSION_ID", "foreign-cursor");
            std::env::set_var("GROK_SESSION_ID", "foreign-grok");
            std::env::set_var("OPENCODE_SESSION_ID", "foreign-opencode");
        }
        let secret = "fixture prompt SECRET-5b9d";
        for (agent, variable, expected, transport) in [
            (
                Agent::Claude,
                "MAGENTS_CLAUDE_BIN",
                "--verbose\n--output-format\nstream-json",
                Transport::ClaudePrint,
            ),
            (
                Agent::Codex,
                "MAGENTS_CODEX_BIN",
                "exec\n--json",
                Transport::CodexExec,
            ),
            (
                Agent::Cursor,
                "MAGENTS_CURSOR_BIN",
                "--workspace",
                Transport::CursorAgent,
            ),
            (
                Agent::Grok,
                "MAGENTS_GROK_BIN",
                "--prompt-file\n/dev/stdin",
                Transport::GrokStream,
            ),
            (
                Agent::OpenCode,
                "MAGENTS_OPENCODE_BIN",
                "run\n--format\njson",
                Transport::OpenCodeRun,
            ),
        ] {
            unsafe {
                std::env::set_var(variable, &binary);
                std::env::set_var("MAGENTS_TEST_AGENT", agent.as_str());
            }
            let (session, launch) = supervise_request(&homes, agent, &cwd, None, secret).unwrap();
            launch.wait();
            assert_eq!(session.agent, agent);
            assert!(!session.live);
            assert_eq!(session.origin.as_deref(), Some(transport.as_str()));
            let command = fs::read_to_string(&args).unwrap();
            assert!(command.contains(expected), "{agent}: {command}");
            assert!(!command.contains(secret), "{agent}: {command}");
            for forbidden in [
                "--always-approve",
                "--force",
                "--yolo",
                "--trust",
                "--dangerously-bypass-approvals-and-sandbox",
                "--skip-git-repo-check",
            ] {
                assert!(!command.contains(forbidden), "{agent}: {command}");
            }
            assert_eq!(fs::read_to_string(&stdin).unwrap(), secret);
            let values = fs::read_to_string(&environment).unwrap();
            let values = values.trim().split('|').collect::<Vec<_>>();
            assert_eq!(values.len(), 8);
            assert_eq!(values[7], homes.magents.to_string_lossy());
            let expected = match agent {
                Agent::Claude => vec![(0, homes.claude.as_path())],
                Agent::Codex => vec![(1, homes.codex.as_path())],
                Agent::Cursor => vec![
                    (2, homes.cursor_config.as_path()),
                    (3, homes.cursor.as_path()),
                ],
                Agent::Grok => vec![(4, homes.grok.as_path())],
                Agent::OpenCode => vec![
                    (5, homes.opencode_data_home()),
                    (6, homes.opencode_config.parent().unwrap()),
                ],
            };
            for (index, value) in values[..7].iter().enumerate() {
                let target = expected.iter().find(|(target, _)| *target == index);
                match target {
                    Some((_, path)) => assert_eq!(*value, path.to_string_lossy()),
                    None => assert!(value.is_empty(), "{agent}: {values:?}"),
                }
            }
        }
        let records = records(&homes).unwrap();
        assert_eq!(records.len(), 5);
        assert!(records.iter().all(|record| record.session().is_some()));
    }

    #[test]
    fn startup_parsers_require_authoritative_events() {
        assert_eq!(
            parse_start(
                Agent::Claude,
                r#"{"type":"system","subtype":"init","session_id":"c1"}"#
            )
            .as_deref(),
            Some("c1")
        );
        assert_eq!(
            parse_start(
                Agent::Codex,
                r#"{"type":"thread.started","thread_id":"cx1"}"#
            )
            .as_deref(),
            Some("cx1")
        );
        assert_eq!(
            parse_start(
                Agent::Cursor,
                r#"{"type":"system","subtype":"init","session_id":"u1"}"#
            )
            .as_deref(),
            Some("u1")
        );
        assert_eq!(
            parse_start(
                Agent::Grok,
                r#"{"method":"session/update","params":{"sessionId":"g1"}}"#
            )
            .as_deref(),
            Some("g1")
        );
        assert_eq!(
            parse_start(
                Agent::OpenCode,
                r#"{"type":"step_start","sessionID":"ses_o1"}"#
            )
            .as_deref(),
            Some("ses_o1")
        );
        assert!(parse_start(Agent::Codex, r#"{"thread_id":"not-started"}"#).is_none());
        assert!(
            parse_start(
                Agent::Claude,
                r#"{"type":"user","subtype":"init","session_id":"c1"}"#
            )
            .is_none()
        );
        assert!(parse_start(Agent::Codex, r#"{"type":"other","thread_id":"cx"}"#).is_none());
        assert!(
            parse_start(
                Agent::Grok,
                r#"{"method":"other","params":{"sessionId":"g1"}}"#
            )
            .is_none()
        );
        assert!(parse_start(Agent::Grok, r#"{"params":{"sessionId":"g1"}}"#).is_none());
        assert!(parse_start(Agent::Claude, "not-json").is_none());
        assert!(
            parse_start(
                Agent::OpenCode,
                r#"{"type":"text","sessionID":"ses_valid"}"#
            )
            .is_none()
        );
        assert!(parse_start(Agent::OpenCode, r#"{"sessionID":"bad id"}"#).is_none());
    }

    #[test]
    fn startup_reader_bounds_lines_then_drains_raw_bytes() {
        let mut output = vec![b'x'; MAX_START_LINE * 4];
        output.push(b'\n');
        output.extend_from_slice(
            br#"{"type":"system","subtype":"init","session_id":"cursor-session"}"#,
        );
        output.push(b'\n');
        output.resize(output.len() + MAX_START_LINE * 8, b'x');
        let (sender, receiver) = mpsc::channel();

        watch_start(Cursor::new(output), Agent::Cursor, sender);

        assert!(matches!(
            receiver.recv().unwrap(),
            Output::Started(session_id) if session_id == "cursor-session"
        ));
        assert!(matches!(receiver.recv().unwrap(), Output::Closed));
    }

    #[test]
    fn resumes_every_agent_with_stdin_and_exact_session() {
        let _guard = test_env::lock(ENV);
        let directory = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(directory.path());
        let cwd = fs::canonicalize(directory.path()).unwrap();
        let binary = directory.path().join("agent");
        let args = directory.path().join("args");
        let stdin = directory.path().join("stdin");
        let environment = directory.path().join("environment");
        test_env::write_executable(&binary, SCRIPT);
        unsafe {
            std::env::set_var("MAGENTS_TEST_ARGS", &args);
            std::env::set_var("MAGENTS_TEST_STDIN", &stdin);
            std::env::set_var("MAGENTS_TEST_ENV", &environment);
        }
        let message = "resume privately SECRET-794a";
        for (agent, variable, session_id, expected) in [
            (
                Agent::Claude,
                "MAGENTS_CLAUDE_BIN",
                "claude-resume-1",
                "--resume\nclaude-resume-1",
            ),
            (
                Agent::Codex,
                "MAGENTS_CODEX_BIN",
                "22222222-2222-4222-8222-222222222222",
                "resume\n22222222-2222-4222-8222-222222222222\n-",
            ),
            (
                Agent::Cursor,
                "MAGENTS_CURSOR_BIN",
                "cursor-resume-1",
                "--resume\ncursor-resume-1",
            ),
            (
                Agent::Grok,
                "MAGENTS_GROK_BIN",
                "grok-resume-1",
                "--resume\ngrok-resume-1",
            ),
            (
                Agent::OpenCode,
                "MAGENTS_OPENCODE_BIN",
                "ses_fixture_444",
                "--session\nses_fixture_444",
            ),
        ] {
            unsafe {
                std::env::set_var(variable, &binary);
                std::env::set_var("MAGENTS_TEST_AGENT", agent.as_str());
            }
            let (session, launch) =
                supervise_request(&homes, agent, &cwd, Some(session_id), message).unwrap();
            launch.wait();
            assert_eq!(session.session_id, session_id);
            let command = fs::read_to_string(&args).unwrap();
            assert!(command.contains(expected), "{agent}: {command}");
            assert!(!command.contains(message));
            assert_eq!(fs::read_to_string(&stdin).unwrap(), message);
        }
        assert!(records(&homes).unwrap().is_empty());
    }

    #[test]
    fn startup_failure_is_bounded_sanitized_and_unregistered() {
        let _guard = test_env::lock(ENV);
        let directory = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(directory.path());
        let cwd = fs::canonicalize(directory.path()).unwrap();
        let binary = directory.path().join("agent");
        test_env::write_executable(
            &binary,
            "cat >/dev/null\nprintf '%s\\n' 'private child failure with token-SECRET' >&2\nsleep 5",
        );
        unsafe {
            std::env::set_var("MAGENTS_CODEX_BIN", &binary);
            std::env::set_var("MAGENTS_STARTUP_TIMEOUT_MS", "50");
        }
        let error =
            match supervise_request(&homes, Agent::Codex, &cwd, None, "private prompt-SECRET") {
                Ok(_) => panic!("startup unexpectedly succeeded"),
                Err(error) => error,
            };
        let error = error.to_string();
        assert!(error.contains("timed out"));
        assert!(!error.contains("private"));
        assert!(!error.contains("token"));
        assert!(records(&homes).unwrap().is_empty());
    }

    #[test]
    fn failed_provider_leader_exit_kills_pipe_holding_group() {
        let _guard = test_env::lock(ENV);
        let directory = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(directory.path());
        let cwd = fs::canonicalize(directory.path()).unwrap();
        let binary = directory.path().join("agent");
        let process = directory.path().join("process");
        test_env::write_executable(
            &binary,
            "cat >/dev/null\n(sleep 5) &\ndescendant=$!\nprintf '%s:%s:%s\\n' \"$$\" \"$descendant\" \"$(ps -o pgid= -p \"$$\" | tr -d ' ')\" > \"$MAGENTS_TEST_PROCESS\"\nexit 0",
        );
        unsafe {
            std::env::set_var("MAGENTS_CODEX_BIN", &binary);
            std::env::set_var("MAGENTS_STARTUP_TIMEOUT_MS", "1000");
            std::env::set_var("MAGENTS_TEST_PROCESS", &process);
        }
        let started = Instant::now();

        let error = match supervise_request(&homes, Agent::Codex, &cwd, None, "private prompt") {
            Ok(_) => panic!("startup unexpectedly succeeded"),
            Err(error) => error.to_string(),
        };

        assert!(error.contains("timed out"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(3));
        let process = fs::read_to_string(process).unwrap();
        let mut process = process.trim().split(':');
        let leader = process.next().unwrap().parse::<u32>().unwrap();
        let descendant = process.next().unwrap().parse::<u32>().unwrap();
        let group = process.next().unwrap();
        assert_eq!(leader.to_string(), group);
        assert!(!pid_alive(leader));
        let deadline = Instant::now() + Duration::from_secs(2);
        while pid_alive(descendant) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(!pid_alive(descendant));
    }

    #[test]
    fn accepted_reply_write_failure_cancels_and_reaps_provider() {
        let _guard = test_env::lock(ENV);
        let directory = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(directory.path());
        let cwd = fs::canonicalize(directory.path()).unwrap();
        let binary = directory.path().join("agent");
        let done = directory.path().join("done");
        test_env::write_executable(
            &binary,
            "cat >/dev/null\nprintf '%s\\n' '{\"type\":\"thread.started\",\"thread_id\":\"codex-valid\"}'\nsleep 0.05\nprintf done > \"$MAGENTS_TEST_DONE\"",
        );
        unsafe {
            std::env::set_var("MAGENTS_CODEX_BIN", &binary);
            std::env::set_var("MAGENTS_TEST_DONE", &done);
        }
        let (_, launch) =
            supervise_request(&homes, Agent::Codex, &cwd, None, "private prompt").unwrap();
        let broken = Err(Error::Io {
            path: "stdout".into(),
            source: std::io::Error::new(std::io::ErrorKind::BrokenPipe, "closed parent"),
        });

        let (_sender, receiver) = mpsc::channel();
        assert!(settle_accepted(launch, broken, &receiver).is_err());

        assert!(!done.exists());
    }

    #[test]
    fn accepted_reply_parent_cancel_terminates_provider() {
        let _guard = test_env::lock(ENV);
        let directory = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(directory.path());
        let cwd = fs::canonicalize(directory.path()).unwrap();
        let binary = directory.path().join("agent");
        test_env::write_executable(
            &binary,
            "cat >/dev/null\nprintf '%s\\n' '{\"type\":\"thread.started\",\"thread_id\":\"codex-valid\"}'\nsleep 5",
        );
        unsafe { std::env::set_var("MAGENTS_CODEX_BIN", &binary) };
        let (_, launch) =
            supervise_request(&homes, Agent::Codex, &cwd, None, "private prompt").unwrap();
        let (sender, receiver) = mpsc::channel();
        sender.send(ParentSignal::Cancel).unwrap();
        let started = Instant::now();

        let error = settle_accepted(launch, Ok(()), &receiver)
            .unwrap_err()
            .to_string();

        assert!(error.contains("not accepted"));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn mismatched_authoritative_session_terminates_provider() {
        let _guard = test_env::lock(ENV);
        let directory = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(directory.path());
        let cwd = fs::canonicalize(directory.path()).unwrap();
        let binary = directory.path().join("agent");
        test_env::write_executable(
            &binary,
            "cat >/dev/null\nprintf '%s\\n' '{\"type\":\"thread.started\",\"thread_id\":\"actual-id\"}'\nsleep 5",
        );
        unsafe { std::env::set_var("MAGENTS_CODEX_BIN", &binary) };

        let error = match supervise_request(
            &homes,
            Agent::Codex,
            &cwd,
            Some("expected-id"),
            "private prompt",
        ) {
            Ok(_) => panic!("resume unexpectedly succeeded"),
            Err(error) => error.to_string(),
        };

        assert!(error.contains("different session id"), "{error}");
    }

    #[test]
    #[cfg(unix)]
    fn provider_cleanup_validates_live_and_leaderless_ownership() {
        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg("sleep 5")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        isolate_provider(&mut command);
        let mut child = command.spawn().unwrap();
        let provider = Provider {
            pid: child.id(),
            group: child.id(),
        };
        let session = u32::try_from(unsafe { libc::getsid(child.id() as i32) }).unwrap();
        assert!(pid_alive(provider.pid));

        terminate_provider(provider, session, false);
        child.wait().unwrap();

        assert!(!pid_alive(provider.pid));
        terminate_provider(
            Provider {
                pid: u32::MAX,
                group: u32::MAX,
            },
            u32::MAX,
            false,
        );
        assert!(!supervisor_session_alive(u32::MAX));

        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg("trap '' TERM; exec /bin/sleep 30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        super::isolate_supervisor(&mut command);
        let mut child = command.spawn().unwrap();
        thread::sleep(Duration::from_millis(30));
        let pid = child.id();
        terminate_provider(Provider { pid, group: pid }, pid, true);
        let _ = child.wait();
        assert!(!pid_alive(pid));
    }

    #[test]
    fn stubborn_supervisor_cancel_and_invalid_json_after_control() {
        let _guard = test_env::lock(ENV);
        let directory = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(directory.path());
        let cwd = fs::canonicalize(directory.path()).unwrap();
        let binary = directory.path().join("supervisor");
        test_env::write_executable(&binary, "IFS= read -r request\ntrap '' TERM\nsleep 5");
        unsafe {
            std::env::set_var("MAGENTS_SUPERVISOR_BIN", &binary);
            std::env::set_var("MAGENTS_HANDSHAKE_TIMEOUT_MS", "30");
        }
        let error = request_supervisor(&homes, Agent::Codex, "private prompt", &cwd, None)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("timed out") || error.contains("invalid"),
            "{error}"
        );

        test_env::write_executable(&binary, SUPERVISOR_SCRIPT);
        unsafe {
            std::env::set_var("MAGENTS_TEST_CONTROL", "auto");
            std::env::set_var("MAGENTS_TEST_REPLY", "not-json\n");
            std::env::remove_var("MAGENTS_HANDSHAKE_TIMEOUT_MS");
        }
        let error = request_supervisor(&homes, Agent::Codex, "private prompt", &cwd, None)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("invalid") || error.contains("startup"),
            "{error}"
        );

        let reply = serde_json::to_string(&Reply {
            accepted: true,
            status: Some(Status::Starting),
            session: Some(resumed_session(
                Agent::Codex,
                "codex-valid",
                &cwd,
                Transport::CodexExec,
            )),
            error: None,
        })
        .unwrap()
            + "\n";
        unsafe {
            std::env::set_var("MAGENTS_TEST_CONTROL", "error-then-provider");
            std::env::set_var("MAGENTS_TEST_REPLY", reply);
        }
        let error = request_supervisor(&homes, Agent::Codex, "private prompt", &cwd, None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("invalid control sequence"), "{error}");
    }

    #[test]
    fn invalid_controls_and_expired_exchange_are_rejected() {
        assert!(
            validate_control(
                SupervisorControl::Provider {
                    version: PROTOCOL_VERSION + 1,
                    supervisor: 1,
                    provider: 2,
                    group: 2,
                },
                1,
            )
            .is_err()
        );
        let (_sender, receiver) = mpsc::channel();
        assert!(receive_exchange(&receiver, Instant::now()).is_err());
    }

    #[test]
    fn provider_group_cleanup_ignores_grandchild_pipe_holders() {
        let _guard = test_env::lock(ENV);
        let directory = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(directory.path());
        let cwd = fs::canonicalize(directory.path()).unwrap();
        let binary = directory.path().join("agent");
        let process = directory.path().join("process");
        test_env::write_executable(
            &binary,
            "cat >/dev/null\nprintf '%s:%s\\n' \"$$\" \"$(ps -o pgid= -p \"$$\" | tr -d ' ')\" > \"$MAGENTS_TEST_PROCESS\"\nprintf '%s\\n' '{\"type\":\"thread.started\",\"thread_id\":\"codex-valid\"}'\n(sleep 5) &",
        );
        unsafe {
            std::env::set_var("MAGENTS_CODEX_BIN", &binary);
            std::env::set_var("MAGENTS_TEST_PROCESS", &process);
        }
        let started = Instant::now();
        let (_, launch) =
            supervise_request(&homes, Agent::Codex, &cwd, None, "private prompt").unwrap();
        launch.wait();

        assert!(started.elapsed() < Duration::from_secs(1));
        let process = fs::read_to_string(process).unwrap();
        let (pid, group) = process.trim().split_once(':').unwrap();
        assert_eq!(pid, group);
    }

    #[test]
    fn supervisor_handshake_timeout_is_bounded_with_inherited_pipes() {
        let _guard = test_env::lock(ENV);
        let directory = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(directory.path());
        let cwd = fs::canonicalize(directory.path()).unwrap();
        let binary = directory.path().join("supervisor");
        test_env::write_executable(&binary, "IFS= read -r request\n(sleep 5) &\nsleep 5");
        unsafe {
            std::env::set_var("MAGENTS_SUPERVISOR_BIN", &binary);
            std::env::set_var("MAGENTS_STARTUP_TIMEOUT_MS", "30");
        }
        let started = Instant::now();

        let error = request_supervisor(&homes, Agent::Codex, "private prompt", &cwd, None)
            .unwrap_err()
            .to_string();

        assert!(error.contains("timed out"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    #[cfg(unix)]
    fn supervisor_timeout_uses_owned_group_before_forced_cleanup() {
        let _guard = test_env::lock(ENV);
        let directory = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(directory.path());
        let cwd = fs::canonicalize(directory.path()).unwrap();
        let binary = directory.path().join("supervisor");
        let process = directory.path().join("process");
        test_env::write_executable(
            &binary,
            "IFS= read -r request\n(sleep 5) &\nprovider=$!\ngroup=$(ps -o pgid= -p \"$provider\" | tr -d ' ')\nprintf '%s:%s\\n' \"$$\" \"$provider\" > \"$MAGENTS_TEST_PROCESS\"\nprintf '{\"control\":\"provider\",\"version\":1,\"supervisor\":%s,\"provider\":%s,\"group\":%s}\\n' \"$$\" \"$provider\" \"$group\"\nsleep 5",
        );
        unsafe {
            std::env::set_var("MAGENTS_SUPERVISOR_BIN", &binary);
            std::env::set_var("MAGENTS_HANDSHAKE_TIMEOUT_MS", "300");
            std::env::set_var("MAGENTS_TEST_PROCESS", &process);
        }
        let started = Instant::now();

        let error = request_supervisor(&homes, Agent::Codex, "private prompt", &cwd, None)
            .unwrap_err()
            .to_string();

        assert!(error.contains("timed out"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(2));
        let process = fs::read_to_string(process).unwrap();
        let (supervisor, provider) = process.trim().split_once(':').unwrap();
        assert!(!pid_alive(supervisor.parse().unwrap()));
        assert!(!pid_alive(provider.parse().unwrap()));
    }

    #[test]
    fn rejects_invalid_resume_and_supervision_metadata_before_launch() {
        let directory = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(directory.path());
        let cwd = fs::canonicalize(directory.path()).unwrap();
        let mut session =
            resumed_session(Agent::Codex, "valid-session", &cwd, Transport::CodexExec);

        assert!(
            resume(&homes, &session, " ")
                .unwrap_err()
                .to_string()
                .contains("must not be empty")
        );
        session.session_id = "bad id".into();
        assert!(
            resume(&homes, &session, "continue")
                .unwrap_err()
                .to_string()
                .contains("invalid id")
        );
        session.session_id = "valid-session".into();
        session.cwd = None;
        assert!(
            resume(&homes, &session, "continue")
                .unwrap_err()
                .to_string()
                .contains("no working directory")
        );
        let file = directory.path().join("file");
        fs::write(&file, "not a directory").unwrap();
        session.cwd = Some(file.to_string_lossy().into_owned());
        assert!(
            resume(&homes, &session, "continue")
                .unwrap_err()
                .to_string()
                .contains("unavailable")
        );

        assert!(
            supervise_request(
                &homes,
                Agent::Codex,
                std::path::Path::new("relative"),
                None,
                "continue",
            )
            .is_err()
        );
        assert!(
            supervise_request(&homes, Agent::Codex, &cwd, Some("bad id"), "continue",).is_err()
        );
    }

    #[test]
    fn cursor_creation_and_registry_failures_are_sanitized() {
        let _guard = test_env::lock(ENV);
        let directory = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(directory.path());
        let cwd = fs::canonicalize(directory.path()).unwrap();
        let binary = directory.path().join("agent");
        unsafe { std::env::set_var("MAGENTS_CURSOR_BIN", &binary) };

        for (body, timeout) in [
            ("exit 7", false),
            ("printf '%s\\n' 'bad id'", false),
            ("printf '%s\\n' valid-id extra-id", false),
            ("sleep 1", true),
        ] {
            test_env::write_executable(&binary, body);
            if timeout {
                unsafe { std::env::set_var("MAGENTS_STARTUP_TIMEOUT_MS", "20") };
            } else {
                unsafe { std::env::remove_var("MAGENTS_STARTUP_TIMEOUT_MS") };
            }
            let error = supervise_request(&homes, Agent::Cursor, &cwd, None, "private prompt")
                .err()
                .expect("cursor creation should fail")
                .to_string();
            assert!(error.contains("cursor chat creation"), "{error}");
            assert!(!error.contains("private prompt"));
        }

        let blocked = Homes::isolated(directory.path().join("blocked"));
        fs::create_dir_all(blocked.magents.parent().unwrap()).unwrap();
        fs::write(&blocked.magents, "not a directory").unwrap();
        test_env::write_executable(
            &binary,
            "cat >/dev/null\nprintf '%s\\n' '{\"type\":\"thread.started\",\"thread_id\":\"valid-id\"}'\nsleep 1",
        );
        unsafe {
            std::env::remove_var("MAGENTS_STARTUP_TIMEOUT_MS");
            std::env::set_var("MAGENTS_CODEX_BIN", &binary);
        }
        assert!(supervise_request(&blocked, Agent::Codex, &cwd, None, "private prompt").is_err());
    }

    #[test]
    fn supervisor_protocol_rejects_missing_invalid_and_mismatched_replies() {
        let _guard = test_env::lock(ENV);
        let directory = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(directory.path());
        let cwd = fs::canonicalize(directory.path()).unwrap();
        let binary = directory.path().join("supervisor");
        test_env::write_executable(&binary, SUPERVISOR_SCRIPT);
        unsafe { std::env::set_var("MAGENTS_SUPERVISOR_BIN", &binary) };

        let cases = [
            ("close".to_string(), String::new(), "no control", None),
            ("truncated".to_string(), String::new(), "no control", None),
            (
                "not-json\n".to_string(),
                String::new(),
                "invalid control message",
                None,
            ),
            (
                String::new(),
                serde_json::to_string(&Reply {
                    accepted: false,
                    status: None,
                    session: None,
                    error: Some("safe failure".into()),
                })
                .unwrap()
                    + "\n",
                "reply without control",
                None,
            ),
            (
                serde_json::to_string(&SupervisorControl::Error {
                    version: PROTOCOL_VERSION,
                })
                .unwrap()
                    + "\n",
                serde_json::to_string(&Reply {
                    accepted: false,
                    status: None,
                    session: None,
                    error: Some("safe failure".into()),
                })
                .unwrap()
                    + "\n",
                "codex startup failed",
                None,
            ),
            (
                serde_json::to_string(&SupervisorControl::Error {
                    version: PROTOCOL_VERSION,
                })
                .unwrap()
                    + "\n",
                serde_json::to_string(&Reply {
                    accepted: true,
                    status: Some(Status::Starting),
                    session: Some(resumed_session(
                        Agent::Codex,
                        "actual-id",
                        &cwd,
                        Transport::CodexExec,
                    )),
                    error: None,
                })
                .unwrap()
                    + "\n",
                "without provider ownership",
                None,
            ),
            (
                "auto".to_string(),
                serde_json::to_string(&Reply {
                    accepted: true,
                    status: Some(Status::Starting),
                    session: None,
                    error: None,
                })
                .unwrap()
                    + "\n",
                "omitted startup metadata",
                None,
            ),
            (
                "auto".to_string(),
                serde_json::to_string(&Reply {
                    accepted: true,
                    status: None,
                    session: Some(resumed_session(
                        Agent::Codex,
                        "actual-id",
                        &cwd,
                        Transport::CodexExec,
                    )),
                    error: None,
                })
                .unwrap()
                    + "\n",
                "mismatched startup metadata",
                None,
            ),
            (
                "auto".to_string(),
                serde_json::to_string(&Reply {
                    accepted: true,
                    status: Some(Status::Starting),
                    session: Some(resumed_session(
                        Agent::Codex,
                        "actual-id",
                        &cwd,
                        Transport::CodexExec,
                    )),
                    error: None,
                })
                .unwrap()
                    + "\n",
                "mismatched startup metadata",
                Some("expected-id"),
            ),
            (
                "auto".to_string(),
                serde_json::to_string(&Reply {
                    accepted: true,
                    status: Some(Status::Starting),
                    session: Some(resumed_session(
                        Agent::Codex,
                        "-p",
                        &cwd,
                        Transport::CodexExec,
                    )),
                    error: None,
                })
                .unwrap()
                    + "\n",
                "mismatched startup metadata",
                None,
            ),
        ];

        for (control, reply, expected, session_id) in cases {
            unsafe {
                std::env::set_var("MAGENTS_TEST_CONTROL", control);
                std::env::set_var("MAGENTS_TEST_REPLY", reply);
            }
            let error =
                request_supervisor(&homes, Agent::Codex, "private prompt", &cwd, session_id)
                    .unwrap_err()
                    .to_string();
            assert!(error.contains(expected), "{error}");
            assert!(!error.contains("private prompt"));
        }
    }

    #[test]
    #[cfg(unix)]
    fn supervisor_starts_in_an_independent_session_and_group() {
        let _guard = test_env::lock(ENV);
        let directory = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(directory.path());
        let cwd = fs::canonicalize(directory.path()).unwrap();
        let binary = directory.path().join("supervisor");
        let process = directory.path().join("process");
        test_env::write_executable(&binary, SUPERVISOR_SCRIPT);
        let reply = serde_json::to_string(&Reply {
            accepted: true,
            status: Some(Status::Starting),
            session: Some(resumed_session(
                Agent::Codex,
                "codex-valid",
                &cwd,
                Transport::CodexExec,
            )),
            error: None,
        })
        .unwrap()
            + "\n";
        unsafe {
            std::env::set_var("MAGENTS_SUPERVISOR_BIN", &binary);
            std::env::set_var("MAGENTS_TEST_PROCESS", &process);
            std::env::set_var("MAGENTS_TEST_CONTROL", "auto");
            std::env::set_var("MAGENTS_TEST_REPLY", reply);
        }

        request_supervisor(&homes, Agent::Codex, "private prompt", &cwd, None).unwrap();

        let process = fs::read_to_string(process).unwrap();
        let (supervisor, provider) = process.trim().split_once(':').unwrap();
        let supervisor = supervisor.parse::<i32>().unwrap();
        let provider = provider.parse::<i32>().unwrap();
        assert_eq!(unsafe { libc::getsid(supervisor) }, supervisor);
        assert_eq!(unsafe { libc::getpgid(supervisor) }, supervisor);
        assert_eq!(unsafe { libc::getsid(provider) }, supervisor);
        assert_eq!(unsafe { libc::getpgid(provider) }, supervisor);
        unsafe {
            libc::kill(-supervisor, libc::SIGTERM);
        }
    }

    #[test]
    fn invalid_accepted_reply_terminates_and_reaps_supervisor() {
        let _guard = test_env::lock(ENV);
        let directory = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(directory.path());
        let cwd = fs::canonicalize(directory.path()).unwrap();
        let binary = directory.path().join("supervisor");
        let process = directory.path().join("process");
        test_env::write_executable(&binary, SUPERVISOR_SCRIPT);
        let reply = serde_json::to_string(&Reply {
            accepted: true,
            status: Some(Status::Starting),
            session: None,
            error: None,
        })
        .unwrap()
            + "\n";
        unsafe {
            std::env::set_var("MAGENTS_SUPERVISOR_BIN", &binary);
            std::env::set_var("MAGENTS_TEST_PROCESS", &process);
            std::env::set_var("MAGENTS_TEST_CONTROL", "auto");
            std::env::set_var("MAGENTS_TEST_REPLY", reply);
        }

        let error = request_supervisor(&homes, Agent::Codex, "private prompt", &cwd, None)
            .unwrap_err()
            .to_string();

        assert!(error.contains("omitted startup metadata"));
        let process = fs::read_to_string(process).unwrap();
        let (supervisor, provider) = process.trim().split_once(':').unwrap();
        assert!(!pid_alive(supervisor.parse().unwrap()));
        assert!(!pid_alive(provider.parse().unwrap()));
    }

    #[test]
    fn resume_markers_are_stable() {
        assert_eq!(resume_marker(Agent::Claude), "claude-cli");
        assert_eq!(resume_marker(Agent::Codex), "codex-exec");
        assert_eq!(resume_marker(Agent::Cursor), "cursor-cli");
        assert_eq!(resume_marker(Agent::Grok), "grok-single");
        assert_eq!(resume_marker(Agent::OpenCode), "opencode-run");
    }
}
