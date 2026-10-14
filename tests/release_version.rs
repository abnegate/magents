use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

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

fn git_version_files(root: &Path, version: &str) -> Vec<String> {
    let output = Command::new("git")
        .args(["grep", "-l", "-F", version])
        .current_dir(root)
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
    paths
}

fn copy_into(fixture: &Path, root: &Path, relative: &str) {
    let destination = fixture.join(relative);
    fs::create_dir_all(destination.parent().unwrap()).unwrap();
    fs::copy(root.join(relative), destination).unwrap();
}

fn run_set_release_version(root: &Path, fixture: &Path, version: &str) -> std::process::Output {
    run_set_release_version_with_env(root, fixture, version, &[])
}

fn run_set_release_version_with_env(
    root: &Path,
    fixture: &Path,
    version: &str,
    extra_env: &[(&str, &str)],
) -> std::process::Output {
    let mut command = Command::new(root.join(".github/scripts/set-release-version.sh"));
    command.arg(version).env("MAGENTS_ROOT", fixture);
    for (key, value) in extra_env {
        command.env(key, value);
    }
    command.output().unwrap()
}

fn assert_script_ok(output: &std::process::Output) {
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn product_version_is_not_hardcoded_in_source() {
    let root = repo_root();
    let version = package_version(&root);
    for path in git_version_files(&root, &version) {
        assert!(
            !path.starts_with("src/"),
            "{path} hardcodes the product version; use CARGO_PKG_VERSION"
        );
        assert!(
            path != "tests/cli.rs" && path != "tests/release_version.rs",
            "{path} hardcodes the product version"
        );
    }
}

#[test]
fn set_release_version_rewrites_every_tracked_version() {
    let root = repo_root();
    let old = package_version(&root);
    let files = git_version_files(&root, &old);
    assert!(
        files.iter().any(|path| path == "Cargo.toml"),
        "expected Cargo.toml to carry the product version, got {files:?}"
    );

    let fixture = TempDir::new().unwrap();
    for relative in &files {
        copy_into(fixture.path(), &root, relative);
    }

    let output = run_set_release_version(&root, fixture.path(), "9.9.9");
    assert_script_ok(&output);

    for relative in &files {
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

#[test]
fn set_release_version_rewrites_a_new_plugin_manifest() {
    let root = repo_root();
    let old = package_version(&root);
    let fixture = TempDir::new().unwrap();
    copy_into(fixture.path(), &root, "Cargo.toml");
    copy_into(fixture.path(), &root, "Cargo.lock");

    let relative = "plugins/future/.future-plugin/plugin.json";
    let destination = fixture.path().join(relative);
    fs::create_dir_all(destination.parent().unwrap()).unwrap();
    fs::write(
        &destination,
        format!("{{\n  \"name\": \"magents\",\n  \"version\": \"{old}\"\n}}\n"),
    )
    .unwrap();

    let output = run_set_release_version(&root, fixture.path(), "9.9.9");
    assert_script_ok(&output);

    let text = fs::read_to_string(destination).unwrap();
    assert!(text.contains("\"version\": \"9.9.9\""), "{text}");
    assert!(!text.contains(&old), "{text}");
}

#[test]
fn set_release_version_rewrites_a_stale_plugin_manifest() {
    let root = repo_root();
    let fixture = TempDir::new().unwrap();
    copy_into(fixture.path(), &root, "Cargo.toml");
    copy_into(fixture.path(), &root, "Cargo.lock");

    let relative = "plugins/stale/.stale-plugin/plugin.json";
    let destination = fixture.path().join(relative);
    fs::create_dir_all(destination.parent().unwrap()).unwrap();
    fs::write(
        destination,
        "{\n  \"name\": \"magents\",\n  \"version\": \"0.1.0\"\n}\n",
    )
    .unwrap();

    let output = run_set_release_version(&root, fixture.path(), "9.9.9");
    assert_script_ok(&output);

    let text = fs::read_to_string(fixture.path().join(relative)).unwrap();
    assert!(text.contains("\"version\": \"9.9.9\""), "{text}");
    assert!(!text.contains("0.1.0"), "{text}");
}

#[test]
fn set_release_version_rewrites_a_new_doc_that_pins_the_version() {
    let root = repo_root();
    let old = package_version(&root);
    let fixture = TempDir::new().unwrap();
    copy_into(fixture.path(), &root, "Cargo.toml");
    copy_into(fixture.path(), &root, "Cargo.lock");

    let relative = "docs/pin.md";
    let destination = fixture.path().join(relative);
    fs::create_dir_all(destination.parent().unwrap()).unwrap();
    fs::write(&destination, format!("install magents {old}\n")).unwrap();

    let output = run_set_release_version(&root, fixture.path(), "9.9.9");
    assert_script_ok(&output);

    let text = fs::read_to_string(destination).unwrap();
    assert_eq!(text, "install magents 9.9.9\n");
}

#[test]
fn set_release_version_stages_discovered_files() {
    let root = repo_root();
    let old = package_version(&root);
    let fixture = TempDir::new().unwrap();
    let staging = TempDir::new().unwrap();
    copy_into(fixture.path(), &root, "Cargo.toml");
    copy_into(fixture.path(), &root, "Cargo.lock");

    let relative = "plugins/staged/.staged-plugin/plugin.json";
    let destination = fixture.path().join(relative);
    fs::create_dir_all(destination.parent().unwrap()).unwrap();
    fs::write(
        &destination,
        format!("{{\n  \"name\": \"magents\",\n  \"version\": \"{old}\"\n}}\n"),
    )
    .unwrap();

    let output = run_set_release_version_with_env(
        &root,
        fixture.path(),
        "9.9.9",
        &[("RELEASE_VERSION_STAGING", staging.path().to_str().unwrap())],
    );
    assert_script_ok(&output);

    for relative in ["Cargo.toml", "Cargo.lock", relative] {
        let staged = fs::read_to_string(staging.path().join(relative)).unwrap();
        assert!(
            staged.contains("9.9.9"),
            "{relative} was not staged:\n{staged}"
        );
    }
}

#[test]
fn server_json_meets_mcp_registry_limits() {
    let text = fs::read_to_string(repo_root().join("server.json")).unwrap();
    let server: serde_json::Value = serde_json::from_str(&text).unwrap();
    let description = server["description"].as_str().expect("description");
    assert!(
        (1..=100).contains(&description.chars().count()),
        "server.json description is {} chars; MCP Registry max is 100:\n{description}",
        description.chars().count()
    );
    if let Some(title) = server["title"].as_str() {
        assert!(
            (1..=100).contains(&title.chars().count()),
            "server.json title is {} chars; MCP Registry max is 100",
            title.chars().count()
        );
    }
}

#[test]
fn prepare_mcp_server_json_rejects_a_long_description() {
    let root = repo_root();
    let fixture = TempDir::new().unwrap();
    copy_into(fixture.path(), &root, "server.json");
    let server = fixture.path().join("server.json");
    let mut value: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&server).unwrap()).unwrap();
    value["description"] = serde_json::Value::String("x".repeat(101));
    fs::write(&server, serde_json::to_string_pretty(&value).unwrap()).unwrap();

    let output = Command::new(root.join(".github/scripts/prepare-mcp-server-json.sh"))
        .args(["9.9.9", "9.9.9"])
        .env("MAGENTS_ROOT", fixture.path())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("description is 101 chars"),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn set_release_version_rejects_an_invalid_tag() {
    let root = repo_root();
    let fixture = TempDir::new().unwrap();
    copy_into(fixture.path(), &root, "Cargo.toml");
    let output = run_set_release_version(&root, fixture.path(), "not-a-version");
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("invalid release version"),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
