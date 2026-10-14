use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

/// Files that carry the magents product version. Keep this in sync with
/// `.github/scripts/set-release-version.sh` and `commit-release-version.sh`.
const VERSION_FILES: &[&str] = &[
    "Cargo.toml",
    "Cargo.lock",
    "gemini-extension.json",
    "server.json",
    "plugins/claude/.claude-plugin/plugin.json",
    "plugins/codex/.codex-plugin/plugin.json",
    "plugins/cursor/.cursor-plugin/plugin.json",
    ".cursor-plugin/marketplace.json",
];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn package_version(root: &Path) -> String {
    let text = fs::read_to_string(root.join("Cargo.toml")).unwrap();
    text.lines()
        .find_map(|line| {
            let rest = line.strip_prefix("version = \"")?;
            rest.strip_suffix('"').map(str::to_string)
        })
        .expect("package version")
}

#[test]
fn product_version_is_only_in_release_files() {
    let root = repo_root();
    let version = package_version(&root);
    let output = Command::new("git")
        .args(["grep", "-l", "-F", &version])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut paths: Vec<String> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect();
    paths.sort();
    let mut expected: Vec<String> = VERSION_FILES
        .iter()
        .map(|path| (*path).to_string())
        .collect();
    expected.sort();
    assert_eq!(paths, expected);
}

#[test]
fn set_release_version_rewrites_every_manifest() {
    let root = repo_root();
    let old = package_version(&root);
    let fixture = TempDir::new().unwrap();
    for relative in VERSION_FILES {
        let destination = fixture.path().join(relative);
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        fs::copy(root.join(relative), &destination).unwrap();
    }

    let output = Command::new(root.join(".github/scripts/set-release-version.sh"))
        .arg("9.9.9")
        .env("MAGENTS_ROOT", fixture.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    for relative in VERSION_FILES {
        let text = fs::read_to_string(fixture.path().join(relative)).unwrap();
        assert!(
            text.contains("9.9.9"),
            "{relative} was not rewritten:\n{text}"
        );
        assert!(!text.contains(&old), "{relative} still contains {old}");
    }
    let lock = fs::read_to_string(fixture.path().join("Cargo.lock")).unwrap();
    assert!(lock.contains("name = \"magents\"\nversion = \"9.9.9\""));
}
