pub mod codex_ipc;
pub mod deliver;
pub mod discover;
pub mod error;
pub mod handoff;
pub mod homes;
pub mod host;
pub mod install;
pub mod learn;
pub mod mailbox;
pub mod mcp;
pub mod memory;
pub mod model;
pub mod notes;
pub mod requester;
pub mod runtime;
pub mod spawn;
pub mod transcript;

pub use error::Error;
pub use homes::Homes;
pub use model::{Agent, Session, Turn};

#[cfg(test)]
mod handoff_tests;

#[cfg(test)]
pub(crate) mod test_env {
    use std::ffi::OsString;
    use std::sync::{Mutex, MutexGuard};

    static LOCK: Mutex<()> = Mutex::new(());

    pub struct Guard {
        _lock: MutexGuard<'static, ()>,
        saved: Vec<(&'static str, Option<OsString>)>,
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            for (key, value) in self.saved.drain(..) {
                unsafe {
                    match value {
                        Some(value) => std::env::set_var(key, value),
                        None => std::env::remove_var(key),
                    }
                }
            }
        }
    }

    pub fn lock(keys: &'static [&'static str]) -> Guard {
        let lock = LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let saved = keys
            .iter()
            .copied()
            .map(|key| (key, std::env::var_os(key)))
            .collect();
        Guard { _lock: lock, saved }
    }

    #[test]
    fn recovers_from_poisoned_env_lock() {
        let handle = std::thread::spawn(|| {
            let _guard = lock(&[]);
            panic!("poison the env lock");
        });
        assert!(handle.join().is_err());
        let _guard = lock(&[]);
    }

    #[test]
    #[cfg(unix)]
    fn warm_waits_for_open_writers() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("script");
        std::fs::write(
            &path,
            format!("#!/bin/sh\n[ -z \"${{{WARM}-}}\" ] || exit 0\nexit 1\n"),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let writer = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        let warming = {
            let path = path.clone();
            std::thread::spawn(move || warm(&path))
        };
        std::thread::sleep(std::time::Duration::from_millis(50));
        drop(writer);
        warming.join().unwrap();
    }

    pub fn write_executable(path: &std::path::Path, script: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(
            path,
            format!("#!/bin/sh\n[ -z \"${{{WARM}-}}\" ] || exit 0\n{script}\n"),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(path).unwrap().permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(path, permissions).unwrap();
            warm(path);
        }
    }

    const WARM: &str = "MAGENTS_TEST_WARM";

    #[cfg(unix)]
    fn warm(path: &std::path::Path) {
        use std::io::ErrorKind::ExecutableFileBusy;
        use std::process::{Command, Stdio};
        let mut command = Command::new(path);
        command
            .env_clear()
            .env(WARM, "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // Linux refuses the exec while another test thread's fork still holds the write descriptor.
        let status = loop {
            match command.status() {
                Err(error) if error.kind() == ExecutableFileBusy => std::thread::yield_now(),
                result => break result.unwrap(),
            }
        };
        assert!(status.success(), "warming {} failed", path.display());
    }
}
