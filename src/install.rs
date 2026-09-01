use crate::error::{Error, Result};
use crate::homes::Homes;
use serde_json::{Map, Value, json};
use std::fmt;
use std::fs;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::Command;
use toml_edit::{Array, DocumentMut, value};

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
    add_or_replace_known(cli_has_magents(homes, "grok"), "grok", &add, || {
        upsert_toml_mcp(&homes.grok.join("config.toml"), exe)
    })
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
    add_or_replace_known(cli_has_magents(homes, "claude"), "claude", &add, || {
        upsert_json_mcp(
            &dirs::home_dir().unwrap_or_default().join(".claude.json"),
            "mcpServers",
            stdio_server(exe, None),
        )
    })
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
    add_or_replace_known(cli_has_magents(homes, "codex"), "codex", &add, || {
        upsert_toml_mcp(&homes.codex.join("config.toml"), exe)
    })
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
    add_or_replace_known(cli_has_magents(homes, "gemini"), "gemini", &add, || {
        upsert_json_mcp(
            &homes.gemini.join("settings.json"),
            "mcpServers",
            stdio_server(exe, None),
        )
    })
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
    add_or_replace_known(cli_has_magents(homes, "copilot"), "copilot", &add, || {
        upsert_json_mcp(
            &homes.copilot.join("mcp-config.json"),
            "mcpServers",
            stdio_server(exe, Some("local")),
        )
    })
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
        "gemini" => {
            json_top_level_mcp(&homes.gemini.join("settings.json"), "magents")
                || mcp_get_exists(program)
        }
        "copilot" => {
            json_top_level_mcp(&homes.copilot.join("mcp-config.json"), "magents")
                || mcp_get_exists(program)
        }
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

fn add_or_replace_known(
    existed: bool,
    program: &str,
    add: &[&str],
    upsert: impl FnOnce() -> Result<()>,
) -> Result<HostStatus> {
    match run(program, add) {
        Ok(_) => Ok(if existed {
            HostStatus::Replaced
        } else {
            HostStatus::Added
        }),
        Err(error) if already_registered(&error) => {
            upsert()?;
            Ok(HostStatus::Replaced)
        }
        Err(error) => Err(error),
    }
}

fn stdio_server(exe: &Path, kind: Option<&str>) -> Value {
    let command = exe.to_str().unwrap_or("magents");
    match kind {
        Some(kind) => json!({
            "type": kind,
            "command": command,
            "args": ["mcp"],
            "tools": ["*"],
        }),
        None => json!({
            "command": command,
            "args": ["mcp"],
        }),
    }
}

fn upsert_json_mcp(path: &Path, servers_key: &str, server: Value) -> Result<()> {
    with_config_update(path, |raw| {
        let mut root = json_object_from_bytes(path, raw)?;
        let servers = root
            .entry(servers_key)
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .ok_or_else(|| {
                Error::msg(format!(
                    "{} {servers_key} must be an object",
                    path.display()
                ))
            })?;
        upsert_object_key(servers, "magents", server.clone());
        Ok((pretty_json_bytes(&Value::Object(root))?, ()))
    })
}

fn upsert_toml_mcp(path: &Path, exe: &Path) -> Result<()> {
    let command = exe.to_str().unwrap_or("magents").to_string();
    with_config_update(path, |raw| {
        let mut doc = toml_from_bytes(path, raw)?;
        doc["mcp_servers"]["magents"]["command"] = value(command.as_str());
        doc["mcp_servers"]["magents"]["args"] = value(Array::from_iter(["mcp"]));
        Ok((doc.to_string().into_bytes(), ()))
    })
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
    let command = exe.to_str().unwrap_or("magents").to_string();
    with_config_update(&path, |raw| {
        let mut root = json_object_from_bytes(&path, raw)?;
        let servers = root
            .entry("mcpServers")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .ok_or_else(|| Error::msg("cursor mcp.json mcpServers must be an object"))?;
        let status = upsert_object_key(
            servers,
            "magents",
            json!({
                "command": command,
                "args": ["mcp"],
            }),
        );
        Ok((pretty_json_bytes(&Value::Object(root))?, status))
    })
}

fn install_opencode(exe: &Path) -> Result<HostStatus> {
    let path = dirs::home_dir()
        .unwrap_or_default()
        .join(".config")
        .join("opencode")
        .join("opencode.json");
    let command = exe.to_str().unwrap_or("magents").to_string();
    with_config_update(&path, |raw| {
        let mut root = json_object_from_bytes(&path, raw)?;
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
                "command": [command, "mcp"],
                "enabled": true,
            }),
        );
        Ok((pretty_json_bytes(&Value::Object(root))?, status))
    })
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
    json_object_from_bytes(path, &read_bytes(path)?)
}

fn json_object_from_bytes(path: &Path, raw: &[u8]) -> Result<Map<String, Value>> {
    if raw.is_empty() || raw.trim_ascii().is_empty() {
        return Ok(Map::new());
    }
    let value: Value = serde_json::from_slice(raw)?;
    match value {
        Value::Object(map) => Ok(map),
        _ => Err(Error::msg(format!(
            "{} is not a JSON object",
            path.display()
        ))),
    }
}

fn toml_from_bytes(path: &Path, raw: &[u8]) -> Result<DocumentMut> {
    if raw.is_empty() || raw.trim_ascii().is_empty() {
        return Ok(DocumentMut::new());
    }
    let text = std::str::from_utf8(raw)
        .map_err(|error| Error::msg(format!("{} is not valid TOML: {error}", path.display())))?;
    text.parse::<DocumentMut>()
        .map_err(|error| Error::msg(format!("{} is not valid TOML: {error}", path.display())))
}

fn pretty_json_bytes(value: &Value) -> Result<Vec<u8>> {
    let raw = serde_json::to_string_pretty(value)?;
    Ok(format!("{raw}\n").into_bytes())
}

#[cfg(test)]
fn write_json(path: &Path, value: &Value) -> Result<()> {
    write_atomic(path, pretty_json_bytes(value)?)
}

fn read_bytes(path: &Path) -> Result<Vec<u8>> {
    if !path.is_file() {
        return Ok(Vec::new());
    }
    fs::read(path).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
fn write_atomic(path: &Path, contents: impl AsRef<[u8]>) -> Result<()> {
    let tmp = write_tmp(path, contents)?;
    rename_tmp(tmp, path)
}

fn write_atomic_if_unchanged(
    path: &Path,
    expected: &[u8],
    contents: impl AsRef<[u8]>,
) -> Result<bool> {
    let tmp = write_tmp(path, contents)?;
    let result = publish_if_unchanged(path, expected, &tmp);
    let _ = fs::remove_file(&tmp);
    result
}

fn stamp_path(path: &Path) -> PathBuf {
    path.with_file_name(format!(
        ".{}.stamp",
        path.file_name().unwrap_or_default().to_string_lossy()
    ))
}

fn recover_live_file(path: &Path) -> Result<()> {
    let stamp = stamp_path(path);
    if path.is_file() || !stamp.is_file() {
        return Ok(());
    }
    match fs::hard_link(&stamp, path) {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(source) => Err(Error::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn link_or_conflict(from: &Path, to: &Path) -> Result<bool> {
    match fs::hard_link(from, to) {
        Ok(()) => Ok(true),
        Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists && to.is_file() => {
            Ok(false)
        }
        Err(source) => Err(Error::Io {
            path: to.to_path_buf(),
            source,
        }),
    }
}

fn aside_path(path: &Path) -> PathBuf {
    path.with_file_name(format!(
        ".{}.{}.{}.aside",
        path.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id(),
        uuid::Uuid::new_v4().as_simple()
    ))
}

fn same_inode(left: &Path, right: &Path) -> Result<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let left_meta = fs::metadata(left).map_err(|source| Error::Io {
            path: left.to_path_buf(),
            source,
        })?;
        let right_meta = fs::metadata(right).map_err(|source| Error::Io {
            path: right.to_path_buf(),
            source,
        })?;
        Ok(left_meta.dev() == right_meta.dev() && left_meta.ino() == right_meta.ino())
    }
    #[cfg(not(unix))]
    {
        Ok(read_bytes(left)? == read_bytes(right)?)
    }
}

fn restore_path(from: &Path, to: &Path) {
    if !to.is_file() {
        let _ = fs::rename(from, to);
    }
}

fn publish_if_unchanged(path: &Path, expected: &[u8], tmp: &Path) -> Result<bool> {
    publish_after_unlink(path, expected, tmp, &|_| Ok(()), &|_| Ok(()), &|_| Ok(()))
}

fn publish_after_unlink(
    path: &Path,
    expected: &[u8],
    tmp: &Path,
    after_compare: &dyn Fn(&Path) -> Result<()>,
    after_aside: &dyn Fn(&Path) -> Result<()>,
    after_link: &dyn Fn(&Path) -> Result<()>,
) -> Result<bool> {
    recover_live_file(path)?;
    let stamp = stamp_path(path);
    let _ = fs::remove_file(&stamp);
    let aside = aside_path(path);
    if path.is_file() {
        fs::hard_link(path, &stamp).map_err(|source| Error::Io {
            path: stamp.clone(),
            source,
        })?;
        if read_bytes(path)? != expected {
            let _ = fs::remove_file(&stamp);
            return Ok(false);
        }
        after_compare(path)?;
        match fs::rename(path, &aside) {
            Ok(()) => {}
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                let _ = fs::remove_file(&stamp);
                return Ok(false);
            }
            Err(source) => {
                let _ = fs::remove_file(&stamp);
                return Err(Error::Io {
                    path: path.to_path_buf(),
                    source,
                });
            }
        }
        if !same_inode(&aside, &stamp)? {
            restore_path(&aside, path);
            let _ = fs::remove_file(&stamp);
            let _ = fs::remove_file(&aside);
            return Ok(false);
        }
    } else if !expected.is_empty() {
        return Ok(false);
    }
    after_aside(&stamp)?;
    match link_or_conflict(tmp, path) {
        Ok(true) => {
            after_link(path)?;
            if stamp.is_file() && read_bytes(&stamp)? != expected {
                if same_inode(path, tmp).unwrap_or(false) {
                    let _ = fs::remove_file(path);
                    let _ = fs::hard_link(&stamp, path);
                }
                let _ = fs::remove_file(&stamp);
                let _ = fs::remove_file(&aside);
                return Ok(false);
            }
            let _ = fs::remove_file(&stamp);
            let _ = fs::remove_file(&aside);
            Ok(true)
        }
        Ok(false) => {
            let _ = fs::remove_file(&stamp);
            let _ = fs::remove_file(&aside);
            Ok(false)
        }
        Err(error) => {
            if !path.is_file() && stamp.is_file() {
                let _ = fs::hard_link(&stamp, path);
            }
            let _ = fs::remove_file(&stamp);
            let _ = fs::remove_file(&aside);
            Err(error)
        }
    }
}

fn lock_exclusive(file: &File, path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
        if rc != 0 {
            return Err(Error::Io {
                path: path.to_path_buf(),
                source: std::io::Error::last_os_error(),
            });
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (file, path);
    }
    Ok(())
}

fn unlock(file: &File) {
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        unsafe {
            libc::flock(file.as_raw_fd(), libc::LOCK_UN);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = file;
    }
}

fn write_tmp(path: &Path, contents: impl AsRef<[u8]>) -> Result<PathBuf> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| Error::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let tmp = atomic_tmp(path);
    if let Err(source) = fs::write(&tmp, contents) {
        let _ = fs::remove_file(&tmp);
        return Err(Error::Io { path: tmp, source });
    }
    Ok(tmp)
}

#[cfg(test)]
fn rename_tmp(tmp: PathBuf, path: &Path) -> Result<()> {
    if let Err(source) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(Error::Io {
            path: path.to_path_buf(),
            source,
        });
    }
    Ok(())
}

fn atomic_tmp(path: &Path) -> PathBuf {
    path.with_file_name(format!(
        ".{}.{}.{}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id(),
        uuid::Uuid::new_v4().as_simple()
    ))
}

fn lock_path(path: &Path) -> PathBuf {
    path.with_file_name(format!(
        ".{}.lock",
        path.file_name().unwrap_or_default().to_string_lossy()
    ))
}

struct FileLock(File);

impl FileLock {
    fn exclusive(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| Error::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(path)
            .map_err(|source| Error::Io {
                path: path.to_path_buf(),
                source,
            })?;
        lock_exclusive(&file, path)?;
        Ok(Self(file))
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        unlock(&self.0);
    }
}

fn with_file_lock<T>(path: &Path, work: impl FnOnce() -> Result<T>) -> Result<T> {
    let _lock = FileLock::exclusive(&lock_path(path))?;
    work()
}

const CONFIG_WRITE_ATTEMPTS: u32 = 8;

fn with_config_update<T>(
    path: &Path,
    mut update: impl FnMut(&[u8]) -> Result<(Vec<u8>, T)>,
) -> Result<T> {
    with_file_lock(path, || {
        recover_live_file(path)?;
        for _ in 0..CONFIG_WRITE_ATTEMPTS {
            let before = read_bytes(path)?;
            let (next, out) = update(&before)?;
            if next == before {
                return Ok(out);
            }
            if write_atomic_if_unchanged(path, &before, &next)? {
                return Ok(out);
            }
        }
        Err(Error::msg(format!(
            "{} changed while installing magents; try again",
            path.display()
        )))
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
            let claude = fs::read_to_string(home.join(".claude.json")).unwrap();
            assert!(claude.contains("\"args\""));
            assert!(claude.contains("mcp"));
            assert!(home.join(".claude/skills/magents/SKILL.md").is_file());
        });
    }

    #[test]
    fn install_updates_stale_registration_when_already_registered() {
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
            write_json(
                &home.join(".claude.json"),
                &json!({"mcpServers": {"magents": {"command": "old"}, "other": {"command": "x"}}}),
            )
            .unwrap();
            let notes = install(true, false, false, false, false, false, false).unwrap();
            assert_eq!(host(&notes, "claude").status, HostStatus::Replaced);
            assert!(!home.join(".removed-magents").is_file());
            let claude = fs::read_to_string(home.join(".claude.json")).unwrap();
            assert!(!claude.contains("old"));
            assert!(claude.contains("other"));
            assert!(claude.contains("mcp"));
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
    fn install_updates_stale_toml_when_already_registered() {
        with_home(|home, bin| {
            test_env::write_executable(
                &bin.join("grok"),
                r#"
if [ "$1" = mcp ] && [ "$2" = remove ]; then
  : > "$HOME/.removed-magents"
  exit 0
fi
echo already registered >&2
exit 1
"#,
            );
            let grok = home.join(".grok");
            fs::create_dir_all(&grok).unwrap();
            fs::write(
                grok.join("config.toml"),
                "[mcp_servers.magents]\ncommand = \"old\"\nenv = \"keep\"\n",
            )
            .unwrap();
            let notes = install(false, true, false, false, false, false, false).unwrap();
            assert_eq!(host(&notes, "grok").status, HostStatus::Replaced);
            assert!(!home.join(".removed-magents").is_file());
            let toml = fs::read_to_string(grok.join("config.toml")).unwrap();
            assert!(!toml.contains("old"));
            assert!(toml.contains("keep"));
            assert!(toml.contains("mcp"));
        });
    }

    #[test]
    fn install_updates_gemini_and_copilot_files_when_already_registered() {
        with_home(|home, bin| {
            test_env::write_executable(
                &bin.join("gemini"),
                r#"
if [ "$1" = mcp ] && [ "$2" = remove ]; then
  : > "$HOME/.removed-gemini"
  exit 0
fi
echo already exists >&2
exit 1
"#,
            );
            test_env::write_executable(
                &bin.join("copilot"),
                r#"
if [ "$1" = mcp ] && [ "$2" = remove ]; then
  : > "$HOME/.removed-copilot"
  exit 0
fi
echo already exists >&2
exit 1
"#,
            );
            write_json(
                &home.join(".gemini").join("settings.json"),
                &json!({"mcpServers": {"magents": {"command": "old-gemini"}, "other": {}}}),
            )
            .unwrap();
            write_json(
                &home.join(".copilot").join("mcp-config.json"),
                &json!({"mcpServers": {"magents": {"command": "old-copilot"}}}),
            )
            .unwrap();
            let notes = install(false, false, false, false, false, true, true).unwrap();
            assert_eq!(host(&notes, "gemini").status, HostStatus::Replaced);
            assert_eq!(host(&notes, "copilot").status, HostStatus::Replaced);
            assert!(!home.join(".removed-gemini").is_file());
            assert!(!home.join(".removed-copilot").is_file());
            let gemini = fs::read_to_string(home.join(".gemini/settings.json")).unwrap();
            assert!(!gemini.contains("old-gemini"));
            assert!(gemini.contains("other"));
            let copilot = fs::read_to_string(home.join(".copilot/mcp-config.json")).unwrap();
            assert!(!copilot.contains("old-copilot"));
            assert!(copilot.contains("\"type\": \"local\""));
        });
    }

    #[test]
    fn install_keeps_existing_server_when_already_registered_upsert_fails() {
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
            fs::create_dir_all(home.join(".claude.json")).unwrap();
            let error = install(true, false, false, false, false, false, false).unwrap_err();
            assert!(error.to_string().contains("failed to read"), "{error}");
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
            assert!(!cli_has_magents(&homes, "copilot"));
        });
    }

    #[test]
    fn cli_has_magents_reads_gemini_and_copilot_files() {
        with_home(|home, _bin| {
            write_json(
                &home.join(".gemini").join("settings.json"),
                &json!({"mcpServers": {"magents": {"command": "x"}}}),
            )
            .unwrap();
            write_json(
                &home.join(".copilot").join("mcp-config.json"),
                &json!({"mcpServers": {"magents": {"command": "x"}}}),
            )
            .unwrap();
            let homes = Homes::from_env();
            assert!(cli_has_magents(&homes, "gemini"));
            assert!(cli_has_magents(&homes, "copilot"));
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
        let invalid_toml = dir.path().join("bad.toml");
        fs::write(&invalid_toml, "[[[not toml").unwrap();
        let error = super::upsert_toml_mcp(&invalid_toml, Path::new("magents")).unwrap_err();
        assert!(error.to_string().contains("not valid TOML"), "{error}");
        super::upsert_toml_mcp(&dir.path().join("missing.toml"), Path::new("magents")).unwrap();
        assert!(toml_has_keys(
            &dir.path().join("missing.toml"),
            &["mcp_servers", "magents"]
        ));
        let empty_toml = dir.path().join("empty.toml");
        fs::write(&empty_toml, "  \n").unwrap();
        super::upsert_toml_mcp(&empty_toml, Path::new("magents")).unwrap();
        assert!(toml_has_keys(&empty_toml, &["mcp_servers", "magents"]));
        let servers = dir.path().join("servers.json");
        write_json(&servers, &json!({"mcpServers": []})).unwrap();
        let error = super::upsert_json_mcp(
            &servers,
            "mcpServers",
            json!({"command": "magents", "args": ["mcp"]}),
        )
        .unwrap_err();
        assert!(error.to_string().contains("must be an object"), "{error}");
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
    fn concurrent_locked_updates_keep_both_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.json");
        write_json(&path, &json!({"mcpServers": {}})).unwrap();
        let left = path.clone();
        let right = path.clone();
        let first = std::thread::spawn(move || {
            super::with_file_lock(&left, || {
                let mut root = read_json_object(&left)?;
                std::thread::sleep(std::time::Duration::from_millis(40));
                root.get_mut("mcpServers")
                    .and_then(Value::as_object_mut)
                    .unwrap()
                    .insert("alpha".into(), json!({"ok": true}));
                write_json(&left, &Value::Object(root))
            })
        });
        let second = std::thread::spawn(move || {
            super::with_file_lock(&right, || {
                let mut root = read_json_object(&right)?;
                std::thread::sleep(std::time::Duration::from_millis(40));
                root.get_mut("mcpServers")
                    .and_then(Value::as_object_mut)
                    .unwrap()
                    .insert("beta".into(), json!({"ok": true}));
                write_json(&right, &Value::Object(root))
            })
        });
        first.join().unwrap().unwrap();
        second.join().unwrap().unwrap();
        let raw = fs::read_to_string(&path).unwrap();
        assert!(raw.contains("alpha"), "{raw}");
        assert!(raw.contains("beta"), "{raw}");
    }

    #[test]
    fn config_lock_path_is_shared_for_the_same_file() {
        let path = Path::new("/tmp/mcp.json");
        assert_eq!(super::lock_path(path), super::lock_path(path));
        assert_eq!(
            super::lock_path(path)
                .file_name()
                .unwrap()
                .to_string_lossy(),
            ".mcp.json.lock"
        );
    }

    #[test]
    fn config_update_keeps_host_edits_made_during_install() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.json");
        write_json(&path, &json!({"mcpServers": {"keep": {"command": "x"}}})).unwrap();
        let mut attempts = 0;
        super::with_config_update(&path, |raw| {
            attempts += 1;
            let mut root = super::json_object_from_bytes(&path, raw)?;
            if attempts == 1 {
                write_json(
                    &path,
                    &json!({
                        "mcpServers": {
                            "keep": {"command": "x"},
                            "host": {"command": "y"}
                        }
                    }),
                )
                .unwrap();
            }
            root.entry("mcpServers")
                .or_insert_with(|| json!({}))
                .as_object_mut()
                .unwrap()
                .insert(
                    "magents".into(),
                    json!({"command": "magents", "args": ["mcp"]}),
                );
            Ok((super::pretty_json_bytes(&Value::Object(root))?, ()))
        })
        .unwrap();
        assert!(attempts >= 2, "attempts={attempts}");
        let raw = fs::read_to_string(&path).unwrap();
        assert!(raw.contains("keep"), "{raw}");
        assert!(raw.contains("host"), "{raw}");
        assert!(raw.contains("magents"), "{raw}");
    }

    #[test]
    fn config_update_skips_write_when_contents_match() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.json");
        let raw = b"same\n";
        fs::write(&path, raw).unwrap();
        super::with_config_update(&path, |_| Ok((raw.to_vec(), ()))).unwrap();
        assert_eq!(fs::read(&path).unwrap(), raw);
    }

    #[test]
    fn config_update_errors_when_the_file_keeps_changing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.json");
        write_json(&path, &json!({"ok": true})).unwrap();
        let mut n = 0;
        let error = super::with_config_update(&path, |_| {
            n += 1;
            fs::write(&path, format!("changed-{n}\n")).unwrap();
            Ok((b"{\"magents\":true}\n".to_vec(), ()))
        })
        .unwrap_err();
        assert!(
            error.to_string().contains("changed while installing"),
            "{error}"
        );
    }

    #[test]
    fn write_atomic_if_unchanged_refuses_a_stale_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.json");
        fs::write(&path, "host\n").unwrap();
        assert!(!super::write_atomic_if_unchanged(&path, b"before\n", b"next\n").unwrap());
        assert_eq!(fs::read_to_string(&path).unwrap(), "host\n");
        assert!(super::write_atomic_if_unchanged(&path, b"host\n", b"next\n").unwrap());
        assert_eq!(fs::read_to_string(&path).unwrap(), "next\n");
        let missing = dir.path().join("missing.json");
        assert!(!super::write_atomic_if_unchanged(&missing, b"before\n", b"next\n").unwrap());
        assert!(!missing.is_file());
        assert!(super::write_atomic_if_unchanged(&missing, b"", b"created\n").unwrap());
        assert_eq!(fs::read_to_string(&missing).unwrap(), "created\n");
        let blocked = dir.path().join("blocked");
        fs::write(&blocked, "file").unwrap();
        assert!(super::write_atomic_if_unchanged(&blocked.join("x.json"), b"", b"x\n").is_err());
    }

    #[test]
    fn recover_live_file_ignores_an_existing_destination() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.json");
        fs::write(&path, "live\n").unwrap();
        fs::write(super::stamp_path(&path), "stamp\n").unwrap();
        super::recover_live_file(&path).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "live\n");
    }

    #[test]
    fn config_update_recovers_a_leftover_stamp_before_merging() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.json");
        write_json(
            &super::stamp_path(&path),
            &json!({"mcpServers": {"keep": {"command": "x"}}}),
        )
        .unwrap();
        super::upsert_json_mcp(
            &path,
            "mcpServers",
            json!({"command": "magents", "args": ["mcp"]}),
        )
        .unwrap();
        let raw = fs::read_to_string(&path).unwrap();
        assert!(raw.contains("keep"), "{raw}");
        assert!(raw.contains("magents"), "{raw}");
        assert!(!super::stamp_path(&path).is_file());
    }

    #[test]
    fn publish_if_unchanged_restores_stamp_when_exclusive_link_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.json");
        let tmp = dir.path().join("next.json");
        fs::write(&path, "before\n").unwrap();
        fs::write(&tmp, "next\n").unwrap();
        let blocked = dir.path().join("blocked");
        fs::write(&blocked, "file").unwrap();
        let dest = blocked.join("mcp.json");
        let error = super::publish_if_unchanged(&dest, b"", &tmp).unwrap_err();
        assert!(error.to_string().contains("failed to read"), "{error}");
    }

    #[test]
    fn link_or_conflict_is_false_when_the_destination_already_exists() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("from");
        let to = dir.path().join("to");
        fs::write(&from, "from\n").unwrap();
        fs::write(&to, "to\n").unwrap();
        assert!(!super::link_or_conflict(&from, &to).unwrap());
        assert_eq!(fs::read_to_string(&to).unwrap(), "to\n");
    }

    #[test]
    fn publish_restores_an_in_place_host_update_from_the_stamp() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.json");
        let tmp = dir.path().join("next.json");
        fs::write(&path, "before\n").unwrap();
        fs::write(&tmp, "next\n").unwrap();
        assert!(
            !super::publish_after_unlink(
                &path,
                b"before\n",
                &tmp,
                &|_| Ok(()),
                &|stamp| {
                    fs::write(stamp, "host\n").unwrap();
                    Ok(())
                },
                &|_| Ok(()),
            )
            .unwrap()
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "host\n");
    }

    #[test]
    fn publish_retries_when_the_live_path_is_recreated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.json");
        let tmp = dir.path().join("next.json");
        fs::write(&path, "before\n").unwrap();
        fs::write(&tmp, "next\n").unwrap();
        assert!(
            !super::publish_after_unlink(
                &path,
                b"before\n",
                &tmp,
                &|_| Ok(()),
                &|_| {
                    fs::write(&path, "host\n").unwrap();
                    Ok(())
                },
                &|_| Ok(()),
            )
            .unwrap()
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "host\n");
    }

    #[test]
    fn publish_restores_a_host_replacement_moved_aside() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.json");
        let tmp = dir.path().join("next.json");
        fs::write(&path, "before\n").unwrap();
        fs::write(&tmp, "next\n").unwrap();
        assert!(
            !super::publish_after_unlink(
                &path,
                b"before\n",
                &tmp,
                &|live| {
                    fs::remove_file(live).unwrap();
                    fs::write(live, "host-replaced\n").unwrap();
                    Ok(())
                },
                &|_| Ok(()),
                &|_| Ok(()),
            )
            .unwrap()
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "host-replaced\n");
    }

    #[test]
    fn publish_keeps_a_host_replacement_after_successful_link() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.json");
        let tmp = dir.path().join("next.json");
        fs::write(&path, "before\n").unwrap();
        fs::write(&tmp, "next\n").unwrap();
        assert!(
            !super::publish_after_unlink(
                &path,
                b"before\n",
                &tmp,
                &|_| Ok(()),
                &|_| Ok(()),
                &|live| {
                    fs::write(super::stamp_path(live), "stale-host\n").unwrap();
                    fs::remove_file(live).unwrap();
                    fs::write(live, "host-replaced\n").unwrap();
                    Ok(())
                },
            )
            .unwrap()
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "host-replaced\n");
    }

    #[test]
    fn publish_restores_the_stamp_when_the_exclusive_link_source_vanishes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.json");
        let tmp = dir.path().join("next.json");
        fs::write(&path, "before\n").unwrap();
        fs::write(&tmp, "next\n").unwrap();
        let error = super::publish_after_unlink(
            &path,
            b"before\n",
            &tmp,
            &|_| Ok(()),
            &|_| {
                fs::remove_file(&tmp).unwrap();
                Ok(())
            },
            &|_| Ok(()),
        )
        .unwrap_err();
        assert!(error.to_string().contains("failed to read"), "{error}");
        assert_eq!(fs::read_to_string(&path).unwrap(), "before\n");
    }

    #[test]
    fn publish_retries_when_the_live_path_disappears_before_aside() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.json");
        let tmp = dir.path().join("next.json");
        fs::write(&path, "before\n").unwrap();
        fs::write(&tmp, "next\n").unwrap();
        assert!(
            !super::publish_after_unlink(
                &path,
                b"before\n",
                &tmp,
                &|live| {
                    fs::remove_file(live).unwrap();
                    Ok(())
                },
                &|_| Ok(()),
                &|_| Ok(()),
            )
            .unwrap()
        );
        assert!(!path.is_file());
    }

    #[test]
    fn publish_errors_when_the_stamp_path_cannot_be_created() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.json");
        let tmp = dir.path().join("next.json");
        fs::write(&path, "before\n").unwrap();
        fs::write(&tmp, "next\n").unwrap();
        fs::create_dir(super::stamp_path(&path)).unwrap();
        assert!(super::publish_if_unchanged(&path, b"before\n", &tmp).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "before\n");
    }

    #[test]
    fn recover_live_file_errors_when_the_destination_cannot_be_created() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.json");
        fs::write(super::stamp_path(&path), "stamp\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(dir.path()).unwrap().permissions();
            permissions.set_mode(0o555);
            fs::set_permissions(dir.path(), permissions).unwrap();
            assert!(super::recover_live_file(&path).is_err());
            let mut permissions = fs::metadata(dir.path()).unwrap().permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(dir.path(), permissions).unwrap();
        }
    }

    #[test]
    fn recover_live_file_treats_an_existing_directory_as_a_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.json");
        fs::create_dir(&path).unwrap();
        fs::write(super::stamp_path(&path), "stamp\n").unwrap();
        super::recover_live_file(&path).unwrap();
        assert!(path.is_dir());
    }

    #[test]
    fn same_inode_and_restore_path_cover_conflict_edges() {
        let dir = tempfile::tempdir().unwrap();
        let left = dir.path().join("left");
        let right = dir.path().join("right");
        fs::write(&left, "left\n").unwrap();
        assert!(super::same_inode(&left, &dir.path().join("missing")).is_err());
        assert!(super::same_inode(&dir.path().join("missing"), &left).is_err());
        fs::write(&right, "right\n").unwrap();
        super::restore_path(&left, &right);
        assert_eq!(fs::read_to_string(&right).unwrap(), "right\n");
        assert!(left.is_file());
    }

    #[cfg(unix)]
    #[test]
    fn lock_exclusive_errors_when_the_descriptor_is_invalid() {
        use std::os::unix::io::{FromRawFd, IntoRawFd};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.json");
        fs::write(&path, "x\n").unwrap();
        let file = super::File::open(&path).unwrap();
        let fd = file.into_raw_fd();
        unsafe { libc::close(fd) };
        let file = unsafe { super::File::from_raw_fd(fd) };
        assert!(super::lock_exclusive(&file, &path).is_err());
        std::mem::forget(file);
    }

    #[test]
    fn publish_errors_when_the_aside_rename_is_blocked() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.json");
        let tmp = dir.path().join("next.json");
        fs::write(&path, "before\n").unwrap();
        fs::write(&tmp, "next\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert!(
                super::publish_after_unlink(
                    &path,
                    b"before\n",
                    &tmp,
                    &|_| {
                        let mut permissions = fs::metadata(dir.path()).unwrap().permissions();
                        permissions.set_mode(0o555);
                        fs::set_permissions(dir.path(), permissions).unwrap();
                        Ok(())
                    },
                    &|_| Ok(()),
                    &|_| Ok(()),
                )
                .is_err()
            );
            let mut permissions = fs::metadata(dir.path()).unwrap().permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(dir.path(), permissions).unwrap();
            assert_eq!(fs::read_to_string(&path).unwrap(), "before\n");
        }
    }

    #[test]
    fn toml_from_bytes_rejects_invalid_utf8() {
        let error = super::toml_from_bytes(Path::new("config.toml"), &[0xff, 0xfe]).unwrap_err();
        assert!(error.to_string().contains("not valid TOML"), "{error}");
        assert!(
            super::json_object_from_bytes(Path::new("mcp.json"), b"  \n")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn atomic_tmp_paths_are_unique_per_call() {
        let path = Path::new("/tmp/mcp.json");
        let first = super::atomic_tmp(path);
        let second = super::atomic_tmp(path);
        assert_ne!(first, second);
        assert!(
            first
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".mcp.json.")
        );
        assert!(first.to_string_lossy().ends_with(".tmp"));
    }

    #[test]
    fn config_write_keeps_existing_file_when_tmp_cannot_be_written() {
        let dir = tempfile::tempdir().unwrap();
        let json = dir.path().join("mcp.json");
        write_json(&json, &json!({"keep": true})).unwrap();
        let before = fs::read_to_string(&json).unwrap();
        let toml = dir.path().join("config.toml");
        fs::write(&toml, "keep = true\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(dir.path()).unwrap().permissions();
            permissions.set_mode(0o555);
            fs::set_permissions(dir.path(), permissions).unwrap();
            let error = write_json(&json, &json!({"keep": false})).unwrap_err();
            assert!(error.to_string().contains("failed to read"), "{error}");
            assert_eq!(fs::read_to_string(&json).unwrap(), before);
            let error =
                super::write_atomic_if_unchanged(&json, before.as_bytes(), b"{\"keep\":false}\n")
                    .unwrap_err();
            assert!(error.to_string().contains("failed to read"), "{error}");
            assert_eq!(fs::read_to_string(&json).unwrap(), before);
            let error = super::upsert_toml_mcp(&toml, Path::new("magents")).unwrap_err();
            assert!(error.to_string().contains("failed to read"), "{error}");
            assert_eq!(fs::read_to_string(&toml).unwrap(), "keep = true\n");
            let mut permissions = fs::metadata(dir.path()).unwrap().permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(dir.path(), permissions).unwrap();
        }
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
        assert!(super::with_file_lock(&blocked.join("mcp.json"), || Ok(())).is_err());
        assert!(
            super::upsert_toml_mcp(&blocked.join("config.toml"), Path::new("magents")).is_err()
        );
        let unreadable_toml = dir.path().join("secret.toml");
        fs::write(&unreadable_toml, "[mcp_servers.magents]\ncommand = \"x\"\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(&unreadable_toml).unwrap().permissions();
            permissions.set_mode(0o000);
            fs::set_permissions(&unreadable_toml, permissions).unwrap();
            assert!(super::upsert_toml_mcp(&unreadable_toml, Path::new("magents")).is_err());
            let mut permissions = fs::metadata(&unreadable_toml).unwrap().permissions();
            permissions.set_mode(0o644);
            fs::set_permissions(&unreadable_toml, permissions).unwrap();
        }

        with_home(|home, bin| {
            test_env::write_executable(&bin.join("grok"), "echo added magents");
            fs::create_dir_all(home.join(".grok")).unwrap();
            fs::write(home.join(".grok").join("skills"), "not-a-dir").unwrap();
            assert!(install(false, true, false, false, false, false, false).is_err());
        });
    }
}
