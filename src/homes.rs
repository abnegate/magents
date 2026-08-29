use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct Homes {
    pub claude: PathBuf,
    pub grok: PathBuf,
    pub codex: PathBuf,
    pub magents: PathBuf,
    pub claude_desktop: PathBuf,
}

impl Homes {
    pub fn from_env() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        let claude = env_path("CLAUDE_CONFIG_DIR").unwrap_or_else(|| home.join(".claude"));
        let grok = env_path("GROK_HOME").unwrap_or_else(|| home.join(".grok"));
        let codex = env_path("CODEX_HOME").unwrap_or_else(|| home.join(".codex"));
        let magents = env_path("MAGENTS_HOME").unwrap_or_else(|| {
            dirs::data_dir()
                .unwrap_or_else(|| home.join(".local").join("share"))
                .join("magents")
        });
        let claude_desktop = home
            .join("Library")
            .join("Application Support")
            .join("Claude")
            .join("claude-code-sessions");
        Self {
            claude,
            grok,
            codex,
            magents,
            claude_desktop,
        }
    }

    pub fn mailbox_dir(&self) -> PathBuf {
        self.magents.join("mailbox")
    }
}

fn env_path(key: &str) -> Option<PathBuf> {
    std::env::var_os(key).map(PathBuf::from)
}

pub fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(unix)]
    {
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}
