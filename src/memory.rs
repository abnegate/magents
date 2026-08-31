use crate::error::{Error, Result};
use crate::homes::Homes;
use crate::model::{Agent, MemoryCreated, MemoryHit, MemoryRead};
use crate::transcript::scan_file;
use chrono::Utc;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use walkdir::WalkDir;

pub fn search_memories(
    homes: &Homes,
    query: &str,
    agent: Option<Agent>,
    limit: usize,
) -> Result<Vec<MemoryHit>> {
    let query = query.trim();
    if query.is_empty() {
        return Err(Error::msg("query is required"));
    }
    let needle = query.to_ascii_lowercase();
    let mut hits = Vec::new();
    if include(agent, Agent::Claude) {
        collect_claude(homes, &needle, &mut hits);
    }
    if include(agent, Agent::Codex) {
        collect_codex(homes, &needle, &mut hits);
    }
    if include(agent, Agent::Grok) {
        collect_grok(homes, &needle, &mut hits);
    }
    hits.sort_by(|left, right| {
        right
            .matches
            .cmp(&left.matches)
            .then_with(|| left.path.cmp(&right.path))
    });
    hits.truncate(limit);
    Ok(hits)
}

pub fn create_memory(
    homes: &Homes,
    agent: Agent,
    content: &str,
    file: Option<&str>,
    project: Option<&str>,
    cwd: Option<&str>,
) -> Result<MemoryCreated> {
    if content.trim().is_empty() {
        return Err(Error::msg("content is required"));
    }
    let file = match file {
        Some(file) => memory_filename(file)?,
        None => default_filename(content),
    };
    match agent {
        Agent::Claude => create_claude(homes, content, &file, project, cwd),
        Agent::Codex => create_codex(homes, content, &file),
        Agent::Grok => create_grok(homes, content, &file, project),
        Agent::Cursor | Agent::OpenCode | Agent::Gemini | Agent::Copilot => Err(Error::msg(
            format!("{agent} has no first-party memory store"),
        )),
    }
}

pub fn read_memory(
    homes: &Homes,
    agent: Agent,
    file: Option<&str>,
    project: Option<&str>,
    cwd: Option<&str>,
    path: Option<&str>,
    limit: usize,
) -> Result<MemoryRead> {
    let limit = if limit == 0 { 8000 } else { limit.min(32_000) };
    let (path, file, project) = resolve_read(homes, agent, file, project, cwd, path)?;
    let raw = fs::read_to_string(&path).map_err(|source| Error::Io {
        path: path.clone(),
        source,
    })?;
    let truncated = raw.chars().count() > limit;
    let content = if truncated {
        raw.chars().take(limit).collect::<String>() + "..."
    } else {
        raw
    };
    Ok(MemoryRead {
        agent,
        path,
        file,
        project,
        content,
        truncated,
        inert: true,
    })
}

fn resolve_read(
    homes: &Homes,
    agent: Agent,
    file: Option<&str>,
    project: Option<&str>,
    cwd: Option<&str>,
    path: Option<&str>,
) -> Result<(PathBuf, String, Option<String>)> {
    if let Some(path) = path.map(str::trim).filter(|value| !value.is_empty()) {
        return resolve_explicit_path(homes, agent, path);
    }
    let file = memory_filename(file.ok_or_else(|| Error::msg("file or path is required"))?)?;
    let (path, project) = match agent {
        Agent::Claude => {
            let slug = match (project, cwd) {
                (Some(project), _) if !project.trim().is_empty() => project_component(project)?,
                (_, Some(cwd)) if !cwd.trim().is_empty() => encode_claude_cwd(cwd)?,
                _ => return Err(Error::msg("project or cwd is required for Claude")),
            };
            (
                homes
                    .claude
                    .join("projects")
                    .join(&slug)
                    .join("memory")
                    .join(&file),
                Some(slug),
            )
        }
        Agent::Codex => (homes.codex.join("memories").join(&file), None),
        Agent::Grok => {
            let (path, project) = match project {
                Some(project) if !project.trim().is_empty() => {
                    let project = project_component(project)?;
                    (
                        homes.grok.join("memory").join(&project).join(&file),
                        Some(project),
                    )
                }
                _ => (homes.grok.join("memory").join(&file), Some("global".into())),
            };
            (path, project)
        }
        Agent::Cursor | Agent::OpenCode | Agent::Gemini | Agent::Copilot => {
            return Err(Error::msg(format!(
                "{agent} has no first-party memory store"
            )));
        }
    };
    if !path.is_file() {
        return Err(Error::msg(format!(
            "memory file not found: {}",
            path.display()
        )));
    }
    Ok((path, file, project))
}

fn resolve_explicit_path(
    homes: &Homes,
    agent: Agent,
    path: &str,
) -> Result<(PathBuf, String, Option<String>)> {
    let requested = PathBuf::from(path);
    let canonical = requested.canonicalize().map_err(|source| Error::Io {
        path: requested,
        source,
    })?;
    if canonical.extension().and_then(|ext| ext.to_str()) != Some("md") {
        return Err(Error::msg("path must be a markdown memory file"));
    }
    let roots = memory_roots(homes, agent)?;
    let Some(root) = roots.iter().find(|root| canonical.starts_with(root)) else {
        return Err(Error::msg("path is outside the memory root"));
    };
    let file = canonical
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| Error::msg("path must be a markdown memory file"))?
        .to_string();
    let project = match agent {
        Agent::Claude => canonical
            .strip_prefix(homes.claude.join("projects"))
            .ok()
            .and_then(|rest| rest.components().next())
            .and_then(|component| component.as_os_str().to_str())
            .map(ToOwned::to_owned),
        Agent::Grok => {
            let relative = canonical.strip_prefix(root).ok();
            match relative.and_then(|path| path.parent()) {
                Some(parent) if parent.as_os_str().is_empty() => Some("global".into()),
                Some(parent) => parent
                    .components()
                    .next()
                    .and_then(|component| component.as_os_str().to_str())
                    .map(ToOwned::to_owned)
                    .or(Some("global".into())),
                None => Some("global".into()),
            }
        }
        _ => None,
    };
    Ok((canonical, file, project))
}

fn memory_roots(homes: &Homes, agent: Agent) -> Result<Vec<PathBuf>> {
    let roots = match agent {
        Agent::Claude => vec![homes.claude.join("projects")],
        Agent::Codex => vec![homes.codex.join("memories")],
        Agent::Grok => vec![homes.grok.join("memory")],
        Agent::Cursor | Agent::OpenCode | Agent::Gemini | Agent::Copilot => {
            return Err(Error::msg(format!(
                "{agent} has no first-party memory store"
            )));
        }
    };
    let canonical: Vec<PathBuf> = roots
        .into_iter()
        .filter_map(|root| root.canonicalize().ok())
        .collect();
    if canonical.is_empty() {
        return Err(Error::msg("memory root is unavailable"));
    }
    Ok(canonical)
}

fn memory_filename(file: &str) -> Result<String> {
    let file = file.trim();
    if file.is_empty() || file.contains('/') || file.contains('\\') {
        return Err(Error::msg("file must be a markdown basename"));
    }
    let name = if file.ends_with(".md") {
        file.to_string()
    } else if file.contains('.') {
        return Err(Error::msg("file must be a markdown basename"));
    } else {
        format!("{file}.md")
    };
    if name == ".md" || name.starts_with('.') {
        return Err(Error::msg("file must be a markdown basename"));
    }
    let path = Path::new(&name);
    if path.file_name().and_then(|name| name.to_str()) != Some(name.as_str())
        || path.extension().and_then(|ext| ext.to_str()) != Some("md")
    {
        return Err(Error::msg("file must be a markdown basename"));
    }
    Ok(name)
}

fn default_filename(content: &str) -> String {
    if let Some(slug) = slug_from_content(content) {
        format!("{slug}.md")
    } else {
        format!("note-{}.md", Utc::now().format("%Y%m%dT%H%M%SZ"))
    }
}

fn slug_from_content(content: &str) -> Option<String> {
    let line = content
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())?;
    let line = line.trim_start_matches('#').trim();
    let mut slug = String::new();
    let mut dash = false;
    for ch in line.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            dash = false;
        } else if !slug.is_empty() && !dash {
            slug.push('-');
            dash = true;
        }
        if slug.len() >= 64 {
            break;
        }
    }
    let slug = slug.trim_end_matches('-');
    if slug.is_empty() {
        None
    } else {
        Some(slug.to_owned())
    }
}

fn project_component(project: &str) -> Result<String> {
    let project = project.trim();
    let path = Path::new(project);
    if project.is_empty()
        || project == "."
        || project.eq_ignore_ascii_case(".git")
        || project.contains('/')
        || project.contains('\\')
        || path.components().count() != 1
        || matches!(
            path.components().next(),
            Some(Component::ParentDir | Component::CurDir | Component::RootDir)
        )
    {
        return Err(Error::msg("project must be a single path component"));
    }
    Ok(project.to_string())
}

fn encode_claude_cwd(cwd: &str) -> Result<String> {
    let cwd = cwd.trim().trim_end_matches(['/', '\\']);
    if cwd.is_empty() {
        return Err(Error::msg("cwd is required"));
    }
    if Path::new(cwd)
        .components()
        .any(|component| matches!(component, Component::ParentDir))
        || cwd.split(['/', '\\']).any(|part| part == "..")
    {
        return Err(Error::msg("cwd is not a valid Claude project path"));
    }
    let slug: String = cwd
        .chars()
        .map(|ch| match ch {
            '/' | '\\' | ':' => '-',
            other => other,
        })
        .collect();
    project_component(&slug).map_err(|_| Error::msg("cwd is not a valid Claude project path"))
}

fn create_claude(
    homes: &Homes,
    content: &str,
    file: &str,
    project: Option<&str>,
    cwd: Option<&str>,
) -> Result<MemoryCreated> {
    let slug = match (project, cwd) {
        (Some(project), _) if !project.trim().is_empty() => project_component(project)?,
        (_, Some(cwd)) if !cwd.trim().is_empty() => encode_claude_cwd(cwd)?,
        _ => return Err(Error::msg("project or cwd is required for Claude")),
    };
    let root = homes.claude.join("projects").join(&slug).join("memory");
    let path = root.join(file);
    write_memory_file(&path, &root, content)?;
    Ok(MemoryCreated {
        agent: Agent::Claude,
        path,
        file: file.to_string(),
        project: Some(slug),
        created: true,
    })
}

fn create_codex(homes: &Homes, content: &str, file: &str) -> Result<MemoryCreated> {
    let root = homes.codex.join("memories");
    let path = root.join(file);
    write_memory_file(&path, &root, content)?;
    Ok(MemoryCreated {
        agent: Agent::Codex,
        path,
        file: file.to_string(),
        project: None,
        created: true,
    })
}

fn create_grok(
    homes: &Homes,
    content: &str,
    file: &str,
    project: Option<&str>,
) -> Result<MemoryCreated> {
    let root = homes.grok.join("memory");
    let (path, project) = match project {
        Some(project) if !project.trim().is_empty() => {
            let project = project_component(project)?;
            (root.join(&project).join(file), Some(project))
        }
        _ => (root.join(file), Some("global".into())),
    };
    write_memory_file(&path, &root, content)?;
    Ok(MemoryCreated {
        agent: Agent::Grok,
        path,
        file: file.to_string(),
        project,
        created: true,
    })
}

fn write_memory_file(path: &Path, root: &Path, content: &str) -> Result<()> {
    if path
        .components()
        .any(|component| component.as_os_str() == ".git")
    {
        return Err(Error::msg("refusing to write under .git"));
    }
    fs::create_dir_all(root).map_err(|source| Error::Io {
        path: root.to_path_buf(),
        source,
    })?;
    let canonical_root = root.canonicalize().map_err(|source| Error::Io {
        path: root.to_path_buf(),
        source,
    })?;
    let parent = path
        .parent()
        .ok_or_else(|| Error::msg("refusing to write outside the memory root"))?;
    fs::create_dir_all(parent).map_err(|source| Error::Io {
        path: parent.to_path_buf(),
        source,
    })?;
    let canonical_parent = parent.canonicalize().map_err(|source| Error::Io {
        path: parent.to_path_buf(),
        source,
    })?;
    if !canonical_parent.starts_with(&canonical_root) {
        return Err(Error::msg("refusing to write outside the memory root"));
    }
    let mut file = match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(file) => file,
        Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {
            return Err(Error::msg(format!(
                "memory file already exists: {}",
                path.display()
            )));
        }
        Err(source) => {
            return Err(Error::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    write_exclusive_contents(path, &mut file, content)
}

fn write_exclusive_contents(path: &Path, file: &mut impl Write, content: &str) -> Result<()> {
    if let Err(source) = file
        .write_all(content.as_bytes())
        .and_then(|_| file.flush())
    {
        let _ = fs::remove_file(path);
        return Err(Error::Io {
            path: path.to_path_buf(),
            source,
        });
    }
    Ok(())
}

fn include(filter: Option<Agent>, agent: Agent) -> bool {
    filter.is_none() || filter == Some(agent)
}

fn collect_claude(homes: &Homes, needle: &str, hits: &mut Vec<MemoryHit>) {
    let projects = homes.claude.join("projects");
    let Ok(entries) = fs::read_dir(&projects) else {
        return;
    };
    for entry in entries.flatten() {
        let project_dir = entry.path();
        if !project_dir.is_dir() {
            continue;
        }
        let Some(slug) = project_dir
            .file_name()
            .and_then(|name| name.to_str())
            .map(ToOwned::to_owned)
        else {
            continue;
        };
        let memory = project_dir.join("memory");
        let Ok(files) = fs::read_dir(&memory) else {
            continue;
        };
        for file in files.flatten() {
            let path = file.path();
            if let Some(hit) = hit(Agent::Claude, path, Some(slug.clone()), needle) {
                hits.push(hit);
            }
        }
    }
}

fn collect_codex(homes: &Homes, needle: &str, hits: &mut Vec<MemoryHit>) {
    collect_markdown_tree(
        &homes.codex.join("memories"),
        Agent::Codex,
        None,
        needle,
        hits,
        true,
    );
}

fn collect_grok(homes: &Homes, needle: &str, hits: &mut Vec<MemoryHit>) {
    let root = homes.grok.join("memory");
    if !root.is_dir() {
        return;
    }
    let Ok(entries) = fs::read_dir(&root) else {
        if let Some(hit) = hit(
            Agent::Grok,
            root.join("MEMORY.md"),
            Some("global".into()),
            needle,
        ) {
            hits.push(hit);
        }
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let Some(slug) = path
                .file_name()
                .and_then(|name| name.to_str())
                .map(ToOwned::to_owned)
            else {
                continue;
            };
            collect_markdown_tree(&path, Agent::Grok, Some(slug), needle, hits, false);
            continue;
        }
        if let Some(hit) = hit(Agent::Grok, path, Some("global".into()), needle) {
            hits.push(hit);
        }
    }
}

fn collect_markdown_tree(
    root: &Path,
    agent: Agent,
    project: Option<String>,
    needle: &str,
    hits: &mut Vec<MemoryHit>,
    skip_git: bool,
) {
    if !root.is_dir() {
        return;
    }
    for entry in WalkDir::new(root)
        .into_iter()
        .filter_entry(|entry| !skip_git || entry.file_name() != ".git")
        .flatten()
    {
        if let Some(hit) = hit(agent, entry.path().to_path_buf(), project.clone(), needle) {
            hits.push(hit);
        }
    }
}

fn hit(agent: Agent, path: PathBuf, project: Option<String>, needle: &str) -> Option<MemoryHit> {
    if !path.is_file() || path.extension().and_then(|ext| ext.to_str()) != Some("md") {
        return None;
    }
    let (matches, snippet) = scan_file(&path, needle)?;
    let file = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    Some(MemoryHit {
        agent,
        path,
        file,
        project,
        matches,
        snippet,
    })
}

#[cfg(test)]
mod tests {
    use super::{create_memory, search_memories};
    use crate::homes::Homes;
    use crate::model::Agent;
    use crate::transcript::{extract_snippet, scan_file};
    use std::fs;
    use std::path::Path;

    fn write(path: &Path, body: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, body).unwrap();
    }

    #[test]
    fn empty_query_is_required() {
        let homes = Homes::isolated(tempfile::tempdir().unwrap().path());
        for query in ["", "   ", "\t\n"] {
            let err = search_memories(&homes, query, None, 10).unwrap_err();
            assert!(
                err.to_string().contains("query is required"),
                "{query:?} -> {err}"
            );
        }
    }

    #[test]
    fn skips_missing_roots() {
        let homes = Homes::isolated(tempfile::tempdir().unwrap().path());
        let hits = search_memories(&homes, "anything", None, 10).unwrap();
        assert!(hits.is_empty());
    }

    #[test]
    fn cursor_and_opencode_return_empty() {
        let dir = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(dir.path());
        write(
            &homes.cursor.join("projects").join("x").join("MEMORY.md"),
            "cursor note needle",
        );
        write(
            &homes.opencode.join("memory").join("MEMORY.md"),
            "opencode note needle",
        );
        write(&homes.gemini.join("GEMINI.md"), "gemini note needle");
        write(
            &homes.copilot.join("memory").join("MEMORY.md"),
            "copilot note needle",
        );
        write(
            &homes
                .claude
                .join("projects")
                .join("tmp-dr")
                .join("memory")
                .join("MEMORY.md"),
            "claude note needle",
        );
        assert!(
            search_memories(&homes, "needle", Some(Agent::Cursor), 10)
                .unwrap()
                .is_empty()
        );
        assert!(
            search_memories(&homes, "needle", Some(Agent::OpenCode), 10)
                .unwrap()
                .is_empty()
        );
        assert!(
            search_memories(&homes, "needle", Some(Agent::Gemini), 10)
                .unwrap()
                .is_empty()
        );
        assert!(
            search_memories(&homes, "needle", Some(Agent::Copilot), 10)
                .unwrap()
                .is_empty()
        );
        let all = search_memories(&homes, "needle", None, 10).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].agent, Agent::Claude);
    }

    #[test]
    fn skips_codex_git_and_filters_agent() {
        let dir = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(dir.path());
        write(
            &homes.codex.join("memories").join("MEMORY.md"),
            "codex visible needle\n",
        );
        write(
            &homes
                .codex
                .join("memories")
                .join("topic")
                .join("feedback_foo.md"),
            "codex topic needle\n",
        );
        write(
            &homes.codex.join("memories").join(".git").join("hidden.md"),
            "codex hidden needle\n",
        );
        write(
            &homes
                .claude
                .join("projects")
                .join("tmp-dr")
                .join("memory")
                .join("MEMORY.md"),
            "claude visible needle\n",
        );
        let hits = search_memories(&homes, "needle", Some(Agent::Codex), 10).unwrap();
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().all(|hit| hit.agent == Agent::Codex));
        assert!(hits.iter().all(|hit| hit.project.is_none()));
        assert!(
            hits.iter()
                .all(|hit| !hit.path.components().any(|part| part.as_os_str() == ".git"))
        );
        assert!(hits.iter().any(|hit| hit.file == "feedback_foo.md"));
    }

    #[test]
    fn sorts_by_matches_then_path_and_respects_limit() {
        let dir = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(dir.path());
        write(&homes.codex.join("memories").join("a.md"), "needle once\n");
        write(
            &homes.codex.join("memories").join("b.md"),
            "needle\nneedle\nneedle\n",
        );
        write(
            &homes.codex.join("memories").join("c.md"),
            "needle twice\nneedle\n",
        );
        let limited = search_memories(&homes, "needle", None, 1).unwrap();
        assert_eq!(limited.len(), 1);
        assert_eq!(limited[0].file, "b.md");
        assert_eq!(limited[0].matches, 3);
        let ranked = search_memories(&homes, "needle", None, 10).unwrap();
        assert_eq!(
            ranked
                .iter()
                .map(|hit| (hit.file.as_str(), hit.matches))
                .collect::<Vec<_>>(),
            vec![("b.md", 3), ("c.md", 2), ("a.md", 1)]
        );
        assert!(
            search_memories(&homes, "needle", None, 0)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn filters_each_supporting_agent() {
        let dir = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(dir.path());
        write(
            &homes
                .claude
                .join("projects")
                .join("tmp-dr")
                .join("memory")
                .join("MEMORY.md"),
            "shared agent needle claude\n",
        );
        write(
            &homes.codex.join("memories").join("MEMORY.md"),
            "shared agent needle codex\n",
        );
        write(
            &homes.grok.join("memory").join("MEMORY.md"),
            "shared agent needle grok\n",
        );
        let claude =
            search_memories(&homes, "shared agent needle", Some(Agent::Claude), 10).unwrap();
        assert_eq!(claude.len(), 1);
        assert_eq!(claude[0].agent, Agent::Claude);
        assert_eq!(claude[0].project.as_deref(), Some("tmp-dr"));
        let codex = search_memories(&homes, "shared agent needle", Some(Agent::Codex), 10).unwrap();
        assert_eq!(codex.len(), 1);
        assert_eq!(codex[0].agent, Agent::Codex);
        let grok = search_memories(&homes, "shared agent needle", Some(Agent::Grok), 10).unwrap();
        assert_eq!(grok.len(), 1);
        assert_eq!(grok[0].agent, Agent::Grok);
        assert_eq!(grok[0].project.as_deref(), Some("global"));
        assert!(
            search_memories(&homes, "shared agent needle", Some(Agent::Cursor), 10)
                .unwrap()
                .is_empty()
        );
        assert!(
            search_memories(&homes, "shared agent needle", Some(Agent::OpenCode), 10)
                .unwrap()
                .is_empty()
        );
        assert!(
            search_memories(&homes, "shared agent needle", Some(Agent::Gemini), 10)
                .unwrap()
                .is_empty()
        );
        assert!(
            search_memories(&homes, "shared agent needle", Some(Agent::Copilot), 10)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn claude_skips_non_dirs_missing_memory_and_non_md() {
        let dir = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(dir.path());
        let projects = homes.claude.join("projects");
        write(
            &projects.join("not-a-project"),
            "claude skip needle in a file project entry\n",
        );
        write(
            &projects.join("no-memory").join("README.md"),
            "claude skip needle without a memory dir\n",
        );
        write(
            &projects.join("memory-is-file").join("memory"),
            "claude skip needle when memory is a file\n",
        );
        let memory = projects.join("tmp-dr").join("memory");
        write(&memory.join("note.txt"), "claude skip needle txt\n");
        write(
            &memory.join("nomatch.md"),
            "unrelated note without the phrase\n",
        );
        write(&memory.join("MEMORY"), "claude skip needle no extension\n");
        write(
            &memory.join("nested").join("deep.md"),
            "claude skip needle nested should stay unseen\n",
        );
        write(&memory.join("hit.md"), "claude skip needle visible\n");
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            let grok_root = homes.grok.join("memory");
            fs::create_dir_all(&grok_root).unwrap();
            let _ = fs::create_dir(grok_root.join(std::ffi::OsString::from_vec(vec![0xff, 0x80])));
            let _ = search_memories(&homes, "claude skip needle", Some(Agent::Grok), 10);
        }
        let hits = search_memories(&homes, "claude skip needle", Some(Agent::Claude), 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].file, "hit.md");
        assert_eq!(hits[0].project.as_deref(), Some("tmp-dr"));
        assert_eq!(hits[0].agent, Agent::Claude);
    }

    #[test]
    fn missing_codex_or_grok_roots_do_not_hide_other_agents() {
        let dir = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(dir.path());
        write(&homes.claude.join("projects"), "not a directory\n");
        assert!(
            search_memories(&homes, "cross root needle", Some(Agent::Claude), 10)
                .unwrap()
                .is_empty()
        );
        fs::remove_file(homes.claude.join("projects")).unwrap();
        write(
            &homes
                .claude
                .join("projects")
                .join("tmp-dr")
                .join("memory")
                .join("MEMORY.md"),
            "cross root needle claude\n",
        );
        assert!(
            search_memories(&homes, "cross root needle", Some(Agent::Codex), 10)
                .unwrap()
                .is_empty()
        );
        assert!(
            search_memories(&homes, "cross root needle", Some(Agent::Grok), 10)
                .unwrap()
                .is_empty()
        );
        write(
            &homes.codex.join("memories").join("MEMORY.md"),
            "cross root needle codex\n",
        );
        write(&homes.grok.join("memory").join("other.md"), "loose only\n");
        fs::create_dir_all(homes.grok.join("memory").join("empty-workspace")).unwrap();
        let all = search_memories(&homes, "cross root needle", None, 10).unwrap();
        assert_eq!(all.len(), 2);
        assert!(all.iter().any(|hit| hit.agent == Agent::Claude));
        assert!(all.iter().any(|hit| hit.agent == Agent::Codex));
        assert!(
            search_memories(&homes, "cross root needle", Some(Agent::Grok), 10)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn grok_global_and_workspace_and_shared_snippet() {
        let dir = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(dir.path());
        let global = homes.grok.join("memory").join("MEMORY.md");
        let nested = homes
            .grok
            .join("memory")
            .join("tmp-edge")
            .join("sub")
            .join("notes.md");
        write(
            &global,
            "loose ignored.md should not match\nGROK_SNIPPET_NEEDLE global\n",
        );
        write(
            &homes.grok.join("memory").join("other.md"),
            "GROK_SNIPPET_NEEDLE loose global note\n",
        );
        write(&nested, "workspace GROK_SNIPPET_NEEDLE note\n");
        write(
            &homes
                .claude
                .join("projects")
                .join("tmp-dr")
                .join("memory")
                .join("nested")
                .join("deep.md"),
            "GROK_SNIPPET_NEEDLE nested claude should stay unseen\n",
        );
        assert!(
            search_memories(&homes, "GROK_SNIPPET_NEEDLE", Some(Agent::Claude), 10)
                .unwrap()
                .is_empty(),
            "Claude scans memory/*.md flat, not nested trees"
        );
        let hits = search_memories(&homes, "GROK_SNIPPET_NEEDLE", Some(Agent::Grok), 10).unwrap();
        assert_eq!(hits.len(), 3);
        let global_hit = hits
            .iter()
            .find(|hit| hit.file == "MEMORY.md")
            .expect("global");
        assert_eq!(global_hit.project.as_deref(), Some("global"));
        let loose = hits
            .iter()
            .find(|hit| hit.file == "other.md")
            .expect("loose");
        assert_eq!(loose.project.as_deref(), Some("global"));
        let workspace = hits
            .iter()
            .find(|hit| hit.file == "notes.md")
            .expect("workspace");
        assert_eq!(workspace.project.as_deref(), Some("tmp-edge"));
        let (matches, snippet) = scan_file(&global, "grok_snippet_needle").unwrap();
        assert_eq!(global_hit.matches, matches);
        assert_eq!(global_hit.snippet, snippet);
        assert!(
            extract_snippet("prefix GROK_SNIPPET_NEEDLE suffix", "grok_snippet_needle")
                .contains("GROK_SNIPPET_NEEDLE")
        );
    }

    #[cfg(unix)]
    #[test]
    fn grok_reads_global_when_memory_dir_is_unlistable() {
        use std::os::unix::fs::PermissionsExt;

        struct Restore {
            path: std::path::PathBuf,
            mode: u32,
        }
        impl Drop for Restore {
            fn drop(&mut self) {
                if let Ok(metadata) = fs::metadata(&self.path) {
                    let mut permissions = metadata.permissions();
                    permissions.set_mode(self.mode);
                    let _ = fs::set_permissions(&self.path, permissions);
                }
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(dir.path());
        let root = homes.grok.join("memory");
        write(&root.join("MEMORY.md"), "unlistable global needle\n");
        write(
            &root.join("tmp-edge").join("notes.md"),
            "unlistable workspace needle\n",
        );
        let mut permissions = fs::metadata(&root).unwrap().permissions();
        permissions.set_mode(0o111);
        fs::set_permissions(&root, permissions).unwrap();
        let _restore = Restore {
            path: root,
            mode: 0o755,
        };
        let hits = search_memories(&homes, "needle", Some(Agent::Grok), 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].project.as_deref(), Some("global"));
        assert_eq!(hits[0].file, "MEMORY.md");
    }

    #[test]
    fn create_memory_requires_content() {
        let homes = Homes::isolated(tempfile::tempdir().unwrap().path());
        for content in ["", "   ", "\t\n"] {
            let err = create_memory(&homes, Agent::Codex, content, Some("note.md"), None, None)
                .unwrap_err();
            assert!(
                err.to_string().contains("content is required"),
                "{content:?} -> {err}"
            );
        }
    }

    #[test]
    fn create_memory_writes_claude_codex_and_grok() {
        let dir = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(dir.path());
        let claude = create_memory(
            &homes,
            Agent::Claude,
            "CLAUDE_CREATE_NEEDLE dedicated db gaps",
            Some("dedicated-db-gaps.md"),
            Some("tmp-dr"),
            None,
        )
        .unwrap();
        assert!(claude.created);
        assert_eq!(claude.agent, Agent::Claude);
        assert_eq!(claude.file, "dedicated-db-gaps.md");
        assert_eq!(claude.project.as_deref(), Some("tmp-dr"));
        assert_eq!(
            claude.path,
            homes
                .claude
                .join("projects")
                .join("tmp-dr")
                .join("memory")
                .join("dedicated-db-gaps.md")
        );
        assert_eq!(
            fs::read_to_string(&claude.path).unwrap(),
            "CLAUDE_CREATE_NEEDLE dedicated db gaps"
        );

        let codex = create_memory(
            &homes,
            Agent::Codex,
            "CODEX_CREATE_NEEDLE billing cache",
            Some("billing-cache"),
            Some("unused-project"),
            None,
        )
        .unwrap();
        assert_eq!(codex.agent, Agent::Codex);
        assert_eq!(codex.file, "billing-cache.md");
        assert!(codex.project.is_none());
        assert_eq!(
            codex.path,
            homes.codex.join("memories").join("billing-cache.md")
        );

        let grok_project = create_memory(
            &homes,
            Agent::Grok,
            "GROK_PROJECT_CREATE_NEEDLE workspace note",
            Some("workspace-note.md"),
            Some("tmp-edge"),
            None,
        )
        .unwrap();
        assert_eq!(grok_project.project.as_deref(), Some("tmp-edge"));
        assert_eq!(
            grok_project.path,
            homes
                .grok
                .join("memory")
                .join("tmp-edge")
                .join("workspace-note.md")
        );

        let grok_global = create_memory(
            &homes,
            Agent::Grok,
            "GROK_GLOBAL_CREATE_NEEDLE unique note",
            Some("unique-note.md"),
            None,
            None,
        )
        .unwrap();
        assert_eq!(grok_global.project.as_deref(), Some("global"));
        assert_eq!(
            grok_global.path,
            homes.grok.join("memory").join("unique-note.md")
        );
        assert_ne!(grok_global.file, "MEMORY.md");

        for (query, agent, file) in [
            (
                "CLAUDE_CREATE_NEEDLE",
                Agent::Claude,
                "dedicated-db-gaps.md",
            ),
            ("CODEX_CREATE_NEEDLE", Agent::Codex, "billing-cache.md"),
            (
                "GROK_PROJECT_CREATE_NEEDLE",
                Agent::Grok,
                "workspace-note.md",
            ),
            ("GROK_GLOBAL_CREATE_NEEDLE", Agent::Grok, "unique-note.md"),
        ] {
            let hits = search_memories(&homes, query, Some(agent), 10).unwrap();
            assert_eq!(hits.len(), 1, "{query}");
            assert_eq!(hits[0].file, file);
            assert_eq!(hits[0].agent, agent);
        }
    }

    #[test]
    fn create_memory_encodes_claude_cwd() {
        let homes = Homes::isolated(tempfile::tempdir().unwrap().path());
        let created = create_memory(
            &homes,
            Agent::Claude,
            "cwd encoded note",
            Some("cwd-note.md"),
            None,
            Some("/Users/foo/bar/"),
        )
        .unwrap();
        assert_eq!(created.project.as_deref(), Some("-Users-foo-bar"));
        assert_eq!(
            created.path,
            homes
                .claude
                .join("projects")
                .join("-Users-foo-bar")
                .join("memory")
                .join("cwd-note.md")
        );
        let err = create_memory(
            &homes,
            Agent::Claude,
            "missing target",
            Some("missing.md"),
            None,
            None,
        )
        .unwrap_err();
        assert!(err.to_string().contains("project or cwd is required"));
        let err = create_memory(
            &homes,
            Agent::Claude,
            "blank cwd",
            Some("blank.md"),
            Some("   "),
            Some("   "),
        )
        .unwrap_err();
        assert!(err.to_string().contains("project or cwd is required"));
        let err = create_memory(
            &homes,
            Agent::Claude,
            "root cwd",
            Some("root.md"),
            None,
            Some("///"),
        )
        .unwrap_err();
        assert!(err.to_string().contains("cwd is required"));
        let err = create_memory(
            &homes,
            Agent::Claude,
            "dotdot cwd",
            Some("escape.md"),
            None,
            Some("/Users/foo/../etc"),
        )
        .unwrap_err();
        assert!(err.to_string().contains("not a valid Claude project path"));
    }

    #[test]
    fn create_memory_rejects_cursor_opencode_overwrite_and_escapes() {
        let dir = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(dir.path());
        for agent in [
            Agent::Cursor,
            Agent::OpenCode,
            Agent::Gemini,
            Agent::Copilot,
        ] {
            let err = create_memory(&homes, agent, "should fail", Some("note.md"), None, None)
                .unwrap_err();
            assert!(
                err.to_string().contains("no first-party memory store"),
                "{agent} -> {err}"
            );
        }

        create_memory(
            &homes,
            Agent::Codex,
            "first write",
            Some("exists.md"),
            None,
            None,
        )
        .unwrap();
        let err = create_memory(
            &homes,
            Agent::Codex,
            "second write",
            Some("exists.md"),
            None,
            None,
        )
        .unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err}");

        for file in [
            "",
            "   ",
            "../secret.md",
            "foo/bar.md",
            "foo\\bar.md",
            "note.txt",
            ".hidden.md",
            ".md",
            "note.",
        ] {
            let err =
                create_memory(&homes, Agent::Codex, "escape", Some(file), None, None).unwrap_err();
            assert!(
                err.to_string().contains("markdown basename"),
                "{file:?} -> {err}"
            );
        }

        for project in ["..", "../escape", "foo/bar", "foo\\bar", ".", ".git"] {
            let err = create_memory(
                &homes,
                Agent::Grok,
                "escape project",
                Some("ok.md"),
                Some(project),
                None,
            )
            .unwrap_err();
            assert!(
                err.to_string().contains("single path component"),
                "{project:?} -> {err}"
            );
        }
    }

    #[test]
    fn create_memory_accepts_consecutive_dots_in_names() {
        let dir = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(dir.path());
        let grok = create_memory(
            &homes,
            Agent::Grok,
            "dotted project note",
            Some("note..v2.md"),
            Some("foo..bar"),
            None,
        )
        .unwrap();
        assert_eq!(grok.file, "note..v2.md");
        assert_eq!(grok.project.as_deref(), Some("foo..bar"));
        assert_eq!(
            grok.path,
            homes
                .grok
                .join("memory")
                .join("foo..bar")
                .join("note..v2.md")
        );
        assert_eq!(
            fs::read_to_string(&grok.path).unwrap(),
            "dotted project note"
        );

        let claude = create_memory(
            &homes,
            Agent::Claude,
            "dotted cwd note",
            Some("cwd..note.md"),
            None,
            Some("/Users/foo..bar/proj"),
        )
        .unwrap();
        assert_eq!(claude.project.as_deref(), Some("-Users-foo..bar-proj"));
        assert_eq!(claude.file, "cwd..note.md");
        assert_eq!(
            claude.path,
            homes
                .claude
                .join("projects")
                .join("-Users-foo..bar-proj")
                .join("memory")
                .join("cwd..note.md")
        );
    }

    #[test]
    fn create_memory_removes_destination_when_write_fails() {
        use super::write_exclusive_contents;
        use std::io::{self, Write};

        struct FailingWrite;
        impl Write for FailingWrite {
            fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
                Err(io::Error::other("disk full"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        assert!(FailingWrite.flush().is_ok());

        struct FlushFails;
        impl Write for FlushFails {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                Ok(buf.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Err(io::Error::other("flush failed"))
            }
        }

        let dir = tempfile::tempdir().unwrap();
        for (mut writer, label) in [
            (Box::new(FailingWrite) as Box<dyn Write>, "write"),
            (Box::new(FlushFails) as Box<dyn Write>, "flush"),
        ] {
            let path = dir.path().join(format!("{label}.md"));
            fs::write(&path, "partial").unwrap();
            let err = write_exclusive_contents(&path, &mut writer, "hello").unwrap_err();
            assert!(
                err.to_string().contains("failed to read"),
                "{label} -> {err}"
            );
            assert!(
                !path.exists(),
                "{label} should remove the destination so create_new can retry"
            );
        }

        let homes = Homes::isolated(dir.path());
        let created = create_memory(
            &homes,
            Agent::Codex,
            "retry after failed write",
            Some("write.md"),
            None,
            None,
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(&created.path).unwrap(),
            "retry after failed write"
        );
    }

    #[test]
    fn create_memory_defaults_filename_from_title_or_utc() {
        let homes = Homes::isolated(tempfile::tempdir().unwrap().path());
        let slugged = create_memory(
            &homes,
            Agent::Codex,
            "# Dedicated DB gaps\n\nbody",
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(slugged.file, "dedicated-db-gaps.md");
        let stamped = create_memory(&homes, Agent::Codex, "!!!\n***", None, None, None).unwrap();
        assert!(
            stamped.file.starts_with("note-") && stamped.file.ends_with(".md"),
            "{}",
            stamped.file
        );
        assert_ne!(stamped.file, "MEMORY.md");
    }

    #[test]
    fn create_memory_reports_io_when_root_is_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(dir.path());
        write(&homes.codex.join("memories"), "not a directory\n");
        let err = create_memory(
            &homes,
            Agent::Codex,
            "cannot create",
            Some("blocked.md"),
            None,
            None,
        )
        .unwrap_err();
        assert!(err.to_string().contains("failed to read"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn create_memory_rejects_symlink_escape_and_unwritable_roots() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(dir.path());
        let root = homes.grok.join("memory");
        let outside = dir.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::create_dir_all(&root).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("escape")).unwrap();
        let err = create_memory(
            &homes,
            Agent::Grok,
            "escaped",
            Some("note.md"),
            Some("escape"),
            None,
        )
        .unwrap_err();
        assert!(err.to_string().contains("outside the memory root"), "{err}");

        let memories = homes.codex.join("memories");
        fs::create_dir_all(&memories).unwrap();
        let mut permissions = fs::metadata(&memories).unwrap().permissions();
        permissions.set_mode(0o555);
        fs::set_permissions(&memories, permissions).unwrap();
        let err = create_memory(
            &homes,
            Agent::Codex,
            "unwritable root",
            Some("blocked.md"),
            None,
            None,
        )
        .unwrap_err();
        assert!(err.to_string().contains("failed to read"), "{err}");
        let mut permissions = fs::metadata(&memories).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&memories, permissions).unwrap();
    }

    #[test]
    fn read_memory_by_file_path_and_rejects_escape() {
        use super::read_memory;

        let dir = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(dir.path());
        let created = create_memory(
            &homes,
            Agent::Codex,
            "READ_MEMORY_NEEDLE dedicated db\n",
            Some("read-me.md"),
            None,
            None,
        )
        .unwrap();
        let by_file = read_memory(
            &homes,
            Agent::Codex,
            Some("read-me.md"),
            None,
            None,
            None,
            8000,
        )
        .unwrap();
        assert!(by_file.inert);
        assert!(!by_file.truncated);
        assert!(by_file.content.contains("READ_MEMORY_NEEDLE"));
        assert_eq!(by_file.file, "read-me.md");

        let by_path = read_memory(
            &homes,
            Agent::Codex,
            None,
            None,
            None,
            Some(created.path.to_str().unwrap()),
            8,
        )
        .unwrap();
        assert!(by_path.truncated);
        assert!(by_path.content.ends_with("..."));

        let claude = create_memory(
            &homes,
            Agent::Claude,
            "claude read note",
            Some("topic.md"),
            Some("tmp-dr"),
            None,
        )
        .unwrap();
        let read = read_memory(
            &homes,
            Agent::Claude,
            Some("topic.md"),
            Some("tmp-dr"),
            None,
            None,
            0,
        )
        .unwrap();
        assert_eq!(read.project.as_deref(), Some("tmp-dr"));
        let via_path = read_memory(
            &homes,
            Agent::Claude,
            None,
            None,
            None,
            Some(claude.path.to_str().unwrap()),
            100,
        )
        .unwrap();
        assert_eq!(via_path.file, "topic.md");

        let grok = create_memory(
            &homes,
            Agent::Grok,
            "grok read note",
            Some("g.md"),
            Some("tmp-edge"),
            None,
        )
        .unwrap();
        let grok_read = read_memory(
            &homes,
            Agent::Grok,
            None,
            None,
            None,
            Some(grok.path.to_str().unwrap()),
            100,
        )
        .unwrap();
        assert_eq!(grok_read.project.as_deref(), Some("tmp-edge"));

        let missing =
            read_memory(&homes, Agent::Codex, Some("nope.md"), None, None, None, 10).unwrap_err();
        assert!(missing.to_string().contains("not found"));

        let outside = dir.path().join("outside.md");
        fs::write(&outside, "secret\n").unwrap();
        let err = read_memory(
            &homes,
            Agent::Codex,
            None,
            None,
            None,
            Some(outside.to_str().unwrap()),
            10,
        )
        .unwrap_err();
        assert!(err.to_string().contains("outside the memory root"));

        let err =
            read_memory(&homes, Agent::Cursor, Some("note.md"), None, None, None, 10).unwrap_err();
        assert!(err.to_string().contains("no first-party memory store"));
        let err =
            read_memory(&homes, Agent::Gemini, Some("note.md"), None, None, None, 10).unwrap_err();
        assert!(err.to_string().contains("no first-party memory store"));
        let err = read_memory(
            &homes,
            Agent::Copilot,
            Some("note.md"),
            None,
            None,
            None,
            10,
        )
        .unwrap_err();
        assert!(err.to_string().contains("no first-party memory store"));

        let err = read_memory(&homes, Agent::Claude, None, None, None, None, 10).unwrap_err();
        assert!(err.to_string().contains("file or path is required"));
        let err = read_memory(
            &homes,
            Agent::Claude,
            Some("topic.md"),
            None,
            None,
            None,
            10,
        )
        .unwrap_err();
        assert!(err.to_string().contains("project or cwd is required"));

        create_memory(
            &homes,
            Agent::Grok,
            "global read note",
            Some("global-read.md"),
            None,
            None,
        )
        .unwrap();
        let global = read_memory(
            &homes,
            Agent::Grok,
            Some("global-read.md"),
            None,
            None,
            None,
            100,
        )
        .unwrap();
        assert_eq!(global.project.as_deref(), Some("global"));
        assert!(global.content.contains("global read note"));

        let via_cwd = create_memory(
            &homes,
            Agent::Claude,
            "cwd read note",
            Some("cwd-topic.md"),
            None,
            Some("/tmp/dr"),
        )
        .unwrap();
        let cwd_read = read_memory(
            &homes,
            Agent::Claude,
            Some("cwd-topic.md"),
            None,
            Some("/tmp/dr"),
            None,
            100,
        )
        .unwrap();
        assert_eq!(cwd_read.path, via_cwd.path);

        let grok_file = read_memory(
            &homes,
            Agent::Grok,
            Some("g.md"),
            Some("tmp-edge"),
            None,
            None,
            100,
        )
        .unwrap();
        assert_eq!(grok_file.project.as_deref(), Some("tmp-edge"));

        let grok_global_path = read_memory(
            &homes,
            Agent::Grok,
            None,
            None,
            None,
            Some(global.path.to_str().unwrap()),
            100,
        )
        .unwrap();
        assert_eq!(grok_global_path.project.as_deref(), Some("global"));

        let missing_path = dir.path().join("missing.md");
        let err = read_memory(
            &homes,
            Agent::Codex,
            None,
            None,
            None,
            Some(missing_path.to_str().unwrap()),
            10,
        )
        .unwrap_err();
        assert!(err.to_string().contains("failed to read"), "{err}");

        let txt = dir.path().join("note.txt");
        fs::write(&txt, "nope").unwrap();
        let err = read_memory(
            &homes,
            Agent::Codex,
            None,
            None,
            None,
            Some(txt.to_str().unwrap()),
            10,
        )
        .unwrap_err();
        assert!(err.to_string().contains("markdown"), "{err}");

        let err = read_memory(
            &homes,
            Agent::OpenCode,
            None,
            None,
            None,
            Some(created.path.to_str().unwrap()),
            10,
        )
        .unwrap_err();
        assert!(err.to_string().contains("no first-party memory store"));

        let empty_roots = Homes::isolated(dir.path().join("empty-roots"));
        let stray = dir.path().join("empty-roots").join("stray.md");
        fs::create_dir_all(stray.parent().unwrap()).unwrap();
        fs::write(&stray, "x").unwrap();
        let err = read_memory(
            &empty_roots,
            Agent::Codex,
            None,
            None,
            None,
            Some(stray.to_str().unwrap()),
            10,
        )
        .unwrap_err();
        assert!(err.to_string().contains("memory root is unavailable"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(&created.path).unwrap().permissions();
            permissions.set_mode(0o000);
            fs::set_permissions(&created.path, permissions).unwrap();
            let err = read_memory(
                &homes,
                Agent::Codex,
                Some("read-me.md"),
                None,
                None,
                None,
                10,
            )
            .unwrap_err();
            assert!(err.to_string().contains("failed to read"));
            let mut permissions = fs::metadata(&created.path).unwrap().permissions();
            permissions.set_mode(0o644);
            fs::set_permissions(&created.path, permissions).unwrap();
        }

        let long = format!("# {}\n", "A".repeat(80));
        let stamped = create_memory(&homes, Agent::Codex, &long, None, None, None).unwrap();
        assert!(stamped.file.len() <= 67, "{}", stamped.file);

        let root = homes.codex.join("memories");
        let err =
            super::write_memory_file(&root.join(".git").join("x.md"), &root, "nope").unwrap_err();
        assert!(err.to_string().contains(".git"));

        fs::write(root.join("blocked"), "file").unwrap();
        let err = super::write_memory_file(&root.join("blocked").join("x.md"), &root, "nope")
            .unwrap_err();
        assert!(err.to_string().contains("failed to read"), "{err}");

        let err = read_memory(
            &homes,
            Agent::Claude,
            Some("topic.md"),
            None,
            Some("/tmp/foo/../etc"),
            None,
            10,
        )
        .unwrap_err();
        assert!(err.to_string().contains("cwd is not a valid"), "{err}");
    }
}
