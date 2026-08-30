use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Agent {
    Claude,
    Codex,
    Cursor,
    Grok,
    OpenCode,
}

impl Agent {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Cursor => "cursor",
            Self::Grok => "grok",
            Self::OpenCode => "opencode",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "claude" | "claude-code" | "ccd" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            "cursor" | "cursor-ide" | "cursor-agent" => Some(Self::Cursor),
            "grok" | "grok-code" => Some(Self::Grok),
            "opencode" | "open-code" | "oc" => Some(Self::OpenCode),
            _ => None,
        }
    }
}

pub(crate) fn valid_session_id(agent: Agent, value: &str) -> bool {
    let safe = !value.is_empty()
        && value.len() <= 256
        && !value.starts_with(['-', '.'])
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'));
    safe && (agent != Agent::OpenCode || value.starts_with("ses_"))
}

#[cfg(test)]
mod tests {
    use super::{Agent, valid_session_id};

    #[test]
    fn parses_all_harness_aliases() {
        assert_eq!(Agent::parse("claude-code"), Some(Agent::Claude));
        assert_eq!(Agent::parse("codex"), Some(Agent::Codex));
        assert_eq!(Agent::parse("cursor-agent"), Some(Agent::Cursor));
        assert_eq!(Agent::parse("grok-code"), Some(Agent::Grok));
        assert_eq!(Agent::parse("open-code"), Some(Agent::OpenCode));
        assert_eq!(Agent::parse("ccd"), Some(Agent::Claude));
        assert_eq!(Agent::parse("cursor-ide"), Some(Agent::Cursor));
        assert_eq!(Agent::parse("oc"), Some(Agent::OpenCode));
        assert_eq!(Agent::parse("nope"), None);
        assert_eq!(Agent::Claude.to_string(), "claude");
        assert_eq!(Agent::Codex.as_str(), "codex");
    }

    #[test]
    fn session_ids_cannot_be_options_or_paths() {
        assert!(valid_session_id(Agent::Claude, "valid.id-1"));
        assert!(!valid_session_id(Agent::Claude, "-p"));
        assert!(!valid_session_id(Agent::Claude, ".hidden"));
        assert!(!valid_session_id(Agent::Claude, "../outside"));
        assert!(valid_session_id(Agent::OpenCode, "ses_valid-1"));
        assert!(!valid_session_id(Agent::OpenCode, "valid-1"));
    }
}

#[cfg(test)]
mod caller_tests {
    use super::{Agent, Caller, Session};
    use crate::test_env;
    use chrono::Utc;

    const KEYS: &[&str] = &[
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
    ];

    fn clear() {
        for key in KEYS {
            unsafe { std::env::remove_var(key) };
        }
    }

    fn session() -> Session {
        Session {
            agent: Agent::Grok,
            session_id: "id".into(),
            desktop_id: Some("desk".into()),
            name: Some("named".into()),
            title: None,
            cwd: Some("/tmp/x".into()),
            branch: Some("main".into()),
            live: true,
            archived: false,
            pid: Some(9),
            model: None,
            last_activity_at: Some(Utc::now()),
            transcript_path: None,
            messaging_socket: None,
            origin: Some("tui".into()),
            tmux: Some("s:0.0".into()),
        }
    }

    #[test]
    fn session_label_haystack_and_activity() {
        let session = session();
        assert_eq!(session.label(), "named");
        assert!(session.haystack().contains("desk"));
        assert!(session.activity_ms() > 0);
        let mut untitled = session.clone();
        untitled.name = None;
        assert_eq!(untitled.label(), "id");
        untitled.last_activity_at = None;
        assert_eq!(untitled.activity_ms(), 0);
        untitled.title = Some("titled".into());
        assert_eq!(untitled.label(), "titled");
    }

    #[test]
    fn caller_detects_each_harness() {
        let _guard = test_env::lock(KEYS);
        clear();
        assert!(Caller::from_env().agent.is_none());

        unsafe { std::env::set_var("GROK_SESSION_ID", "g1") };
        let caller = Caller::from_env();
        assert_eq!(caller.agent, Some(Agent::Grok));
        assert_eq!(caller.session_id.as_deref(), Some("g1"));
        unsafe { std::env::remove_var("GROK_SESSION_ID") };

        unsafe { std::env::set_var("CLAUDE_PROJECT_DIR", "/tmp") };
        let caller = Caller::from_env();
        assert_eq!(caller.agent, Some(Agent::Claude));
        unsafe { std::env::remove_var("CLAUDE_PROJECT_DIR") };
        unsafe { std::env::set_var("CLAUDE_CODE_MESSAGING_SOCKET", "/tmp/x") };
        unsafe { std::env::set_var("CLAUDE_SESSION_ID", "c1") };
        assert_eq!(Caller::from_env().session_id.as_deref(), Some("c1"));
        unsafe { std::env::remove_var("CLAUDE_CODE_MESSAGING_SOCKET") };
        unsafe { std::env::remove_var("CLAUDE_SESSION_ID") };

        unsafe { std::env::set_var("CURSOR_AGENT", "1") };
        unsafe { std::env::set_var("COMPOSER_SESSION_ID", "cur") };
        let caller = Caller::from_env();
        assert_eq!(caller.agent, Some(Agent::Cursor));
        assert_eq!(caller.session_id.as_deref(), Some("cur"));
        unsafe { std::env::remove_var("CURSOR_AGENT") };
        unsafe { std::env::remove_var("COMPOSER_SESSION_ID") };
        unsafe { std::env::set_var("CURSOR_SESSION_ID", "cur2") };
        assert_eq!(Caller::from_env().session_id.as_deref(), Some("cur2"));
        unsafe { std::env::remove_var("CURSOR_SESSION_ID") };

        unsafe { std::env::set_var("OPENCODE_DIRECTORY", "/tmp") };
        unsafe { std::env::set_var("OPENCODE_SESSION", "oc") };
        let caller = Caller::from_env();
        assert_eq!(caller.agent, Some(Agent::OpenCode));
        assert_eq!(caller.session_id.as_deref(), Some("oc"));
        unsafe { std::env::remove_var("OPENCODE_DIRECTORY") };
        unsafe { std::env::remove_var("OPENCODE_SESSION") };

        unsafe { std::env::set_var("CODEX_HOME", "/tmp/codex") };
        assert!(Caller::from_env().agent.is_none());
        unsafe { std::env::set_var("CODEX_SESSION_ID", "cx") };
        let caller = Caller::from_env();
        assert_eq!(caller.agent, Some(Agent::Codex));
        assert_eq!(caller.session_id.as_deref(), Some("cx"));
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
        if std::env::var_os("CURSOR_SESSION_ID").is_some()
            || std::env::var_os("CURSOR_PROJECT_DIR").is_some()
            || std::env::var_os("CURSOR_AGENT").is_some()
        {
            return Self {
                agent: Some(Agent::Cursor),
                session_id: std::env::var("CURSOR_SESSION_ID")
                    .ok()
                    .or_else(|| std::env::var("COMPOSER_SESSION_ID").ok()),
            };
        }
        if std::env::var_os("OPENCODE_SESSION_ID").is_some()
            || std::env::var_os("OPENCODE_DIRECTORY").is_some()
            || std::env::var_os("OPENCODE_SERVER").is_some()
        {
            return Self {
                agent: Some(Agent::OpenCode),
                session_id: std::env::var("OPENCODE_SESSION_ID")
                    .ok()
                    .or_else(|| std::env::var("OPENCODE_SESSION").ok()),
            };
        }
        if std::env::var_os("CODEX_THREAD_ID").is_some()
            || std::env::var_os("CODEX_SESSION_ID").is_some()
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
