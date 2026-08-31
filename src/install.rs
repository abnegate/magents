use crate::error::{Error, Result};
use serde_json::{Map, Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const SKILL: &str = include_str!("../skills/magents.md");

pub fn install(
    claude: bool,
    grok: bool,
    codex: bool,
    cursor: bool,
    opencode: bool,
) -> Result<Vec<String>> {
    let exe = std::env::current_exe().map_err(|source| Error::Io {
        path: PathBuf::from("magents"),
        source,
    })?;
    let mut notes = Vec::new();
    if grok {
        notes.push(install_grok(&exe)?);
        notes.push(write_skill(
            dirs::home_dir()
                .unwrap_or_default()
                .join(".grok")
                .join("skills")
                .join("magents")
                .join("SKILL.md"),
        )?);
    }
    if claude {
        notes.push(install_claude(&exe)?);
        notes.push(write_skill(
            dirs::home_dir()
                .unwrap_or_default()
                .join(".claude")
                .join("skills")
                .join("magents")
                .join("SKILL.md"),
        )?);
    }
    if codex {
        notes.push(install_codex(&exe)?);
    }
    if cursor {
        notes.push(install_cursor(&exe)?);
        notes.push(write_skill(
            dirs::home_dir()
                .unwrap_or_default()
                .join(".cursor")
                .join("skills")
                .join("magents")
                .join("SKILL.md"),
        )?);
    }
    if opencode {
        notes.push(install_opencode(&exe)?);
        notes.push(write_skill(
            dirs::home_dir()
                .unwrap_or_default()
                .join(".config")
                .join("opencode")
                .join("skills")
                .join("magents")
                .join("SKILL.md"),
        )?);
    }
    Ok(notes)
}

fn install_grok(exe: &Path) -> Result<String> {
    run(
        "grok",
        &[
            "mcp",
            "add",
            "magents",
            "--",
            exe.to_str().unwrap_or("magents"),
            "mcp",
        ],
    )
}

fn install_claude(exe: &Path) -> Result<String> {
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
    add_or_replace(
        "claude",
        &add,
        &["mcp", "remove", "--scope", "user", "magents"],
    )
}

fn install_codex(exe: &Path) -> Result<String> {
    let add = [
        "mcp",
        "add",
        "magents",
        "--",
        exe.to_str().unwrap_or("magents"),
        "mcp",
    ];
    add_or_replace("codex", &add, &["mcp", "remove", "magents"])
}

fn add_or_replace(program: &str, add: &[&str], remove: &[&str]) -> Result<String> {
    match run(program, add) {
        Ok(note) => Ok(note),
        Err(error) if already_registered(&error) => {
            let _ = Command::new(program).args(remove).output();
            match run(program, add) {
                Ok(note) => Ok(note),
                Err(add_error) => {
                    let _ = run(program, add);
                    Err(add_error)
                }
            }
        }
        Err(error) => Err(error),
    }
}

fn already_registered(error: &Error) -> bool {
    let text = error.to_string().to_ascii_lowercase();
    text.contains("already exists") || text.contains("already registered")
}

fn install_cursor(exe: &Path) -> Result<String> {
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
    servers.insert(
        "magents".into(),
        json!({
            "command": exe.to_str().unwrap_or("magents"),
            "args": ["mcp"],
        }),
    );
    write_json(&path, &Value::Object(root))?;
    Ok(format!("wrote {}", path.display()))
}

fn install_opencode(exe: &Path) -> Result<String> {
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
    mcp.insert(
        "magents".into(),
        json!({
            "type": "local",
            "command": [exe.to_str().unwrap_or("magents"), "mcp"],
            "enabled": true,
        }),
    );
    write_json(&path, &Value::Object(root))?;
    Ok(format!("wrote {}", path.display()))
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

fn write_skill(path: PathBuf) -> Result<String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| Error::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    fs::write(&path, SKILL).map_err(|source| Error::Io {
        path: path.clone(),
        source,
    })?;
    Ok(format!("wrote skill {}", path.display()))
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
        Err(error) => Err(Error::msg(format!("{program} not found: {error}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::{install, read_json_object, write_json, write_skill};
    use crate::error::Error;
    use crate::test_env;
    use serde_json::{Value, json};
    use std::fs;
    use std::path::Path;

    const ENV: &[&str] = &["HOME", "PATH"];

    fn with_home(run: impl FnOnce(&Path, &Path)) {
        let _guard = test_env::lock(ENV);
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let bin = home.join("bin");
        fs::create_dir_all(&bin).unwrap();
        unsafe {
            std::env::set_var("HOME", home);
            std::env::set_var("PATH", &bin);
        }
        run(home, &bin);
    }

    #[test]
    fn install_none_is_empty() {
        let notes = install(false, false, false, false, false).unwrap();
        assert!(notes.is_empty());
    }

    #[test]
    fn install_cursor_and_opencode_merge_json() {
        with_home(|home, _bin| {
            let cursor = home.join(".cursor").join("mcp.json");
            write_json(&cursor, &json!({"mcpServers": {"other": {"command": "x"}}})).unwrap();
            let opencode = home.join(".config").join("opencode").join("opencode.json");
            fs::create_dir_all(opencode.parent().unwrap()).unwrap();
            fs::write(&opencode, "{}\n").unwrap();
            let notes = install(false, false, false, true, true).unwrap();
            assert!(notes.iter().any(|note| note.contains("mcp.json")));
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
            let notes = install(true, false, false, false, false).unwrap();
            assert!(
                notes.iter().any(|note| note.contains("added magents")),
                "{notes:?}"
            );
            assert!(home.join(".claude/skills/magents/SKILL.md").is_file());
        });
    }

    #[test]
    fn install_cli_hosts_with_stubs() {
        with_home(|home, bin| {
            test_env::write_executable(&bin.join("grok"), "echo added magents");
            test_env::write_executable(&bin.join("claude"), "echo added magents");
            test_env::write_executable(&bin.join("codex"), "exit 0");
            let notes = install(true, true, true, false, false).unwrap();
            assert!(notes.iter().any(|note| note.contains("added magents")));
            assert!(
                notes
                    .iter()
                    .any(|note| note.contains("codex mcp add magents"))
            );
            assert!(home.join(".grok/skills/magents/SKILL.md").is_file());
            assert!(home.join(".claude/skills/magents/SKILL.md").is_file());
        });
    }

    #[test]
    fn install_cli_host_missing_binary() {
        with_home(|_home, _bin| {
            let error = install(false, true, false, false, false).unwrap_err();
            assert!(error.to_string().contains("grok not found"));
        });
    }

    #[test]
    fn install_cli_host_failure() {
        with_home(|_home, bin| {
            test_env::write_executable(&bin.join("claude"), "echo boom >&2; exit 1");
            let error = install(true, false, false, false, false).unwrap_err();
            assert!(error.to_string().contains("claude mcp add failed"));
        });
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
        let skill = write_skill(dir.path().join("nested").join("SKILL.md")).unwrap();
        assert!(skill.contains("SKILL.md"));
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
            let error = install(false, false, false, true, false).unwrap_err();
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
            let error = install(false, false, false, false, true).unwrap_err();
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

        with_home(|home, bin| {
            test_env::write_executable(
                &bin.join("claude"),
                r#"
echo already exists >&2
exit 1
"#,
            );
            let error = install(true, false, false, false, false).unwrap_err();
            assert!(
                error.to_string().contains("already exists")
                    || error.to_string().contains("failed")
            );

            test_env::write_executable(&bin.join("grok"), "echo added magents");
            fs::create_dir_all(home.join(".grok")).unwrap();
            fs::write(home.join(".grok").join("skills"), "not-a-dir").unwrap();
            assert!(install(false, true, false, false, false).is_err());
        });
    }
}
