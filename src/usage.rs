use crate::error::Result;
use crate::homes::Homes;
use crate::model::Agent;
use chrono::DateTime;
use serde::Serialize;
use serde_json::Value;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use walkdir::WalkDir;

const DEFAULT_WARN: f64 = 75.0;
const DEFAULT_CRITICAL: f64 = 90.0;
const JSONL_TAIL: usize = 1024 * 1024;
const CODEX_ROLL_LIMIT: usize = 8;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Ok,
    Warn,
    Critical,
}

#[derive(Clone, Debug, Serialize)]
pub struct Bucket {
    pub name: String,
    pub used_percentage: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<i64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Snapshot {
    pub agent: Agent,
    pub source: String,
    pub buckets: Vec<Bucket>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limiting: Option<String>,
    pub level: Level,
}

pub fn report(homes: &Homes) -> Vec<Snapshot> {
    Agent::all()
        .into_iter()
        .filter_map(|agent| for_agent(homes, agent))
        .collect()
}

pub fn for_agent(homes: &Homes, agent: Agent) -> Option<Snapshot> {
    match agent {
        Agent::Claude => claude(homes),
        Agent::Grok => grok(homes),
        Agent::Codex => codex(homes),
        Agent::Cursor => snapshot_files(homes.cursor.as_path(), Agent::Cursor),
        Agent::OpenCode => snapshot_files(homes.opencode.as_path(), Agent::OpenCode),
    }
}

pub fn claude(homes: &Homes) -> Option<Snapshot> {
    for name in ["abtop-rate-limits.json", "rate-limits.json"] {
        let path = homes.claude.join(name);
        if let Some(snapshot) = load_file(&path, name, Agent::Claude) {
            return Some(snapshot);
        }
    }
    let history = homes
        .claude_desktop
        .parent()
        .unwrap_or(homes.claude_desktop.as_path())
        .join("plan-usage-history.json");
    load_file(&history, "plan-usage-history.json", Agent::Claude).or_else(|| {
        load_file(
            &homes.claude.join("plan-usage-history.json"),
            "plan-usage-history.json",
            Agent::Claude,
        )
    })
}

pub fn grok(homes: &Homes) -> Option<Snapshot> {
    grok_log(homes).or_else(|| snapshot_files(homes.grok.as_path(), Agent::Grok))
}

pub fn codex(homes: &Homes) -> Option<Snapshot> {
    codex_rollouts(homes).or_else(|| snapshot_files(homes.codex.as_path(), Agent::Codex))
}

pub fn ingest_statusline(homes: &Homes, raw: &str) -> Result<Option<Snapshot>> {
    let value: Value = serde_json::from_str(raw)?;
    let Some(agent) = detect_agent(&value) else {
        return Ok(None);
    };
    let Some(mut snapshot) = from_value(agent, "ingest", &value) else {
        return Ok(None);
    };
    let encoded = encode_snapshot(&snapshot)?;
    let path = agent_home(homes, agent).join("rate-limits.json");
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    fs::write(&path, encoded).map_err(|source| crate::error::Error::Io {
        path: path.clone(),
        source,
    })?;
    snapshot.source = "rate-limits.json".into();
    Ok(Some(snapshot))
}

pub fn exhausted(homes: &Homes, agent: Agent) -> bool {
    for_agent(homes, agent).is_some_and(|snapshot| snapshot.level == Level::Critical)
}

fn snapshot_files(home: &Path, agent: Agent) -> Option<Snapshot> {
    for name in ["rate-limits.json", "usage-limits.json"] {
        if let Some(snapshot) = load_file(&home.join(name), name, agent) {
            return Some(snapshot);
        }
    }
    None
}

fn load_file(path: &Path, source: &str, agent: Agent) -> Option<Snapshot> {
    let raw = fs::read_to_string(path).ok()?;
    let value: Value = serde_json::from_str(&raw).ok()?;
    from_value(agent, source, &value)
}

fn grok_log(homes: &Homes) -> Option<Snapshot> {
    let path = homes.grok.join("logs").join("unified.jsonl");
    let value = last_jsonl(&path, JSONL_TAIL, is_grok_billing)?;
    from_value(Agent::Grok, "logs/unified.jsonl", &value)
}

fn codex_rollouts(homes: &Homes) -> Option<Snapshot> {
    for path in newest_jsonl(&homes.codex.join("sessions"), CODEX_ROLL_LIMIT) {
        if let Some(value) = last_jsonl(&path, JSONL_TAIL, is_codex_limits) {
            let source = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("rollout.jsonl");
            if let Some(snapshot) = from_value(Agent::Codex, source, &value) {
                return Some(snapshot);
            }
        }
    }
    None
}

fn from_value(agent: Agent, source: &str, value: &Value) -> Option<Snapshot> {
    let mut buckets = Vec::new();
    if let Some(config) = grok_config(value) {
        push_grok(&mut buckets, config);
    }
    let limits = value.get("rate_limits").or_else(|| find_rate_limits(value));
    if let Some(limits) = limits {
        collect_windows(&mut buckets, limits);
        push_named_windows(&mut buckets, limits);
        push_scoped(&mut buckets, limits);
    }
    collect_windows(&mut buckets, value);
    push_named_windows(&mut buckets, value);
    push_scoped(&mut buckets, value);
    if let Some(samples) = value.get("samples").and_then(Value::as_array)
        && let Some(usage) = samples.last().and_then(|sample| sample.get("u"))
    {
        push_history(&mut buckets, usage);
    }
    if buckets.is_empty() {
        return None;
    }
    Some(finish(agent, source, buckets))
}

fn finish(agent: Agent, source: &str, buckets: Vec<Bucket>) -> Snapshot {
    let warn = env_f64("MAGENTS_USAGE_WARN", DEFAULT_WARN);
    let critical = env_f64("MAGENTS_USAGE_CRITICAL", DEFAULT_CRITICAL);
    let mut limiting = None;
    let mut level = Level::Ok;
    let mut best = f64::NEG_INFINITY;
    for bucket in &buckets {
        let bucket_level = if bucket.used_percentage >= critical {
            Level::Critical
        } else if bucket.used_percentage >= warn {
            Level::Warn
        } else {
            Level::Ok
        };
        if rank(bucket_level) > rank(level) {
            level = bucket_level;
        }
        if bucket.used_percentage > best {
            best = bucket.used_percentage;
            limiting = Some(bucket.name.clone());
        }
    }
    Snapshot {
        agent,
        source: source.to_string(),
        buckets,
        limiting,
        level,
    }
}

fn push_scoped(buckets: &mut Vec<Bucket>, value: &Value) {
    if let Some(Value::Object(map)) = value.get("model_scoped") {
        for (name, bucket) in map {
            push_bucket(buckets, name, Some(bucket));
        }
    }
}

fn push_named_windows(buckets: &mut Vec<Bucket>, value: &Value) {
    push_bucket(buckets, "five_hour", value.get("five_hour"));
    push_bucket(buckets, "seven_day", value.get("seven_day"));
    for key in ["seven_day_sonnet", "seven_day_opus", "seven_day_oauth_apps"] {
        push_bucket(buckets, key, value.get(key));
    }
}

fn collect_windows(buckets: &mut Vec<Bucket>, value: &Value) {
    for key in ["primary", "secondary"] {
        if let Some(window) = value.get(key) {
            let name = window_name(window, key);
            push_bucket(buckets, &name, Some(window));
        }
    }
}

fn window_name(window: &Value, fallback: &str) -> String {
    let minutes = window.get("window_minutes").and_then(number).or_else(|| {
        window
            .get("limit_window_seconds")
            .and_then(number)
            .map(|seconds| seconds / 60.0)
    });
    match minutes {
        Some(minutes) if (240.0..360.0).contains(&minutes) => "five_hour".into(),
        Some(minutes) if (1_200.0..1_800.0).contains(&minutes) => "daily".into(),
        Some(minutes) if (9_000.0..12_000.0).contains(&minutes) => "seven_day".into(),
        Some(minutes) if (40_000.0..50_000.0).contains(&minutes) => "thirty_day".into(),
        _ => fallback.to_string(),
    }
}

fn push_grok(buckets: &mut Vec<Bucket>, config: &Value) {
    let Some(used) = number_field(config, "creditUsagePercent")
        .or_else(|| number_field(config, "credit_usage_percent"))
    else {
        return;
    };
    let period = config
        .get("currentPeriod")
        .or_else(|| config.get("current_period"));
    let name = match period
        .and_then(|value| value.get("type"))
        .and_then(Value::as_str)
    {
        Some(kind) if kind.to_ascii_uppercase().contains("MONTH") => "thirty_day",
        _ => "seven_day",
    };
    let resets_at = period
        .and_then(|value| value.get("end"))
        .and_then(timestamp)
        .or_else(|| timestamp_field(config, "billingPeriodEnd"))
        .or_else(|| timestamp_field(config, "billing_period_end"));
    buckets.push(Bucket {
        name: name.into(),
        used_percentage: used,
        resets_at,
    });
}

fn push_history(buckets: &mut Vec<Bucket>, usage: &Value) {
    if let Some(used) = number_field(usage, "fh") {
        buckets.push(Bucket {
            name: "five_hour".into(),
            used_percentage: used,
            resets_at: None,
        });
    }
    if let Some(used) = number_field(usage, "sd") {
        buckets.push(Bucket {
            name: "seven_day".into(),
            used_percentage: used,
            resets_at: None,
        });
    }
}

fn push_bucket(buckets: &mut Vec<Bucket>, name: &str, value: Option<&Value>) {
    let Some(value) = value else {
        return;
    };
    if value.is_null() {
        return;
    }
    let Some(used) = percent(value) else {
        return;
    };
    let resets_at = value
        .get("resets_at")
        .or_else(|| value.get("reset_at"))
        .or_else(|| value.get("end"))
        .and_then(timestamp);
    buckets.push(Bucket {
        name: name.to_string(),
        used_percentage: used,
        resets_at,
    });
}

fn percent(value: &Value) -> Option<f64> {
    if let Some(number) = number(value) {
        return Some(number);
    }
    let raw = value
        .get("used_percentage")
        .or_else(|| value.get("used_percent"))
        .or_else(|| value.get("utilization"))?;
    let mut amount = number(raw)?;
    if amount <= 1.0 && value.get("utilization").is_some() && value.get("used_percentage").is_none()
    {
        amount *= 100.0;
    }
    Some(amount)
}

fn grok_config(value: &Value) -> Option<&Value> {
    let nested = value
        .get("ctx")
        .and_then(|ctx| ctx.get("config"))
        .or_else(|| value.get("config"));
    let candidate = nested.unwrap_or(value);
    if number_field(candidate, "creditUsagePercent").is_some()
        || number_field(candidate, "credit_usage_percent").is_some()
    {
        Some(candidate)
    } else {
        None
    }
}

fn is_grok_billing(value: &Value) -> bool {
    value
        .get("msg")
        .and_then(Value::as_str)
        .is_some_and(|message| message.contains("credits config"))
        || grok_config(value).is_some()
}

fn is_codex_limits(value: &Value) -> bool {
    find_rate_limits(value).is_some()
}

fn find_rate_limits(value: &Value) -> Option<&Value> {
    find_key(value, "rate_limits", 4)
        .filter(|limits| limits.get("primary").is_some() || limits.get("secondary").is_some())
}

fn find_key<'a>(value: &'a Value, key: &str, depth: u8) -> Option<&'a Value> {
    let object = value.as_object()?;
    if let Some(found) = object.get(key) {
        return Some(found);
    }
    if depth == 0 {
        return None;
    }
    for nested in object.values() {
        if let Some(found) = find_key(nested, key, depth - 1) {
            return Some(found);
        }
    }
    None
}

fn detect_agent(value: &Value) -> Option<Agent> {
    if let Some(agent) = value
        .get("agent")
        .or_else(|| value.get("source"))
        .and_then(Value::as_str)
        .and_then(Agent::parse)
    {
        return Some(agent);
    }
    if value.get("rate_limits").is_some() {
        return Some(Agent::Claude);
    }
    if is_grok_billing(value) {
        return Some(Agent::Grok);
    }
    if is_codex_limits(value) {
        return Some(Agent::Codex);
    }
    None
}

fn agent_home(homes: &Homes, agent: Agent) -> &Path {
    match agent {
        Agent::Claude => homes.claude.as_path(),
        Agent::Codex => homes.codex.as_path(),
        Agent::Cursor => homes.cursor.as_path(),
        Agent::Grok => homes.grok.as_path(),
        Agent::OpenCode => homes.opencode.as_path(),
    }
}

fn encode_snapshot(snapshot: &Snapshot) -> Result<String> {
    let mut root = serde_json::Map::new();
    root.insert("source".into(), Value::from(snapshot.agent.as_str()));
    root.insert(
        "updated_at".into(),
        Value::from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_secs())
                .unwrap_or(0),
        ),
    );
    for bucket in &snapshot.buckets {
        let mut body = serde_json::Map::new();
        body.insert(
            "used_percentage".into(),
            Value::from(bucket.used_percentage),
        );
        if let Some(resets_at) = bucket.resets_at {
            body.insert("resets_at".into(), Value::from(resets_at));
        }
        root.insert(bucket.name.clone(), Value::Object(body));
    }
    Ok(serde_json::to_string(&Value::Object(root))?)
}

fn last_jsonl(path: &Path, max_bytes: usize, pred: fn(&Value) -> bool) -> Option<Value> {
    let tail = read_tail(path, max_bytes)?;
    let mut found = None;
    for line in tail.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if pred(&value) {
            found = Some(value);
        }
    }
    found
}

fn read_tail(path: &Path, max_bytes: usize) -> Option<String> {
    let mut file = fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(max_bytes as u64);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = String::new();
    file.read_to_string(&mut buf).ok()?;
    if start > 0
        && let Some(offset) = buf.find('\n')
    {
        buf = buf[offset + 1..].to_string();
    }
    Some(buf)
}

fn newest_jsonl(root: &Path, limit: usize) -> Vec<PathBuf> {
    if !root.is_dir() || limit == 0 {
        return Vec::new();
    }
    let mut files: Vec<PathBuf> = WalkDir::new(root)
        .into_iter()
        .flatten()
        .map(|entry| entry.into_path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("jsonl"))
        .collect();
    files.sort_by_key(|path| {
        std::cmp::Reverse(
            path.metadata()
                .and_then(|meta| meta.modified())
                .unwrap_or(UNIX_EPOCH),
        )
    });
    files.truncate(limit);
    files
}

fn number_field(value: &Value, key: &str) -> Option<f64> {
    value.get(key).and_then(number)
}

fn timestamp_field(value: &Value, key: &str) -> Option<i64> {
    value.get(key).and_then(timestamp)
}

fn number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_i64().map(|n| n as f64))
        .or_else(|| value.as_u64().map(|n| n as f64))
}

fn timestamp(value: &Value) -> Option<i64> {
    if let Some(number) = value.as_i64() {
        return Some(number);
    }
    if let Some(number) = value.as_u64() {
        return Some(number as i64);
    }
    DateTime::parse_from_rfc3339(value.as_str()?)
        .ok()
        .map(|time| time.timestamp())
}

fn rank(level: Level) -> u8 {
    match level {
        Level::Ok => 0,
        Level::Warn => 1,
        Level::Critical => 2,
    }
}

fn env_f64(key: &str, default: f64) -> f64 {
    std::env::var(key)
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|value| *value > 0.0)
        .unwrap_or(default)
}

pub fn summary(snapshot: &Snapshot) -> String {
    let limiting = snapshot
        .limiting
        .as_deref()
        .and_then(|name| snapshot.buckets.iter().find(|bucket| bucket.name == name));
    match limiting {
        Some(bucket) => {
            let label = match bucket.name.as_str() {
                "seven_day" => "weekly",
                "five_hour" => "5-hour",
                "thirty_day" => "monthly",
                "daily" => "daily",
                other => other,
            };
            format!(
                "{} {} usage {:.0}%",
                snapshot.agent, label, bucket.used_percentage
            )
        }
        None => format!("{} usage", snapshot.agent),
    }
}

#[cfg(test)]
mod tests {
    use super::{Level, claude, exhausted, ingest_statusline, report};
    use crate::homes::Homes;
    use crate::model::Agent;
    use crate::test_env;
    use serde_json::json;

    const ENV: &[&str] = &["MAGENTS_USAGE_WARN", "MAGENTS_USAGE_CRITICAL"];

    fn write_limits(homes: &Homes, weekly: i64, five: i64) {
        std::fs::create_dir_all(&homes.claude).unwrap();
        std::fs::write(
            homes.claude.join("abtop-rate-limits.json"),
            json!({
                "source": "claude",
                "five_hour": { "used_percentage": five, "resets_at": 1 },
                "seven_day": { "used_percentage": weekly, "resets_at": 2 }
            })
            .to_string(),
        )
        .unwrap();
    }

    fn write_grok_log(homes: &Homes, percent: f64, period: &str) {
        std::fs::create_dir_all(homes.grok.join("logs")).unwrap();
        std::fs::write(
            homes.grok.join("logs").join("unified.jsonl"),
            format!(
                "{}\n{}\n",
                json!({"msg":"noise"}),
                json!({
                    "msg": "billing: fetched credits config",
                    "ctx": {
                        "config": {
                            "creditUsagePercent": percent,
                            "currentPeriod": {
                                "type": period,
                                "end": "2026-09-04T02:07:17.527597+00:00"
                            }
                        }
                    }
                })
            ),
        )
        .unwrap();
    }

    fn write_codex_rollout(homes: &Homes, weekly: f64, five: Option<f64>) {
        let dir = homes
            .codex
            .join("sessions")
            .join("2026")
            .join("08")
            .join("29");
        std::fs::create_dir_all(&dir).unwrap();
        let mut limits = json!({
            "primary": { "used_percent": weekly, "window_minutes": 10080, "resets_at": 9 }
        });
        if let Some(five) = five {
            limits["secondary"] = json!({
                "used_percent": five,
                "window_minutes": 300,
                "resets_at": 8
            });
        }
        std::fs::write(
            dir.join("rollout.jsonl"),
            format!(
                "{}\n{}\n",
                json!({"type":"noise"}),
                json!({
                    "type": "event_msg",
                    "payload": { "type": "token_count", "rate_limits": limits }
                })
            ),
        )
        .unwrap();
    }

    #[test]
    fn weekly_100_is_critical() {
        let _guard = test_env::lock(ENV);
        unsafe {
            std::env::remove_var("MAGENTS_USAGE_WARN");
            std::env::remove_var("MAGENTS_USAGE_CRITICAL");
        }
        let dir = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(dir.path());
        write_limits(&homes, 100, 0);
        let snapshot = claude(&homes).unwrap();
        assert_eq!(snapshot.level, Level::Critical);
        assert_eq!(snapshot.limiting.as_deref(), Some("seven_day"));
        assert!(exhausted(&homes, Agent::Claude));
        assert!(!exhausted(&homes, Agent::Grok));
    }

    #[test]
    fn weekly_80_is_warn() {
        let _guard = test_env::lock(ENV);
        unsafe {
            std::env::remove_var("MAGENTS_USAGE_WARN");
            std::env::remove_var("MAGENTS_USAGE_CRITICAL");
        }
        let dir = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(dir.path());
        write_limits(&homes, 80, 10);
        let snapshot = claude(&homes).unwrap();
        assert_eq!(snapshot.level, Level::Warn);
        assert_eq!(snapshot.limiting.as_deref(), Some("seven_day"));
    }

    #[test]
    fn five_hour_critical_beats_low_weekly() {
        let _guard = test_env::lock(ENV);
        let dir = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(dir.path());
        write_limits(&homes, 20, 95);
        let snapshot = claude(&homes).unwrap();
        assert_eq!(snapshot.level, Level::Critical);
        assert_eq!(snapshot.limiting.as_deref(), Some("five_hour"));
    }

    #[test]
    fn ingest_writes_rate_limits_json() {
        let _guard = test_env::lock(ENV);
        unsafe {
            std::env::remove_var("MAGENTS_USAGE_WARN");
            std::env::remove_var("MAGENTS_USAGE_CRITICAL");
        }
        let dir = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(dir.path());
        let snapshot = ingest_statusline(
            &homes,
            &json!({
                "model": "opus",
                "rate_limits": {
                    "five_hour": { "used_percentage": 12.5, "resets_at": 9 },
                    "seven_day": { "used_percentage": 40, "resets_at": 10 },
                    "model_scoped": {
                        "sonnet": { "used_percentage": 11, "resets_at": 11 }
                    }
                }
            })
            .to_string(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(snapshot.level, Level::Ok);
        assert!(homes.claude.join("rate-limits.json").is_file());
        assert!(
            snapshot
                .buckets
                .iter()
                .any(|bucket| bucket.name == "sonnet")
        );
        assert_eq!(super::summary(&snapshot), "claude weekly usage 40%");
    }

    #[test]
    fn ingest_skips_statusline_without_rate_limits() {
        let dir = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(dir.path());
        assert!(
            ingest_statusline(&homes, r#"{"model":"opus"}"#)
                .unwrap()
                .is_none()
        );
        assert!(ingest_statusline(&homes, "not-json").is_err());
    }

    #[test]
    fn missing_or_empty_files_are_none() {
        let dir = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(dir.path());
        assert!(claude(&homes).is_none());
        assert!(!exhausted(&homes, Agent::Claude));
        assert!(super::for_agent(&homes, Agent::Grok).is_none());
        std::fs::create_dir_all(&homes.claude).unwrap();
        std::fs::write(homes.claude.join("abtop-rate-limits.json"), "not-json").unwrap();
        std::fs::write(homes.claude.join("rate-limits.json"), "{}").unwrap();
        assert!(claude(&homes).is_none());
        std::fs::write(
            homes.claude.join("rate-limits.json"),
            json!({ "five_hour": { "resets_at": 1 } }).to_string(),
        )
        .unwrap();
        assert!(claude(&homes).is_none());
    }

    #[test]
    fn falls_back_to_rate_limits_json() {
        let _guard = test_env::lock(ENV);
        unsafe {
            std::env::remove_var("MAGENTS_USAGE_WARN");
            std::env::remove_var("MAGENTS_USAGE_CRITICAL");
        }
        let dir = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(dir.path());
        std::fs::create_dir_all(&homes.claude).unwrap();
        std::fs::write(homes.claude.join("abtop-rate-limits.json"), "{").unwrap();
        std::fs::write(
            homes.claude.join("rate-limits.json"),
            json!({
                "five_hour": { "used_percentage": 10, "resets_at": 9223372036854775808u64 },
                "seven_day": { "used_percentage": 20 },
                "seven_day_sonnet": { "used_percentage": 30 },
                "seven_day_opus": { "used_percentage": 40 },
                "seven_day_oauth_apps": { "used_percentage": 50 },
                "model_scoped": {
                    "haiku": { "used_percentage": 15 },
                    "skip": { "used_percentage": "x" }
                }
            })
            .to_string(),
        )
        .unwrap();
        let snapshot = claude(&homes).unwrap();
        assert_eq!(snapshot.source, "rate-limits.json");
        assert_eq!(snapshot.level, Level::Ok);
        assert!(snapshot.buckets.iter().any(|bucket| bucket.name == "haiku"));
        assert!(
            snapshot
                .buckets
                .iter()
                .any(|bucket| bucket.name == "seven_day_opus")
        );
        assert!(
            snapshot
                .buckets
                .iter()
                .any(|bucket| bucket.resets_at.is_some())
        );
        assert!(!snapshot.buckets.iter().any(|bucket| bucket.name == "skip"));
    }

    #[test]
    fn utilization_and_integer_percentages() {
        let _guard = test_env::lock(ENV);
        unsafe {
            std::env::remove_var("MAGENTS_USAGE_WARN");
            std::env::remove_var("MAGENTS_USAGE_CRITICAL");
        }
        let dir = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(dir.path());
        std::fs::create_dir_all(&homes.claude).unwrap();
        std::fs::write(
            homes.claude.join("rate-limits.json"),
            json!({
                "five_hour": { "utilization": 0.95 },
                "seven_day": { "utilization": 80 },
                "seven_day_sonnet": { "used_percentage": 1, "utilization": 0.9 }
            })
            .to_string(),
        )
        .unwrap();
        let snapshot = claude(&homes).unwrap();
        assert_eq!(snapshot.level, Level::Critical);
        assert_eq!(snapshot.limiting.as_deref(), Some("five_hour"));
        let five = snapshot
            .buckets
            .iter()
            .find(|bucket| bucket.name == "five_hour")
            .unwrap();
        assert!((five.used_percentage - 95.0).abs() < 0.01);
        let weekly = snapshot
            .buckets
            .iter()
            .find(|bucket| bucket.name == "seven_day")
            .unwrap();
        assert!((weekly.used_percentage - 80.0).abs() < 0.01);
        assert_eq!(super::summary(&snapshot), "claude 5-hour usage 95%");
        let scoped = super::Snapshot {
            agent: Agent::Claude,
            source: "test".into(),
            buckets: snapshot.buckets.clone(),
            limiting: Some("seven_day_sonnet".into()),
            level: Level::Ok,
        };
        assert_eq!(super::summary(&scoped), "claude seven_day_sonnet usage 1%");
    }

    #[test]
    fn custom_and_invalid_thresholds() {
        let _guard = test_env::lock(ENV);
        let dir = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(dir.path());
        write_limits(&homes, 55, 10);
        unsafe {
            std::env::set_var("MAGENTS_USAGE_WARN", "50");
            std::env::set_var("MAGENTS_USAGE_CRITICAL", "60");
        }
        assert_eq!(claude(&homes).unwrap().level, Level::Warn);
        write_limits(&homes, 61, 10);
        assert_eq!(claude(&homes).unwrap().level, Level::Critical);
        unsafe {
            std::env::set_var("MAGENTS_USAGE_WARN", "0");
            std::env::set_var("MAGENTS_USAGE_CRITICAL", "nope");
        }
        write_limits(&homes, 80, 10);
        assert_eq!(claude(&homes).unwrap().level, Level::Warn);
    }

    #[test]
    fn grok_log_weekly_and_monthly() {
        let _guard = test_env::lock(ENV);
        unsafe {
            std::env::remove_var("MAGENTS_USAGE_WARN");
            std::env::remove_var("MAGENTS_USAGE_CRITICAL");
        }
        let dir = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(dir.path());
        write_grok_log(&homes, 61.0, "USAGE_PERIOD_TYPE_WEEKLY");
        let snapshot = super::grok(&homes).unwrap();
        assert_eq!(snapshot.level, Level::Ok);
        assert_eq!(snapshot.limiting.as_deref(), Some("seven_day"));
        assert!(snapshot.buckets[0].resets_at.is_some());
        assert_eq!(super::summary(&snapshot), "grok weekly usage 61%");
        write_grok_log(&homes, 95.0, "USAGE_PERIOD_TYPE_MONTHLY");
        let snapshot = super::grok(&homes).unwrap();
        assert_eq!(snapshot.level, Level::Critical);
        assert_eq!(snapshot.limiting.as_deref(), Some("thirty_day"));
        assert!(exhausted(&homes, Agent::Grok));
    }

    #[test]
    fn grok_falls_back_to_snapshot_file() {
        let _guard = test_env::lock(ENV);
        let dir = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(dir.path());
        std::fs::create_dir_all(&homes.grok).unwrap();
        std::fs::write(
            homes.grok.join("rate-limits.json"),
            json!({ "seven_day": { "used_percentage": 80 } }).to_string(),
        )
        .unwrap();
        let snapshot = super::grok(&homes).unwrap();
        assert_eq!(snapshot.level, Level::Warn);
        assert_eq!(super::summary(&snapshot), "grok weekly usage 80%");
    }

    #[test]
    fn codex_rollout_maps_windows() {
        let _guard = test_env::lock(ENV);
        unsafe {
            std::env::remove_var("MAGENTS_USAGE_WARN");
            std::env::remove_var("MAGENTS_USAGE_CRITICAL");
        }
        let dir = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(dir.path());
        write_codex_rollout(&homes, 5.0, Some(95.0));
        let snapshot = super::codex(&homes).unwrap();
        assert_eq!(snapshot.level, Level::Critical);
        assert_eq!(snapshot.limiting.as_deref(), Some("five_hour"));
        assert!(exhausted(&homes, Agent::Codex));
        write_codex_rollout(&homes, 40.0, None);
        let snapshot = super::codex(&homes).unwrap();
        assert_eq!(snapshot.level, Level::Ok);
        assert_eq!(super::summary(&snapshot), "codex weekly usage 40%");
    }

    #[test]
    fn cursor_and_opencode_read_snapshot_files() {
        let _guard = test_env::lock(ENV);
        let dir = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(dir.path());
        std::fs::create_dir_all(&homes.cursor).unwrap();
        std::fs::create_dir_all(&homes.opencode).unwrap();
        std::fs::write(
            homes.cursor.join("usage-limits.json"),
            json!({ "seven_day": { "used_percentage": 100 } }).to_string(),
        )
        .unwrap();
        std::fs::write(
            homes.opencode.join("rate-limits.json"),
            json!({ "five_hour": { "used_percentage": 12 }, "seven_day": { "used_percentage": 20 } })
                .to_string(),
        )
        .unwrap();
        assert!(exhausted(&homes, Agent::Cursor));
        let opencode = super::for_agent(&homes, Agent::OpenCode).unwrap();
        assert_eq!(opencode.level, Level::Ok);
        assert_eq!(super::summary(&opencode), "opencode weekly usage 20%");
    }

    #[test]
    fn claude_plan_history_and_report() {
        let _guard = test_env::lock(ENV);
        unsafe {
            std::env::remove_var("MAGENTS_USAGE_WARN");
            std::env::remove_var("MAGENTS_USAGE_CRITICAL");
        }
        let dir = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(dir.path());
        std::fs::write(
            dir.path().join("plan-usage-history.json"),
            json!({
                "version": "2",
                "samples": [
                    { "u": { "fh": 1, "sd": 1 } },
                    { "u": { "fh": 8, "sd": 92 } }
                ]
            })
            .to_string(),
        )
        .unwrap();
        write_grok_log(&homes, 10.0, "USAGE_PERIOD_TYPE_WEEKLY");
        let snapshot = claude(&homes).unwrap();
        assert_eq!(snapshot.level, Level::Critical);
        assert_eq!(snapshot.limiting.as_deref(), Some("seven_day"));
        let listed = report(&homes);
        assert!(listed.iter().any(|row| row.agent == Agent::Claude));
        assert!(listed.iter().any(|row| row.agent == Agent::Grok));
    }

    #[test]
    fn ingest_routes_grok_and_codex() {
        let _guard = test_env::lock(ENV);
        let dir = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(dir.path());
        let grok = ingest_statusline(
            &homes,
            &json!({
                "msg": "billing: fetched credits config",
                "ctx": { "config": { "creditUsagePercent": 77 } }
            })
            .to_string(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(grok.agent, Agent::Grok);
        assert_eq!(grok.level, Level::Warn);
        assert!(homes.grok.join("rate-limits.json").is_file());
        let codex = ingest_statusline(
            &homes,
            &json!({
                "agent": "codex",
                "payload": {
                    "rate_limits": {
                        "primary": { "used_percent": 11, "window_minutes": 10080 }
                    }
                }
            })
            .to_string(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(codex.agent, Agent::Codex);
        assert!(homes.codex.join("rate-limits.json").is_file());
    }

    #[test]
    fn jsonl_skips_noise_and_empty_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let homes = Homes::isolated(dir.path());
        std::fs::create_dir_all(homes.grok.join("logs")).unwrap();
        std::fs::write(
            homes.grok.join("logs").join("unified.jsonl"),
            "not-json\n{}\n",
        )
        .unwrap();
        assert!(super::grok(&homes).is_none());
        assert!(super::codex(&homes).is_none());
        std::fs::create_dir_all(homes.codex.join("sessions")).unwrap();
        std::fs::write(homes.codex.join("sessions").join("empty.jsonl"), "{}\n").unwrap();
        assert!(super::codex(&homes).is_none());
    }
}
