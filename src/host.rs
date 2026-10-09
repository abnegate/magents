use crate::model::Agent;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::{Command, Stdio};

const HOSTS: &[(&str, Agent)] = &[("opencode", Agent::OpenCode)];

pub fn detect() -> Option<Agent> {
    let output = Command::new("ps")
        .env("LC_ALL", "C")
        .args(["-A", "-o", "pid=,ppid=,comm="])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    ancestor(
        &String::from_utf8_lossy(&output.stdout),
        std::os::unix::process::parent_id(),
    )
}

fn ancestor(table: &str, pid: u32) -> Option<Agent> {
    let processes: HashMap<u32, (u32, &str)> = table.lines().filter_map(process).collect();
    let mut visited = HashSet::new();
    let mut current = pid;
    while current > 1 && visited.insert(current) {
        let (parent, command) = processes.get(&current)?;
        if let Some(agent) = host(command) {
            return Some(agent);
        }
        current = *parent;
    }
    None
}

fn process(line: &str) -> Option<(u32, (u32, &str))> {
    let line = line.trim_start();
    let (pid, rest) = line.split_once(char::is_whitespace)?;
    let rest = rest.trim_start();
    let (parent, command) = rest.split_once(char::is_whitespace)?;
    Some((pid.parse().ok()?, (parent.parse().ok()?, command.trim())))
}

fn host(command: &str) -> Option<Agent> {
    let name = Path::new(command)
        .file_name()?
        .to_str()?
        .trim_start_matches(['-', '.'])
        .to_ascii_lowercase();
    HOSTS
        .iter()
        .find(|(binary, _)| *binary == name)
        .map(|(_, agent)| *agent)
}

#[cfg(test)]
mod tests {
    use super::{ancestor, host};
    use crate::model::Agent;

    const TABLE: &str = "    1     0 /sbin/launchd
  100     1 /usr/local/bin/opencode
  200   100 /Users/me/.cargo/bin/magents
  300     1 /Applications/Cursor.app/Contents/Frameworks/Cursor Helper (Plugin)
  400   300 /Users/me/.cargo/bin/magents
  500   100 node
  600   500 magents
  700   700 loop
";

    #[test]
    fn finds_nearest_opencode_ancestor() {
        assert_eq!(ancestor(TABLE, 100), Some(Agent::OpenCode));
        assert_eq!(ancestor(TABLE, 200), Some(Agent::OpenCode));
        assert_eq!(ancestor(TABLE, 600), Some(Agent::OpenCode));
    }

    #[test]
    fn ignores_other_hosts_cycles_and_unknown_processes() {
        assert_eq!(ancestor(TABLE, 400), None);
        assert_eq!(ancestor(TABLE, 700), None);
        assert_eq!(ancestor(TABLE, 999), None);
        assert_eq!(ancestor("", 200), None);
    }

    #[test]
    fn matches_binary_basenames() {
        assert_eq!(host("opencode"), Some(Agent::OpenCode));
        assert_eq!(host("/opt/bin/.opencode"), Some(Agent::OpenCode));
        assert_eq!(host("OpenCode"), Some(Agent::OpenCode));
        assert_eq!(host("opencode-helper"), None);
        assert_eq!(host("claude"), None);
        assert_eq!(host(""), None);
    }
}
