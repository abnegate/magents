use crate::error::{Error, Result};
use crate::homes::Homes;
use crate::homes::pid_alive;
use crate::model::{Agent, Caller, Session, StopReport, valid_session_id};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use uuid::Uuid;

const VERSION: u8 = 1;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Starting,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Report {
    pub accepted: bool,
    pub status: Status,
    pub session: Session,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Transport {
    ClaudePrint,
    CodexExec,
    CursorAgent,
    GrokStream,
    OpenCodeRun,
}

impl Transport {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ClaudePrint => "claude-print",
            Self::CodexExec => "codex-exec",
            Self::CursorAgent => "cursor-agent",
            Self::GrokStream => "grok-stream",
            Self::OpenCodeRun => "opencode-run",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub version: u8,
    pub agent: Agent,
    pub session_id: String,
    pub cwd: String,
    pub created_at: DateTime<Utc>,
    pub transport: Transport,
}

impl Record {
    pub fn session(&self) -> Option<Session> {
        if self.version != VERSION
            || !valid_session_id(self.agent, &self.session_id)
            || self.cwd.is_empty()
        {
            return None;
        }
        Some(Session {
            agent: self.agent,
            session_id: self.session_id.clone(),
            desktop_id: None,
            name: None,
            title: None,
            cwd: Some(self.cwd.clone()),
            branch: None,
            live: false,
            archived: false,
            pid: None,
            model: None,
            last_activity_at: Some(self.created_at),
            transcript_path: None,
            messaging_socket: None,
            origin: Some(self.transport.as_str().to_string()),
            tmux: None,
        })
    }
}

pub fn run(homes: &Homes, agent: Agent, prompt: &str, cwd: Option<&Path>) -> Result<Report> {
    if prompt.trim().is_empty() {
        return Err(Error::msg("prompt must not be empty"));
    }
    let caller = Caller::from_env();
    let cwd = canonical_cwd(homes, cwd, &caller)?;
    let prompt = routed_prompt(&caller, prompt);
    crate::runtime::start(homes, agent, &prompt, &cwd)
}

pub fn records(homes: &Homes) -> Result<Vec<Record>> {
    let directory = homes.spawn_dir();
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(Error::Io {
                path: directory,
                source,
            });
        }
    };
    let mut records = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            continue;
        }
        let Ok(raw) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(record) = serde_json::from_str::<Record>(&raw) else {
            continue;
        };
        if record.session().is_some() {
            records.push(record);
        }
    }
    records.sort_by_key(|record| record.created_at);
    Ok(records)
}

pub fn sessions(homes: &Homes) -> Result<Vec<Session>> {
    Ok(records(homes)?.iter().filter_map(Record::session).collect())
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Live {
    pub agent: Agent,
    pub session_id: String,
    pub supervisor: u32,
    pub provider: u32,
    pub group: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supervisor_started: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_started: Option<String>,
}

pub fn write_live(
    homes: &Homes,
    session: &Session,
    supervisor: u32,
    provider: u32,
    group: u32,
) -> Result<PathBuf> {
    let directory = homes.live_dir();
    fs::create_dir_all(&directory).map_err(|source| Error::Io {
        path: directory.clone(),
        source,
    })?;
    #[cfg(unix)]
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).map_err(|source| {
        Error::Io {
            path: directory.clone(),
            source,
        }
    })?;
    let path = live_path(homes, session.agent, &session.session_id);
    let live = Live {
        agent: session.agent,
        session_id: session.session_id.clone(),
        supervisor,
        provider,
        group,
        supervisor_started: process_started(supervisor),
        provider_started: process_started(provider),
    };
    fs::write(&path, serde_json::to_vec(&live)?).map_err(|source| Error::Io {
        path: path.clone(),
        source,
    })?;
    #[cfg(unix)]
    {
        let mut permissions = fs::metadata(&path)
            .map_err(|source| Error::Io {
                path: path.clone(),
                source,
            })?
            .permissions();
        permissions.set_mode(0o600);
        fs::set_permissions(&path, permissions).map_err(|source| Error::Io {
            path: path.clone(),
            source,
        })?;
    }
    Ok(path)
}

pub fn stop(homes: &Homes, reference: &str) -> Result<StopReport> {
    let session = crate::discover::resolve(homes, reference)?;
    let path = live_path(homes, session.agent, &session.session_id);
    if !path.is_file() {
        return Err(Error::msg("no magents supervisor for that session"));
    }
    let raw = fs::read_to_string(&path).map_err(|source| Error::Io {
        path: path.clone(),
        source,
    })?;
    let live: Live = serde_json::from_str(&raw)?;
    let mut signaled = Vec::new();
    let supervisor_ours = claimable(live.supervisor, live.supervisor_started.as_deref());
    let provider_ours = live.provider != live.supervisor
        && claimable(live.provider, live.provider_started.as_deref());
    if !supervisor_ours && !provider_ours {
        let _ = fs::remove_file(&path);
        return Ok(StopReport {
            stopped: true,
            already_exited: true,
            session,
            signaled,
        });
    }
    if supervisor_ours {
        signal_pid(live.supervisor);
        signaled.push("supervisor".into());
    }
    if provider_ours {
        signal_pid(live.provider);
        signaled.push("provider".into());
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(400);
    while std::time::Instant::now() < deadline
        && (claimable(live.supervisor, live.supervisor_started.as_deref())
            || (live.provider != live.supervisor
                && claimable(live.provider, live.provider_started.as_deref())))
    {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    if claimable(live.supervisor, live.supervisor_started.as_deref()) {
        force_pid(live.supervisor);
    }
    if live.provider != live.supervisor
        && claimable(live.provider, live.provider_started.as_deref())
    {
        force_pid(live.provider);
    }
    let _ = fs::remove_file(&path);
    Ok(StopReport {
        stopped: true,
        already_exited: false,
        session,
        signaled,
    })
}

fn live_path(homes: &Homes, agent: Agent, session_id: &str) -> PathBuf {
    homes
        .live_dir()
        .join(format!("{}-{session_id}.json", agent.as_str()))
}

fn process_started(pid: u32) -> Option<String> {
    if pid == 0 {
        return None;
    }
    let output = std::process::Command::new("ps")
        .env("LC_ALL", "C")
        .args(["-o", "lstart=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let started = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if started.is_empty() {
        None
    } else {
        Some(started)
    }
}

fn claimable(pid: u32, started: Option<&str>) -> bool {
    if !pid_alive(pid) {
        return false;
    }
    match started {
        Some(started) => process_started(pid).as_deref() == Some(started),
        None => false,
    }
}

fn signal_pid(pid: u32) {
    #[cfg(unix)]
    unsafe {
        libc::kill(pid as i32, libc::SIGTERM);
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
    }
}

fn force_pid(pid: u32) {
    #[cfg(unix)]
    unsafe {
        libc::kill(pid as i32, libc::SIGKILL);
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
    }
}

pub(crate) fn record(
    homes: &Homes,
    agent: Agent,
    session_id: &str,
    cwd: &Path,
    transport: Transport,
) -> Result<Session> {
    if !valid_session_id(agent, session_id) {
        return Err(Error::msg("agent returned an invalid session id"));
    }
    let cwd = cwd
        .to_str()
        .ok_or_else(|| Error::msg("working directory is not valid UTF-8"))?;
    let record = Record {
        version: VERSION,
        agent,
        session_id: session_id.to_string(),
        cwd: cwd.to_string(),
        created_at: Utc::now(),
        transport,
    };
    write_record(homes, &record)?;
    record
        .session()
        .ok_or_else(|| Error::msg("agent returned invalid startup metadata"))
}

fn canonical_cwd(homes: &Homes, cwd: Option<&Path>, caller: &Caller) -> Result<PathBuf> {
    if let Some(cwd) = cwd {
        return canonical_directory(cwd);
    }

    if let (Some(agent), Some(session_id)) = (caller.agent, caller.session_id.as_deref())
        && valid_session_id(agent, session_id)
    {
        let reference = format!("{agent}:{session_id}");
        if let Ok(session) = crate::discover::resolve(homes, &reference)
            && session.agent == agent
            && session.session_id == session_id
            && let Some(cwd) = session.cwd
            && let Ok(cwd) = canonical_directory(Path::new(&cwd))
        {
            return Ok(cwd);
        }
    }

    let cwd = std::env::current_dir().map_err(|source| Error::Io {
        path: PathBuf::from("."),
        source,
    })?;
    canonical_directory(&cwd)
}

fn canonical_directory(cwd: &Path) -> Result<PathBuf> {
    let cwd = cwd.to_path_buf();
    let canonical = fs::canonicalize(&cwd).map_err(|source| Error::Io { path: cwd, source })?;
    if !canonical.is_dir() {
        return Err(Error::msg("working directory is not a directory"));
    }
    Ok(canonical)
}

fn routed_prompt(caller: &Caller, prompt: &str) -> String {
    let Some(agent) = caller.agent else {
        return prompt.to_string();
    };
    let Some(session_id) = caller
        .session_id
        .as_deref()
        .filter(|id| valid_session_id(agent, id))
    else {
        return prompt.to_string();
    };
    format!(
        "<magents-reply-to agent=\"{agent}\" session=\"{session_id}\">\n\
Reply to the requesting chat with magents send_message to {agent}:{session_id}.\n\
</magents-reply-to>\n\n{prompt}"
    )
}

fn write_record(homes: &Homes, record: &Record) -> Result<()> {
    let directory = homes.spawn_dir();
    fs::create_dir_all(&directory).map_err(|source| Error::Io {
        path: directory.clone(),
        source,
    })?;
    #[cfg(unix)]
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).map_err(|source| {
        Error::Io {
            path: directory.clone(),
            source,
        }
    })?;

    let name = Uuid::new_v4().to_string();
    let temporary = directory.join(format!("{name}.tmp"));
    let destination = directory.join(format!("{name}.json"));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(&temporary).map_err(|source| Error::Io {
        path: temporary.clone(),
        source,
    })?;
    let raw = serde_json::to_vec(record)?;
    if let Err(source) = file
        .write_all(&raw)
        .and_then(|_| file.flush())
        .and_then(|_| file.sync_all())
    {
        let _ = fs::remove_file(&temporary);
        return Err(Error::Io {
            path: temporary,
            source,
        });
    }
    drop(file);
    if let Err(source) = fs::rename(&temporary, &destination) {
        let _ = fs::remove_file(&temporary);
        return Err(Error::Io {
            path: destination,
            source,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        Record, Status, Transport, canonical_cwd, record, records, routed_prompt, run, sessions,
    };
    use crate::homes::Homes;
    use crate::model::{Agent, Caller};
    use chrono::Utc;
    use serde_json::Value;
    use std::fs;

    #[test]
    fn canonicalizes_explicit_working_directory() {
        let directory = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(directory.path());
        let caller = Caller {
            agent: None,
            session_id: None,
        };
        let nested = directory.path().join("a").join("..");
        fs::create_dir_all(directory.path().join("a")).unwrap();
        assert_eq!(
            canonical_cwd(&homes, Some(&nested), &caller).unwrap(),
            fs::canonicalize(directory.path()).unwrap()
        );
        let file = directory.path().join("file");
        fs::write(&file, "x").unwrap();
        assert!(
            canonical_cwd(&homes, Some(&file), &caller)
                .unwrap_err()
                .to_string()
                .contains("directory")
        );
        assert_eq!(
            canonical_cwd(&homes, None, &caller).unwrap(),
            std::env::current_dir().unwrap()
        );
    }

    #[test]
    fn explicit_working_directory_wins_over_known_caller() {
        let directory = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(directory.path());
        let caller_cwd = directory.path().join("caller");
        let explicit = directory.path().join("explicit");
        fs::create_dir_all(&caller_cwd).unwrap();
        fs::create_dir_all(&explicit).unwrap();
        record(
            &homes,
            Agent::Codex,
            "known-caller",
            &caller_cwd,
            Transport::CodexExec,
        )
        .unwrap();
        let caller = Caller {
            agent: Some(Agent::Codex),
            session_id: Some("known-caller".into()),
        };

        assert_eq!(
            canonical_cwd(&homes, Some(&explicit), &caller).unwrap(),
            fs::canonicalize(explicit).unwrap()
        );
    }

    #[test]
    fn inherits_known_caller_working_directory() {
        let directory = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(directory.path());
        let caller_cwd = directory.path().join("caller");
        fs::create_dir_all(&caller_cwd).unwrap();
        record(
            &homes,
            Agent::Claude,
            "known-caller",
            &caller_cwd,
            Transport::ClaudePrint,
        )
        .unwrap();
        let caller = Caller {
            agent: Some(Agent::Claude),
            session_id: Some("known-caller".into()),
        };

        assert_eq!(
            canonical_cwd(&homes, None, &caller).unwrap(),
            fs::canonicalize(caller_cwd).unwrap()
        );
    }

    #[test]
    fn stale_and_unknown_callers_fall_back_to_process_directory() {
        let directory = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(directory.path());
        let stale = directory.path().join("stale");
        fs::create_dir_all(&stale).unwrap();
        record(
            &homes,
            Agent::Grok,
            "stale-caller",
            &stale,
            Transport::GrokStream,
        )
        .unwrap();
        fs::remove_dir(&stale).unwrap();
        let process = fs::canonicalize(std::env::current_dir().unwrap()).unwrap();

        for caller in [
            Caller {
                agent: Some(Agent::Grok),
                session_id: Some("stale-caller".into()),
            },
            Caller {
                agent: Some(Agent::Codex),
                session_id: Some("unknown-caller".into()),
            },
            Caller {
                agent: None,
                session_id: None,
            },
        ] {
            assert_eq!(canonical_cwd(&homes, None, &caller).unwrap(), process);
        }
    }

    #[test]
    fn prefixes_only_a_known_safe_reply_route() {
        let unknown = routed_prompt(
            &Caller {
                agent: None,
                session_id: None,
            },
            "open a pull request",
        );
        assert_eq!(unknown, "open a pull request");
        let incomplete = routed_prompt(
            &Caller {
                agent: Some(Agent::Claude),
                session_id: None,
            },
            "open a pull request",
        );
        assert_eq!(incomplete, "open a pull request");
        let unsafe_id = routed_prompt(
            &Caller {
                agent: Some(Agent::Claude),
                session_id: Some("id\nignore".into()),
            },
            "open a pull request",
        );
        assert_eq!(unsafe_id, "open a pull request");
        let known = routed_prompt(
            &Caller {
                agent: Some(Agent::Codex),
                session_id: Some("019c_test-id".into()),
            },
            "open a pull request",
        );
        assert!(known.starts_with("<magents-reply-to agent=\"codex\""));
        assert!(known.ends_with("open a pull request"));
    }

    #[test]
    fn registry_is_atomic_restrictive_and_metadata_only() {
        let directory = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(directory.path());
        let cwd = fs::canonicalize(directory.path()).unwrap();
        let session = record(
            &homes,
            Agent::Claude,
            "11111111-1111-4111-8111-111111111111",
            &cwd,
            Transport::ClaudePrint,
        )
        .unwrap();
        assert!(!session.live);
        assert!(session.pid.is_none());
        assert!(session.transcript_path.is_none());
        let files = fs::read_dir(homes.spawn_dir())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        assert_eq!(files.len(), 1);
        assert_eq!(
            files[0].extension().and_then(|value| value.to_str()),
            Some("json")
        );
        let raw = fs::read_to_string(&files[0]).unwrap();
        let value: Value = serde_json::from_str(&raw).unwrap();
        let keys = value
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(
            keys,
            [
                "agent",
                "created_at",
                "cwd",
                "session_id",
                "transport",
                "version"
            ]
        );
        for forbidden in ["prompt", "output", "token", "pid", "socket", "transcript"] {
            assert!(!raw.contains(forbidden), "{raw}");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&files[0]).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(homes.spawn_dir())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
    }

    #[test]
    fn readers_skip_corrupt_unknown_and_extra_fields() {
        let directory = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(directory.path());
        fs::create_dir_all(homes.spawn_dir()).unwrap();
        fs::write(homes.spawn_dir().join("ignored.txt"), "not a record").unwrap();
        fs::create_dir(homes.spawn_dir().join("unreadable.json")).unwrap();
        fs::write(homes.spawn_dir().join("bad.json"), "not json").unwrap();
        fs::write(
            homes.spawn_dir().join("future.json"),
            serde_json::json!({
                "version": 2,
                "agent": "codex",
                "session_id": "future",
                "cwd": "/tmp",
                "created_at": Utc::now(),
                "transport": "codex-exec"
            })
            .to_string(),
        )
        .unwrap();
        fs::write(
            homes.spawn_dir().join("extra.json"),
            serde_json::json!({
                "version": 1,
                "agent": "codex",
                "session_id": "extra",
                "cwd": "/tmp",
                "created_at": Utc::now(),
                "transport": "codex-exec",
                "prompt": "must be rejected"
            })
            .to_string(),
        )
        .unwrap();
        assert!(records(&homes).unwrap().is_empty());
        assert!(sessions(&homes).unwrap().is_empty());
    }

    #[test]
    fn registry_and_prompt_failures_are_explicit() {
        let directory = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(directory.path());
        assert!(
            run(&homes, Agent::Codex, " ", None)
                .unwrap_err()
                .to_string()
                .contains("must not be empty")
        );
        assert!(
            record(
                &homes,
                Agent::Codex,
                "bad id",
                directory.path(),
                Transport::CodexExec,
            )
            .unwrap_err()
            .to_string()
            .contains("invalid session id")
        );
        for session_id in ["-p", ".hidden", "../outside"] {
            assert!(
                record(
                    &homes,
                    Agent::Codex,
                    session_id,
                    directory.path(),
                    Transport::CodexExec,
                )
                .is_err()
            );
        }

        fs::create_dir_all(&homes.magents).unwrap();
        fs::write(homes.spawn_dir(), "not a directory").unwrap();
        assert!(records(&homes).is_err());

        let blocked = Homes::isolated(directory.path().join("blocked"));
        fs::create_dir_all(blocked.magents.parent().unwrap()).unwrap();
        fs::write(&blocked.magents, "not a directory").unwrap();
        assert!(
            record(
                &blocked,
                Agent::Codex,
                "valid-id",
                directory.path(),
                Transport::CodexExec,
            )
            .is_err()
        );
    }

    #[test]
    fn record_materializes_every_transport() {
        for (agent, transport) in [
            (Agent::Claude, Transport::ClaudePrint),
            (Agent::Codex, Transport::CodexExec),
            (Agent::Cursor, Transport::CursorAgent),
            (Agent::Grok, Transport::GrokStream),
            (Agent::OpenCode, Transport::OpenCodeRun),
        ] {
            let session_id = if agent == Agent::OpenCode {
                "ses_valid-1"
            } else {
                "valid.id-1"
            };
            let record = Record {
                version: 1,
                agent,
                session_id: session_id.into(),
                cwd: "/tmp".into(),
                created_at: Utc::now(),
                transport,
            };
            let session = record.session().unwrap();
            assert_eq!(session.agent, agent);
            assert_eq!(session.origin.as_deref(), Some(transport.as_str()));
            assert!(!session.live);
        }
        assert_eq!(
            serde_json::to_string(&Status::Starting).unwrap(),
            "\"starting\""
        );
    }

    #[test]
    fn stop_supervised_process_and_stale_live() {
        use super::{stop, write_live};
        use crate::homes::pid_alive;
        use std::process::{Command, Stdio};

        let directory = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(directory.path());
        let session = record(
            &homes,
            Agent::Codex,
            "stop-me",
            directory.path(),
            Transport::CodexExec,
        )
        .unwrap();
        let err = stop(&homes, "codex:stop-me").unwrap_err();
        assert!(err.to_string().contains("no magents supervisor"));

        write_live(&homes, &session, 0, 0, 0).unwrap();
        let stale = stop(&homes, "codex:stop-me").unwrap();
        assert!(stale.already_exited);
        assert!(stale.signaled.is_empty());

        let mut child = Command::new("sleep")
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();
        write_live(&homes, &session, pid, pid, pid).unwrap();
        let stopped = stop(&homes, "codex:stop-me").unwrap();
        assert!(stopped.stopped);
        assert!(!stopped.already_exited);
        assert!(stopped.signaled.contains(&"supervisor".into()));
        let _ = child.wait();
        assert!(!pid_alive(pid));
    }

    #[test]
    fn write_live_and_stop_cover_error_and_force_paths() {
        use super::{live_path, stop, write_live};
        use std::os::unix::fs::PermissionsExt;
        use std::process::{Command, Stdio};

        let directory = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(directory.path());
        let session = record(
            &homes,
            Agent::Codex,
            "live-io",
            directory.path(),
            Transport::CodexExec,
        )
        .unwrap();

        let blocked = Homes::isolated(directory.path().join("blocked"));
        std::fs::create_dir_all(&blocked.magents).unwrap();
        std::fs::write(blocked.spawn_dir(), "not-a-dir").unwrap();
        assert!(write_live(&blocked, &session, 1, 2, 3).is_err());

        let path = live_path(&homes, session.agent, &session.session_id);
        std::fs::create_dir_all(&path).unwrap();
        assert!(write_live(&homes, &session, 1, 2, 3).is_err());
        std::fs::remove_dir_all(&path).unwrap();

        write_live(&homes, &session, 0, 0, 0).unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o000);
        std::fs::set_permissions(&path, permissions).unwrap();
        assert!(stop(&homes, "codex:live-io").is_err());
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o644);
        std::fs::set_permissions(&path, permissions).unwrap();
        std::fs::remove_file(&path).unwrap();

        let mut orphan = Command::new("/bin/sh")
            .args(["-c", "trap '' TERM; exec /bin/sleep 30"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        write_live(&homes, &session, 0, orphan.id(), 0).unwrap();
        let stopped = stop(&homes, "codex:live-io").unwrap();
        assert!(stopped.signaled.contains(&"provider".into()));
        let _ = orphan.wait();

        let mut supervisor = Command::new("/bin/sh")
            .args(["-c", "trap '' TERM; exec /bin/sleep 30"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut provider = Command::new("/bin/sh")
            .args(["-c", "trap '' TERM; exec /bin/sleep 30"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        write_live(
            &homes,
            &session,
            supervisor.id(),
            provider.id(),
            supervisor.id(),
        )
        .unwrap();
        let stopped = stop(&homes, "codex:live-io").unwrap();
        assert!(stopped.signaled.contains(&"supervisor".into()));
        assert!(stopped.signaled.contains(&"provider".into()));
        let _ = supervisor.wait();
        let _ = provider.wait();

        let mut reused = Command::new("/bin/sh")
            .args(["-c", "exec /bin/sleep 30"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        write_live(&homes, &session, reused.id(), reused.id(), reused.id()).unwrap();
        let path = live_path(&homes, session.agent, &session.session_id);
        let mut live: super::Live =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        live.supervisor_started = Some("Thu Jan  1 00:00:00 1970".into());
        live.provider_started = Some("Thu Jan  1 00:00:00 1970".into());
        std::fs::write(&path, serde_json::to_vec(&live).unwrap()).unwrap();
        let stale = stop(&homes, "codex:live-io").unwrap();
        assert!(stale.already_exited);
        assert!(stale.signaled.is_empty());
        assert!(crate::homes::pid_alive(reused.id()));
        let _ = reused.kill();
        let _ = reused.wait();
        assert!(!super::claimable(std::process::id(), None));
        assert!(super::process_started(0).is_none());
        assert!(super::process_started(u32::MAX).is_none());

        let registry = Homes::isolated(directory.path().join("registry-io"));
        std::fs::create_dir_all(&registry.magents).unwrap();
        std::fs::write(registry.spawn_dir(), "not-a-dir").unwrap();
        assert!(
            record(
                &registry,
                Agent::Codex,
                "blocked-record",
                directory.path(),
                Transport::CodexExec,
            )
            .is_err()
        );

        let restricted = Homes::isolated(directory.path().join("restricted"));
        std::fs::create_dir_all(restricted.spawn_dir()).unwrap();
        std::os::unix::fs::symlink("/usr/bin", restricted.live_dir()).unwrap();
        assert!(write_live(&restricted, &session, 1, 2, 3).is_err());

        let protected = Homes::isolated(directory.path().join("protected"));
        std::fs::create_dir_all(&protected.magents).unwrap();
        std::os::unix::fs::symlink("/usr/bin", protected.spawn_dir()).unwrap();
        assert!(
            record(
                &protected,
                Agent::Codex,
                "protected-record",
                directory.path(),
                Transport::CodexExec,
            )
            .is_err()
        );
    }
}
