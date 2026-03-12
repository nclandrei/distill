use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::scanner::timeline::SessionDescriptor;

const SCAN_STATE_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowFinding {
    pub workflow_key: String,
    pub workflow_label: Option<String>,
    pub note: String,
    pub start_event: usize,
    pub end_event: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ScanState {
    #[serde(default = "default_scan_state_version")]
    pub version: u32,
    #[serde(default)]
    pub sessions: BTreeMap<String, StoredSessionFinding>,
    #[serde(default)]
    pub workflows: BTreeMap<String, StoredWorkflowState>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredSessionFinding {
    pub session_id: String,
    pub agent: String,
    pub timestamp: DateTime<Utc>,
    pub last_processed_at: DateTime<Utc>,
    #[serde(default)]
    pub findings: Vec<WorkflowFinding>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StoredWorkflowState {
    #[serde(default)]
    pub workflow_label: Option<String>,
    #[serde(default)]
    pub proposed: bool,
    #[serde(default)]
    pub last_attempted_count: usize,
    #[serde(default)]
    pub last_attempted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowMatch {
    pub session_path: PathBuf,
    pub session_id: String,
    pub agent: String,
    pub timestamp: DateTime<Utc>,
    pub finding: WorkflowFinding,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadyWorkflow {
    pub workflow_key: String,
    pub workflow_label: Option<String>,
    pub matches: Vec<WorkflowMatch>,
}

fn default_scan_state_version() -> u32 {
    SCAN_STATE_VERSION
}

impl ScanState {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self {
                version: SCAN_STATE_VERSION,
                ..Self::default()
            });
        }

        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        let mut state: Self = serde_json::from_str(&contents)
            .with_context(|| format!("Failed to parse {}", path.display()))?;
        if state.version == 0 {
            state.version = SCAN_STATE_VERSION;
        }
        Ok(state)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json =
            serde_json::to_string_pretty(self).context("Failed to serialize scan-state.json")?;
        std::fs::write(path, json)
            .with_context(|| format!("Failed to write {}", path.display()))?;
        Ok(())
    }

    pub fn record_session_findings(
        &mut self,
        descriptor: &SessionDescriptor,
        findings: Vec<WorkflowFinding>,
        processed_at: DateTime<Utc>,
    ) {
        let session_path = descriptor.session.path.to_string_lossy().to_string();
        self.sessions.insert(
            session_path,
            StoredSessionFinding {
                session_id: descriptor.session.id.clone(),
                agent: descriptor.session.agent.to_string(),
                timestamp: descriptor.session.timestamp,
                last_processed_at: processed_at,
                findings: findings.clone(),
            },
        );

        for finding in findings {
            let workflow = self.workflows.entry(finding.workflow_key).or_default();
            if workflow.workflow_label.is_none() {
                workflow.workflow_label = finding.workflow_label;
            }
        }
    }

    pub fn ready_workflows(&self, min_matches: usize) -> Vec<ReadyWorkflow> {
        let mut grouped = BTreeMap::<String, Vec<WorkflowMatch>>::new();
        for (session_path, stored) in &self.sessions {
            for finding in &stored.findings {
                grouped
                    .entry(finding.workflow_key.clone())
                    .or_default()
                    .push(WorkflowMatch {
                        session_path: PathBuf::from(session_path),
                        session_id: stored.session_id.clone(),
                        agent: stored.agent.clone(),
                        timestamp: stored.timestamp,
                        finding: finding.clone(),
                    });
            }
        }

        let mut ready = grouped
            .into_iter()
            .filter_map(|(workflow_key, mut matches)| {
                matches.sort_by(|left, right| {
                    right
                        .timestamp
                        .cmp(&left.timestamp)
                        .then_with(|| left.session_path.cmp(&right.session_path))
                });
                let workflow_state = self
                    .workflows
                    .get(&workflow_key)
                    .cloned()
                    .unwrap_or_default();
                if workflow_state.proposed
                    || matches.len() < min_matches
                    || matches.len() <= workflow_state.last_attempted_count
                {
                    return None;
                }
                Some(ReadyWorkflow {
                    workflow_label: workflow_state.workflow_label.clone().or_else(|| {
                        matches
                            .iter()
                            .find_map(|item| item.finding.workflow_label.clone())
                    }),
                    workflow_key,
                    matches,
                })
            })
            .collect::<Vec<_>>();

        ready.sort_by(|left, right| {
            right
                .matches
                .len()
                .cmp(&left.matches.len())
                .then_with(|| left.workflow_key.cmp(&right.workflow_key))
        });
        ready
    }

    pub fn mark_workflow_attempted(
        &mut self,
        workflow_key: &str,
        count: usize,
        attempted_at: DateTime<Utc>,
    ) {
        let workflow = self.workflows.entry(workflow_key.to_string()).or_default();
        workflow.last_attempted_count = workflow.last_attempted_count.max(count);
        workflow.last_attempted_at = Some(attempted_at);
    }

    pub fn mark_workflow_proposed(
        &mut self,
        workflow_key: &str,
        count: usize,
        proposed_at: DateTime<Utc>,
    ) {
        let workflow = self.workflows.entry(workflow_key.to_string()).or_default();
        workflow.proposed = true;
        workflow.last_attempted_count = workflow.last_attempted_count.max(count);
        workflow.last_attempted_at = Some(proposed_at);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{AgentKind, Session};

    fn sample_descriptor(id: &str, hours_ago: i64) -> SessionDescriptor {
        SessionDescriptor {
            session: Session {
                id: id.to_string(),
                agent: AgentKind::Codex,
                path: PathBuf::from(format!("/tmp/{id}.jsonl")),
                timestamp: Utc::now() - chrono::Duration::hours(hours_ago),
                content: String::new(),
            },
            raw_bytes: 42,
            cwd: Some("/tmp/demo".to_string()),
            project: Some("demo".to_string()),
            cohort_key: "codex:demo".to_string(),
        }
    }

    fn finding(key: &str, label: &str) -> WorkflowFinding {
        WorkflowFinding {
            workflow_key: key.to_string(),
            workflow_label: Some(label.to_string()),
            note: "repeated workflow".to_string(),
            start_event: 5,
            end_event: 8,
        }
    }

    #[test]
    fn test_scan_state_ready_workflow_requires_three_sessions() {
        let mut state = ScanState::default();
        let now = Utc::now();
        state.record_session_findings(
            &sample_descriptor("one", 3),
            vec![finding("jj-land-tests", "jj land and test")],
            now,
        );
        state.record_session_findings(
            &sample_descriptor("two", 2),
            vec![finding("jj-land-tests", "jj land and test")],
            now,
        );
        assert!(state.ready_workflows(3).is_empty());

        state.record_session_findings(
            &sample_descriptor("three", 1),
            vec![finding("jj-land-tests", "jj land and test")],
            now,
        );
        let ready = state.ready_workflows(3);
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].workflow_key, "jj-land-tests");
        assert_eq!(ready[0].matches.len(), 3);
    }

    #[test]
    fn test_scan_state_attempted_workflow_waits_for_new_evidence() {
        let mut state = ScanState::default();
        let now = Utc::now();
        for id in ["one", "two", "three"] {
            state.record_session_findings(
                &sample_descriptor(id, 1),
                vec![finding("jj-land-tests", "jj land and test")],
                now,
            );
        }

        state.mark_workflow_attempted("jj-land-tests", 3, now);
        assert!(state.ready_workflows(3).is_empty());

        state.record_session_findings(
            &sample_descriptor("four", 0),
            vec![finding("jj-land-tests", "jj land and test")],
            now,
        );
        assert_eq!(state.ready_workflows(3)[0].matches.len(), 4);
    }

    #[test]
    fn test_scan_state_proposed_workflow_is_not_ready_again() {
        let mut state = ScanState::default();
        let now = Utc::now();
        for id in ["one", "two", "three"] {
            state.record_session_findings(
                &sample_descriptor(id, 1),
                vec![finding("jj-land-tests", "jj land and test")],
                now,
            );
        }

        state.mark_workflow_proposed("jj-land-tests", 3, now);
        assert!(state.ready_workflows(3).is_empty());
    }
}
