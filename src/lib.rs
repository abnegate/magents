pub mod codex_ipc;
pub mod deliver;
pub mod discover;
pub mod error;
pub mod homes;
pub mod install;
pub mod mailbox;
pub mod mcp;
pub mod model;
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

    pub fn write_executable(path: &std::path::Path, script: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, format!("#!/bin/sh\n{script}\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(path).unwrap().permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(path, permissions).unwrap();
        }
    }
}
