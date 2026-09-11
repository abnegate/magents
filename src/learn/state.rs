use crate::error::{Error, Result};
use crate::homes::Homes;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};

pub const STATUSES: &[&str] = &["collected", "running", "report_ready", "curating", "done"];
pub const DECISIONS: &[&str] = &["applied", "rejected", "deferred"];

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct LearnState {
    #[serde(default)]
    pub magents_home: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending: Option<Pending>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_completed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_completed_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sessions_kept: Option<usize>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Pending {
    pub run_dir: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub report_ready: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Clone, Debug)]
pub struct StateUpdate {
    pub run_dir: PathBuf,
    pub status: String,
    pub mode: Option<String>,
    pub scope: Option<String>,
    pub note: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Decision {
    pub run_dir: String,
    pub id: String,
    pub kind: String,
    pub action: String,
    pub target: String,
    pub path: String,
    pub decision: String,
    pub undo: Option<String>,
}

pub fn state_path(homes: &Homes) -> PathBuf {
    homes.learn_dir().join("state.json")
}

pub fn get(homes: &Homes) -> Result<LearnState> {
    let mut state = load(&state_path(homes))?;
    state.magents_home = homes.magents.clone();
    Ok(state)
}

pub fn set(homes: &Homes, update: &StateUpdate) -> Result<LearnState> {
    if !STATUSES.contains(&update.status.as_str()) {
        return Err(Error::msg(format!(
            "status must be one of {}",
            STATUSES.join(", ")
        )));
    }
    let path = state_path(homes);
    let mut state = load(&path)?;
    let now = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let run_dir = update.run_dir.display().to_string();
    let mut pending = state.pending.take().unwrap_or(Pending {
        run_dir: run_dir.clone(),
        status: update.status.clone(),
        started_at: Some(now.clone()),
        updated_at: Some(now.clone()),
        report_ready: false,
        mode: None,
        scope: None,
        note: None,
    });
    if pending.run_dir != run_dir {
        pending = Pending {
            run_dir: run_dir.clone(),
            status: update.status.clone(),
            started_at: Some(now.clone()),
            updated_at: Some(now.clone()),
            report_ready: false,
            mode: None,
            scope: None,
            note: None,
        };
    }
    pending.status = update.status.clone();
    pending.updated_at = Some(now.clone());
    pending.report_ready =
        update.run_dir.join("report.md").is_file() && update.run_dir.join("actions.json").is_file();
    if let Some(mode) = &update.mode {
        pending.mode = Some(mode.clone());
    }
    if let Some(scope) = &update.scope {
        pending.scope = Some(scope.clone());
    }
    if let Some(note) = &update.note {
        pending.note = Some(note.clone());
    }
    if update.status == "done" {
        state.last_completed_at = Some(now);
        state.last_completed_dir = Some(run_dir);
    }
    state.pending = Some(pending);
    state.magents_home = homes.magents.clone();
    save(&path, &state)?;
    Ok(state)
}

pub fn record_collection(
    homes: &Homes,
    run_dir: &Path,
    generated_at: &str,
    kept: usize,
) -> Result<()> {
    let path = state_path(homes);
    let mut state = load(&path)?;
    state.last_run_at = Some(generated_at.to_string());
    state.last_run_dir = Some(run_dir.display().to_string());
    state.sessions_kept = Some(kept);
    if kept > 0 {
        state.pending = Some(Pending {
            run_dir: run_dir.display().to_string(),
            status: "collected".into(),
            started_at: Some(generated_at.to_string()),
            updated_at: Some(generated_at.to_string()),
            report_ready: false,
            mode: None,
            scope: None,
            note: None,
        });
    }
    state.magents_home = homes.magents.clone();
    save(&path, &state)
}

pub fn clear(homes: &Homes) -> Result<LearnState> {
    let path = state_path(homes);
    let mut state = load(&path)?;
    state.pending = None;
    state.magents_home = homes.magents.clone();
    save(&path, &state)?;
    Ok(state)
}

pub fn decide(homes: &Homes, decision: &Decision) -> Result<Value> {
    if !DECISIONS.contains(&decision.decision.as_str()) {
        return Err(Error::msg(
            "decision must be applied, rejected, or deferred",
        ));
    }
    let line = json!({
        "date": Utc::now().date_naive().to_string(),
        "run_dir": decision.run_dir,
        "id": decision.id,
        "kind": decision.kind,
        "action": decision.action,
        "target": decision.target,
        "path": decision.path,
        "decision": decision.decision,
        "undo": decision.undo,
    });
    let path = homes.learn_dir().join("decisions.jsonl");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| io(parent, source))?;
    }
    let mut body = if path.is_file() {
        fs::read_to_string(&path).map_err(|source| io(&path, source))?
    } else {
        String::new()
    };
    if !body.is_empty() && !body.ends_with('\n') {
        body.push('\n');
    }
    body.push_str(&serde_json::to_string(&line)?);
    body.push('\n');
    fs::write(&path, body).map_err(|source| io(path, source))?;
    Ok(line)
}

pub fn trash(homes: &Homes, run_name: &str, paths: &[PathBuf]) -> Result<Value> {
    if run_name.is_empty()
        || run_name == "."
        || run_name == ".."
        || Path::new(run_name).file_name().and_then(|n| n.to_str()) != Some(run_name)
    {
        return Err(Error::msg("run-name must be a single path component"));
    }
    for src in paths {
        if !src.exists() {
            return Err(Error::msg(format!("missing {}", src.display())));
        }
        if !trashable(homes, src) {
            return Err(Error::msg(format!(
                "refusing: only a skills/<name> directory, a workflows/*.rhai file, or a hooks/* file under an agent home, ~/.agents, or a project .grok/.claude/.cursor directory can be trashed: {}",
                src.display()
            )));
        }
    }
    let dest_dir = homes.learn_dir().join("trash").join(run_name);
    fs::create_dir_all(&dest_dir).map_err(|source| io(&dest_dir, source))?;
    let mut moved = Vec::new();
    for src in paths {
        let src = src.canonicalize().unwrap_or_else(|_| src.clone());
        let mut dest = dest_dir.join(src.file_name().unwrap_or_default());
        if dest.exists() {
            dest = dest_dir.join(format!(
                "{}-{}",
                src.file_name().and_then(|n| n.to_str()).unwrap_or("item"),
                Utc::now().format("%H%M%S")
            ));
        }
        fs::rename(&src, &dest).or_else(|_| {
            if src.is_dir() {
                copy_dir(&src, &dest)?;
                fs::remove_dir_all(&src).map_err(|source| io(&src, source))
            } else {
                fs::copy(&src, &dest).map_err(|source| io(&src, source))?;
                fs::remove_file(&src).map_err(|source| io(&src, source))
            }
        })?;
        moved.push(json!({"from": src, "to": dest}));
    }
    Ok(json!({ "moved": moved }))
}

pub fn restrict(paths: &[PathBuf]) -> Result<Value> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for path in paths {
            if let Ok(meta) = fs::metadata(path) {
                let mut permissions = meta.permissions();
                permissions.set_mode(0o600);
                let _ = fs::set_permissions(path, permissions);
            }
        }
    }
    Ok(json!({ "restricted": paths }))
}

fn load(path: &Path) -> Result<LearnState> {
    match fs::read_to_string(path) {
        Ok(raw) => Ok(serde_json::from_str(&raw).unwrap_or_default()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(LearnState::default()),
        Err(source) => Err(io(path, source)),
    }
}

fn save(path: &Path, state: &LearnState) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| io(parent, source))?;
    }
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(state)?).map_err(|source| io(&tmp, source))?;
    fs::rename(&tmp, path).map_err(|source| io(path, source))
}

pub fn copy_decisions(homes: &Homes, run_dir: &Path) -> Result<()> {
    let src = homes.learn_dir().join("decisions.jsonl");
    if src.is_file() {
        let dest = run_dir.join("decisions.jsonl");
        fs::copy(&src, &dest).map_err(|source| io(dest, source))?;
    }
    Ok(())
}

fn trashable(homes: &Homes, path: &Path) -> bool {
    let real = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if !real.exists() {
        return false;
    }
    let parent = real.parent();
    let kind = parent.and_then(|p| p.file_name()).and_then(|n| n.to_str());
    let grand = parent.and_then(|p| p.parent());
    let is_skill_dir = real.is_dir() && kind == Some("skills");
    let is_workflow = real.is_file()
        && real.extension().and_then(|e| e.to_str()) == Some("rhai")
        && kind == Some("workflows");
    let is_hook = real.is_file() && kind == Some("hooks");
    if !is_skill_dir && !is_workflow && !is_hook {
        return false;
    }
    let Some(grand) = grand else {
        return false;
    };
    agent_roots(homes).iter().any(|root| {
        let root = root.canonicalize().unwrap_or_else(|_| root.clone());
        under(grand, &root)
    }) || grand.file_name().and_then(|n| n.to_str()) == Some(".agents")
        || matches!(
            grand.file_name().and_then(|n| n.to_str()),
            Some(".grok" | ".claude" | ".cursor" | ".gemini" | ".copilot" | ".opencode")
        )
}

fn agent_roots(homes: &Homes) -> Vec<PathBuf> {
    vec![
        homes.claude.clone(),
        homes.grok.clone(),
        homes.codex.clone(),
        homes.cursor_config.clone(),
        homes.cursor.clone(),
        homes.opencode_config.clone(),
        homes.gemini.clone(),
        homes.copilot.clone(),
        homes.magents.clone(),
    ]
}

fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Error {
    Error::Io {
        path: path.into(),
        source,
    }
}

fn under(path: &Path, base: &Path) -> bool {
    path == base || path.starts_with(base)
}

pub(crate) fn copy_dir(src: &Path, dest: &Path) -> Result<()> {
    fs::create_dir_all(dest).map_err(|source| io(dest, source))?;
    for entry in fs::read_dir(src).map_err(|source| io(src, source))? {
        let entry = entry.map_err(|source| io(src, source))?;
        let from = entry.path();
        let to = dest.join(entry.file_name());
        if from.is_dir() {
            copy_dir(&from, &to)?;
        } else {
            fs::copy(&from, &to).map_err(|source| io(from, source))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Decision, StateUpdate, clear, copy_dir, decide, get, restrict, set, trash};
    use crate::homes::Homes;
    use std::fs;
    use tempfile::tempdir;

    fn homes() -> (tempfile::TempDir, Homes) {
        let dir = tempdir().unwrap();
        let homes = Homes::isolated(dir.path());
        (dir, homes)
    }

    #[test]
    fn get_missing_state_is_empty() {
        let (_dir, homes) = homes();
        let state = get(&homes).unwrap();
        assert!(state.pending.is_none());
        assert_eq!(state.magents_home, homes.magents);
    }

    #[test]
    fn set_tracks_pending_and_done() {
        let (dir, homes) = homes();
        let run = dir.path().join("run");
        fs::create_dir_all(&run).unwrap();
        let err = set(
            &homes,
            &StateUpdate {
                run_dir: run.clone(),
                status: "nope".into(),
                mode: None,
                scope: None,
                note: None,
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("status"));
        let state = set(
            &homes,
            &StateUpdate {
                run_dir: run.clone(),
                status: "collected".into(),
                mode: Some("step".into()),
                scope: Some("quick".into()),
                note: Some("hi".into()),
            },
        )
        .unwrap();
        assert_eq!(state.pending.as_ref().unwrap().status, "collected");
        assert_eq!(
            state.pending.as_ref().unwrap().mode.as_deref(),
            Some("step")
        );
        fs::write(run.join("report.md"), "r").unwrap();
        fs::write(run.join("actions.json"), "{}").unwrap();
        let other = dir.path().join("run2");
        fs::create_dir_all(&other).unwrap();
        let switched = set(
            &homes,
            &StateUpdate {
                run_dir: other,
                status: "running".into(),
                mode: None,
                scope: None,
                note: None,
            },
        )
        .unwrap();
        assert_eq!(switched.pending.as_ref().unwrap().status, "running");
        let done = set(
            &homes,
            &StateUpdate {
                run_dir: run,
                status: "done".into(),
                mode: None,
                scope: None,
                note: None,
            },
        )
        .unwrap();
        assert!(done.last_completed_at.is_some());
        assert!(!crate::learn::pending(&done));
        let cleared = clear(&homes).unwrap();
        assert!(cleared.pending.is_none());
        assert!(cleared.last_completed_at.is_some());
    }

    #[test]
    fn decide_appends_jsonl() {
        let (_dir, homes) = homes();
        let line = decide(
            &homes,
            &Decision {
                run_dir: "/tmp/run".into(),
                id: "A1".into(),
                kind: "skill".into(),
                action: "delete".into(),
                target: "unused".into(),
                path: "/x".into(),
                decision: "applied".into(),
                undo: Some("/trash".into()),
            },
        )
        .unwrap();
        assert_eq!(line["id"], "A1");
        let bad = decide(
            &homes,
            &Decision {
                run_dir: "/tmp/run".into(),
                id: "A1".into(),
                kind: "skill".into(),
                action: "delete".into(),
                target: "unused".into(),
                path: "/x".into(),
                decision: "maybe".into(),
                undo: None,
            },
        )
        .unwrap_err();
        assert!(bad.to_string().contains("decision"));
        let body = fs::read_to_string(homes.learn_dir().join("decisions.jsonl")).unwrap();
        assert_eq!(body.lines().count(), 1);
    }

    #[test]
    fn trash_moves_skill_and_refuses_other_paths() {
        let (_dir, homes) = homes();
        let skill = homes.grok.join("skills").join("unused");
        fs::create_dir_all(&skill).unwrap();
        fs::write(skill.join("SKILL.md"), "x").unwrap();
        let moved = trash(&homes, "20260101-010101", std::slice::from_ref(&skill)).unwrap();
        assert!(moved["moved"][0]["to"].as_str().unwrap().contains("unused"));
        assert!(!skill.exists());
        let config = homes.grok.join("config.toml");
        fs::write(&config, "x").unwrap();
        assert!(trash(&homes, "run", &[config]).is_err());
        assert!(trash(&homes, "../escape", &[homes.grok.join("skills")]).is_err());
        let missing = homes.grok.join("skills").join("gone");
        assert!(trash(&homes, "run", &[missing]).is_err());
    }

    #[test]
    fn restrict_is_ok() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("secret.toml");
        fs::write(&path, "x").unwrap();
        let out = restrict(std::slice::from_ref(&path)).unwrap();
        assert_eq!(out["restricted"][0], path.display().to_string());
        let missing = dir.path().join("gone.toml");
        assert!(restrict(std::slice::from_ref(&missing)).is_ok());
    }

    #[test]
    fn trash_workflows_hooks_and_duplicate_dest() {
        let (_dir, homes) = homes();
        let workflow = homes.grok.join("workflows").join("demo.rhai");
        fs::create_dir_all(workflow.parent().unwrap()).unwrap();
        fs::write(&workflow, "let meta = #{};\n").unwrap();
        trash(&homes, "run1", std::slice::from_ref(&workflow)).unwrap();
        assert!(!workflow.exists());

        let hook = homes.grok.join("hooks").join("on-stop.sh");
        fs::create_dir_all(hook.parent().unwrap()).unwrap();
        fs::write(&hook, "true\n").unwrap();
        trash(&homes, "run1", std::slice::from_ref(&hook)).unwrap();

        let skill = homes.grok.join("skills").join("dup");
        fs::create_dir_all(&skill).unwrap();
        fs::write(skill.join("SKILL.md"), "x").unwrap();
        trash(&homes, "run1", std::slice::from_ref(&skill)).unwrap();
        fs::create_dir_all(&skill).unwrap();
        fs::write(skill.join("SKILL.md"), "y").unwrap();
        let again = trash(&homes, "run1", std::slice::from_ref(&skill)).unwrap();
        assert!(again["moved"][0]["to"].as_str().unwrap().contains("dup"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let skill = homes.grok.join("skills").join("ro");
            fs::create_dir_all(&skill).unwrap();
            fs::write(skill.join("SKILL.md"), "z").unwrap();
            let dest_dir = homes.learn_dir().join("trash").join("readonly");
            fs::create_dir_all(&dest_dir).unwrap();
            let mut permissions = fs::metadata(&dest_dir).unwrap().permissions();
            permissions.set_mode(0o500);
            fs::set_permissions(&dest_dir, permissions).unwrap();
            assert!(trash(&homes, "readonly", std::slice::from_ref(&skill)).is_err());
            let mut permissions = fs::metadata(&dest_dir).unwrap().permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&dest_dir, permissions).unwrap();
        }

        let project = homes
            .claude
            .parent()
            .unwrap()
            .join("repo")
            .join(".grok")
            .join("skills")
            .join("local");
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("SKILL.md"), "x").unwrap();
        assert!(trash(&homes, "run2", std::slice::from_ref(&project)).is_ok());
    }

    #[test]
    fn get_recovers_invalid_json_and_dir_state() {
        let (_dir, homes) = homes();
        let path = homes.learn_dir().join("state.json");
        fs::create_dir_all(homes.learn_dir()).unwrap();
        fs::write(&path, "not-json").unwrap();
        assert!(get(&homes).is_ok());
        fs::remove_file(&path).unwrap();
        fs::create_dir_all(&path).unwrap();
        assert!(get(&homes).is_err());
    }

    #[test]
    fn decide_appends_when_file_has_no_trailing_newline() {
        let (_dir, homes) = homes();
        let path = homes.learn_dir().join("decisions.jsonl");
        fs::create_dir_all(homes.learn_dir()).unwrap();
        fs::write(&path, "{}").unwrap();
        decide(
            &homes,
            &Decision {
                run_dir: "/tmp/run".into(),
                id: "A2".into(),
                kind: "skill".into(),
                action: "edit".into(),
                target: "x".into(),
                path: "/x".into(),
                decision: "rejected".into(),
                undo: None,
            },
        )
        .unwrap();
        let body = fs::read_to_string(path).unwrap();
        assert_eq!(body.lines().count(), 2);
    }

    #[test]
    fn copy_dir_copies_nested_files() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src");
        let dest = dir.path().join("dest");
        fs::create_dir_all(src.join("nested")).unwrap();
        fs::write(src.join("a.txt"), "a").unwrap();
        fs::write(src.join("nested/b.txt"), "b").unwrap();
        copy_dir(&src, &dest).unwrap();
        assert_eq!(fs::read_to_string(dest.join("a.txt")).unwrap(), "a");
        assert_eq!(fs::read_to_string(dest.join("nested/b.txt")).unwrap(), "b");
        assert!(copy_dir(&dir.path().join("missing"), &dir.path().join("x")).is_err());
    }

    #[test]
    fn set_fails_when_learn_dir_is_a_file() {
        let (_dir, homes) = homes();
        fs::create_dir_all(&homes.magents).unwrap();
        fs::write(homes.magents.join("learn"), "not-a-dir").unwrap();
        let err = set(
            &homes,
            &StateUpdate {
                run_dir: homes.magents.join("run"),
                status: "collected".into(),
                mode: None,
                scope: None,
                note: None,
            },
        );
        assert!(err.is_err());
    }

    #[test]
    fn set_fails_when_tmp_or_state_path_is_a_directory() {
        let (_dir, homes) = homes();
        fs::create_dir_all(homes.learn_dir()).unwrap();
        fs::create_dir_all(homes.learn_dir().join("state.json.tmp")).unwrap();
        let err = set(
            &homes,
            &StateUpdate {
                run_dir: homes.magents.join("run"),
                status: "collected".into(),
                mode: None,
                scope: None,
                note: None,
            },
        );
        assert!(err.is_err());
        fs::remove_dir_all(homes.learn_dir().join("state.json.tmp")).unwrap();
        fs::create_dir_all(homes.learn_dir().join("state.json")).unwrap();
        let err = set(
            &homes,
            &StateUpdate {
                run_dir: homes.magents.join("run"),
                status: "collected".into(),
                mode: None,
                scope: None,
                note: None,
            },
        );
        assert!(err.is_err());
    }

    #[test]
    fn decide_fails_when_learn_home_is_not_a_directory() {
        let (_dir, homes) = homes();
        fs::create_dir_all(&homes.magents).unwrap();
        fs::write(homes.magents.join("learn"), "file").unwrap();
        assert!(
            decide(
                &homes,
                &Decision {
                    run_dir: "/r".into(),
                    id: "A1".into(),
                    kind: "skill".into(),
                    action: "edit".into(),
                    target: "x".into(),
                    path: "/x".into(),
                    decision: "applied".into(),
                    undo: None,
                }
            )
            .is_err()
        );
    }

    #[test]
    fn decide_fails_when_decisions_file_is_a_directory() {
        let (_dir, homes) = homes();
        fs::create_dir_all(homes.learn_dir().join("decisions.jsonl")).unwrap();
        assert!(
            decide(
                &homes,
                &Decision {
                    run_dir: "/r".into(),
                    id: "A1".into(),
                    kind: "skill".into(),
                    action: "edit".into(),
                    target: "x".into(),
                    path: "/x".into(),
                    decision: "applied".into(),
                    undo: None,
                }
            )
            .is_err()
        );
    }

    #[test]
    fn trash_fails_when_trash_root_is_a_file() {
        let (_dir, homes) = homes();
        let skill = homes.grok.join("skills").join("x");
        fs::create_dir_all(&skill).unwrap();
        fs::write(skill.join("SKILL.md"), "x").unwrap();
        fs::create_dir_all(homes.learn_dir()).unwrap();
        fs::write(homes.learn_dir().join("trash"), "file").unwrap();
        assert!(trash(&homes, "run", std::slice::from_ref(&skill)).is_err());
    }
}
