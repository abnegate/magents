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
        Error::msg("pass session_id, or call from a Claude/Codex/Grok MCP session")
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
