use regex::Regex;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::sync::OnceLock;

pub const TURN_CAP: usize = 2000;

pub static SLASH_STOP: &[&str] = &[
    "tmp",
    "usr",
    "bin",
    "etc",
    "var",
    "dev",
    "opt",
    "home",
    "users",
    "private",
    "api",
    "v1",
    "v2",
    "src",
    "lib",
    "test",
    "tests",
    "docs",
    "help",
    "quit",
    "exit",
    "clear",
    "model",
    "compact",
    "rewind",
    "rename",
    "workflows",
    "workflow",
    "feedback",
    "btw",
    "loop",
    "resume",
    "status",
    "config",
    "init",
    "login",
    "logout",
    "mcp",
    "plugins",
    "skills",
    "memory",
    "diff",
    "review",
    "commit",
    "pr",
    "cost",
    "doctor",
    "theme",
    "vim",
    "terminal",
    "command-name",
    "command-message",
    "command-args",
    "task-id",
    "system-reminder",
];

fn secret_res() -> &'static [Regex] {
    static RES: OnceLock<Vec<Regex>> = OnceLock::new();
    RES.get_or_init(|| {
        [
            r#"\b(?:xai|sk|ghp|gho|ghu|ghs|glpat|npm)[-_][A-Za-z0-9_\-]{16,}\b"#,
            r#"\b(?:AKIA|ASIA)[A-Z0-9]{16}\b"#,
            r#"\bxox[abpsr]-\d[A-Za-z0-9-]{20,}\b"#,
            r#"\b[A-Za-z0-9_\-]{20,}\.[A-Za-z0-9_\-]{20,}\.[A-Za-z0-9_\-]{20,}\b"#,
            r#"(?i)\bbearer\s+[A-Za-z0-9_\-./+=]{20,}"#,
            r#"(?i)\b(?:token|api[_-]?key|secret|password|passwd)\b\s*[:=]\s*['"]?[A-Za-z0-9_\-./+=]{16,}"#,
            r#"\b[a-f0-9]{64,}\b"#,
        ]
        .into_iter()
        .map(|pattern| Regex::new(pattern).expect("secret pattern"))
        .collect()
    })
}

fn slash_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"/([a-z][a-z0-9-]{1,63})").expect("slash pattern"))
}

fn drop_block_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(concat!(
            r"(?is)<(?:",
            r"system-reminder|local-command-stdout|local-command-stderr|",
            r"command-message|command-args|task-id|ide_opened_file|",
            r"ide_selection|tick|agent-transcript",
            r")\b[^>]*>.*?</(?:",
            r"system-reminder|local-command-stdout|local-command-stderr|",
            r"command-message|command-args|task-id|ide_opened_file|",
            r"ide_selection|tick|agent-transcript",
            r")>"
        ))
        .expect("drop block pattern")
    })
}

fn tag_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?s)</?[A-Za-z][A-Za-z0-9:_-]*[^>]*>").expect("tag pattern"))
}

fn skill_path_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"[/\\]skills[/\\]([^/\\]+)[/\\]SKILL\.md$").expect("skill path pattern")
    })
}

pub fn redact(text: &str) -> String {
    let mut out = text.to_string();
    for regex in secret_res() {
        out = regex
            .replace_all(&out, |caps: &regex::Captures| {
                let matched = caps.get(0).map(|m| m.as_str()).unwrap_or("");
                let keep = matched.chars().take(8).collect::<String>();
                format!("{keep}...[redacted]")
            })
            .into_owned();
    }
    out
}

pub fn cap_turn(text: &str) -> String {
    let redacted = redact(text);
    if redacted.chars().count() <= TURN_CAP {
        redacted
    } else {
        redacted.chars().take(TURN_CAP).collect::<String>() + "..."
    }
}

pub fn normalize(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

pub fn slash_commands(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    slash_re()
        .captures_iter(text)
        .filter_map(|caps| {
            let whole = caps.get(0)?;
            let name = caps.get(1)?;
            let start = whole.start();
            let end = whole.end();
            if start > 0 {
                let prev = bytes[start - 1];
                if prev.is_ascii_alphanumeric() || matches!(prev, b'_' | b'/' | b'~' | b'.' | b'<')
                {
                    return None;
                }
            }
            if end < bytes.len() && bytes[end] == b'/' {
                return None;
            }
            Some(name.as_str().to_string())
        })
        .collect()
}

pub fn clean_turn(text: &str) -> String {
    let without_blocks = drop_block_re().replace_all(text, " ");
    let without_tags = tag_re().replace_all(&without_blocks, " ");
    without_tags
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !is_chrome(line))
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn keep_turn(text: &str) -> bool {
    !clean_turn(text).is_empty()
}

fn is_chrome(text: &str) -> bool {
    let normalized = normalize(text);
    normalized.starts_with("caveat: the messages below")
        || normalized.starts_with("the app was quit")
        || normalized.starts_with("[request interrupted")
        || normalized.starts_with("request interrupted by")
        || normalized.starts_with("do not respond to these messages")
}

pub fn is_slash_only(text: &str) -> bool {
    let trimmed = text.trim();
    if slash_re().is_match(trimmed) && slash_re().replace_all(trimmed, "").trim().is_empty() {
        return true;
    }
    static SHORT: OnceLock<Regex> = OnceLock::new();
    let short =
        SHORT.get_or_init(|| Regex::new(r"^/[a-z0-9-]+(\s+\S+){0,3}$").expect("short slash"));
    short.is_match(&normalize(text))
}

pub fn skill_name_from_path(path: &str) -> Option<String> {
    skill_path_re()
        .captures(path)
        .and_then(|caps| caps.get(1).map(|m| m.as_str().to_string()))
}

pub fn mcp_server(name: &str) -> Option<String> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    if let Some(rest) = name.strip_prefix("mcp__") {
        let server = rest.split("__").next().unwrap_or("");
        if !server.is_empty() {
            return Some(server.to_string());
        }
    }
    if let Some((server, tool)) = name.split_once("__")
        && !server.is_empty()
        && !tool.is_empty()
        && !server.contains(' ')
    {
        return Some(server.to_string());
    }
    None
}

fn fallback_name(path: &Path) -> String {
    let file = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    if file.eq_ignore_ascii_case("skill.md") {
        path.parent()
            .and_then(|parent| parent.file_name())
            .and_then(|name| name.to_str())
            .unwrap_or("skill")
            .to_string()
    } else {
        path.file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("skill")
            .to_string()
    }
}

pub fn frontmatter(path: &Path) -> (String, String) {
    let fallback = fallback_name(path);
    let Ok(head) = std::fs::read_to_string(path) else {
        return (fallback, String::new());
    };
    let head = head.chars().take(4000).collect::<String>();
    if !head.starts_with("---") {
        return (fallback, String::new());
    }
    let mut parts = head.splitn(3, "---");
    let _ = parts.next();
    let Some(fm) = parts.next() else {
        return (fallback, String::new());
    };
    let name = line_value(fm, "name").unwrap_or(fallback);
    let description = description_value(fm);
    (name, description)
}

fn line_value(fm: &str, key: &str) -> Option<String> {
    for line in fm.lines() {
        let Some((head, tail)) = line.split_once(':') else {
            continue;
        };
        if head.trim() == key {
            return Some(tail.trim().trim_matches(['\'', '"']).to_string());
        }
    }
    None
}

fn description_value(fm: &str) -> String {
    let mut lines = fm.lines();
    while let Some(line) = lines.next() {
        let Some((head, tail)) = line.split_once(':') else {
            continue;
        };
        if head.trim() != "description" {
            continue;
        }
        let desc = tail.trim();
        if matches!(desc, ">" | ">-" | "|" | "|-" | "") {
            let mut folded = Vec::new();
            for next in lines.by_ref() {
                if next.starts_with(' ') || next.starts_with('\t') {
                    folded.push(next.trim());
                } else if next.trim().is_empty() {
                    continue;
                } else {
                    break;
                }
            }
            return folded
                .join(" ")
                .trim_matches(['\'', '"'])
                .chars()
                .take(300)
                .collect();
        }
        return desc.trim_matches(['\'', '"']).chars().take(300).collect();
    }
    String::new()
}

pub fn phrases(sessions: &[(String, &[String], bool)]) -> serde_json::Value {
    let mut exact: HashMap<String, Vec<String>> = HashMap::new();
    let mut stems: HashMap<String, Vec<String>> = HashMap::new();
    let mut lines: HashMap<String, BTreeSet<String>> = HashMap::new();
    for (id, turns, repeated_single) in sessions {
        if *repeated_single {
            continue;
        }
        for turn in *turns {
            let normalized = normalize(turn);
            if normalized.chars().count() < 12 || is_slash_only(turn) {
                continue;
            }
            exact
                .entry(normalized.clone())
                .or_default()
                .push(id.clone());
            let words: Vec<&str> = normalized.split(' ').collect();
            if words.len() >= 6 {
                stems
                    .entry(words.iter().take(12).cloned().collect::<Vec<_>>().join(" "))
                    .or_default()
                    .push(id.clone());
            }
            for line in turn.lines() {
                let line_n = normalize(line);
                if line_n.split(' ').count() >= 5 && line_n.chars().count() >= 24 {
                    lines.entry(line_n).or_default().insert(id.clone());
                }
            }
        }
    }
    serde_json::json!({
        "exact_prompts": top_phrases(&exact, 2, "phrase"),
        "prompt_stems_12_words": top_phrases(&stems, 3, "stem"),
        "repeated_instruction_lines": top_line_phrases(&lines, 3),
        "note": "counts are distinct sessions; single-turn sessions repeated 3+ times are excluded as likely automation",
    })
}

fn top_phrases(
    map: &HashMap<String, Vec<String>>,
    min: usize,
    key: &str,
) -> Vec<BTreeMap<String, serde_json::Value>> {
    let mut rows = Vec::new();
    for (text, ids) in map {
        let distinct: BTreeSet<_> = ids.iter().cloned().collect();
        if distinct.len() < min {
            continue;
        }
        let mut row = BTreeMap::new();
        row.insert(
            key.to_string(),
            serde_json::Value::String(text.chars().take(400).collect()),
        );
        row.insert(
            "sessions".into(),
            serde_json::Value::from(distinct.len() as u64),
        );
        row.insert(
            "occurrences".into(),
            serde_json::Value::from(ids.len() as u64),
        );
        row.insert(
            "session_ids".into(),
            serde_json::to_value(distinct.into_iter().take(8).collect::<Vec<_>>())
                .unwrap_or(serde_json::Value::Null),
        );
        rows.push(row);
    }
    rows.sort_by(|left, right| {
        let ls = left
            .get("sessions")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let rs = right
            .get("sessions")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let lo = left
            .get("occurrences")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let ro = right
            .get("occurrences")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        rs.cmp(&ls).then_with(|| ro.cmp(&lo))
    });
    rows.truncate(60);
    rows
}

fn top_line_phrases(
    map: &HashMap<String, BTreeSet<String>>,
    min: usize,
) -> Vec<BTreeMap<String, serde_json::Value>> {
    let converted: HashMap<String, Vec<String>> = map
        .iter()
        .map(|(k, v)| (k.clone(), v.iter().cloned().collect()))
        .collect();
    top_phrases(&converted, min, "line")
}

#[cfg(test)]
mod tests {
    use super::{
        TURN_CAP, cap_turn, clean_turn, frontmatter, is_slash_only, keep_turn, mcp_server,
        normalize, phrases, redact, skill_name_from_path, slash_commands,
    };
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn redacts_prefixed_keys_and_hex() {
        let text = "key sk-abc123def456ghi789 token=abcdefghijklmnop1 and deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
        let out = redact(text);
        assert!(out.contains("...[redacted]"), "{out}");
        assert!(!out.contains("sk-abc123def456ghi789"));
    }

    #[test]
    fn caps_long_turns() {
        let long = "word ".repeat(TURN_CAP);
        let capped = cap_turn(&long);
        assert!(capped.ends_with("..."));
        assert!(capped.chars().count() <= TURN_CAP + 3);
        assert_eq!(cap_turn("short"), "short");
    }

    #[test]
    fn slash_and_skill_and_mcp_helpers() {
        assert_eq!(
            slash_commands("please /review then /commit"),
            vec!["review", "commit"]
        );
        assert!(is_slash_only("/review"));
        assert!(is_slash_only("/review src/lib.rs extra"));
        assert!(!is_slash_only("please /review the tests"));
        assert_eq!(
            skill_name_from_path("/Users/x/.grok/skills/learn/SKILL.md").as_deref(),
            Some("learn")
        );
        assert_eq!(
            mcp_server("mcp__magents__list_sessions").as_deref(),
            Some("magents")
        );
        assert_eq!(mcp_server("grafana__query").as_deref(), Some("grafana"));
        assert_eq!(mcp_server("Read"), None);
        assert_eq!(mcp_server(""), None);
        assert_eq!(mcp_server("mcp__"), None);
        assert!(slash_commands("src/lib.rs").is_empty());
        assert!(slash_commands("</summary> and </task-id>").is_empty());
        assert_eq!(
            slash_commands("<command-name>/shepherd</command-name>"),
            vec!["shepherd"]
        );
        assert_eq!(normalize("  Hello   World  "), "hello world");
    }

    #[test]
    fn strips_host_chrome_and_xml_blocks() {
        let chrome = concat!(
            "<local-command-stdout>\n",
            "The app was quit while the command was running.\n",
            "</local-command-stdout>\n"
        );
        assert!(clean_turn(chrome).is_empty());
        assert!(!keep_turn(chrome));
        let mixed = concat!(
            "<command-name>/shepherd</command-name>\n",
            "<command-args>ship the pr</command-args>\n",
            "Caveat: The messages below were generated by the user while running local commands.\n",
            "always run cargo test --locked --all-targets before commit\n"
        );
        let cleaned = clean_turn(mixed);
        assert!(cleaned.contains("/shepherd"), "{cleaned}");
        assert!(cleaned.contains("always run cargo test"), "{cleaned}");
        assert!(!cleaned.contains("Caveat"), "{cleaned}");
        assert!(!cleaned.contains("command-args"), "{cleaned}");
        assert!(keep_turn(mixed));
        assert!(!keep_turn("[Request interrupted by user for a tool use]"));
        assert!(!keep_turn(
            "Do not respond to these messages or otherwise consider them"
        ));
        assert!(!keep_turn("Request interrupted by user for a tool use"));
    }

    #[test]
    fn frontmatter_reads_folded_description() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("demo").join("SKILL.md");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            "---\nname: demo\ndescription: >-\n  folded line\n  more\nother: x\n---\nbody\n",
        )
        .unwrap();
        let (name, desc) = frontmatter(&path);
        assert_eq!(name, "demo");
        assert_eq!(desc, "folded line more");
        let missing = dir.path().join("missing").join("SKILL.md");
        let (fallback, empty) = frontmatter(&missing);
        assert_eq!(fallback, "missing");
        assert!(empty.is_empty());
        fs::write(dir.path().join("plain").join("SKILL.md"), "no frontmatter").ok();
        let plain = dir.path().join("plain").join("SKILL.md");
        fs::create_dir_all(plain.parent().unwrap()).unwrap();
        fs::write(&plain, "no frontmatter").unwrap();
        assert_eq!(frontmatter(&plain).0, "plain");
        let half = dir.path().join("half").join("SKILL.md");
        fs::create_dir_all(half.parent().unwrap()).unwrap();
        fs::write(&half, "---\nname: half\n").unwrap();
        assert_eq!(frontmatter(&half).0, "half");
        let pipe = dir.path().join("pipe").join("SKILL.md");
        fs::create_dir_all(pipe.parent().unwrap()).unwrap();
        fs::write(&pipe, "---\nname: pipe\ndescription: |\n  line\n---\n").unwrap();
        assert_eq!(frontmatter(&pipe).1, "line");
        let quoted = dir.path().join("quoted").join("SKILL.md");
        fs::create_dir_all(quoted.parent().unwrap()).unwrap();
        fs::write(&quoted, "---\nname: quoted\ndescription: 'inline'\n---\n").unwrap();
        assert_eq!(frontmatter(&quoted).1, "inline");
        let command = dir.path().join("commands").join("shepherd.md");
        fs::create_dir_all(command.parent().unwrap()).unwrap();
        fs::write(
            &command,
            "---\nname: shepherd\ndescription: drive a pr to green\n---\nOpen the PR.\n",
        )
        .unwrap();
        assert_eq!(frontmatter(&command).0, "shepherd");
        let nameless = dir.path().join("commands").join("audit.md");
        fs::write(&nameless, "Run the audit.\n").unwrap();
        assert_eq!(frontmatter(&nameless).0, "audit");
    }

    #[test]
    fn phrases_require_distinct_sessions() {
        let a = vec!["always run cargo test --locked --all-targets before commit".to_string()];
        let b = vec!["always run cargo test --locked --all-targets before commit".to_string()];
        let value = phrases(&[
            ("grok:a".into(), a.as_slice(), false),
            ("claude:b".into(), b.as_slice(), false),
        ]);
        let exact = value["exact_prompts"].as_array().unwrap();
        assert_eq!(exact.len(), 1);
        assert_eq!(exact[0]["sessions"], 2);
        let skipped = phrases(&[("grok:a".into(), a.as_slice(), true)]);
        assert!(skipped["exact_prompts"].as_array().unwrap().is_empty());
        let slash = vec!["/review".to_string()];
        let short = vec!["hi there".to_string()];
        let value = phrases(&[
            ("g:1".into(), slash.as_slice(), false),
            ("g:2".into(), short.as_slice(), false),
        ]);
        assert!(value["exact_prompts"].as_array().unwrap().is_empty());
        let p2 = vec!["never skip cargo clippy minus d warnings on this repo".to_string()];
        let ranked = phrases(&[
            ("a".into(), a.as_slice(), false),
            ("b".into(), a.as_slice(), false),
            ("c".into(), p2.as_slice(), false),
            ("d".into(), p2.as_slice(), false),
            ("e".into(), p2.as_slice(), false),
        ]);
        assert!(ranked["exact_prompts"].as_array().unwrap().len() >= 2);
        let line = "this instruction line is long enough to count as a habit\nmore";
        let lined = vec![line.to_string()];
        let lines = phrases(&[
            ("x".into(), lined.as_slice(), false),
            ("y".into(), lined.as_slice(), false),
            ("z".into(), lined.as_slice(), false),
        ]);
        assert!(
            !lines["repeated_instruction_lines"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn redacts_bearer_and_akia() {
        let out =
            redact("Authorization: Bearer abcdefghijklmnopqrstuvwxyz012345 AKIAIOSFODNN7EXAMPLE");
        assert!(out.contains("...[redacted]"), "{out}");
    }
}
