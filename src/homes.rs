use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Clone, Debug)]
pub struct Homes {
    pub claude: PathBuf,
    pub grok: PathBuf,
    pub codex: PathBuf,
    /// Cursor Agent configuration.
    pub cursor_config: PathBuf,
    /// Cursor Agent data. This field retains the original `cursor` meaning.
    pub cursor: PathBuf,
    /// Cursor Desktop application support.
    pub cursor_app: PathBuf,
    pub opencode: PathBuf,
    pub opencode_config: PathBuf,
    pub gemini: PathBuf,
    pub copilot: PathBuf,
    pub magents: PathBuf,
    pub claude_desktop: PathBuf,
}

impl Homes {
    pub fn from_env() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        let claude = env_path("CLAUDE_CONFIG_DIR").unwrap_or_else(|| home.join(".claude"));
        let grok = env_path("GROK_HOME").unwrap_or_else(|| home.join(".grok"));
        let codex = env_path("CODEX_HOME").unwrap_or_else(|| home.join(".codex"));
        let cursor_home = env_path("CURSOR_HOME");
        let cursor_config = env_path("CURSOR_CONFIG_DIR")
            .or_else(|| cursor_home.clone())
            .or_else(|| env_path("XDG_CONFIG_HOME").map(|path| path.join("cursor")))
            .unwrap_or_else(|| home.join(".cursor"));
        let cursor = env_path("CURSOR_DATA_DIR")
            .or(cursor_home)
            .unwrap_or_else(|| home.join(".cursor"));
        let cursor_app = env_path("CURSOR_APP_SUPPORT").unwrap_or_else(|| {
            home.join("Library")
                .join("Application Support")
                .join("Cursor")
        });
        let opencode = env_path("OPENCODE_DATA")
            .or_else(|| env_path("XDG_DATA_HOME").map(|path| path.join("opencode")))
            .unwrap_or_else(|| home.join(".local").join("share").join("opencode"));
        let opencode_config = env_path("XDG_CONFIG_HOME")
            .map(|path| path.join("opencode"))
            .unwrap_or_else(|| home.join(".config").join("opencode"));
        let gemini = env_path("GEMINI_CLI_HOME").unwrap_or_else(|| home.join(".gemini"));
        let copilot = env_path("COPILOT_HOME").unwrap_or_else(|| home.join(".copilot"));
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
            cursor_config,
            cursor,
            cursor_app,
            opencode,
            opencode_config,
            gemini,
            copilot,
            magents,
            claude_desktop,
        }
    }

    pub fn mailbox_dir(&self) -> PathBuf {
        self.magents.join("mailbox")
    }

    pub fn spawn_dir(&self) -> PathBuf {
        self.magents.join("spawns")
    }

    pub fn notes_dir(&self) -> PathBuf {
        self.magents.join("notes")
    }

    pub fn live_dir(&self) -> PathBuf {
        self.spawn_dir().join("live")
    }

    pub fn opencode_data_home(&self) -> &Path {
        self.opencode.parent().unwrap_or_else(|| Path::new("."))
    }

    pub fn isolated(root: impl AsRef<Path>) -> Self {
        let root = root.as_ref();
        Self {
            claude: root.join("claude"),
            grok: root.join("grok"),
            codex: root.join("codex"),
            cursor_config: root.join("cursor-config"),
            cursor: root.join("cursor"),
            cursor_app: root.join("cursor-app"),
            opencode: root.join("opencode"),
            opencode_config: root.join("opencode-config").join("opencode"),
            gemini: root.join("gemini"),
            copilot: root.join("copilot"),
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
        "CURSOR_APP_SUPPORT",
        "CURSOR_CONFIG_DIR",
        "CURSOR_DATA_DIR",
        "CURSOR_HOME",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "MAGENTS_HOME",
        "OPENCODE_DATA",
        "GEMINI_CLI_HOME",
        "COPILOT_HOME",
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
            std::env::set_var("CURSOR_CONFIG_DIR", root.join("cursor-config"));
            std::env::set_var("CURSOR_DATA_DIR", root.join("cursor-data"));
            std::env::set_var("CURSOR_APP_SUPPORT", root.join("cursor-app"));
            std::env::set_var("XDG_CONFIG_HOME", root.join("xdg-config"));
            std::env::set_var("XDG_DATA_HOME", root.join("xdg"));
            std::env::set_var("MAGENTS_HOME", root.join("m"));
            std::env::set_var("GEMINI_CLI_HOME", root.join("gm"));
            std::env::set_var("COPILOT_HOME", root.join("cp"));
            std::env::remove_var("OPENCODE_DATA");
        }
        let homes = Homes::from_env();
        assert_eq!(homes.claude, root.join("c"));
        assert_eq!(homes.grok, root.join("g"));
        assert_eq!(homes.codex, root.join("x"));
        assert_eq!(homes.cursor_config, root.join("cursor-config"));
        assert_eq!(homes.cursor, root.join("cursor-data"));
        assert_eq!(homes.cursor_app, root.join("cursor-app"));
        assert_eq!(homes.opencode, root.join("xdg").join("opencode"));
        assert_eq!(homes.opencode_data_home(), root.join("xdg"));
        assert_eq!(
            homes.opencode_config,
            root.join("xdg-config").join("opencode")
        );
        assert_eq!(homes.gemini, root.join("gm"));
        assert_eq!(homes.copilot, root.join("cp"));
        assert_eq!(homes.magents, root.join("m"));
        assert_eq!(homes.mailbox_dir(), root.join("m").join("mailbox"));
        assert_eq!(homes.spawn_dir(), root.join("m").join("spawns"));
        assert_eq!(homes.notes_dir(), root.join("m").join("notes"));
        assert_eq!(homes.live_dir(), root.join("m").join("spawns").join("live"));

        unsafe {
            std::env::set_var("OPENCODE_DATA", root.join("legacy").join("opencode"));
        }
        let homes = Homes::from_env();
        assert_eq!(homes.opencode, root.join("legacy").join("opencode"));
        assert_eq!(homes.opencode_data_home(), root.join("legacy"));

        unsafe {
            std::env::remove_var("OPENCODE_DATA");
            std::env::remove_var("XDG_DATA_HOME");
            std::env::remove_var("XDG_CONFIG_HOME");
            std::env::remove_var("CLAUDE_CONFIG_DIR");
            std::env::remove_var("GROK_HOME");
            std::env::remove_var("CODEX_HOME");
            std::env::remove_var("CURSOR_APP_SUPPORT");
            std::env::remove_var("CURSOR_CONFIG_DIR");
            std::env::remove_var("CURSOR_DATA_DIR");
            std::env::remove_var("MAGENTS_HOME");
            std::env::remove_var("GEMINI_CLI_HOME");
            std::env::remove_var("COPILOT_HOME");
            std::env::set_var("CURSOR_HOME", root.join("legacy-cursor"));
        }
        let homes = Homes::from_env();
        assert_eq!(homes.claude, root.join(".claude"));
        assert_eq!(homes.gemini, root.join(".gemini"));
        assert_eq!(homes.copilot, root.join(".copilot"));
        assert_eq!(homes.cursor_config, root.join("legacy-cursor"));
        assert_eq!(homes.cursor, root.join("legacy-cursor"));
        assert_eq!(
            homes.cursor_app,
            root.join("Library")
                .join("Application Support")
                .join("Cursor")
        );
        assert_eq!(
            homes.opencode,
            root.join(".local").join("share").join("opencode")
        );
        assert_eq!(homes.opencode_config, root.join(".config").join("opencode"));
        assert_eq!(homes.opencode_data_home(), root.join(".local/share"));
        assert!(homes.magents.ends_with("magents"));

        unsafe {
            std::env::remove_var("CURSOR_HOME");
            std::env::set_var("XDG_CONFIG_HOME", root.join("xdg-only"));
        }
        let homes = Homes::from_env();
        assert_eq!(homes.cursor_config, root.join("xdg-only").join("cursor"));
        assert_eq!(homes.cursor, root.join(".cursor"));
    }

    #[test]
    fn opencode_data_home_falls_back_without_parent() {
        use std::path::PathBuf;
        let homes = Homes {
            claude: PathBuf::new(),
            grok: PathBuf::new(),
            codex: PathBuf::new(),
            cursor_config: PathBuf::new(),
            cursor: PathBuf::new(),
            cursor_app: PathBuf::new(),
            opencode: PathBuf::new(),
            opencode_config: PathBuf::new(),
            gemini: PathBuf::new(),
            copilot: PathBuf::new(),
            magents: PathBuf::new(),
            claude_desktop: PathBuf::new(),
        };
        assert_eq!(homes.opencode_data_home(), std::path::Path::new("."));
    }

    #[test]
    fn isolated_cursor_roots_are_distinct() {
        let directory = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(directory.path());

        assert_eq!(homes.cursor_config, directory.path().join("cursor-config"));
        assert_eq!(homes.cursor, directory.path().join("cursor"));
        assert_eq!(homes.cursor_app, directory.path().join("cursor-app"));
        assert_eq!(homes.opencode_data_home(), directory.path());
        assert_eq!(homes.gemini, directory.path().join("gemini"));
        assert_eq!(homes.copilot, directory.path().join("copilot"));
        assert_ne!(homes.cursor_config, homes.cursor);
        assert_ne!(homes.cursor_config, homes.cursor_app);
        assert_ne!(homes.cursor, homes.cursor_app);
    }
}
