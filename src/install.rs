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
    run(
        "claude",
        &[
            "mcp",
            "add",
            "--scope",
            "user",
            "magents",
            "--",
            exe.to_str().unwrap_or("magents"),
            "mcp",
        ],
    )
}

fn install_codex(exe: &Path) -> Result<String> {
    run(
        "codex",
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
