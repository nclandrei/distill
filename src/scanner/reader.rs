use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;

use crate::agents::{Agent, Session};

/// Persisted scan watermark — tracks when the last scan ran.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LastScan {
    pub timestamp: DateTime<Utc>,
    pub session_ids: Vec<String>,
}

impl LastScan {
    /// Load last scan info from disk, or return None if no scan has run yet.
    pub fn load(path: &Path) -> Result<Option<Self>> {
        if !path.exists() {
            return Ok(None);
        }
        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        let last_scan: Self =
            serde_json::from_str(&contents).with_context(|| "Failed to parse last-scan.json")?;
        Ok(Some(last_scan))
    }

    /// Save scan watermark to disk.
    pub fn save(&self, path: &Path) -> Result<()> {
        let json =
            serde_json::to_string_pretty(self).context("Failed to serialize last-scan.json")?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, json)
            .with_context(|| format!("Failed to write {}", path.display()))?;
        Ok(())
    }
}

/// Collect sessions from all agents since `since`, deduplicated by session ID.
pub fn collect_sessions(agents: &[Box<dyn Agent>], since: DateTime<Utc>) -> Result<Vec<Session>> {
    let mut seen_ids = HashSet::new();
    let mut all_sessions = Vec::new();

    for agent in agents {
        let sessions = agent
            .read_sessions(since)
            .with_context(|| format!("Failed to read sessions from {} agent", agent.kind()))?;

        for session in sessions {
            if seen_ids.insert(session.id.clone()) {
                all_sessions.push(session);
            }
        }
    }

    // Sort by timestamp ascending so oldest sessions come first
    all_sessions.sort_by_key(|s| s.timestamp);

    Ok(all_sessions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::AgentKind;
    use std::path::PathBuf;

    fn make_session(id: &str, agent: AgentKind, hours_ago: i64) -> Session {
        Session {
            id: id.to_string(),
            agent,
            path: PathBuf::from(format!("/fake/{id}.jsonl")),
            timestamp: Utc::now() - chrono::Duration::hours(hours_ago),
            content: format!("session content for {id}"),
        }
    }

    /// A test agent that returns pre-configured sessions.
    struct MockAgent {
        kind: AgentKind,
        sessions: Vec<Session>,
    }

    impl Agent for MockAgent {
        fn kind(&self) -> AgentKind {
            self.kind
        }

        fn read_sessions(&self, _since: DateTime<Utc>) -> Result<Vec<Session>> {
            Ok(self.sessions.clone())
        }

        fn write_skill(&self, _skill: &crate::agents::Skill) -> Result<()> {
            Ok(())
        }

        fn config_dir(&self) -> PathBuf {
            PathBuf::from("/fake")
        }
    }

    #[test]
    fn test_collect_sessions_deduplicates() {
        let shared = make_session("dup-1", AgentKind::Claude, 2);

        let agent1: Box<dyn Agent> = Box::new(MockAgent {
            kind: AgentKind::Claude,
            sessions: vec![shared.clone(), make_session("a-1", AgentKind::Claude, 3)],
        });
        let agent2: Box<dyn Agent> = Box::new(MockAgent {
            kind: AgentKind::Codex,
            sessions: vec![shared, make_session("b-1", AgentKind::Codex, 1)],
        });

        let result =
            collect_sessions(&[agent1, agent2], Utc::now() - chrono::Duration::days(1)).unwrap();
        assert_eq!(result.len(), 3); // dup-1 only counted once
        let ids: Vec<&str> = result.iter().map(|s| s.id.as_str()).collect();
        assert!(ids.contains(&"dup-1"));
        assert!(ids.contains(&"a-1"));
        assert!(ids.contains(&"b-1"));
    }

    #[test]
    fn test_collect_sessions_sorted_by_timestamp() {
        let agent: Box<dyn Agent> = Box::new(MockAgent {
            kind: AgentKind::Claude,
            sessions: vec![
                make_session("newer", AgentKind::Claude, 1),
                make_session("older", AgentKind::Claude, 10),
            ],
        });

        let result = collect_sessions(&[agent], Utc::now() - chrono::Duration::days(1)).unwrap();
        assert_eq!(result[0].id, "older");
        assert_eq!(result[1].id, "newer");
    }

    #[test]
    fn test_last_scan_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("last-scan.json");

        let scan = LastScan {
            timestamp: Utc::now(),
            session_ids: vec!["s1".into(), "s2".into()],
        };

        scan.save(&path).unwrap();

        let loaded = LastScan::load(&path).unwrap().unwrap();
        assert_eq!(loaded.session_ids, scan.session_ids);
    }

    #[test]
    fn test_last_scan_load_missing_file() {
        let result = LastScan::load(Path::new("/nonexistent/last-scan.json")).unwrap();
        assert!(result.is_none());
    }
}
