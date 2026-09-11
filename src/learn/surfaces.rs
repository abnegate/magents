use super::scan::{CompactSession, git_root, under};
use super::text::{SLASH_STOP, frontmatter};
use crate::homes::Homes;
use crate::model::Agent;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use toml_edit::DocumentMut;
use walkdir::WalkDir;

#[derive(Clone, Debug)]
pub struct Surfaces {
    pub value: Value,
    pub skill_names: BTreeSet<String>,
}

pub fn collect(homes: &Homes, sessions: &[CompactSession]) -> Surfaces {
    let mut skills = Vec::new();
    let mut user_dirs = BTreeMap::new();
    for (agent, dir) in user_skill_roots(homes) {
        user_dirs.insert(agent.as_str().to_string(), dir.clone());
        add_skills(&mut skills, &dir, "user", Some(agent), None);
    }
    let agents_dir = homes
        .claude
        .parent()
        .unwrap_or(Path::new("."))
        .join(".agents")
        .join("skills");
    if agents_dir.is_dir() {
        add_skills(&mut skills, &agents_dir, "user", None, None);
    }
    add_commands(
        &mut skills,
        &homes.claude.join("commands"),
        "command",
        Some(Agent::Claude),
        None,
    );
    add_skills(
        &mut skills,
        &homes.grok.join("bundled").join("skills"),
        "bundled",
        Some(Agent::Grok),
        None,
    );

    let mut project_dirs: BTreeMap<PathBuf, PathBuf> = BTreeMap::new();
    for session in sessions {
        let cwd = session.cwd.as_str();
        let root = if session.git_root.is_empty() {
            git_root(cwd)
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| cwd.to_string())
        } else {
            session.git_root.clone()
        };
        for (skills_dir, owner) in project_skill_dirs(cwd, &root) {
            if skills_dir.is_dir() {
                project_dirs.entry(skills_dir).or_insert(owner);
            }
        }
    }
    let project_roots: Vec<String> = {
        let mut roots: BTreeSet<String> = BTreeSet::new();
        for owner in project_dirs.values() {
            roots.insert(owner.display().to_string());
        }
        roots.into_iter().collect()
    };
    let mut project_seen: HashMap<String, usize> = HashMap::new();
    for (dir, owner) in &project_dirs {
        let before = skills.len();
        add_skills(
            &mut skills,
            dir,
            "project",
            None,
            Some(json!({"project_roots": [owner]})),
        );
        let added: Vec<Value> = skills.drain(before..).collect();
        let mut keep = Vec::new();
        for rec in added {
            let name = rec["name"].as_str().unwrap_or("").to_string();
            if let Some(index) = project_seen.get(&name).copied() {
                if let Some(roots) = skills[index]
                    .get_mut("project_roots")
                    .and_then(Value::as_array_mut)
                {
                    let owner_s = json!(owner);
                    if !roots.contains(&owner_s) {
                        roots.push(owner_s);
                    }
                }
            } else {
                let mut rec = rec;
                rec["git_tracked"] = json!(git_tracked(
                    rec["path"].as_str().unwrap_or(""),
                    &owner.display().to_string()
                ));
                project_seen.insert(name, before + keep.len());
                keep.push(rec);
            }
        }
        skills.extend(keep);
    }
    for root in &project_roots {
        add_commands(
            &mut skills,
            &PathBuf::from(root).join(".claude").join("commands"),
            "command",
            None,
            Some(json!({"project_roots": [root]})),
        );
    }

    let grok_cfg = read_to_string(&homes.grok.join("config.toml"));
    let enabled = toml_string_array(&grok_cfg, "plugins", "enabled");
    let disabled = toml_string_array(&grok_cfg, "plugins", "disabled");
    let skills_disabled = toml_string_array(&grok_cfg, "skills", "disabled");

    let mut plugins = Vec::new();
    let plugin_root = homes.grok.join("installed-plugins");
    if plugin_root.is_dir() {
        let registry = read_plugin_registry(&plugin_root);
        if let Ok(entries) = fs::read_dir(&plugin_root) {
            for entry in entries.flatten() {
                let full = entry.path();
                if !full.is_dir() {
                    continue;
                }
                let dir_name = entry.file_name().to_string_lossy().into_owned();
                let name = registry
                    .get(&full)
                    .cloned()
                    .unwrap_or_else(|| strip_hash_suffix(&dir_name));
                let state = if enabled.contains(&name) {
                    "enabled"
                } else if disabled.contains(&name) {
                    "disabled"
                } else {
                    "unlisted"
                };
                let before = skills.len();
                add_skills(
                    &mut skills,
                    &full.join("skills"),
                    "plugin",
                    Some(Agent::Grok),
                    Some(json!({"plugin": name, "plugin_state": state})),
                );
                plugins.push(json!({
                    "name": name,
                    "dir": full,
                    "state": state,
                    "skill_count": skills.len() - before,
                    "has_hooks": full.join("hooks").is_dir(),
                    "has_commands": full.join("commands").is_dir(),
                }));
            }
        }
    }
    let claude_plugins = homes.claude.join("plugins");
    if claude_plugins.is_dir() {
        for skill in WalkDir::new(&claude_plugins)
            .max_depth(6)
            .into_iter()
            .flatten()
        {
            let path = skill.path();
            if path.file_name().and_then(|n| n.to_str()) != Some("SKILL.md") {
                continue;
            }
            let parent = path.parent().map(Path::to_path_buf);
            let Some(parent) = parent else { continue };
            if parent
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                != Some("skills")
            {
                continue;
            }
            let (name, description) = frontmatter(path);
            skills.push(json!({
                "path": path,
                "source": "plugin",
                "name": name,
                "description": description,
                "agent": "claude",
                "plugin": parent.parent().and_then(|p| p.parent()).and_then(|p| p.file_name()).and_then(|n| n.to_str()),
                "plugin_state": "enabled",
            }));
        }
    }

    let mut mcp_sources: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for name in toml_subtables(&grok_cfg, "mcp_servers") {
        mcp_sources
            .entry(name)
            .or_default()
            .push("grok:config.toml".into());
    }
    add_json_mcp(
        &mut mcp_sources,
        &homes.grok.join("settings.json"),
        "mcpServers",
        "grok:settings.json",
    );
    add_json_mcp(
        &mut mcp_sources,
        &homes.claude.join("settings.json"),
        "mcpServers",
        "claude:settings.json",
    );
    if let Some(parent) = homes.claude.parent() {
        add_json_mcp(
            &mut mcp_sources,
            &parent.join(".claude.json"),
            "mcpServers",
            "claude:.claude.json",
        );
    }
    for name in toml_subtables(
        &read_to_string(&homes.codex.join("config.toml")),
        "mcp_servers",
    ) {
        mcp_sources
            .entry(name)
            .or_default()
            .push("codex:config.toml".into());
    }
    add_json_mcp(
        &mut mcp_sources,
        &homes.cursor_config.join("mcp.json"),
        "mcpServers",
        "cursor:mcp.json",
    );
    add_json_mcp(
        &mut mcp_sources,
        &homes.opencode_config.join("opencode.json"),
        "mcp",
        "opencode:opencode.json",
    );
    add_json_mcp(
        &mut mcp_sources,
        &homes.gemini.join("settings.json"),
        "mcpServers",
        "gemini:settings.json",
    );
    add_json_mcp(
        &mut mcp_sources,
        &homes.copilot.join("mcp-config.json"),
        "mcpServers",
        "copilot:mcp-config.json",
    );
    let mcp: Vec<Value> = mcp_sources
        .into_iter()
        .map(|(name, sources)| json!({"name": name, "sources": sources}))
        .collect();

    let mut hooks = Vec::new();
    let hooks_dir = homes.grok.join("hooks");
    if let Ok(entries) = fs::read_dir(&hooks_dir) {
        for entry in entries.flatten() {
            hooks.push(entry.file_name().to_string_lossy().into_owned());
        }
        hooks.sort();
    }

    let mut workflows = Vec::new();
    add_workflows(&mut workflows, &homes.grok.join("workflows"), "user");
    add_workflows(
        &mut workflows,
        &homes.grok.join("bundled").join("workflows"),
        "bundled",
    );
    for root in &project_roots {
        add_workflows(
            &mut workflows,
            &PathBuf::from(root).join(".grok").join("workflows"),
            "project",
        );
    }

    let mut mru = Vec::new();
    if let Ok(raw) = fs::read_to_string(homes.grok.join("slash-mru.json"))
        && let Ok(value) = serde_json::from_str::<Value>(&raw)
        && let Some(map) = value.get("by_command").and_then(Value::as_object)
    {
        mru = map.keys().cloned().collect();
        mru.sort();
    }

    let disabled_names: BTreeSet<String> = skills_disabled.into_iter().collect();
    for skill in &mut skills {
        let source = skill
            .get("source")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let plugin_state = skill
            .get("plugin_state")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        let from_enabled = source != "plugin" || plugin_state.as_deref() == Some("enabled");
        let name = skill
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let git_tracked_false = skill.get("git_tracked") == Some(&json!(false));
        skill["loaded"] = json!(from_enabled && !disabled_names.contains(&name));
        skill["disabled_by_name"] = json!(disabled_names.contains(&name));
        skill["protected"] = json!(match source.as_str() {
            "plugin" => Some("plugin"),
            "bundled" => Some("bundled"),
            "project" if !git_tracked_false => Some("git-tracked"),
            _ => None,
        });
    }

    let mut bodies = HashMap::new();
    for skill in &skills {
        if skill.get("loaded") == Some(&json!(true))
            && let Some(path) = skill.get("path").and_then(Value::as_str)
            && let Ok(body) = fs::read_to_string(path)
        {
            bodies.insert(path.to_string(), body);
        }
    }
    let names: Vec<(String, String)> = skills
        .iter()
        .filter_map(|skill| {
            Some((
                skill.get("name")?.as_str()?.to_string(),
                skill.get("path")?.as_str()?.to_string(),
            ))
        })
        .collect();
    for skill in &mut skills {
        let name = skill.get("name").and_then(Value::as_str).unwrap_or("");
        let path = skill.get("path").and_then(Value::as_str).unwrap_or("");
        let needle = format!("/{name}");
        let referenced: BTreeSet<String> = names
            .iter()
            .filter(|(_other, other_path)| {
                other_path != path
                    && bodies.get(other_path).is_some_and(|body| {
                        body.contains(&format!("`/{name}`"))
                            || body.contains(&format!("`{name}`"))
                            || body.contains(&format!("skills/{name}"))
                            || body.contains(&needle)
                    })
            })
            .map(|(other, _)| other.clone())
            .collect();
        skill["referenced_by"] = json!(referenced.into_iter().collect::<Vec<_>>());
    }

    let mut by_name: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for skill in &skills {
        if skill.get("loaded") != Some(&json!(true)) {
            continue;
        }
        let name = skill.get("name").and_then(Value::as_str).unwrap_or("");
        let mut tag = skill
            .get("source")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if tag == "plugin"
            && let Some(plugin) = skill.get("plugin").and_then(Value::as_str)
        {
            tag = format!("plugin:{plugin}");
        }
        by_name.entry(name.to_string()).or_default().push(tag);
    }
    let collisions: BTreeMap<String, Vec<String>> =
        by_name.into_iter().filter(|(_, v)| v.len() > 1).collect();

    let skill_names = skills
        .iter()
        .filter_map(|skill| skill.get("name")?.as_str().map(ToOwned::to_owned))
        .collect();

    let value = json!({
        "magents_home": homes.magents,
        "user_skills_dirs": user_dirs,
        "project_roots": project_roots,
        "skills": skills,
        "skills_disabled_by_name": disabled_names.into_iter().collect::<Vec<_>>(),
        "plugins": plugins,
        "mcp_servers": mcp,
        "hooks": hooks,
        "workflows": workflows,
        "slash_mru": mru,
        "skill_name_collisions": collisions,
    });
    Surfaces { value, skill_names }
}

pub fn usage(surfaces: &Surfaces, sessions: &[CompactSession]) -> Value {
    let mut hits: HashMap<(String, String), Hit> = HashMap::new();
    let workflows: Vec<String> = surfaces.value["workflows"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| row.get("name")?.as_str().map(ToOwned::to_owned))
        .collect();
    for session in sessions {
        for (name, count) in &session.skills_loaded {
            hit(&mut hits, "skill", name, session, *count, "skill_file_read");
        }
        for (name, count) in &session.slash_commands {
            if surfaces.skill_names.contains(name) {
                hit(&mut hits, "skill", name, session, *count, "slash");
            } else if workflows.iter().any(|workflow| workflow == name) {
                hit(&mut hits, "workflow", name, session, *count, "slash");
            }
        }
        for (name, count) in &session.mcp_servers_used {
            hit(&mut hits, "mcp", name, session, *count, "use_tool");
        }
        for workflow in &workflows {
            if session
                .turns
                .iter()
                .any(|turn| turn.contains(workflow.as_str()))
            {
                hit(&mut hits, "workflow", workflow, session, 1, "mention");
            }
        }
    }

    let mut items = Vec::new();
    let empty = Hit::default();
    if let Some(skills) = surfaces.value["skills"].as_array() {
        for skill in skills {
            let name = skill.get("name").and_then(Value::as_str).unwrap_or("");
            let used = hits.get(&("skill".into(), name.into())).unwrap_or(&empty);
            let mru = surfaces.value["slash_mru"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|row| row.as_str() == Some(name));
            items.push(json!({
                "kind": "skill",
                "name": name,
                "source": skill.get("source"),
                "path": skill.get("path"),
                "protected": skill.get("protected"),
                "referenced_by": skill.get("referenced_by"),
                "plugin": skill.get("plugin"),
                "plugin_state": skill.get("plugin_state"),
                "loaded": skill.get("loaded"),
                "disabled_by_name": skill.get("disabled_by_name"),
                "in_slash_mru": mru,
                "count": used.count,
                "sessions": used.sessions,
                "last_used": used.last_used,
                "via": used.via,
            }));
        }
    }
    if let Some(plugins) = surfaces.value["plugins"].as_array() {
        for plugin in plugins {
            let name = plugin.get("name").and_then(Value::as_str).unwrap_or("");
            let related: Vec<&Value> = items
                .iter()
                .filter(|item| item.get("plugin").and_then(Value::as_str) == Some(name))
                .collect();
            let count: usize = related
                .iter()
                .filter_map(|item| item.get("count")?.as_u64())
                .sum::<u64>() as usize;
            let mut sessions = Vec::new();
            for item in related {
                if let Some(ids) = item.get("sessions").and_then(Value::as_array) {
                    for id in ids {
                        if let Some(id) = id.as_str()
                            && !sessions.iter().any(|have: &String| have == id)
                            && sessions.len() < 8
                        {
                            sessions.push(id.to_string());
                        }
                    }
                }
            }
            items.push(json!({
                "kind": "plugin",
                "name": name,
                "state": plugin.get("state"),
                "path": plugin.get("dir"),
                "skill_count": plugin.get("skill_count"),
                "count": count,
                "sessions": sessions,
            }));
        }
    }
    if let Some(mcp) = surfaces.value["mcp_servers"].as_array() {
        for server in mcp {
            let name = server.get("name").and_then(Value::as_str).unwrap_or("");
            let used = hits.get(&("mcp".into(), name.into())).unwrap_or(&empty);
            items.push(json!({
                "kind": "mcp",
                "name": name,
                "sources": server.get("sources"),
                "count": used.count,
                "sessions": used.sessions,
                "last_used": used.last_used,
            }));
        }
    }
    if let Some(workflows) = surfaces.value["workflows"].as_array() {
        for workflow in workflows {
            let name = workflow.get("name").and_then(Value::as_str).unwrap_or("");
            let used = hits
                .get(&("workflow".into(), name.into()))
                .unwrap_or(&empty);
            items.push(json!({
                "kind": "workflow",
                "name": name,
                "source": workflow.get("source"),
                "path": workflow.get("path"),
                "count": used.count,
                "sessions": used.sessions,
            }));
        }
    }
    if let Some(hooks) = surfaces.value["hooks"].as_array() {
        for hook in hooks {
            items.push(json!({
                "kind": "hook",
                "name": hook,
                "count": Value::Null,
                "note": "hook use is not visible in transcripts; judge by config only",
            }));
        }
    }

    let unused_loaded: Vec<&Value> = items
        .iter()
        .filter(|item| {
            item.get("kind").and_then(Value::as_str) == Some("skill")
                && item.get("loaded") == Some(&json!(true))
                && item.get("count").and_then(Value::as_u64) == Some(0)
                && item.get("in_slash_mru") != Some(&json!(true))
        })
        .collect();

    let mut unknown_slash: BTreeMap<String, usize> = BTreeMap::new();
    for session in sessions {
        for (name, count) in &session.slash_commands {
            if !surfaces.skill_names.contains(name)
                && !SLASH_STOP.contains(&name.as_str())
                && !workflows.iter().any(|workflow| workflow == name)
            {
                *unknown_slash.entry(name.clone()).or_default() += *count;
            }
        }
    }
    let configured_mcp: BTreeSet<String> = surfaces.value["mcp_servers"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| row.get("name")?.as_str().map(ToOwned::to_owned))
        .collect();
    let mcp_unconfigured: BTreeMap<String, usize> = hits
        .iter()
        .filter_map(|((kind, name), hit)| {
            if kind == "mcp" && !configured_mcp.contains(name) {
                Some((name.clone(), hit.count))
            } else {
                None
            }
        })
        .collect();

    json!({
        "kept_sessions": sessions.len(),
        "items": items,
        "unused_loaded": unused_loaded,
        "slash_commands_with_no_loaded_skill": unknown_slash,
        "mcp_servers_used_but_not_in_local_config": mcp_unconfigured,
        "note": "hooks and managed-gateway MCP servers are not visible in local config; count is null when it cannot be measured",
    })
}

#[derive(Default)]
struct Hit {
    count: usize,
    sessions: Vec<String>,
    last_used: Option<String>,
    via: BTreeMap<String, usize>,
}

fn hit(
    hits: &mut HashMap<(String, String), Hit>,
    kind: &str,
    name: &str,
    session: &CompactSession,
    n: usize,
    via: &str,
) {
    let entry = hits
        .entry((kind.to_string(), name.to_string()))
        .or_default();
    entry.count += n;
    if !entry.sessions.iter().any(|id| id == &session.id) && entry.sessions.len() < 8 {
        entry.sessions.push(session.id.clone());
    }
    let updated = session.updated_at.clone().unwrap_or_default();
    if updated > entry.last_used.clone().unwrap_or_default() {
        entry.last_used = session.updated_at.clone();
    }
    *entry.via.entry(via.to_string()).or_default() += n;
}

fn user_skill_roots(homes: &Homes) -> Vec<(Agent, PathBuf)> {
    vec![
        (Agent::Claude, homes.claude.join("skills")),
        (Agent::Grok, homes.grok.join("skills")),
        (Agent::Codex, homes.codex.join("skills")),
        (Agent::Cursor, homes.cursor_config.join("skills")),
        (Agent::OpenCode, homes.opencode_config.join("skills")),
        (Agent::Gemini, homes.gemini.join("skills")),
        (Agent::Copilot, homes.copilot.join("skills")),
    ]
}

fn add_skills(
    skills: &mut Vec<Value>,
    dir: &Path,
    source: &str,
    agent: Option<Agent>,
    extra: Option<Value>,
) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut names: Vec<_> = entries.flatten().map(|entry| entry.path()).collect();
    names.sort();
    for path in names {
        let skill = path.join("SKILL.md");
        if !skill.is_file() {
            continue;
        }
        let (name, description) = frontmatter(&skill);
        let mut rec = json!({
            "path": skill,
            "source": source,
            "name": name,
            "description": description,
        });
        if let Some(agent) = agent {
            rec["agent"] = json!(agent.as_str());
        }
        if let Some(Value::Object(map)) = extra.clone()
            && let Some(object) = rec.as_object_mut()
        {
            object.extend(map);
        }
        skills.push(rec);
    }
}

fn add_commands(
    skills: &mut Vec<Value>,
    dir: &Path,
    source: &str,
    agent: Option<Agent>,
    extra: Option<Value>,
) {
    if !dir.is_dir() {
        return;
    }
    for entry in WalkDir::new(dir).max_depth(3).into_iter().flatten() {
        let path = entry.path();
        if !entry.file_type().is_file() {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        if path.extension().and_then(|ext| ext.to_str()) != Some("md")
            || name.eq_ignore_ascii_case("readme.md")
            || name.eq_ignore_ascii_case("skill.md")
        {
            continue;
        }
        let path_s = path.display().to_string();
        if skills
            .iter()
            .any(|skill| skill.get("path").and_then(Value::as_str) == Some(path_s.as_str()))
        {
            continue;
        }
        let (skill_name, description) = frontmatter(path);
        let mut rec = json!({
            "path": path,
            "source": source,
            "name": skill_name,
            "description": description,
        });
        if let Some(agent) = agent {
            rec["agent"] = json!(agent.as_str());
        }
        if let Some(Value::Object(map)) = extra.clone()
            && let Some(object) = rec.as_object_mut()
        {
            object.extend(map);
        }
        skills.push(rec);
    }
}

fn project_skill_dirs(cwd: &str, git_root: &str) -> Vec<(PathBuf, PathBuf)> {
    if cwd.is_empty() {
        return Vec::new();
    }
    let mut dirs = Vec::new();
    let mut current = PathBuf::from(cwd);
    let top = if git_root.is_empty() {
        PathBuf::from(cwd)
    } else {
        PathBuf::from(git_root)
    };
    loop {
        let owner = current.clone();
        for folder in [
            ".grok", ".claude", ".cursor", ".agents", ".gemini", ".copilot",
        ] {
            dirs.push((current.join(folder).join("skills"), owner.clone()));
        }
        if current == top || !under(&current.display().to_string(), &top.display().to_string()) {
            break;
        }
        if !current.pop() {
            break;
        }
    }
    dirs
}

fn add_workflows(out: &mut Vec<Value>, dir: &Path, source: &str) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<_> = entries.flatten().map(|entry| entry.path()).collect();
    files.sort();
    for path in files {
        if path.extension().and_then(|ext| ext.to_str()) != Some("rhai") {
            continue;
        }
        let name = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("")
            .to_string();
        out.push(json!({"name": name, "source": source, "path": path}));
    }
}

fn add_json_mcp(out: &mut BTreeMap<String, Vec<String>>, path: &Path, key: &str, source: &str) {
    let Ok(raw) = fs::read_to_string(path) else {
        return;
    };
    let Ok(value) = serde_json::from_str::<Value>(&raw) else {
        return;
    };
    if let Some(map) = value.get(key).and_then(Value::as_object) {
        for name in map.keys() {
            out.entry(name.clone())
                .or_default()
                .push(source.to_string());
        }
    }
}

fn read_to_string(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_default()
}

fn toml_string_array(text: &str, table: &str, key: &str) -> Vec<String> {
    let Ok(doc) = text.parse::<DocumentMut>() else {
        return Vec::new();
    };
    doc.get(table)
        .and_then(|item| item.get(key))
        .and_then(|item| item.as_array())
        .map(|array| {
            array
                .iter()
                .filter_map(|item| item.as_str().map(ToOwned::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn toml_subtables(text: &str, table: &str) -> Vec<String> {
    let Ok(doc) = text.parse::<DocumentMut>() else {
        return Vec::new();
    };
    doc.get(table)
        .and_then(|item| item.as_table())
        .map(|table| table.iter().map(|(key, _)| key.to_string()).collect())
        .unwrap_or_default()
}

fn git_tracked(path: &str, root: &str) -> Option<bool> {
    let output = std::process::Command::new("git")
        .args(["-C", root, "ls-files", "--error-unmatch", "--", path])
        .output()
        .ok()?;
    Some(output.status.success())
}

fn read_plugin_registry(root: &Path) -> HashMap<PathBuf, String> {
    let mut out = HashMap::new();
    let Ok(raw) = fs::read_to_string(root.join("registry.json")) else {
        return out;
    };
    let Ok(value) = serde_json::from_str::<Value>(&raw) else {
        return out;
    };
    if let Some(repos) = value.get("repos").and_then(Value::as_object) {
        for repo in repos.values() {
            let Some(path) = repo.get("path").and_then(Value::as_str) else {
                continue;
            };
            if let Some(name) = repo
                .get("plugins")
                .and_then(Value::as_object)
                .and_then(|plugins| plugins.keys().next())
            {
                out.insert(PathBuf::from(path), name.clone());
            }
        }
    }
    out
}

fn strip_hash_suffix(name: &str) -> String {
    let re = regex::Regex::new(r"-[0-9a-f]{8}$").expect("hash suffix");
    re.replace(name, "").into_owned()
}

#[cfg(test)]
mod tests {
    use super::{toml_string_array, toml_subtables};

    #[test]
    fn toml_helpers_read_names() {
        let text = "[plugins]\nenabled = [\"one\"]\n[mcp_servers.magents]\ncommand = \"x\"\n";
        assert_eq!(toml_string_array(text, "plugins", "enabled"), vec!["one"]);
        assert_eq!(toml_subtables(text, "mcp_servers"), vec!["magents"]);
        assert!(toml_string_array("nope", "plugins", "enabled").is_empty());
    }

    #[test]
    fn usage_counts_plugins_workflows_unknown_slash_and_unconfigured_mcp() {
        use super::{Surfaces, usage};
        use crate::learn::scan::CompactSession;
        use serde_json::json;
        use std::collections::BTreeMap;

        let surfaces = Surfaces {
            value: json!({
                "skills": [{
                    "name": "review",
                    "source": "plugin",
                    "path": "/x",
                    "protected": "plugin",
                    "referenced_by": [],
                    "plugin": "demo",
                    "plugin_state": "enabled",
                    "loaded": true,
                    "disabled_by_name": false
                }],
                "plugins": [{"name": "demo", "dir": "/p", "state": "enabled", "skill_count": 1}],
                "mcp_servers": [{"name": "magents", "sources": ["grok"]}],
                "workflows": [{"name": "demo", "source": "user", "path": "/w.rhai"}],
                "hooks": ["on-stop.sh"],
                "slash_mru": ["review"]
            }),
            skill_names: ["review".into()].into_iter().collect(),
        };
        let session = CompactSession {
            index: 0,
            agent: "grok".into(),
            id: "grok:1".into(),
            session_id: "1".into(),
            cwd: "/Users/test/app".into(),
            git_root: String::new(),
            branch: String::new(),
            title: "t".into(),
            session_kind: None,
            updated_at: Some("2026-01-01T00:00:00Z".into()),
            model: None,
            human_turns: 1,
            smoke_test: false,
            repeated_single_turn: false,
            turns: vec!["please demo this workflow now".into()],
            slash_commands: BTreeMap::from([
                ("review".into(), 1),
                ("demo".into(), 1),
                ("weirdcmd".into(), 1),
            ]),
            skills_loaded: BTreeMap::from([("review".into(), 1)]),
            skill_load_paths: BTreeMap::new(),
            mcp_servers_used: BTreeMap::from([("magents".into(), 1), ("unknownmcp".into(), 2)]),
            tools: BTreeMap::new(),
            top_dirs_touched: BTreeMap::new(),
            origin: None,
        };
        let value = usage(&surfaces, std::slice::from_ref(&session));
        assert_eq!(value["kept_sessions"], 1);
        assert!(
            value["slash_commands_with_no_loaded_skill"]
                .get("weirdcmd")
                .is_some(),
            "{value}"
        );
        assert!(
            value["mcp_servers_used_but_not_in_local_config"]
                .get("unknownmcp")
                .is_some(),
            "{value}"
        );
        assert!(
            value["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["kind"] == "plugin" && item["count"].as_u64().unwrap_or(0) >= 1),
            "{value}"
        );
        assert!(
            value["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["kind"] == "hook"),
            "{value}"
        );
    }
}
