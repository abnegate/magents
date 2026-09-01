use crate::error::{Error, Result};
use crate::homes::Homes;
use serde_json::{Map, Value, json};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const SKILL: &str = include_str!("../skills/magents.md");

#[derive(Clone, Copy, Default)]
pub struct InstallSpec {
    pub claude: bool,
    pub grok: bool,
    pub codex: bool,
    pub cursor: bool,
    pub opencode: bool,
    pub gemini: bool,
    pub copilot: bool,
    pub skip_missing: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum HostStatus {
    Added,
    Replaced,
    Skipped,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct HostInstall {
    pub host: &'static str,
    pub status: HostStatus,
    pub detail: String,
}

impl fmt::Display for HostInstall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.status {
            HostStatus::Added => write!(f, "added {} MCP server", self.host),
            HostStatus::Replaced => write!(f, "replaced existing {} MCP server", self.host),
            HostStatus::Skipped => write!(f, "skipped {} ({})", self.host, self.detail),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstallEvent {
    Started { host: &'static str },
    Finished { result: HostInstall },
}

pub fn install(
    claude: bool,
    grok: bool,
    codex: bool,
    cursor: bool,
    opencode: bool,
    gemini: bool,
    copilot: bool,
) -> Result<Vec<HostInstall>> {
    install_spec(InstallSpec {
        claude,
        grok,
        codex,
        cursor,
        opencode,
        gemini,
        copilot,
        skip_missing: false,
    })
}

pub fn install_spec(spec: InstallSpec) -> Result<Vec<HostInstall>> {
    install_spec_with(spec, |_| {})
}

pub fn install_spec_with(
    spec: InstallSpec,
    mut on_event: impl FnMut(InstallEvent),
) -> Result<Vec<HostInstall>> {
    let exe = std::env::current_exe().map_err(|source| Error::Io {
        path: PathBuf::from("magents"),
        source,
    })?;
    let homes = Homes::from_env();
    let mut notes = Vec::new();
    try_host(
        spec.grok,
        spec.skip_missing,
        &mut notes,
        "grok",
        || install_grok(&homes, &exe),
        Some(skill_path(
            dirs::home_dir().unwrap_or_default().join(".grok"),
        )),
        &mut on_event,
    )?;
    try_host(
        spec.claude,
        spec.skip_missing,
        &mut notes,
        "claude",
        || install_claude(&homes, &exe),
        Some(skill_path(
            dirs::home_dir().unwrap_or_default().join(".claude"),
        )),
        &mut on_event,
    )?;
    try_host(
        spec.codex,
        spec.skip_missing,
        &mut notes,
        "codex",
        || install_codex(&homes, &exe),
        None,
        &mut on_event,
    )?;
    try_host(
        spec.cursor,
        spec.skip_missing,
        &mut notes,
        "cursor",
        || install_cursor(&exe),
        Some(skill_path(
            dirs::home_dir().unwrap_or_default().join(".cursor"),
        )),
        &mut on_event,
    )?;
    try_host(
        spec.opencode,
        spec.skip_missing,
        &mut notes,
        "opencode",
        || install_opencode(&exe),
        Some(skill_path(
            dirs::home_dir()
                .unwrap_or_default()
                .join(".config")
                .join("opencode"),
        )),
        &mut on_event,
    )?;
    try_host(
        spec.gemini,
        spec.skip_missing,
        &mut notes,
        "gemini",
        || install_gemini(&homes, &exe),
        Some(skill_path(homes.gemini.clone())),
        &mut on_event,
    )?;
    try_host(
        spec.copilot,
        spec.skip_missing,
        &mut notes,
        "copilot",
        || install_copilot(&homes, &exe),
        Some(skill_path(homes.copilot.clone())),
        &mut on_event,
    )?;
    Ok(notes)
}

fn skill_path(root: PathBuf) -> PathBuf {
    root.join("skills").join("magents").join("SKILL.md")
}

fn try_host(
    enabled: bool,
    skip_missing: bool,
    notes: &mut Vec<HostInstall>,
    program: &'static str,
    install: impl FnOnce() -> Result<HostStatus>,
    skill: Option<PathBuf>,
    on_event: &mut impl FnMut(InstallEvent),
) -> Result<()> {
    if !enabled {
        return Ok(());
    }
    on_event(InstallEvent::Started { host: program });
    match install() {
        Ok(status) => {
            if let Some(path) = skill {
                write_skill(path)?;
            }
            let result = HostInstall {
                host: program,
                status,
                detail: String::new(),
            };
            on_event(InstallEvent::Finished {
                result: result.clone(),
            });
            notes.push(result);
            Ok(())
        }
        Err(error) if skip_missing && missing_binary(program, &error) => {
            let result = HostInstall {
                host: program,
                status: HostStatus::Skipped,
                detail: "not installed".into(),
            };
            on_event(InstallEvent::Finished {
                result: result.clone(),
            });
            notes.push(result);
            Ok(())
        }
        Err(error) => Err(error),
    }
}

fn missing_binary(program: &str, error: &Error) -> bool {
    error
        .to_string()
        .starts_with(&format!("{program} not found:"))
}

fn install_grok(homes: &Homes, exe: &Path) -> Result<HostStatus> {
    let add = [
        "mcp",
        "add",
        "magents",
        "--",
        exe.to_str().unwrap_or("magents"),
        "mcp",
    ];
    add_or_replace_known(cli_has_magents(homes, "grok"), "grok", &add)
}

fn install_claude(homes: &Homes, exe: &Path) -> Result<HostStatus> {
    let add = [
        "mcp",
        "add",
        "--scope",
        "user",
        "magents",
        "--",
        exe.to_str().unwrap_or("magents"),
        "mcp",
    ];
    add_or_replace_known(cli_has_magents(homes, "claude"), "claude", &add)
}

fn install_codex(homes: &Homes, exe: &Path) -> Result<HostStatus> {
    let add = [
        "mcp",
        "add",
        "magents",
        "--",
        exe.to_str().unwrap_or("magents"),
        "mcp",
    ];
    add_or_replace_known(cli_has_magents(homes, "codex"), "codex", &add)
}

fn install_gemini(homes: &Homes, exe: &Path) -> Result<HostStatus> {
    let add = [
        "mcp",
        "add",
        "-s",
        "user",
        "magents",
        "--",
        exe.to_str().unwrap_or("magents"),
        "mcp",
    ];
    add_or_replace_known(cli_has_magents(homes, "gemini"), "gemini", &add)
}

fn install_copilot(homes: &Homes, exe: &Path) -> Result<HostStatus> {
    let add = [
        "mcp",
        "add",
        "magents",
        "--",
        exe.to_str().unwrap_or("magents"),
        "mcp",
    ];
    add_or_replace_known(cli_has_magents(homes, "copilot"), "copilot", &add)
}

fn cli_has_magents(homes: &Homes, program: &str) -> bool {
    match program {
        "grok" => toml_has_keys(&homes.grok.join("config.toml"), &["mcp_servers", "magents"]),
        "codex" => toml_has_keys(
            &homes.codex.join("config.toml"),
            &["mcp_servers", "magents"],
        ),
        "claude" => json_top_level_mcp(
            &dirs::home_dir().unwrap_or_default().join(".claude.json"),
            "magents",
        ),
        _ => mcp_get_exists(program),
    }
}

fn json_top_level_mcp(path: &Path, name: &str) -> bool {
    let Ok(root) = read_json_object(path) else {
        return false;
    };
    root.get("mcpServers")
        .and_then(Value::as_object)
        .is_some_and(|servers| servers.contains_key(name))
}

fn mcp_get_exists(program: &str) -> bool {
    let Ok(output) = Command::new(program)
        .args(["mcp", "get", "magents"])
        .output()
    else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    text.to_ascii_lowercase().lines().any(|line| {
        let line = line.trim_start();
        line == "magents" || line.starts_with("magents:") || line.starts_with("magents ")
    })
}

fn add_or_replace_known(existed: bool, program: &str, add: &[&str]) -> Result<HostStatus> {
    match run(program, add) {
        Ok(_) => Ok(if existed {
            HostStatus::Replaced
        } else {
            HostStatus::Added
        }),
        // Hosts that refuse a second add have already confirmed the server is
        // registered. Removing it here would open a gap if the retry failed.
        Err(error) if already_registered(&error) => Ok(HostStatus::Replaced),
        Err(error) => Err(error),
    }
}

fn already_registered(error: &Error) -> bool {
    let text = error.to_string().to_ascii_lowercase();
    text.contains("already exists") || text.contains("already registered")
}

fn install_cursor(exe: &Path) -> Result<HostStatus> {
    let path = dirs::home_dir()
        .unwrap_or_default()
        .join(".cursor")
        .join("mcp.json");
    let mut root = read_json_object(&path)?;
    let servers = root
        .entry("mcpServers")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| Error::msg("cursor mcp.json mcpServers must be an object"))?;
    let status = upsert_object_key(
        servers,
        "magents",
        json!({
            "command": exe.to_str().unwrap_or("magents"),
            "args": ["mcp"],
        }),
    );
    write_json(&path, &Value::Object(root))?;
    Ok(status)
}

fn install_opencode(exe: &Path) -> Result<HostStatus> {
    let path = dirs::home_dir()
        .unwrap_or_default()
        .join(".config")
        .join("opencode")
        .join("opencode.json");
    let mut root = read_json_object(&path)?;
    if !root.contains_key("$schema") {
        root.insert("$schema".into(), json!("https://opencode.ai/config.json"));
    }
    let mcp = root
        .entry("mcp")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| Error::msg("opencode.json mcp must be an object"))?;
    let status = upsert_object_key(
        mcp,
        "magents",
        json!({
            "type": "local",
            "command": [exe.to_str().unwrap_or("magents"), "mcp"],
            "enabled": true,
        }),
    );
    write_json(&path, &Value::Object(root))?;
    Ok(status)
}

fn upsert_object_key(map: &mut Map<String, Value>, key: &str, value: Value) -> HostStatus {
    let replaced = map.contains_key(key);
    map.insert(key.into(), value);
    if replaced {
        HostStatus::Replaced
    } else {
        HostStatus::Added
    }
}

fn toml_has_keys(path: &Path, keys: &[&str]) -> bool {
    let Ok(raw) = fs::read_to_string(path) else {
        return false;
    };
    let Ok(doc) = raw.parse::<toml_edit::DocumentMut>() else {
        return false;
    };
    let mut current = doc.as_item();
    for key in keys {
        match current.get(key) {
            Some(next) => current = next,
            None => return false,
        }
    }
    true
}

fn read_json_object(path: &Path) -> Result<Map<String, Value>> {
    if !path.is_file() {
        return Ok(Map::new());
    }
    let raw = fs::read_to_string(path).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if raw.trim().is_empty() {
        return Ok(Map::new());
    }
    let value: Value = serde_json::from_str(&raw)?;
    match value {
        Value::Object(map) => Ok(map),
        _ => Err(Error::msg(format!(
            "{} is not a JSON object",
            path.display()
        ))),
    }
}

fn write_json(path: &Path, value: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| Error::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let raw = serde_json::to_string_pretty(value)?;
    fs::write(path, format!("{raw}\n")).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn write_skill(path: PathBuf) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| Error::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    fs::write(&path, SKILL).map_err(|source| Error::Io {
        path: path.clone(),
        source,
    })
}

fn run(program: &str, args: &[&str]) -> Result<String> {
    let output = Command::new(program).args(args).output();
    match output {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            if output.status.success() {
                Ok(if stdout.is_empty() {
                    format!("{program} mcp add magents")
                } else {
                    stdout
                })
            } else {
                Err(Error::msg(format!(
                    "{program} mcp add failed: {stderr} {stdout}"
                )))
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Err(Error::msg(format!("{program} not found: {error}")))
        }
        Err(error) => Err(Error::msg(format!("{program} mcp add failed: {error}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        HostInstall, HostStatus, InstallEvent, InstallSpec, cli_has_magents, install, install_spec,
        install_spec_with, json_top_level_mcp, mcp_get_exists, read_json_object, toml_has_keys,
        write_json, write_skill,
    };
    use crate::error::Error;
    use crate::homes::Homes;
    use crate::test_env;
    use serde_json::{Value, json};
    use std::fs;
    use std::path::Path;

    const ENV: &[&str] = &[
        "HOME",
        "PATH",
        "GEMINI_CLI_HOME",
        "COPILOT_HOME",
        "GROK_HOME",
        "CODEX_HOME",
    ];

    fn with_home(run: impl FnOnce(&Path, &Path)) {
        let _guard = test_env::lock(ENV);
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let bin = home.join("bin");
        fs::create_dir_all(&bin).unwrap();
        unsafe {
            std::env::set_var("HOME", home);
            std::env::set_var("PATH", &bin);
            std::env::remove_var("GROK_HOME");
            std::env::remove_var("CODEX_HOME");
            std::env::remove_var("GEMINI_CLI_HOME");
            std::env::remove_var("COPILOT_HOME");
        }
        run(home, &bin);
    }

    fn host<'a>(notes: &'a [HostInstall], name: &str) -> &'a HostInstall {
        notes
            .iter()
            .find(|note| note.host == name)
            .unwrap_or_else(|| panic!("missing {name} in {notes:?}"))
    }

    #[test]
    fn install_none_is_empty() {
        let notes = install(false, false, false, false, false, false, false).unwrap();
        assert!(notes.is_empty());
    }

    #[test]
    fn host_install_displays_each_status() {
        assert_eq!(
            HostInstall {
                host: "grok",
                status: HostStatus::Added,
                detail: String::new(),
            }
            .to_string(),
            "added grok MCP server"
        );
        assert_eq!(
            HostInstall {
                host: "claude",
                status: HostStatus::Replaced,
                detail: String::new(),
            }
            .to_string(),
            "replaced existing claude MCP server"
        );
        assert_eq!(
            HostInstall {
                host: "gemini",
                status: HostStatus::Skipped,
                detail: "not installed".into(),
            }
            .to_string(),
            "skipped gemini (not installed)"
        );
    }

    #[test]
    fn install_cursor_and_opencode_merge_json() {
        with_home(|home, _bin| {
            let cursor = home.join(".cursor").join("mcp.json");
            write_json(&cursor, &json!({"mcpServers": {"other": {"command": "x"}}})).unwrap();
            let opencode = home.join(".config").join("opencode").join("opencode.json");
            fs::create_dir_all(opencode.parent().unwrap()).unwrap();
            fs::write(&opencode, "{}\n").unwrap();
            let notes = install(false, false, false, true, true, false, false).unwrap();
            assert_eq!(host(&notes, "cursor").status, HostStatus::Added);
            assert_eq!(host(&notes, "opencode").status, HostStatus::Added);
            let cursor_raw = fs::read_to_string(&cursor).unwrap();
            assert!(cursor_raw.contains("magents"));
            assert!(cursor_raw.contains("other"));
            let opencode_raw = fs::read_to_string(&opencode).unwrap();
            assert!(opencode_raw.contains("$schema"));
            assert!(opencode_raw.contains("\"type\": \"local\""));
            assert!(home.join(".cursor/skills/magents/SKILL.md").is_file());
            assert!(
                home.join(".config/opencode/skills/magents/SKILL.md")
                    .is_file()
            );
        });
    }

    #[test]
    fn install_json_hosts_replace_existing_server() {
        with_home(|home, _bin| {
            let cursor = home.join(".cursor").join("mcp.json");
            write_json(
                &cursor,
                &json!({"mcpServers": {"magents": {"command": "old"}, "other": {"command": "x"}}}),
            )
            .unwrap();
            let opencode = home.join(".config").join("opencode").join("opencode.json");
            write_json(
                &opencode,
                &json!({"mcp": {"magents": {"type": "local", "command": ["old"]}}}),
            )
            .unwrap();
            let notes = install(false, false, false, true, true, false, false).unwrap();
            assert_eq!(host(&notes, "cursor").status, HostStatus::Replaced);
            assert_eq!(host(&notes, "opencode").status, HostStatus::Replaced);
            let cursor_raw = fs::read_to_string(&cursor).unwrap();
            assert!(cursor_raw.contains("other"));
            assert!(!cursor_raw.contains("old"));
            let opencode_raw = fs::read_to_string(&opencode).unwrap();
            assert!(!opencode_raw.contains("old"));
            assert!(opencode_raw.contains("\"enabled\": true"));
        });
    }

    #[test]
    fn already_registered_matches_host_wording() {
        assert!(super::already_registered(&Error::msg(
            "claude mcp add failed: MCP server magents already exists in user config"
        )));
        assert!(super::already_registered(&Error::msg(
            "server already registered"
        )));
        assert!(!super::already_registered(&Error::msg(
            "claude mcp add failed: boom"
        )));
    }

    #[test]
    fn install_claude_replaces_existing() {
        with_home(|home, bin| {
            test_env::write_executable(
                &bin.join("claude"),
                r#"
if [ "$1" = mcp ] && [ "$2" = remove ]; then
  : > "$HOME/.removed-magents"
  exit 0
fi
if [ -f "$HOME/.removed-magents" ]; then
  echo added magents
  exit 0
fi
echo already exists >&2
exit 1
"#,
            );
            let notes = install(true, false, false, false, false, false, false).unwrap();
            assert_eq!(host(&notes, "claude").status, HostStatus::Replaced);
            assert!(!home.join(".removed-magents").is_file());
            assert!(home.join(".claude/skills/magents/SKILL.md").is_file());
        });
    }

    #[test]
    fn install_keeps_existing_server_when_already_registered() {
        with_home(|home, bin| {
            test_env::write_executable(
                &bin.join("claude"),
                r#"
if [ "$1" = mcp ] && [ "$2" = remove ]; then
  : > "$HOME/.removed-magents"
  exit 0
fi
echo already exists >&2
exit 1
"#,
            );
            let notes = install(true, false, false, false, false, false, false).unwrap();
            assert_eq!(host(&notes, "claude").status, HostStatus::Replaced);
            assert!(!home.join(".removed-magents").is_file());
        });
    }

    #[test]
    fn install_grok_replaces_existing_from_config() {
        with_home(|home, bin| {
            test_env::write_executable(
                &bin.join("grok"),
                r#"
if [ "$1" = mcp ] && [ "$2" = remove ]; then
  : > "$HOME/.removed-magents"
  exit 0
fi
echo added magents
"#,
            );
            let grok = home.join(".grok");
            fs::create_dir_all(&grok).unwrap();
            fs::write(
                grok.join("config.toml"),
                "[mcp_servers.magents]\ncommand = \"old\"\n",
            )
            .unwrap();
            let notes = install(false, true, false, false, false, false, false).unwrap();
            assert_eq!(host(&notes, "grok").status, HostStatus::Replaced);
            assert!(!home.join(".removed-magents").is_file());
            assert!(home.join(".grok/skills/magents/SKILL.md").is_file());
        });
    }

    #[test]
    fn install_codex_replaces_existing_from_config() {
        with_home(|home, bin| {
            test_env::write_executable(
                &bin.join("codex"),
                r#"
if [ "$1" = mcp ] && [ "$2" = remove ]; then
  : > "$HOME/.removed-magents"
  exit 0
fi
echo added magents
"#,
            );
            let codex = home.join(".codex");
            fs::create_dir_all(&codex).unwrap();
            fs::write(
                codex.join("config.toml"),
                "[mcp_servers.magents]\ncommand = \"old\"\n",
            )
            .unwrap();
            let notes = install(false, false, true, false, false, false, false).unwrap();
            assert_eq!(host(&notes, "codex").status, HostStatus::Replaced);
            assert!(!home.join(".removed-magents").is_file());
        });
    }

    #[test]
    fn install_claude_replaces_existing_from_user_json() {
        with_home(|home, bin| {
            test_env::write_executable(
                &bin.join("claude"),
                r#"
if [ "$1" = mcp ] && [ "$2" = remove ]; then
  : > "$HOME/.removed-magents"
  exit 0
fi
echo added magents
"#,
            );
            write_json(
                &home.join(".claude.json"),
                &json!({"mcpServers": {"magents": {"command": "old"}}}),
            )
            .unwrap();
            let notes = install(true, false, false, false, false, false, false).unwrap();
            assert_eq!(host(&notes, "claude").status, HostStatus::Replaced);
            assert!(!home.join(".removed-magents").is_file());
        });
    }

    #[test]
    fn install_keeps_existing_server_when_upsert_add_fails() {
        with_home(|home, bin| {
            test_env::write_executable(
                &bin.join("grok"),
                r#"
if [ "$1" = mcp ] && [ "$2" = remove ]; then
  : > "$HOME/.removed-magents"
  exit 0
fi
echo boom >&2
exit 1
"#,
            );
            let grok = home.join(".grok");
            fs::create_dir_all(&grok).unwrap();
            fs::write(
                grok.join("config.toml"),
                "[mcp_servers.magents]\ncommand = \"old\"\n",
            )
            .unwrap();
            let error = install(false, true, false, false, false, false, false).unwrap_err();
            assert!(error.to_string().contains("grok mcp add failed"), "{error}");
            assert!(!home.join(".removed-magents").is_file());
        });
    }

    #[test]
    fn install_gemini_replaces_when_mcp_get_lists_server() {
        with_home(|home, bin| {
            test_env::write_executable(
                &bin.join("gemini"),
                r#"
if [ "$1" = mcp ] && [ "$2" = get ]; then
  echo 'magents:'
  exit 0
fi
if [ "$1" = mcp ] && [ "$2" = remove ]; then
  : > "$HOME/.removed-magents"
  exit 0
fi
echo added gemini magents
"#,
            );
            let notes = install(false, false, false, false, false, true, false).unwrap();
            assert_eq!(host(&notes, "gemini").status, HostStatus::Replaced);
            assert!(!home.join(".removed-magents").is_file());
        });
    }

    #[test]
    fn mcp_get_exists_requires_a_magents_listing() {
        with_home(|_home, bin| {
            test_env::write_executable(&bin.join("gemini"), "echo other; exit 0");
            assert!(!mcp_get_exists("gemini"));
            test_env::write_executable(&bin.join("gemini"), "echo magents; exit 0");
            assert!(mcp_get_exists("gemini"));
            test_env::write_executable(&bin.join("gemini"), "echo magents listed >&2; exit 0");
            assert!(mcp_get_exists("gemini"));
            test_env::write_executable(&bin.join("gemini"), "echo magents; exit 1");
            assert!(!mcp_get_exists("gemini"));
            assert!(!mcp_get_exists("definitely-not-a-host"));
        });
    }

    #[test]
    fn json_top_level_mcp_covers_missing_invalid_and_present() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!json_top_level_mcp(
            &dir.path().join("nope.json"),
            "magents"
        ));
        let invalid = dir.path().join("bad.json");
        fs::write(&invalid, "[1]\n").unwrap();
        assert!(!json_top_level_mcp(&invalid, "magents"));
        let empty_servers = dir.path().join("empty.json");
        write_json(&empty_servers, &json!({"mcpServers": {}})).unwrap();
        assert!(!json_top_level_mcp(&empty_servers, "magents"));
        let not_object = dir.path().join("list.json");
        write_json(&not_object, &json!({"mcpServers": []})).unwrap();
        assert!(!json_top_level_mcp(&not_object, "magents"));
        let present = dir.path().join("ok.json");
        write_json(
            &present,
            &json!({"mcpServers": {"magents": {"command": "x"}}}),
        )
        .unwrap();
        assert!(json_top_level_mcp(&present, "magents"));
        let no_key = dir.path().join("none.json");
        write_json(&no_key, &json!({})).unwrap();
        assert!(!json_top_level_mcp(&no_key, "magents"));
    }

    #[test]
    fn cli_has_magents_is_false_when_hosts_have_no_server() {
        with_home(|_home, _bin| {
            let homes = Homes::from_env();
            assert!(!cli_has_magents(&homes, "grok"));
            assert!(!cli_has_magents(&homes, "codex"));
            assert!(!cli_has_magents(&homes, "claude"));
            assert!(!cli_has_magents(&homes, "gemini"));
        });
    }

    #[test]
    fn install_cli_hosts_with_stubs() {
        with_home(|home, bin| {
            test_env::write_executable(&bin.join("grok"), "echo added magents");
            test_env::write_executable(&bin.join("claude"), "echo added magents");
            test_env::write_executable(&bin.join("codex"), "exit 0");
            let notes = install(true, true, true, false, false, false, false).unwrap();
            assert_eq!(host(&notes, "grok").status, HostStatus::Added);
            assert_eq!(host(&notes, "claude").status, HostStatus::Added);
            assert_eq!(host(&notes, "codex").status, HostStatus::Added);
            assert!(home.join(".grok/skills/magents/SKILL.md").is_file());
            assert!(home.join(".claude/skills/magents/SKILL.md").is_file());
        });
    }

    #[test]
    fn install_gemini_and_copilot_with_stubs() {
        with_home(|home, bin| {
            test_env::write_executable(&bin.join("gemini"), "echo added gemini magents");
            test_env::write_executable(&bin.join("copilot"), "echo added copilot magents");
            let notes = install(false, false, false, false, false, true, true).unwrap();
            assert_eq!(host(&notes, "gemini").status, HostStatus::Added);
            assert_eq!(host(&notes, "copilot").status, HostStatus::Added);
            assert!(home.join(".gemini/skills/magents/SKILL.md").is_file());
            assert!(home.join(".copilot/skills/magents/SKILL.md").is_file());
        });
    }

    #[test]
    fn install_cli_host_missing_binary() {
        with_home(|_home, _bin| {
            let error = install(false, true, false, false, false, false, false).unwrap_err();
            assert!(error.to_string().contains("grok not found"));
        });
    }

    #[test]
    fn install_all_skips_missing_gemini_and_copilot() {
        with_home(|home, bin| {
            test_env::write_executable(&bin.join("grok"), "echo added magents");
            test_env::write_executable(&bin.join("claude"), "echo added magents");
            test_env::write_executable(&bin.join("codex"), "exit 0");
            let notes = install_spec(InstallSpec {
                claude: true,
                grok: true,
                codex: true,
                cursor: true,
                opencode: true,
                gemini: true,
                copilot: true,
                skip_missing: true,
            })
            .unwrap();
            assert_eq!(host(&notes, "grok").status, HostStatus::Added);
            assert_eq!(host(&notes, "gemini").status, HostStatus::Skipped);
            assert_eq!(host(&notes, "gemini").detail, "not installed");
            assert_eq!(host(&notes, "copilot").status, HostStatus::Skipped);
            assert!(!host(&notes, "gemini").to_string().contains("os error"));
            assert!(home.join(".grok/skills/magents/SKILL.md").is_file());
            assert!(!home.join(".gemini/skills/magents/SKILL.md").is_file());
            assert!(!home.join(".copilot/skills/magents/SKILL.md").is_file());
        });
    }

    #[test]
    fn install_spec_with_emits_start_and_finish() {
        with_home(|home, _bin| {
            let mut events = Vec::new();
            let notes = install_spec_with(
                InstallSpec {
                    cursor: true,
                    ..InstallSpec::default()
                },
                |event| events.push(event),
            )
            .unwrap();
            assert_eq!(host(&notes, "cursor").status, HostStatus::Added);
            assert_eq!(
                events,
                vec![
                    InstallEvent::Started { host: "cursor" },
                    InstallEvent::Finished {
                        result: HostInstall {
                            host: "cursor",
                            status: HostStatus::Added,
                            detail: String::new(),
                        }
                    }
                ]
            );
            assert!(home.join(".cursor/skills/magents/SKILL.md").is_file());
        });
    }

    #[test]
    fn install_gemini_and_copilot_skills_follow_home_overrides() {
        with_home(|home, bin| {
            test_env::write_executable(&bin.join("gemini"), "echo added gemini magents");
            test_env::write_executable(&bin.join("copilot"), "echo added copilot magents");
            unsafe {
                std::env::set_var("GEMINI_CLI_HOME", home.join("custom-gemini"));
                std::env::set_var("COPILOT_HOME", home.join("custom-copilot"));
            }
            let notes = install(false, false, false, false, false, true, true).unwrap();
            assert_eq!(host(&notes, "gemini").status, HostStatus::Added);
            assert!(home.join("custom-gemini/skills/magents/SKILL.md").is_file());
            assert!(
                home.join("custom-copilot/skills/magents/SKILL.md")
                    .is_file()
            );
            assert!(!home.join(".gemini/skills/magents/SKILL.md").is_file());
            assert!(!home.join(".copilot/skills/magents/SKILL.md").is_file());
        });
    }

    #[test]
    fn install_all_does_not_skip_host_stderr_not_found() {
        with_home(|_home, bin| {
            test_env::write_executable(
                &bin.join("gemini"),
                "echo 'config not found: magents' >&2; exit 1",
            );
            let error = install_spec(InstallSpec {
                gemini: true,
                skip_missing: true,
                ..InstallSpec::default()
            })
            .unwrap_err();
            assert!(error.to_string().contains("mcp add failed"), "{error}");
            assert!(
                error.to_string().contains("config not found: magents"),
                "{error}"
            );
        });
    }

    #[test]
    fn install_all_does_not_skip_unexecutable_host() {
        with_home(|_home, bin| {
            let gemini = bin.join("gemini");
            fs::write(&gemini, "#!/bin/sh\necho added\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut permissions = fs::metadata(&gemini).unwrap().permissions();
                permissions.set_mode(0o644);
                fs::set_permissions(&gemini, permissions).unwrap();
            }
            let error = install_spec(InstallSpec {
                gemini: true,
                skip_missing: true,
                ..InstallSpec::default()
            })
            .unwrap_err();
            assert!(error.to_string().contains("mcp add failed"), "{error}");
            assert!(!error.to_string().contains("skipped"), "{error}");
        });
    }

    #[test]
    fn install_all_still_fails_on_host_error() {
        with_home(|_home, bin| {
            test_env::write_executable(&bin.join("claude"), "echo boom >&2; exit 1");
            let error = install_spec(InstallSpec {
                claude: true,
                skip_missing: true,
                ..InstallSpec::default()
            })
            .unwrap_err();
            assert!(error.to_string().contains("claude mcp add failed"));
        });
    }

    #[test]
    fn install_cli_host_failure() {
        with_home(|_home, bin| {
            test_env::write_executable(&bin.join("claude"), "echo boom >&2; exit 1");
            let error = install(true, false, false, false, false, false, false).unwrap_err();
            assert!(error.to_string().contains("claude mcp add failed"));
        });
    }

    #[test]
    fn toml_has_keys_covers_missing_invalid_and_nested() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope.toml");
        assert!(!toml_has_keys(&missing, &["mcp_servers", "magents"]));
        let invalid = dir.path().join("bad.toml");
        fs::write(&invalid, "[[[not toml").unwrap();
        assert!(!toml_has_keys(&invalid, &["mcp_servers", "magents"]));
        let empty = dir.path().join("empty.toml");
        fs::write(&empty, "[cli]\ninstaller = \"npm\"\n").unwrap();
        assert!(!toml_has_keys(&empty, &["mcp_servers", "magents"]));
        let present = dir.path().join("ok.toml");
        fs::write(&present, "[mcp_servers.magents]\ncommand = \"magents\"\n").unwrap();
        assert!(toml_has_keys(&present, &["mcp_servers", "magents"]));
        assert!(!toml_has_keys(&present, &["mcp_servers", "other"]));
    }

    #[test]
    fn json_helpers_cover_empty_invalid_and_merge() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope.json");
        assert!(read_json_object(&missing).unwrap().is_empty());
        let empty = dir.path().join("empty.json");
        fs::write(&empty, "  \n").unwrap();
        assert!(read_json_object(&empty).unwrap().is_empty());
        let array = dir.path().join("array.json");
        fs::write(&array, "[1]\n").unwrap();
        let error = read_json_object(&array).unwrap_err();
        assert!(error.to_string().contains("not a JSON object"));
        write_skill(dir.path().join("nested").join("SKILL.md")).unwrap();
        assert!(dir.path().join("nested").join("SKILL.md").is_file());
        write_json(
            &dir.path().join("ok.json"),
            &Value::Object(Default::default()),
        )
        .unwrap();
    }

    #[test]
    fn cursor_rejects_non_object_servers() {
        with_home(|home, _bin| {
            let path = home.join(".cursor").join("mcp.json");
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, r#"{"mcpServers":[]}"#).unwrap();
            let error = install(false, false, false, true, false, false, false).unwrap_err();
            assert!(error.to_string().contains("mcpServers must be an object"));
        });
    }

    #[test]
    fn json_write_fails_when_parent_is_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("blocked");
        fs::write(&parent, "file").unwrap();
        let error = write_json(&parent.join("mcp.json"), &json!({})).unwrap_err();
        assert!(error.to_string().contains("failed to read"));
        let error = write_skill(parent.join("SKILL.md")).unwrap_err();
        assert!(error.to_string().contains("failed to read"));
    }

    #[test]
    fn opencode_rejects_non_object_mcp() {
        with_home(|home, _bin| {
            let path = home.join(".config").join("opencode").join("opencode.json");
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, r#"{"mcp":[]}"#).unwrap();
            let error = install(false, false, false, false, true, false, false).unwrap_err();
            assert!(error.to_string().contains("mcp must be an object"));
        });
    }

    #[test]
    fn json_and_replace_error_paths() {
        let dir = tempfile::tempdir().unwrap();
        let unreadable = dir.path().join("secret.json");
        fs::write(&unreadable, r#"{"ok":true}"#).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(&unreadable).unwrap().permissions();
            permissions.set_mode(0o000);
            fs::set_permissions(&unreadable, permissions).unwrap();
            assert!(read_json_object(&unreadable).is_err());
            let mut permissions = fs::metadata(&unreadable).unwrap().permissions();
            permissions.set_mode(0o644);
            fs::set_permissions(&unreadable, permissions).unwrap();
        }

        let dest = dir.path().join("as-dir.json");
        fs::create_dir_all(&dest).unwrap();
        assert!(write_json(&dest, &json!({})).is_err());
        let skill = dir.path().join("skill-dir");
        fs::create_dir_all(&skill).unwrap();
        assert!(write_skill(skill).is_err());
        let blocked = dir.path().join("blocked-parent");
        fs::write(&blocked, "not a directory").unwrap();
        assert!(write_json(&blocked.join("x.json"), &json!({})).is_err());
        assert!(write_skill(blocked.join("SKILL.md")).is_err());

        with_home(|home, bin| {
            test_env::write_executable(&bin.join("grok"), "echo added magents");
            fs::create_dir_all(home.join(".grok")).unwrap();
            fs::write(home.join(".grok").join("skills"), "not-a-dir").unwrap();
            assert!(install(false, true, false, false, false, false, false).is_err());
        });
    }
}
