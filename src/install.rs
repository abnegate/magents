use crate::error::{Error, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const SKILL: &str = include_str!("../skills/magents.md");

pub fn install(claude: bool, grok: bool, codex: bool) -> Result<Vec<String>> {
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
