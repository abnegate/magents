use crate::error::Result;
use crate::homes::Homes;
use crate::model::Agent;
use serde::Serialize;
use serde_json::Value;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

const DEFAULT_WARN: f64 = 75.0;
const DEFAULT_CRITICAL: f64 = 90.0;

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

pub fn for_agent(homes: &Homes, agent: Agent) -> Option<Snapshot> {
    match agent {
        Agent::Claude => claude(homes),
        Agent::Codex | Agent::Cursor | Agent::Grok | Agent::OpenCode => None,
    }
}

pub fn claude(homes: &Homes) -> Option<Snapshot> {
    for name in ["abtop-rate-limits.json", "rate-limits.json"] {
        let path = homes.claude.join(name);
        if let Some(snapshot) = load_claude(&path, name) {
            return Some(snapshot);
        }
    }
    None
}

pub fn ingest_statusline(homes: &Homes, raw: &str) -> Result<Option<Snapshot>> {
    let value: Value = serde_json::from_str(raw)?;
    let Some(limits) = value.get("rate_limits") else {
        return Ok(None);
    };
    let mut root = serde_json::Map::new();
    root.insert("source".into(), Value::from("claude"));
    root.insert(
        "updated_at".into(),
        Value::from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_secs())
                .unwrap_or(0),
        ),
    );
    for key in ["five_hour", "seven_day"] {
        if let Some(bucket) = limits.get(key) {
            root.insert(key.into(), bucket.clone());
        }
    }
    if let Some(scoped) = limits.get("model_scoped") {
        root.insert("model_scoped".into(), scoped.clone());
    }
    let encoded = serde_json::to_string(&Value::Object(root))?;
    let path = homes.claude.join("rate-limits.json");
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    fs::write(&path, encoded).map_err(|source| crate::error::Error::Io {
        path: path.clone(),
        source,
    })?;
    Ok(load_claude(&path, "rate-limits.json"))
}

pub fn exhausted(homes: &Homes, agent: Agent) -> bool {
    for_agent(homes, agent).is_some_and(|snapshot| snapshot.level == Level::Critical)
}

fn load_claude(path: &std::path::Path, source: &str) -> Option<Snapshot> {
    let raw = fs::read_to_string(path).ok()?;
    let value: Value = serde_json::from_str(&raw).ok()?;
    let mut buckets = Vec::new();
    push_bucket(&mut buckets, "five_hour", value.get("five_hour"));
    push_bucket(&mut buckets, "seven_day", value.get("seven_day"));
    if let Some(Value::Object(map)) = value.get("model_scoped") {
        for (name, bucket) in map {
            push_bucket(&mut buckets, name, Some(bucket));
        }
    }
    for key in ["seven_day_sonnet", "seven_day_opus", "seven_day_oauth_apps"] {
        push_bucket(&mut buckets, key, value.get(key));
    }
    if buckets.is_empty() {
        return None;
    }
    let warn = env_f64("MAGENTS_USAGE_WARN", DEFAULT_WARN);
    let critical = env_f64("MAGENTS_USAGE_CRITICAL", DEFAULT_CRITICAL);
    let mut limiting = None;
    let mut level = Level::Ok;
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
            limiting = Some(bucket.name.clone());
        }
    }
    Some(Snapshot {
        agent: Agent::Claude,
        source: source.to_string(),
        buckets,
        limiting,
        level,
    })
}

fn push_bucket(buckets: &mut Vec<Bucket>, name: &str, value: Option<&Value>) {
    let Some(value) = value else {
        return;
    };
    let Some(used) = percent(value) else {
        return;
    };
    let resets_at = value
        .get("resets_at")
        .and_then(|value| value.as_i64().or_else(|| value.as_u64().map(|n| n as i64)));
    buckets.push(Bucket {
        name: name.to_string(),
        used_percentage: used,
        resets_at,
    });
}

fn percent(value: &Value) -> Option<f64> {
    let raw = value
        .get("used_percentage")
        .or_else(|| value.get("utilization"))?;
    let mut number = raw.as_f64().or_else(|| raw.as_i64().map(|n| n as f64))?;
    if number <= 1.0 && value.get("utilization").is_some() && value.get("used_percentage").is_none()
    {
        number *= 100.0;
    }
    Some(number)
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
    use super::{Level, claude, exhausted, ingest_statusline};
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
        assert_eq!(super::summary(&snapshot), "claude usage");
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
}
