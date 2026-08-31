use crate::error::{Error, Result};
use crate::homes::Homes;
use crate::model::{Agent, MemoryHit};
use crate::transcript::scan_file;
use std::fs;
use std::path::{Path, PathBuf};
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
    let global = root.join("MEMORY.md");
    if let Some(hit) = hit(Agent::Grok, global, Some("global".into()), needle) {
        hits.push(hit);
    }
    let Ok(entries) = fs::read_dir(&root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(slug) = path
            .file_name()
            .and_then(|name| name.to_str())
            .map(ToOwned::to_owned)
        else {
            continue;
        };
        collect_markdown_tree(&path, Agent::Grok, Some(slug), needle, hits, false);
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
    use super::search_memories;
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
            "GROK_SNIPPET_NEEDLE should stay unseen\n",
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
        assert_eq!(hits.len(), 2);
        let global_hit = hits
            .iter()
            .find(|hit| hit.file == "MEMORY.md")
            .expect("global");
        assert_eq!(global_hit.project.as_deref(), Some("global"));
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
}
