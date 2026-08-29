use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Agent {
    Claude,
    Codex,
    Grok,
}

impl Agent {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Grok => "grok",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "claude" | "claude-code" | "ccd" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            "grok" | "grok-code" => Some(Self::Grok),
            _ => None,
        }
    }
}

impl std::fmt::Display for Agent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    pub agent: Agent,
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub desktop_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    pub live: bool,
    pub archived: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_activity_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub messaging_socket: Option<PathBuf>,
    /// cli, claude-desktop, tui, vscode/desktop
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    /// Claude CLI tmux pane, e.g. `session:@window.%pane`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tmux: Option<String>,
}

impl Session {
    pub fn activity_ms(&self) -> i64 {
        self.last_activity_at
            .map(|time| time.timestamp_millis())
            .unwrap_or(0)
    }

    pub fn haystack(&self) -> String {
        [
            Some(self.agent.as_str().to_string()),
            Some(self.session_id.clone()),
            self.desktop_id.clone(),
            self.name.clone(),
            self.title.clone(),
            self.cwd.clone(),
            self.branch.clone(),
            self.origin.clone(),
            self.tmux.clone(),
            self.pid.map(|pid| pid.to_string()),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
    }

    pub fn label(&self) -> String {
        self.title
            .as_deref()
            .or(self.name.as_deref())
            .unwrap_or(self.session_id.as_str())
            .to_string()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Turn {
    pub role: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Transcript {
    pub session: Session,
    pub turn_count: usize,
    pub returned_turns: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_user_request: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_assistant_action: Option<String>,
    pub turns: Vec<Turn>,
    pub inert: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchHit {
    pub session: Session,
    pub matches: usize,
    pub snippet: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Mail {
    pub id: String,
    pub ts: DateTime<Utc>,
    pub from_agent: Option<Agent>,
    pub from_session: Option<String>,
    pub from_name: Option<String>,
    pub to_agent: Agent,
    pub to_session: String,
    pub message: String,
    pub delivered: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct Caller {
    pub agent: Option<Agent>,
    pub session_id: Option<String>,
}

impl Caller {
    pub fn from_env() -> Self {
        if let Ok(session_id) = std::env::var("GROK_SESSION_ID")
            && !session_id.is_empty()
        {
            return Self {
                agent: Some(Agent::Grok),
                session_id: Some(session_id),
            };
        }
        if std::env::var_os("CLAUDE_CODE_MESSAGING_SOCKET").is_some()
            || std::env::var_os("CLAUDE_PROJECT_DIR").is_some()
        {
            return Self {
                agent: Some(Agent::Claude),
                session_id: std::env::var("CLAUDE_SESSION_ID").ok(),
            };
        }
        if std::env::var_os("CODEX_HOME").is_some() || std::env::var_os("CODEX_THREAD_ID").is_some()
        {
            return Self {
                agent: Some(Agent::Codex),
                session_id: std::env::var("CODEX_THREAD_ID")
                    .ok()
                    .or_else(|| std::env::var("CODEX_SESSION_ID").ok()),
            };
        }
        Self {
            agent: None,
            session_id: None,
        }
    }
}
