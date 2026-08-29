use crate::error::{Error, Result};
use crate::homes::Homes;
use crate::model::{Agent, Caller, Mail};
use chrono::Utc;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use uuid::Uuid;

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

pub fn inbox(
    homes: &Homes,
    caller: &Caller,
    session_id: Option<&str>,
    agent: Option<Agent>,
) -> Result<Vec<Mail>> {
    let agent = agent.or(caller.agent).ok_or_else(|| {
        Error::msg("pass session_id, or call from a Claude/Codex/Cursor/Grok/OpenCode MCP session")
    })?;
    let session_id = session_id
        .map(ToOwned::to_owned)
        .or_else(|| caller.session_id.clone())
        .ok_or_else(|| Error::msg("pass session_id"))?;
    let path = mailbox_path(homes, agent, &session_id);
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

fn mailbox_path(homes: &Homes, agent: Agent, session_id: &str) -> PathBuf {
    homes
        .mailbox_dir()
        .join(agent.as_str())
        .join(format!("{session_id}.jsonl"))
}

#[cfg(test)]
mod tests {
    use super::{compose, inbox, post};
    use crate::homes::Homes;
    use crate::model::{Agent, Caller};

    #[test]
    fn inbox_requires_identity_and_skips_junk() {
        let dir = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(dir.path());
        let empty = inbox(
            &homes,
            &Caller {
                agent: Some(Agent::Grok),
                session_id: Some("s1".into()),
            },
            None,
            None,
        )
        .unwrap();
        assert!(empty.is_empty());

        let missing_agent = inbox(
            &homes,
            &Caller {
                agent: None,
                session_id: None,
            },
            None,
            None,
        )
        .unwrap_err();
        assert!(missing_agent.to_string().contains("pass session_id"));

        let missing_session = inbox(
            &homes,
            &Caller {
                agent: Some(Agent::Grok),
                session_id: None,
            },
            None,
            None,
        )
        .unwrap_err();
        assert!(missing_session.to_string().contains("pass session_id"));

        let mail = compose(
            &Caller {
                agent: Some(Agent::Claude),
                session_id: Some("from".into()),
            },
            Agent::Grok,
            "s1".into(),
            "hello".into(),
            vec!["claude-uds".into()],
        );
        post(&homes, &mail).unwrap();
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
            Some("s1"),
            Some(Agent::Grok),
        )
        .unwrap();
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn post_and_inbox_io_errors() {
        let dir = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(dir.path());
        std::fs::create_dir_all(homes.mailbox_dir()).unwrap();
        std::fs::write(homes.mailbox_dir().join("grok"), "not-a-dir").unwrap();
        let mail = compose(
            &Caller {
                agent: Some(Agent::Claude),
                session_id: Some("from".into()),
            },
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
            let error = inbox(
                &homes,
                &Caller {
                    agent: Some(Agent::Grok),
                    session_id: Some("s1".into()),
                },
                None,
                None,
            )
            .unwrap_err();
            assert!(error.to_string().contains("failed to read"));
            let mut permissions = std::fs::metadata(&path).unwrap().permissions();
            permissions.set_mode(0o644);
            std::fs::set_permissions(&path, permissions).unwrap();
        }
    }
}
