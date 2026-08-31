use crate::discover::identify;
use crate::error::{Error, Result};
use crate::homes::Homes;
use crate::model::{Caller, Note};
use chrono::{TimeZone, Utc};
use std::fs;
use std::path::{Component, Path, PathBuf};

pub fn get_note(homes: &Homes, cwd: Option<&str>, caller: &Caller) -> Result<Note> {
    let cwd = resolve_cwd(homes, cwd, caller)?;
    let path = note_path(homes, &cwd)?;
    if !path.is_file() {
        return Ok(Note {
            cwd,
            path,
            content: String::new(),
            exists: false,
            updated_at: None,
        });
    }
    let content = fs::read_to_string(&path).map_err(|source| Error::Io {
        path: path.clone(),
        source,
    })?;
    let updated_at = fs::metadata(&path)
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .and_then(DateTimeExt::from_system);
    Ok(Note {
        cwd,
        path,
        content,
        exists: true,
        updated_at,
    })
}

pub fn put_note(homes: &Homes, content: &str, cwd: Option<&str>, caller: &Caller) -> Result<Note> {
    if content.trim().is_empty() {
        return Err(Error::msg("content is required"));
    }
    let cwd = resolve_cwd(homes, cwd, caller)?;
    let path = note_path(homes, &cwd)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| Error::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    fs::write(&path, content).map_err(|source| Error::Io {
        path: path.clone(),
        source,
    })?;
    get_note(homes, Some(&cwd), caller)
}

fn resolve_cwd(homes: &Homes, cwd: Option<&str>, caller: &Caller) -> Result<String> {
    if let Some(cwd) = cwd.map(str::trim).filter(|value| !value.is_empty()) {
        return canonicalize_cwd(cwd);
    }
    let identity = identify(homes);
    if let Some(cwd) = identity.cwd.as_deref().filter(|value| !value.is_empty()) {
        return canonicalize_cwd(cwd);
    }
    if let Some(session_id) = caller.session_id.as_deref()
        && let Some(agent) = caller.agent
        && let Ok(session) = crate::discover::resolve(homes, &format!("{agent}:{session_id}"))
        && let Some(cwd) = session.cwd
    {
        return canonicalize_cwd(&cwd);
    }
    let cwd = std::env::current_dir().map_err(|source| Error::Io {
        path: PathBuf::from("."),
        source,
    })?;
    canonicalize_cwd(&cwd.to_string_lossy())
}

fn canonicalize_cwd(cwd: &str) -> Result<String> {
    let cwd = cwd.trim();
    if cwd.is_empty() {
        return Err(Error::msg("cwd is required"));
    }
    if Path::new(cwd)
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(Error::msg("cwd must not contain .."));
    }
    let canonical =
        fs::canonicalize(cwd).unwrap_or_else(|_| PathBuf::from(cwd.trim_end_matches('/')));
    Ok(canonical.to_string_lossy().into_owned())
}

fn note_path(homes: &Homes, cwd: &str) -> Result<PathBuf> {
    let slug = slug_cwd(cwd)?;
    Ok(homes.notes_dir().join(format!("{slug}.md")))
}

fn slug_cwd(cwd: &str) -> Result<String> {
    let cwd = cwd.trim().trim_end_matches(['/', '\\']);
    if cwd.is_empty() || cwd.contains("..") {
        return Err(Error::msg("cwd is not a valid note key"));
    }
    use sha2::{Digest, Sha256};
    Ok(Sha256::digest(cwd.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

struct DateTimeExt;
impl DateTimeExt {
    fn from_system(time: std::time::SystemTime) -> Option<chrono::DateTime<Utc>> {
        let duration = time.duration_since(std::time::UNIX_EPOCH).ok()?;
        Utc.timestamp_opt(duration.as_secs() as i64, duration.subsec_nanos())
            .single()
    }
}

#[cfg(test)]
mod tests {
    use super::{get_note, put_note, slug_cwd};
    use crate::homes::Homes;
    use crate::model::Caller;

    fn caller() -> Caller {
        Caller {
            agent: None,
            session_id: None,
        }
    }

    #[test]
    fn get_missing_and_put_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(dir.path());
        let cwd = dir.path().to_str().unwrap();
        let missing = get_note(&homes, Some(cwd), &caller()).unwrap();
        assert!(!missing.exists);
        assert!(missing.content.is_empty());

        let written = put_note(&homes, "current plan: ship digest", Some(cwd), &caller()).unwrap();
        assert!(written.exists);
        assert_eq!(written.content, "current plan: ship digest");
        assert_eq!(written.cwd, fs_canon(cwd));
        assert!(written.updated_at.is_some());

        let read = get_note(&homes, Some(cwd), &caller()).unwrap();
        assert_eq!(read.content, "current plan: ship digest");
    }

    fn fs_canon(cwd: &str) -> String {
        std::fs::canonicalize(cwd)
            .unwrap()
            .to_string_lossy()
            .into_owned()
    }

    #[test]
    fn rejects_empty_and_dotdot() {
        let homes = Homes::isolated(tempfile::tempdir().unwrap().path());
        let err = put_note(&homes, "   ", Some("/tmp"), &caller()).unwrap_err();
        assert!(err.to_string().contains("content is required"));
        let err = put_note(&homes, "note", Some("/tmp/foo/../etc"), &caller()).unwrap_err();
        assert!(err.to_string().contains(".."));
        assert!(slug_cwd("").is_err());
        assert!(super::canonicalize_cwd("   ").is_err());
        assert_ne!(
            slug_cwd("/home/user/project-x").unwrap(),
            slug_cwd("/home/user/project/x").unwrap()
        );
        assert_eq!(slug_cwd("C:\\Users\\work").unwrap().len(), 64);
    }

    #[test]
    fn resolve_cwd_from_identity_and_io_errors() {
        use crate::test_env;
        use std::os::unix::fs::PermissionsExt;

        const KEYS: &[&str] = &[
            "GROK_SESSION_ID",
            "CLAUDE_CODE_MESSAGING_SOCKET",
            "CLAUDE_PROJECT_DIR",
            "CLAUDE_SESSION_ID",
            "CURSOR_SESSION_ID",
            "CURSOR_PROJECT_DIR",
            "CURSOR_AGENT",
            "COMPOSER_SESSION_ID",
            "OPENCODE_SESSION_ID",
            "OPENCODE_DIRECTORY",
            "OPENCODE_SERVER",
            "OPENCODE_SESSION",
            "CODEX_HOME",
            "CODEX_THREAD_ID",
            "CODEX_SESSION_ID",
        ];
        let _guard = test_env::lock(KEYS);
        for key in KEYS {
            unsafe { std::env::remove_var(key) };
        }

        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        unsafe {
            std::env::set_var("CLAUDE_PROJECT_DIR", &project);
        }
        let homes = Homes::isolated(dir.path());
        let written = put_note(&homes, "from identity cwd", None, &caller()).unwrap();
        assert!(written.exists);
        let read = get_note(&homes, None, &caller()).unwrap();
        assert_eq!(read.content, "from identity cwd");
        unsafe { std::env::remove_var("CLAUDE_PROJECT_DIR") };

        let blocked = Homes::isolated(dir.path().join("blocked"));
        std::fs::create_dir_all(blocked.magents.parent().unwrap()).unwrap();
        std::fs::write(&blocked.magents, "not-a-dir").unwrap();
        let err = put_note(
            &blocked,
            "cannot create notes dir",
            Some(project.to_str().unwrap()),
            &caller(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("failed to read"), "{err}");

        let homes = Homes::isolated(dir.path().join("write-fail"));
        let cwd = project.to_str().unwrap();
        let path = super::note_path(&homes, &super::canonicalize_cwd(cwd).unwrap()).unwrap();
        std::fs::create_dir_all(&path).unwrap();
        let err =
            put_note(&homes, "cannot overwrite a directory", Some(cwd), &caller()).unwrap_err();
        assert!(err.to_string().contains("failed to read"), "{err}");

        let homes = Homes::isolated(dir.path().join("unreadable"));
        let written = put_note(&homes, "secret", Some(cwd), &caller()).unwrap();
        let mut permissions = std::fs::metadata(&written.path).unwrap().permissions();
        permissions.set_mode(0o000);
        std::fs::set_permissions(&written.path, permissions).unwrap();
        let err = get_note(&homes, Some(cwd), &caller()).unwrap_err();
        assert!(err.to_string().contains("failed to read"), "{err}");
        let mut permissions = std::fs::metadata(&written.path).unwrap().permissions();
        permissions.set_mode(0o644);
        std::fs::set_permissions(&written.path, permissions).unwrap();
    }

    #[test]
    fn resolve_cwd_falls_back_to_caller_session_and_process() {
        use crate::model::Agent;
        use crate::test_env;

        const KEYS: &[&str] = &[
            "GROK_SESSION_ID",
            "CLAUDE_CODE_MESSAGING_SOCKET",
            "CLAUDE_PROJECT_DIR",
            "CLAUDE_SESSION_ID",
            "CURSOR_SESSION_ID",
            "CURSOR_PROJECT_DIR",
            "CURSOR_AGENT",
            "COMPOSER_SESSION_ID",
            "OPENCODE_SESSION_ID",
            "OPENCODE_DIRECTORY",
            "OPENCODE_SERVER",
            "OPENCODE_SESSION",
            "CODEX_HOME",
            "CODEX_THREAD_ID",
            "CODEX_SESSION_ID",
        ];
        let _guard = test_env::lock(KEYS);
        for key in KEYS {
            unsafe { std::env::remove_var(key) };
        }

        let dir = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(dir.path());
        let sid = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
        std::fs::create_dir_all(homes.claude.join("sessions")).unwrap();
        std::fs::write(
            homes
                .claude
                .join("sessions")
                .join(format!("{}.json", std::process::id())),
            serde_json::json!({
                "pid": std::process::id(),
                "sessionId": sid,
            })
            .to_string(),
        )
        .unwrap();
        unsafe {
            std::env::set_var("CLAUDE_SESSION_ID", sid);
            std::env::set_var("CLAUDE_PROJECT_DIR", "/tmp");
        }
        crate::spawn::record(
            &homes,
            Agent::Codex,
            "note-cwd",
            dir.path(),
            crate::spawn::Transport::CodexExec,
        )
        .unwrap();
        let from_caller = get_note(
            &homes,
            None,
            &Caller {
                agent: Some(Agent::Codex),
                session_id: Some("note-cwd".into()),
            },
        )
        .unwrap();
        assert!(!from_caller.exists);

        let from_process = get_note(&homes, None, &caller()).unwrap();
        assert_eq!(
            from_process.cwd,
            std::fs::canonicalize(std::env::current_dir().unwrap())
                .unwrap()
                .to_string_lossy()
                .into_owned()
        );
    }
}
