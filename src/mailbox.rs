use crate::deliver;
use crate::discover::{identify, resolve};
use crate::error::{Error, Result};
use crate::homes::Homes;
use crate::model::{AckReport, Agent, AwaitReport, Caller, InboxReport, Mail, SendReport};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};
use uuid::Uuid;

#[derive(Clone, Debug, Default)]
pub struct InboxQuery {
    pub session_id: Option<String>,
    pub agent: Option<Agent>,
    pub since: Option<String>,
    pub unread_only: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct AckCursor {
    mail_id: String,
    ts: DateTime<Utc>,
}

pub fn post(homes: &Homes, mail: &Mail) -> Result<PathBuf> {
    let path = mailbox_path(homes, mail.to_agent, &mail.to_session);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| Error::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|source| Error::Io {
            path: path.clone(),
            source,
        })?;
    writeln!(file, "{}", serde_json::to_string(mail)?).map_err(|source| Error::Io {
        path: path.clone(),
        source,
    })?;
    Ok(path)
}

pub fn inbox(homes: &Homes, caller: &Caller, query: InboxQuery) -> Result<InboxReport> {
    let (agent, session_id) = target(homes, caller, query.session_id.as_deref(), query.agent)?;
    let items = read_mail(homes, agent, &session_id)?;
    let acked_through = load_ack(homes, agent, &session_id)?.map(|cursor| cursor.mail_id);
    let unread_start = acked_through
        .as_deref()
        .and_then(|acked| items.iter().position(|mail| mail.id == acked))
        .map(|index| index + 1)
        .unwrap_or(0);
    let unread = items.len().saturating_sub(unread_start);
    let mut items = apply_since(items, query.since.as_deref())?;
    if query.unread_only
        && let Some(acked) = acked_through.as_deref()
        && let Some(index) = items.iter().position(|mail| mail.id == acked)
    {
        items = items.split_off(index + 1);
    }
    Ok(InboxReport {
        items,
        unread,
        acked_through,
    })
}

pub fn ack(
    homes: &Homes,
    caller: &Caller,
    through: Option<&str>,
    session_id: Option<&str>,
    agent: Option<Agent>,
) -> Result<AckReport> {
    let (agent, session_id) = target(homes, caller, session_id, agent)?;
    let items = read_mail(homes, agent, &session_id)?;
    let mail = match through {
        Some(id) => items
            .iter()
            .find(|mail| mail.id == id)
            .ok_or_else(|| Error::msg("mail_id not found"))?,
        None => items.last().ok_or_else(|| Error::msg("inbox is empty"))?,
    };
    let cursor = AckCursor {
        mail_id: mail.id.clone(),
        ts: mail.ts,
    };
    save_ack(homes, agent, &session_id, &cursor)?;
    let unread_start = items
        .iter()
        .position(|item| item.id == cursor.mail_id)
        .map(|index| index + 1)
        .unwrap_or(items.len());
    Ok(AckReport {
        acked_through: cursor.mail_id,
        unread: items.len().saturating_sub(unread_start),
    })
}

pub fn await_reply(
    homes: &Homes,
    caller: &Caller,
    from: Option<&str>,
    timeout_secs: Option<u32>,
    session_id: Option<&str>,
    agent: Option<Agent>,
) -> Result<AwaitReport> {
    let timeout = timeout_secs.unwrap_or(5).min(30);
    let poll = poll_interval();
    let from_session = match from {
        Some(reference) => Some(resolve(homes, reference)?),
        None => None,
    };
    let baseline = inbox(
        homes,
        caller,
        InboxQuery {
            session_id: session_id.map(ToOwned::to_owned),
            agent,
            since: None,
            unread_only: false,
        },
    )?;
    let last_id = baseline.items.last().map(|mail| mail.id.clone());
    let started = Instant::now();
    loop {
        let report = inbox(
            homes,
            caller,
            InboxQuery {
                session_id: session_id.map(ToOwned::to_owned),
                agent,
                since: last_id.clone(),
                unread_only: false,
            },
        )?;
        let items: Vec<Mail> = report
            .items
            .into_iter()
            .filter(|mail| match &from_session {
                Some(from) => {
                    mail.from_agent == Some(from.agent)
                        && mail.from_session.as_deref() == Some(from.session_id.as_str())
                }
                None => true,
            })
            .collect();
        if !items.is_empty() {
            return Ok(AwaitReport {
                status: "received".into(),
                waited_ms: started.elapsed().as_millis() as u64,
                items,
            });
        }
        if started.elapsed() >= Duration::from_secs(timeout.into()) {
            return Ok(AwaitReport {
                status: "pending".into(),
                waited_ms: started.elapsed().as_millis() as u64,
                items: Vec::new(),
            });
        }
        thread::sleep(poll);
    }
}

pub fn send(homes: &Homes, caller: &Caller, to: &str, message: &str) -> Result<SendReport> {
    if message.trim().is_empty() {
        return Err(Error::msg("message must not be empty"));
    }
    let session = resolve(homes, to)?;
    let delivered = deliver::deliver_live(homes, &session, message)?;
    let mail = compose(
        caller,
        session.agent,
        session.session_id.clone(),
        message.to_string(),
        delivered.clone(),
    );
    post(homes, &mail)?;
    Ok(SendReport {
        queued: true,
        to: session,
        delivered,
        mail_id: mail.id,
    })
}

pub fn reply(
    homes: &Homes,
    caller: &Caller,
    message: &str,
    mail_id: Option<&str>,
    session_id: Option<&str>,
    agent: Option<Agent>,
) -> Result<SendReport> {
    let report = inbox(
        homes,
        caller,
        InboxQuery {
            session_id: session_id.map(ToOwned::to_owned),
            agent,
            since: None,
            unread_only: false,
        },
    )?;
    let mail = match mail_id {
        Some(id) => report
            .items
            .into_iter()
            .find(|mail| mail.id == id)
            .ok_or_else(|| Error::msg("mail_id not found"))?,
        None => report
            .items
            .last()
            .cloned()
            .ok_or_else(|| Error::msg("inbox is empty"))?,
    };
    let from_agent = mail
        .from_agent
        .ok_or_else(|| Error::msg("mail has no sender agent"))?;
    let from_session = mail
        .from_session
        .as_deref()
        .ok_or_else(|| Error::msg("mail has no sender session"))?;
    send(
        homes,
        caller,
        &format!("{from_agent}:{from_session}"),
        message,
    )
}

pub fn compose(
    caller: &Caller,
    to_agent: Agent,
    to_session: String,
    message: String,
    delivered: Vec<String>,
) -> Mail {
    Mail {
        id: Uuid::new_v4().to_string(),
        ts: Utc::now(),
        from_agent: caller.agent,
        from_session: caller.session_id.clone(),
        from_name: None,
        to_agent,
        to_session,
        message,
        delivered,
    }
}

fn target(
    homes: &Homes,
    caller: &Caller,
    session_id: Option<&str>,
    agent: Option<Agent>,
) -> Result<(Agent, String)> {
    if let (Some(agent), Some(session_id)) = (agent, session_id) {
        return Ok((agent, session_id.to_string()));
    }
    let identity = identify(homes);
    let agent = agent.or(caller.agent).or(identity.agent).ok_or_else(|| {
        Error::msg("pass session_id, or call from a Claude/Codex/Cursor/Grok/OpenCode MCP session")
    })?;
    let session_id = session_id
        .map(ToOwned::to_owned)
        .or_else(|| caller.session_id.clone())
        .or(identity.session_id)
        .ok_or_else(|| Error::msg("pass session_id"))?;
    Ok((agent, session_id))
}

fn read_mail(homes: &Homes, agent: Agent, session_id: &str) -> Result<Vec<Mail>> {
    let path = mailbox_path(homes, agent, session_id);
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let file = fs::File::open(&path).map_err(|source| Error::Io {
        path: path.clone(),
        source,
    })?;
    let mut mail = Vec::new();
    for line in BufReader::new(file).lines() {
        let line = line.map_err(|source| Error::Io {
            path: path.clone(),
            source,
        })?;
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(item) = serde_json::from_str::<Mail>(&line) {
            mail.push(item);
        }
    }
    Ok(mail)
}

fn apply_since(items: Vec<Mail>, since: Option<&str>) -> Result<Vec<Mail>> {
    let Some(since) = since.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(items);
    };
    if let Ok(ts) = DateTime::parse_from_rfc3339(since) {
        let ts = ts.with_timezone(&Utc);
        return Ok(items.into_iter().filter(|mail| mail.ts > ts).collect());
    }
    match items.iter().position(|mail| mail.id == since) {
        Some(index) => Ok(items[index + 1..].to_vec()),
        None => Err(Error::msg(
            "since must be a mail_id from this inbox or an RFC3339 timestamp",
        )),
    }
}

fn load_ack(homes: &Homes, agent: Agent, session_id: &str) -> Result<Option<AckCursor>> {
    let path = ack_path(homes, agent, session_id);
    if !path.is_file() {
        return Ok(None);
    }
    let raw = fs::read_to_string(&path).map_err(|source| Error::Io {
        path: path.clone(),
        source,
    })?;
    Ok(serde_json::from_str(&raw).ok())
}

fn save_ack(homes: &Homes, agent: Agent, session_id: &str, cursor: &AckCursor) -> Result<()> {
    let path = ack_path(homes, agent, session_id);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| Error::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    fs::write(&path, serde_json::to_vec(cursor)?).map_err(|source| Error::Io { path, source })
}

fn mailbox_path(homes: &Homes, agent: Agent, session_id: &str) -> PathBuf {
    homes
        .mailbox_dir()
        .join(agent.as_str())
        .join(format!("{session_id}.jsonl"))
}

fn ack_path(homes: &Homes, agent: Agent, session_id: &str) -> PathBuf {
    homes
        .mailbox_dir()
        .join("acks")
        .join(agent.as_str())
        .join(format!("{session_id}.json"))
}

fn poll_interval() -> Duration {
    let millis = std::env::var("MAGENTS_AWAIT_POLL_MS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(200u64)
        .clamp(1, 1000);
    Duration::from_millis(millis)
}

#[cfg(test)]
mod tests {
    use super::{InboxQuery, ack, await_reply, compose, inbox, post, reply, send};
    use crate::homes::Homes;
    use crate::model::{Agent, Caller};
    use crate::test_env;
    use chrono::{Duration as ChronoDuration, Utc};

    const CALLER_ENV: &[&str] = &[
        "GROK_SESSION_ID",
        "CLAUDE_CODE_MESSAGING_SOCKET",
        "CLAUDE_PROJECT_DIR",
        "CLAUDE_SESSION_ID",
        "CURSOR_SESSION_ID",
        "CURSOR_PROJECT_DIR",
        "CURSOR_AGENT",
        "COMPOSER_SESSION_ID",
        "OPENCODE_SESSION_ID",
        "OPENCODE_DIRECTORY",
        "OPENCODE_SERVER",
        "OPENCODE_SESSION",
        "CODEX_HOME",
        "CODEX_THREAD_ID",
        "CODEX_SESSION_ID",
        "MAGENTS_AWAIT_POLL_MS",
    ];

    fn caller(agent: Agent, session: &str) -> Caller {
        Caller {
            agent: Some(agent),
            session_id: Some(session.into()),
        }
    }

    fn posted(homes: &Homes, to: &str, message: &str) -> crate::model::Mail {
        let mail = compose(
            &caller(Agent::Claude, "from"),
            Agent::Grok,
            to.into(),
            message.into(),
            vec!["claude-uds".into()],
        );
        post(homes, &mail).unwrap();
        mail
    }

    #[test]
    fn inbox_requires_identity_and_skips_junk() {
        let _guard = test_env::lock(CALLER_ENV);
        for key in CALLER_ENV {
            unsafe { std::env::remove_var(key) };
        }
        let dir = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(dir.path());
        let empty = inbox(&homes, &caller(Agent::Grok, "s1"), InboxQuery::default()).unwrap();
        assert!(empty.items.is_empty());
        assert_eq!(empty.unread, 0);

        let missing_agent = inbox(
            &homes,
            &Caller {
                agent: None,
                session_id: None,
            },
            InboxQuery::default(),
        )
        .unwrap_err();
        assert!(missing_agent.to_string().contains("pass session_id"));

        let missing_session = inbox(
            &homes,
            &Caller {
                agent: Some(Agent::Grok),
                session_id: None,
            },
            InboxQuery::default(),
        )
        .unwrap_err();
        assert!(missing_session.to_string().contains("pass session_id"));

        let mail = posted(&homes, "s1", "hello");
        let path = homes.mailbox_dir().join("grok").join("s1.jsonl");
        std::fs::write(
            &path,
            format!(
                "\n{}\nnot-json\n{}\n",
                serde_json::to_string(&mail).unwrap(),
                serde_json::to_string(&mail).unwrap()
            ),
        )
        .unwrap();
        let items = inbox(
            &homes,
            &Caller {
                agent: None,
                session_id: None,
            },
            InboxQuery {
                session_id: Some("s1".into()),
                agent: Some(Agent::Grok),
                ..InboxQuery::default()
            },
        )
        .unwrap();
        assert_eq!(items.items.len(), 2);
        assert_eq!(items.unread, 2);
    }

    #[test]
    fn ack_and_unread_and_since() {
        let _guard = test_env::lock(CALLER_ENV);
        for key in CALLER_ENV {
            unsafe { std::env::remove_var(key) };
        }
        let dir = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(dir.path());
        let first = posted(&homes, "s1", "one");
        let second = posted(&homes, "s1", "two");
        let acked = ack(
            &homes,
            &caller(Agent::Grok, "s1"),
            Some(&first.id),
            None,
            None,
        )
        .unwrap();
        assert_eq!(acked.acked_through, first.id);
        assert_eq!(acked.unread, 1);

        let unread = inbox(
            &homes,
            &caller(Agent::Grok, "s1"),
            InboxQuery {
                unread_only: true,
                ..InboxQuery::default()
            },
        )
        .unwrap();
        assert_eq!(unread.items.len(), 1);
        assert_eq!(unread.items[0].id, second.id);
        assert_eq!(unread.unread, 1);

        let since_id = inbox(
            &homes,
            &caller(Agent::Grok, "s1"),
            InboxQuery {
                since: Some(first.id.clone()),
                ..InboxQuery::default()
            },
        )
        .unwrap();
        assert_eq!(since_id.items.len(), 1);
        assert_eq!(since_id.items[0].message, "two");

        let past = (Utc::now() - ChronoDuration::seconds(5)).to_rfc3339();
        let since_ts = inbox(
            &homes,
            &caller(Agent::Grok, "s1"),
            InboxQuery {
                since: Some(past),
                ..InboxQuery::default()
            },
        )
        .unwrap();
        assert_eq!(since_ts.items.len(), 2);

        let bad = inbox(
            &homes,
            &caller(Agent::Grok, "s1"),
            InboxQuery {
                since: Some("not-a-mail".into()),
                ..InboxQuery::default()
            },
        )
        .unwrap_err();
        assert!(bad.to_string().contains("since must be"));

        ack(&homes, &caller(Agent::Grok, "s1"), None, None, None).unwrap();
        let cleared = inbox(
            &homes,
            &caller(Agent::Grok, "s1"),
            InboxQuery {
                unread_only: true,
                ..InboxQuery::default()
            },
        )
        .unwrap();
        assert!(cleared.items.is_empty());
        assert_eq!(cleared.unread, 0);
    }

    #[test]
    fn await_reply_pending_and_received() {
        let _guard = test_env::lock(CALLER_ENV);
        for key in CALLER_ENV {
            unsafe { std::env::remove_var(key) };
        }
        let dir = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(dir.path());
        unsafe { std::env::set_var("MAGENTS_AWAIT_POLL_MS", "1") };
        let pending = await_reply(
            &homes,
            &caller(Agent::Grok, "s1"),
            None,
            Some(0),
            None,
            None,
        )
        .unwrap();
        assert_eq!(pending.status, "pending");
        assert!(pending.items.is_empty());

        posted(&homes, "s1", "already-there");
        let homes_thread = homes.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(300));
            posted(&homes_thread, "s1", "new-mail");
        });
        let received = await_reply(
            &homes,
            &caller(Agent::Grok, "s1"),
            None,
            Some(5),
            None,
            None,
        )
        .unwrap();
        assert_eq!(received.status, "received");
        assert!(received.items.iter().any(|mail| mail.message == "new-mail"));
        unsafe { std::env::remove_var("MAGENTS_AWAIT_POLL_MS") };
    }

    #[test]
    fn await_reply_filters_sender_and_sleeps() {
        let _guard = test_env::lock(CALLER_ENV);
        for key in CALLER_ENV {
            unsafe { std::env::remove_var(key) };
        }
        let dir = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(dir.path());
        crate::spawn::record(
            &homes,
            Agent::Claude,
            "peer-session",
            dir.path(),
            crate::spawn::Transport::ClaudePrint,
        )
        .unwrap();
        unsafe { std::env::set_var("MAGENTS_AWAIT_POLL_MS", "1") };

        let other = compose(
            &caller(Agent::Cursor, "other"),
            Agent::Grok,
            "s1".into(),
            "from-cursor".into(),
            vec![],
        );
        post(&homes, &other).unwrap();
        let pending = await_reply(
            &homes,
            &caller(Agent::Grok, "s1"),
            Some("claude:peer-session"),
            Some(0),
            None,
            None,
        )
        .unwrap();
        assert_eq!(pending.status, "pending");

        let homes_thread = homes.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(300));
            let from_claude = compose(
                &caller(Agent::Claude, "peer-session"),
                Agent::Grok,
                "s1".into(),
                "from-claude".into(),
                vec![],
            );
            post(&homes_thread, &from_claude).unwrap();
        });
        let received = await_reply(
            &homes,
            &caller(Agent::Grok, "s1"),
            Some("claude:peer-session"),
            Some(5),
            None,
            None,
        )
        .unwrap();
        assert_eq!(received.status, "received");
        assert!(
            received
                .items
                .iter()
                .any(|mail| mail.message == "from-claude")
        );

        let slept = await_reply(
            &homes,
            &caller(Agent::Grok, "empty-await"),
            None,
            Some(1),
            None,
            None,
        )
        .unwrap();
        assert_eq!(slept.status, "pending");
        unsafe { std::env::remove_var("MAGENTS_AWAIT_POLL_MS") };
    }

    #[test]
    fn reply_requires_sender() {
        let _guard = test_env::lock(CALLER_ENV);
        for key in CALLER_ENV {
            unsafe { std::env::remove_var(key) };
        }
        let dir = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(dir.path());
        let err = reply(&homes, &caller(Agent::Grok, "s1"), "pong", None, None, None).unwrap_err();
        assert!(err.to_string().contains("inbox is empty"));
        let missing_ack = ack(
            &homes,
            &caller(Agent::Grok, "s1"),
            Some("missing"),
            None,
            None,
        )
        .unwrap_err();
        assert!(missing_ack.to_string().contains("mail_id not found"));
        let missing_reply = reply(
            &homes,
            &caller(Agent::Grok, "s1"),
            "pong",
            Some("missing"),
            None,
            None,
        )
        .unwrap_err();
        assert!(missing_reply.to_string().contains("mail_id not found"));
        posted(&homes, "s1", "hello");
        let missing_id = reply(
            &homes,
            &caller(Agent::Grok, "s1"),
            "pong",
            Some("missing"),
            None,
            None,
        )
        .unwrap_err();
        assert!(missing_id.to_string().contains("mail_id not found"));
    }

    #[test]
    fn send_rejects_empty_message() {
        let homes = Homes::isolated(tempfile::tempdir().unwrap().path());
        let err = send(&homes, &caller(Agent::Grok, "s1"), "claude:x", "  ").unwrap_err();
        assert!(err.to_string().contains("message must not be empty"));
    }

    #[test]
    fn post_and_inbox_io_errors() {
        let dir = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(dir.path());
        std::fs::create_dir_all(homes.mailbox_dir()).unwrap();
        std::fs::write(homes.mailbox_dir().join("grok"), "not-a-dir").unwrap();
        let mail = compose(
            &caller(Agent::Claude, "from"),
            Agent::Grok,
            "s1".into(),
            "hello".into(),
            vec![],
        );
        assert!(post(&homes, &mail).is_err());

        let homes = Homes::isolated(dir.path().join("inbox-io"));
        let path = homes.mailbox_dir().join("grok").join("s1.jsonl");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{}\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&path).unwrap().permissions();
            permissions.set_mode(0o000);
            std::fs::set_permissions(&path, permissions).unwrap();
            let error =
                inbox(&homes, &caller(Agent::Grok, "s1"), InboxQuery::default()).unwrap_err();
            assert!(error.to_string().contains("failed to read"));
            let mut permissions = std::fs::metadata(&path).unwrap().permissions();
            permissions.set_mode(0o644);
            std::fs::set_permissions(&path, permissions).unwrap();
        }

        let homes = Homes::isolated(dir.path().join("post-open"));
        let path = homes.mailbox_dir().join("grok").join("s1.jsonl");
        std::fs::create_dir_all(&path).unwrap();
        let mail = compose(
            &caller(Agent::Claude, "from"),
            Agent::Grok,
            "s1".into(),
            "hello".into(),
            vec![],
        );
        assert!(post(&homes, &mail).is_err());

        let homes = Homes::isolated(dir.path().join("post-write"));
        let path = homes.mailbox_dir().join("grok").join("s1.jsonl");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&path).unwrap().permissions();
            permissions.set_mode(0o444);
            std::fs::set_permissions(&path, permissions).unwrap();
            assert!(post(&homes, &mail).is_err());
            let mut permissions = std::fs::metadata(&path).unwrap().permissions();
            permissions.set_mode(0o644);
            std::fs::set_permissions(&path, permissions).unwrap();
        }

        let homes = Homes::isolated(dir.path().join("ack-io"));
        posted(&homes, "s1", "hello");
        ack(&homes, &caller(Agent::Grok, "s1"), None, None, None).unwrap();
        let ack_file = homes
            .mailbox_dir()
            .join("acks")
            .join("grok")
            .join("s1.json");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&ack_file).unwrap().permissions();
            permissions.set_mode(0o000);
            std::fs::set_permissions(&ack_file, permissions).unwrap();
            let error =
                inbox(&homes, &caller(Agent::Grok, "s1"), InboxQuery::default()).unwrap_err();
            assert!(error.to_string().contains("failed to read"));
            let mut permissions = std::fs::metadata(&ack_file).unwrap().permissions();
            permissions.set_mode(0o644);
            std::fs::set_permissions(&ack_file, permissions).unwrap();
        }

        let homes = Homes::isolated(dir.path().join("ack-parent"));
        posted(&homes, "s1", "hello");
        std::fs::create_dir_all(homes.mailbox_dir().join("acks")).unwrap();
        std::fs::write(homes.mailbox_dir().join("acks").join("grok"), "file").unwrap();
        assert!(ack(&homes, &caller(Agent::Grok, "s1"), None, None, None).is_err());

        let homes = Homes::isolated(dir.path().join("bad-utf8"));
        let path = homes.mailbox_dir().join("grok").join("s1.jsonl");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, [0xff, 0x80]).unwrap();
        let error = inbox(&homes, &caller(Agent::Grok, "s1"), InboxQuery::default()).unwrap_err();
        assert!(error.to_string().contains("failed to read"), "{error}");
    }

    #[test]
    fn await_and_reply_surface_inbox_io_errors() {
        let _guard = test_env::lock(CALLER_ENV);
        for key in CALLER_ENV {
            unsafe { std::env::remove_var(key) };
        }
        let dir = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(dir.path());
        let path = homes.mailbox_dir().join("grok").join("s1.jsonl");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{}\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&path).unwrap().permissions();
            permissions.set_mode(0o000);
            std::fs::set_permissions(&path, permissions).unwrap();
            assert!(
                await_reply(
                    &homes,
                    &caller(Agent::Grok, "s1"),
                    None,
                    Some(0),
                    None,
                    None
                )
                .is_err()
            );
            assert!(reply(&homes, &caller(Agent::Grok, "s1"), "pong", None, None, None).is_err());
            let mut permissions = std::fs::metadata(&path).unwrap().permissions();
            permissions.set_mode(0o644);
            std::fs::set_permissions(&path, permissions).unwrap();
        }

        posted(&homes, "s1", "hello");
        unsafe { std::env::set_var("MAGENTS_AWAIT_POLL_MS", "1") };
        let homes_thread = homes.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(20));
            let path = homes_thread.mailbox_dir().join("grok").join("s1.jsonl");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut permissions = std::fs::metadata(&path).unwrap().permissions();
                permissions.set_mode(0o000);
                std::fs::set_permissions(&path, permissions).unwrap();
            }
        });
        assert!(
            await_reply(
                &homes,
                &caller(Agent::Grok, "s1"),
                None,
                Some(2),
                None,
                None
            )
            .is_err()
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&path).unwrap().permissions();
            permissions.set_mode(0o644);
            std::fs::set_permissions(&path, permissions).unwrap();
        }
        unsafe { std::env::remove_var("MAGENTS_AWAIT_POLL_MS") };
    }
}
