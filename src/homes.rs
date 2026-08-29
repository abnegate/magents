use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Clone, Debug)]
pub struct Homes {
    pub claude: PathBuf,
    pub grok: PathBuf,
    pub codex: PathBuf,
    pub cursor: PathBuf,
    pub cursor_app: PathBuf,
    pub opencode: PathBuf,
    pub magents: PathBuf,
    pub claude_desktop: PathBuf,
}

impl Homes {
    pub fn from_env() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        let claude = env_path("CLAUDE_CONFIG_DIR").unwrap_or_else(|| home.join(".claude"));
        let grok = env_path("GROK_HOME").unwrap_or_else(|| home.join(".grok"));
        let codex = env_path("CODEX_HOME").unwrap_or_else(|| home.join(".codex"));
        let cursor = env_path("CURSOR_HOME").unwrap_or_else(|| home.join(".cursor"));
        let cursor_app = env_path("CURSOR_APP_SUPPORT").unwrap_or_else(|| {
            home.join("Library")
                .join("Application Support")
                .join("Cursor")
        });
        let opencode = env_path("OPENCODE_DATA")
            .or_else(|| env_path("XDG_DATA_HOME").map(|path| path.join("opencode")))
            .unwrap_or_else(|| home.join(".local").join("share").join("opencode"));
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
            cursor,
            cursor_app,
            opencode,
            magents,
            claude_desktop,
        }
    }

    pub fn mailbox_dir(&self) -> PathBuf {
        self.magents.join("mailbox")
    }

    pub fn isolated(root: impl AsRef<Path>) -> Self {
        let root = root.as_ref();
        Self {
            claude: root.join("claude"),
            grok: root.join("grok"),
            codex: root.join("codex"),
            cursor: root.join("cursor"),
            cursor_app: root.join("cursor-app"),
            opencode: root.join("opencode"),
            magents: root.join("magents"),
            claude_desktop: root.join("claude-desktop"),
        }
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

pub fn named_process_alive(name: &str) -> bool {
    Command::new("pgrep")
        .args(["-x", name])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::{Homes, named_process_alive, pid_alive};
    use crate::test_env;

    const KEYS: &[&str] = &[
        "HOME",
        "CLAUDE_CONFIG_DIR",
        "GROK_HOME",
        "CODEX_HOME",
        "CURSOR_HOME",
        "CURSOR_APP_SUPPORT",
        "OPENCODE_DATA",
        "XDG_DATA_HOME",
        "MAGENTS_HOME",
    ];

    #[test]
    fn pid_and_named_process() {
        assert!(!pid_alive(0));
        assert!(pid_alive(std::process::id()));
        assert!(!named_process_alive("magents-no-such-process-xyz"));
    }

    #[test]
    fn from_env_reads_overrides_and_xdg() {
        let _guard = test_env::lock(KEYS);
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        unsafe {
            std::env::set_var("HOME", root);
            std::env::set_var("CLAUDE_CONFIG_DIR", root.join("c"));
            std::env::set_var("GROK_HOME", root.join("g"));
            std::env::set_var("CODEX_HOME", root.join("x"));
            std::env::set_var("CURSOR_HOME", root.join("u"));
            std::env::set_var("CURSOR_APP_SUPPORT", root.join("ua"));
            std::env::remove_var("OPENCODE_DATA");
            std::env::set_var("XDG_DATA_HOME", root.join("xdg"));
            std::env::set_var("MAGENTS_HOME", root.join("m"));
        }
        let homes = Homes::from_env();
        assert_eq!(homes.claude, root.join("c"));
        assert_eq!(homes.grok, root.join("g"));
        assert_eq!(homes.codex, root.join("x"));
        assert_eq!(homes.cursor, root.join("u"));
        assert_eq!(homes.cursor_app, root.join("ua"));
        assert_eq!(homes.opencode, root.join("xdg").join("opencode"));
        assert_eq!(homes.magents, root.join("m"));
        assert_eq!(homes.mailbox_dir(), root.join("m").join("mailbox"));

        unsafe {
            std::env::remove_var("XDG_DATA_HOME");
            std::env::remove_var("CLAUDE_CONFIG_DIR");
            std::env::remove_var("GROK_HOME");
            std::env::remove_var("CODEX_HOME");
            std::env::remove_var("CURSOR_HOME");
            std::env::remove_var("CURSOR_APP_SUPPORT");
            std::env::remove_var("MAGENTS_HOME");
            std::env::set_var("OPENCODE_DATA", root.join("oc"));
        }
        let homes = Homes::from_env();
        assert_eq!(homes.claude, root.join(".claude"));
        assert_eq!(homes.opencode, root.join("oc"));
        assert!(homes.magents.ends_with("magents"));
    }
}
