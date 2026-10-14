use std::collections::HashSet;
use std::fs;
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
    /// Database chosen with `OPENCODE_DB`, resolved against the OpenCode data directory.
    pub opencode_database: Option<PathBuf>,
    pub opencode_config: PathBuf,
    pub gemini: PathBuf,
    pub agy: PathBuf,
    pub agy_desktop: PathBuf,
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
        let opencode_database = std::env::var_os("OPENCODE_DB")
            .filter(|value| !value.is_empty() && value != ":memory:")
            .map(|value| opencode.join(value));
        let opencode_config = env_path("XDG_CONFIG_HOME")
            .map(|path| path.join("opencode"))
            .unwrap_or_else(|| home.join(".config").join("opencode"));
        let gemini = env_path("GEMINI_CLI_HOME").unwrap_or_else(|| home.join(".gemini"));
        let agy =
            env_path("ANTIGRAVITY_CLI_HOME").unwrap_or_else(|| gemini.join("antigravity-cli"));
        let agy_desktop =
            env_path("ANTIGRAVITY_HOME").unwrap_or_else(|| gemini.join("antigravity"));
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
            opencode_database,
            opencode_config,
            gemini,
            agy,
            agy_desktop,
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

    pub fn learn_dir(&self) -> PathBuf {
        self.magents.join("learn")
    }

    pub fn live_dir(&self) -> PathBuf {
        self.spawn_dir().join("live")
    }

    pub fn opencode_data_home(&self) -> &Path {
        self.opencode.parent().unwrap_or_else(|| Path::new("."))
    }

    pub fn claude_profiles(&self) -> Vec<ClaudeProfile> {
        let mut profiles: Vec<ClaudeProfile> = Vec::new();
        for dir in claude_cli_dirs(self) {
            merge_profile(
                &mut profiles,
                ClaudeProfile {
                    name: claude_profile_name(&dir),
                    config_dir: Some(dir),
                    desktop_sessions: None,
                },
            );
        }
        for (name, desktop) in claude_desktop_dirs(self) {
            if attach_desktop(&mut profiles, name.as_deref(), desktop.clone()) {
                continue;
            }
            merge_profile(
                &mut profiles,
                ClaudeProfile {
                    name,
                    config_dir: None,
                    desktop_sessions: Some(desktop),
                },
            );
        }
        profiles.sort_by(|left, right| {
            let left_primary = left.config_dir.as_deref() == Some(self.claude.as_path());
            let right_primary = right.config_dir.as_deref() == Some(self.claude.as_path());
            right_primary
                .cmp(&left_primary)
                .then_with(|| left.name.cmp(&right.name))
                .then_with(|| left.config_dir.cmp(&right.config_dir))
                .then_with(|| left.desktop_sessions.cmp(&right.desktop_sessions))
        });
        profiles
    }

    pub fn claude_config_dir(&self, profile: Option<&str>) -> PathBuf {
        if let Some(name) = profile {
            for candidate in self.claude_profiles() {
                if same_profile_name(candidate.name.as_deref(), Some(name))
                    && let Some(dir) = candidate.config_dir
                {
                    return dir;
                }
            }
        }
        self.claude.clone()
    }

    pub fn with_claude_home(&self, config_dir: Option<&Path>, profile: Option<&str>) -> Self {
        let mut overlay = self.clone();
        overlay.claude = config_dir
            .filter(|path| !path.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.claude_config_dir(profile));
        overlay
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
            opencode_database: None,
            opencode_config: root.join("opencode-config").join("opencode"),
            gemini: root.join("gemini"),
            agy: root.join("agy"),
            agy_desktop: root.join("agy-desktop"),
            copilot: root.join("copilot"),
            magents: root.join("magents"),
            claude_desktop: root.join("claude-desktop"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaudeProfile {
    pub name: Option<String>,
    pub config_dir: Option<PathBuf>,
    pub desktop_sessions: Option<PathBuf>,
}

fn env_path(key: &str) -> Option<PathBuf> {
    std::env::var_os(key).map(PathBuf::from)
}

fn path_key(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn same_profile_name(left: Option<&str>, right: Option<&str>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => left.eq_ignore_ascii_case(right),
        _ => false,
    }
}

fn attach_desktop(profiles: &mut [ClaudeProfile], name: Option<&str>, desktop: PathBuf) -> bool {
    let mut fallback = None;
    let mut preferred = None;
    for (index, profile) in profiles.iter().enumerate() {
        if !same_profile_name(profile.name.as_deref(), name) {
            continue;
        }
        if fallback.is_none() {
            fallback = Some(index);
        }
        if profile
            .config_dir
            .as_ref()
            .and_then(|dir| dir.file_name())
            .and_then(|file| file.to_str())
            == Some(".claude")
        {
            preferred = Some(index);
            break;
        }
    }
    let Some(index) = preferred.or(fallback) else {
        return false;
    };
    if profiles[index].desktop_sessions.is_none() {
        profiles[index].desktop_sessions = Some(desktop);
    }
    true
}

fn merge_profile(profiles: &mut Vec<ClaudeProfile>, incoming: ClaudeProfile) {
    let config_key = incoming.config_dir.as_deref().map(path_key);
    let desktop_key = incoming.desktop_sessions.as_deref().map(path_key);
    if let Some(existing) = profiles.iter_mut().find(|profile| {
        let same_config = match (profile.config_dir.as_deref().map(path_key), &config_key) {
            (Some(left), Some(right)) => left == *right,
            _ => false,
        };
        let same_desktop = match (
            profile.desktop_sessions.as_deref().map(path_key),
            &desktop_key,
        ) {
            (Some(left), Some(right)) => left == *right,
            _ => false,
        };
        same_config || same_desktop
    }) {
        if existing.name.is_none() {
            existing.name = incoming.name;
        }
        if existing.config_dir.is_none() {
            existing.config_dir = incoming.config_dir;
        }
        if existing.desktop_sessions.is_none() {
            existing.desktop_sessions = incoming.desktop_sessions;
        }
        return;
    }
    profiles.push(incoming);
}

fn claude_profile_name(dir: &Path) -> Option<String> {
    let name = dir.file_name().and_then(|name| name.to_str())?;
    let suffix = name.strip_prefix(".claude-")?.trim();
    if suffix.is_empty() {
        None
    } else {
        Some(suffix.to_ascii_lowercase())
    }
}

fn looks_like_claude_home(path: &Path) -> bool {
    path.is_dir()
        && (path.join("projects").exists()
            || path.join("sessions").exists()
            || path.join("settings.json").is_file()
            || path.join("history.jsonl").is_file())
}

fn claude_cli_dirs(homes: &Homes) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let mut seen = HashSet::new();
    let mut push = |path: PathBuf| {
        let key = path_key(&path);
        if seen.insert(key) {
            dirs.push(path);
        }
    };
    push(homes.claude.clone());
    for root in claude_cli_scan_roots(homes) {
        let Ok(entries) = fs::read_dir(&root) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if name != ".claude" && !name.starts_with(".claude-") {
                continue;
            }
            if name == ".claude-" || name.eq_ignore_ascii_case(".claude-backup") {
                continue;
            }
            if path_key(&path) == path_key(&homes.claude) || looks_like_claude_home(&path) {
                push(path);
            }
        }
    }
    dirs
}

fn claude_cli_scan_roots(homes: &Homes) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let mut seen = HashSet::new();
    let mut push = |path: PathBuf| {
        if seen.insert(path_key(&path)) {
            roots.push(path);
        }
    };
    if let Some(parent) = homes.claude.parent() {
        push(parent.to_path_buf());
    }
    if let Some(home) = user_home_from_claude_desktop(&homes.claude_desktop) {
        push(home);
    }
    roots
}

fn user_home_from_claude_desktop(desktop: &Path) -> Option<PathBuf> {
    let claude_dir = desktop.parent()?;
    let app_support = claude_dir.parent()?;
    let library = app_support.parent()?;
    let claude_name = claude_dir.file_name()?.to_str()?;
    if !claude_name.eq_ignore_ascii_case("Claude") {
        return None;
    }
    if app_support.file_name()?.to_str()? != "Application Support" {
        return None;
    }
    if library.file_name()?.to_str()? != "Library" {
        return None;
    }
    Some(library.parent()?.to_path_buf())
}

fn claude_desktop_dirs(homes: &Homes) -> Vec<(Option<String>, PathBuf)> {
    let mut dirs = Vec::new();
    let mut seen = HashSet::new();
    let mut push = |name: Option<String>, path: PathBuf| {
        let key = path_key(&path);
        if seen.insert(key) {
            dirs.push((name, path));
        }
    };
    push(
        desktop_profile_name(&homes.claude_desktop),
        homes.claude_desktop.clone(),
    );
    let Some(claude_dir) = homes.claude_desktop.parent() else {
        return dirs;
    };
    let Some(folder) = claude_dir.file_name().and_then(|name| name.to_str()) else {
        return dirs;
    };
    if !folder.eq_ignore_ascii_case("Claude") {
        return dirs;
    }
    let Some(app_support) = claude_dir.parent() else {
        return dirs;
    };
    let Ok(entries) = fs::read_dir(app_support) else {
        return dirs;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let lower = name.to_ascii_lowercase();
        if !lower.starts_with("claude") || lower.ends_with("-3p") {
            continue;
        }
        let sessions = path.join("claude-code-sessions");
        if sessions.is_dir() {
            push(desktop_profile_name_from_folder(name), sessions);
        }
    }
    dirs
}

fn desktop_profile_name(sessions_dir: &Path) -> Option<String> {
    if sessions_dir.file_name().and_then(|name| name.to_str()) != Some("claude-code-sessions") {
        return None;
    }
    desktop_profile_name_from_folder(sessions_dir.parent()?.file_name()?.to_str()?)
}

fn desktop_profile_name_from_folder(name: &str) -> Option<String> {
    let suffix = name
        .strip_prefix("Claude")
        .or_else(|| name.strip_prefix("claude"))?;
    let suffix = suffix.strip_prefix('-').unwrap_or(suffix).trim();
    if suffix.is_empty() {
        None
    } else {
        Some(suffix.to_ascii_lowercase())
    }
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

#[cfg(target_os = "macos")]
const PGREP_ARGS: &[&str] = &["-a", "-x"];
#[cfg(not(target_os = "macos"))]
const PGREP_ARGS: &[&str] = &["-x"];

pub fn named_process_alive(name: &str) -> bool {
    Command::new("pgrep")
        .args(PGREP_ARGS)
        .arg(name)
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
    use std::fs;
    use std::path::{Path, PathBuf};

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
        "OPENCODE_DB",
        "GEMINI_CLI_HOME",
        "ANTIGRAVITY_CLI_HOME",
        "ANTIGRAVITY_HOME",
        "COPILOT_HOME",
    ];

    #[test]
    fn pid_and_named_process() {
        let _guard = test_env::lock(&["PATH"]);
        assert!(!pid_alive(0));
        assert!(pid_alive(std::process::id()));
        assert!(!named_process_alive("magents-no-such-process-xyz"));
        let parent = std::process::Command::new("ps")
            .args([
                "-o",
                "comm=",
                "-p",
                &std::os::unix::process::parent_id().to_string(),
            ])
            .output()
            .unwrap();
        let parent = String::from_utf8_lossy(&parent.stdout);
        let parent = std::path::Path::new(parent.trim())
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(
            named_process_alive(&parent),
            "ancestor {parent} should count as alive"
        );
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
            std::env::remove_var("ANTIGRAVITY_CLI_HOME");
            std::env::remove_var("ANTIGRAVITY_HOME");
            std::env::set_var("COPILOT_HOME", root.join("cp"));
            std::env::remove_var("OPENCODE_DATA");
            std::env::set_var("OPENCODE_DB", "opencode-custom.db");
        }
        let homes = Homes::from_env();
        assert_eq!(homes.claude, root.join("c"));
        assert_eq!(homes.grok, root.join("g"));
        assert_eq!(homes.codex, root.join("x"));
        assert_eq!(homes.cursor_config, root.join("cursor-config"));
        assert_eq!(homes.cursor, root.join("cursor-data"));
        assert_eq!(homes.cursor_app, root.join("cursor-app"));
        assert_eq!(homes.opencode, root.join("xdg").join("opencode"));
        assert_eq!(
            homes.opencode_database,
            Some(root.join("xdg").join("opencode").join("opencode-custom.db"))
        );
        assert_eq!(homes.opencode_data_home(), root.join("xdg"));
        assert_eq!(
            homes.opencode_config,
            root.join("xdg-config").join("opencode")
        );
        assert_eq!(homes.gemini, root.join("gm"));
        assert_eq!(homes.agy, root.join("gm").join("antigravity-cli"));
        assert_eq!(homes.agy_desktop, root.join("gm").join("antigravity"));
        assert_eq!(homes.copilot, root.join("cp"));
        assert_eq!(homes.magents, root.join("m"));
        assert_eq!(homes.mailbox_dir(), root.join("m").join("mailbox"));
        assert_eq!(homes.spawn_dir(), root.join("m").join("spawns"));
        assert_eq!(homes.notes_dir(), root.join("m").join("notes"));
        assert_eq!(homes.learn_dir(), root.join("m").join("learn"));
        assert_eq!(homes.live_dir(), root.join("m").join("spawns").join("live"));

        unsafe {
            std::env::set_var("OPENCODE_DATA", root.join("legacy").join("opencode"));
            std::env::set_var("OPENCODE_DB", root.join("absolute.db"));
        }
        let homes = Homes::from_env();
        assert_eq!(homes.opencode, root.join("legacy").join("opencode"));
        assert_eq!(homes.opencode_database, Some(root.join("absolute.db")));
        assert_eq!(homes.opencode_data_home(), root.join("legacy"));

        unsafe {
            std::env::remove_var("OPENCODE_DATA");
            std::env::set_var("OPENCODE_DB", ":memory:");
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
            std::env::remove_var("ANTIGRAVITY_CLI_HOME");
            std::env::remove_var("ANTIGRAVITY_HOME");
            std::env::remove_var("COPILOT_HOME");
            std::env::set_var("CURSOR_HOME", root.join("legacy-cursor"));
        }
        let homes = Homes::from_env();
        assert_eq!(homes.claude, root.join(".claude"));
        assert_eq!(homes.gemini, root.join(".gemini"));
        assert_eq!(homes.agy, root.join(".gemini").join("antigravity-cli"));
        assert_eq!(homes.agy_desktop, root.join(".gemini").join("antigravity"));
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
        assert_eq!(homes.opencode_database, None);
        assert_eq!(homes.opencode_data_home(), root.join(".local/share"));
        assert!(homes.magents.ends_with("magents"));

        unsafe {
            std::env::remove_var("CURSOR_HOME");
            std::env::set_var("XDG_CONFIG_HOME", root.join("xdg-only"));
        }
        let homes = Homes::from_env();
        assert_eq!(homes.cursor_config, root.join("xdg-only").join("cursor"));
        assert_eq!(homes.cursor, root.join(".cursor"));

        unsafe {
            std::env::set_var("ANTIGRAVITY_CLI_HOME", root.join("custom-agy"));
            std::env::set_var("ANTIGRAVITY_HOME", root.join("custom-agy-desktop"));
        }
        let homes = Homes::from_env();
        assert_eq!(homes.agy, root.join("custom-agy"));
        assert_eq!(homes.agy_desktop, root.join("custom-agy-desktop"));
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
            opencode_database: None,
            opencode_config: PathBuf::new(),
            gemini: PathBuf::new(),
            agy: PathBuf::new(),
            agy_desktop: PathBuf::new(),
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
        assert_eq!(homes.agy, directory.path().join("agy"));
        assert_eq!(homes.agy_desktop, directory.path().join("agy-desktop"));
        assert_eq!(homes.copilot, directory.path().join("copilot"));
        assert_ne!(homes.cursor_config, homes.cursor);
        assert_ne!(homes.cursor_config, homes.cursor_app);
        assert_ne!(homes.cursor, homes.cursor_app);
        assert_eq!(homes.claude_profiles().len(), 1);
        assert_eq!(
            homes.claude_profiles()[0].config_dir.as_deref(),
            Some(homes.claude.as_path())
        );
        assert_eq!(
            homes.claude_profiles()[0].desktop_sessions.as_deref(),
            Some(homes.claude_desktop.as_path())
        );
        assert_eq!(homes.claude_config_dir(Some("work")), homes.claude);
        let overlay = homes.with_claude_home(Some(directory.path().join("work").as_path()), None);
        assert_eq!(overlay.claude, directory.path().join("work"));
        assert_eq!(
            homes.with_claude_home(Some(Path::new("")), None).claude,
            homes.claude
        );
    }

    #[test]
    fn claude_profiles_include_sibling_cli_and_desktop_homes() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let homes = Homes::isolated(root);
        fs::create_dir_all(homes.claude.join("sessions")).unwrap();
        let work = root.join(".claude-work");
        fs::create_dir_all(work.join("sessions")).unwrap();
        fs::create_dir_all(work.join("projects")).unwrap();
        fs::write(work.join("settings.json"), "{}\n").unwrap();
        let junk = root.join(".claude-junk");
        fs::create_dir_all(&junk).unwrap();
        fs::create_dir_all(root.join(".claude-")).unwrap();
        fs::write(root.join(".claude-notes"), "skip").unwrap();
        let backup = root.join(".claude-backup");
        fs::create_dir_all(backup.join("projects")).unwrap();
        let settings_only = root.join(".claude-settings");
        fs::create_dir_all(&settings_only).unwrap();
        fs::write(settings_only.join("settings.json"), "{}\n").unwrap();
        let names: Vec<Option<String>> = homes
            .claude_profiles()
            .into_iter()
            .map(|profile| profile.name)
            .collect();
        assert!(names.contains(&None), "{names:?}");
        assert!(names.contains(&Some("work".into())), "{names:?}");
        assert!(names.contains(&Some("settings".into())), "{names:?}");
        assert!(!names.contains(&Some("junk".into())), "{names:?}");
        assert!(!names.contains(&Some("backup".into())), "{names:?}");
        assert_eq!(homes.claude_config_dir(Some("WORK")), work);

        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        let default = home.join(".claude");
        fs::create_dir_all(default.join("sessions")).unwrap();
        let personal = home.join(".claude-personal");
        fs::create_dir_all(&personal).unwrap();
        fs::write(personal.join("history.jsonl"), "{}\n").unwrap();
        let app = home.join("Library").join("Application Support");
        let desktop = app.join("Claude").join("claude-code-sessions");
        fs::create_dir_all(&desktop).unwrap();
        let work_desktop = app.join("Claude-WORK").join("claude-code-sessions");
        fs::create_dir_all(&work_desktop).unwrap();
        fs::create_dir_all(app.join("Claude-WORK-3p").join("configLibrary")).unwrap();
        let solo = app.join("Claude-SOLO").join("claude-code-sessions");
        fs::create_dir_all(&solo).unwrap();
        fs::create_dir_all(app.join("Cursor")).unwrap();
        fs::create_dir_all(home.join(".claude-work").join("sessions")).unwrap();
        let mut homes = Homes::isolated(home);
        homes.claude = default.clone();
        homes.claude_desktop = desktop.clone();
        let profiles = homes.claude_profiles();
        let work = profiles
            .iter()
            .find(|profile| profile.name.as_deref() == Some("work"))
            .unwrap();
        assert_eq!(
            work.desktop_sessions.as_deref(),
            Some(work_desktop.as_path())
        );
        let default_profile = profiles
            .iter()
            .find(|profile| profile.name.is_none())
            .unwrap();
        assert_eq!(
            default_profile.desktop_sessions.as_deref(),
            Some(desktop.as_path())
        );
        assert!(
            profiles
                .iter()
                .any(|profile| profile.name.as_deref() == Some("personal"))
        );
        assert!(
            profiles
                .iter()
                .all(|profile| profile.name.as_deref() != Some("work-3p"))
        );
        let solo_profile = profiles
            .iter()
            .find(|profile| profile.name.as_deref() == Some("solo"))
            .unwrap();
        assert!(solo_profile.config_dir.is_none());
        assert_eq!(
            solo_profile.desktop_sessions.as_deref(),
            Some(solo.as_path())
        );
        assert_eq!(
            homes.with_claude_home(None, Some("personal")).claude,
            personal
        );

        homes.claude = home.join("custom-claude");
        fs::create_dir_all(homes.claude.join("sessions")).unwrap();
        let profiles = homes.claude_profiles();
        let default_profile = profiles
            .iter()
            .find(|profile| {
                profile.name.is_none() && profile.config_dir.as_deref() == Some(default.as_path())
            })
            .unwrap();
        assert_eq!(
            default_profile.desktop_sessions.as_deref(),
            Some(desktop.as_path())
        );
    }

    #[test]
    fn claude_profile_names_and_desktop_folders() {
        use super::{
            claude_profile_name, desktop_profile_name, desktop_profile_name_from_folder,
            user_home_from_claude_desktop,
        };
        assert_eq!(
            claude_profile_name(Path::new("/tmp/.claude-work")),
            Some("work".into())
        );
        assert_eq!(
            claude_profile_name(Path::new("/tmp/.claude-Personal")),
            Some("personal".into())
        );
        assert_eq!(claude_profile_name(Path::new("/tmp/.claude")), None);
        assert_eq!(claude_profile_name(Path::new("/tmp/.claude-")), None);
        assert_eq!(claude_profile_name(Path::new("/tmp/claude")), None);
        assert_eq!(desktop_profile_name_from_folder("Claude"), None);
        assert_eq!(desktop_profile_name_from_folder("claude"), None);
        assert_eq!(
            desktop_profile_name_from_folder("Claude-WORK"),
            Some("work".into())
        );
        assert_eq!(desktop_profile_name_from_folder("Claude-"), None);
        assert_eq!(
            desktop_profile_name(Path::new(
                "/tmp/Library/Application Support/Claude-Personal/claude-code-sessions"
            )),
            Some("personal".into())
        );
        assert_eq!(
            desktop_profile_name(Path::new("/tmp/isolated/claude-desktop")),
            None
        );
        assert_eq!(
            user_home_from_claude_desktop(Path::new(
                "/Users/jake/Library/Application Support/Claude/claude-code-sessions"
            )),
            Some(PathBuf::from("/Users/jake"))
        );
        assert_eq!(
            user_home_from_claude_desktop(Path::new("/tmp/claude-desktop")),
            None
        );
        assert_eq!(
            user_home_from_claude_desktop(Path::new(
                "/Users/jake/Library/Application Support/Cursor/claude-code-sessions"
            )),
            None
        );
        assert_eq!(
            user_home_from_claude_desktop(Path::new(
                "/Users/jake/Library/Other Support/Claude/claude-code-sessions"
            )),
            None
        );
        assert_eq!(
            user_home_from_claude_desktop(Path::new(
                "/Users/jake/Other/Application Support/Claude/claude-code-sessions"
            )),
            None
        );
        assert_eq!(desktop_profile_name_from_folder("NotClaude"), None);
    }

    #[test]
    fn merge_and_attach_claude_profiles() {
        use super::{ClaudeProfile, attach_desktop, merge_profile};
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("config");
        let desktop = directory.path().join("desktop");
        let other = directory.path().join("other");
        fs::create_dir_all(&config).unwrap();
        fs::create_dir_all(&desktop).unwrap();
        fs::create_dir_all(&other).unwrap();

        let mut profiles = vec![ClaudeProfile {
            name: None,
            config_dir: Some(config.clone()),
            desktop_sessions: None,
        }];
        merge_profile(
            &mut profiles,
            ClaudeProfile {
                name: Some("work".into()),
                config_dir: Some(config.clone()),
                desktop_sessions: Some(desktop.clone()),
            },
        );
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].name.as_deref(), Some("work"));
        assert_eq!(
            profiles[0].desktop_sessions.as_deref(),
            Some(desktop.as_path())
        );

        let mut profiles = vec![ClaudeProfile {
            name: None,
            config_dir: None,
            desktop_sessions: Some(desktop.clone()),
        }];
        merge_profile(
            &mut profiles,
            ClaudeProfile {
                name: Some("solo".into()),
                config_dir: Some(config.clone()),
                desktop_sessions: Some(desktop.clone()),
            },
        );
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].config_dir.as_deref(), Some(config.as_path()));
        assert_eq!(profiles[0].name.as_deref(), Some("solo"));

        let mut profiles = vec![ClaudeProfile {
            name: Some("work".into()),
            config_dir: Some(config.clone()),
            desktop_sessions: Some(desktop.clone()),
        }];
        assert!(attach_desktop(&mut profiles, Some("work"), other.clone()));
        assert_eq!(
            profiles[0].desktop_sessions.as_deref(),
            Some(desktop.as_path())
        );
        assert!(!attach_desktop(&mut profiles, Some("missing"), other));

        let mut homes = Homes::isolated(directory.path());
        homes.claude_desktop = directory
            .path()
            .join("Library")
            .join("Application Support")
            .join("Claude")
            .join("claude-code-sessions");
        assert_eq!(homes.claude_profiles().len(), 1);
        let app = directory.path().join("Library").join("Application Support");
        fs::create_dir_all(app.join("Claude").join("claude-code-sessions")).unwrap();
        fs::write(app.join("Claude-FILE"), "not a directory\n").unwrap();
        fs::create_dir_all(app.join("NotClaude")).unwrap();
        let names: Vec<_> = homes
            .claude_profiles()
            .into_iter()
            .filter_map(|profile| profile.name)
            .collect();
        assert!(!names.iter().any(|name| name == "file"), "{names:?}");

        homes.claude_desktop = PathBuf::from("/");
        assert_eq!(homes.claude_profiles().len(), 1);
        homes.claude_desktop = PathBuf::from("/claude-code-sessions");
        assert_eq!(homes.claude_profiles().len(), 1);
        homes.claude_desktop = PathBuf::from("claude-code-sessions");
        assert_eq!(homes.claude_profiles().len(), 1);
        homes.claude_desktop = PathBuf::from("Claude/claude-code-sessions");
        let _ = homes.claude_profiles();

        #[cfg(unix)]
        {
            use std::ffi::OsString;
            use std::os::unix::ffi::OsStringExt;
            let invalid = directory
                .path()
                .join(OsString::from_vec(b".claude-work\xff".to_vec()));
            if fs::create_dir_all(invalid.join("sessions")).is_ok() {
                let _ = homes.claude_profiles();
            }
            homes.claude_desktop = app.join("Claude").join("claude-code-sessions");
            let invalid_desktop = app.join(OsString::from_vec(b"Claude-\xff".to_vec()));
            if fs::create_dir_all(invalid_desktop.join("claude-code-sessions")).is_ok() {
                let names: Vec<_> = homes
                    .claude_profiles()
                    .into_iter()
                    .filter_map(|profile| profile.name)
                    .collect();
                assert!(!names.iter().any(|name| name.contains('\u{fffd}')));
            }
        }
    }
}
