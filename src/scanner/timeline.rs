use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use crate::agents::{AgentKind, Session, read_session_source};

const MAX_TIMELINE_TEXT_CHARS: usize = 240;
const SESSION_META_SCAN_LINES: usize = 16;

pub const DEFAULT_WINDOW_BEFORE_EVENTS: usize = 3;
pub const DEFAULT_WINDOW_AFTER_EVENTS: usize = 6;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionDescriptor {
    pub session: Session,
    pub raw_bytes: u64,
    pub cwd: Option<String>,
    pub project: Option<String>,
    pub cohort_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TimelineEntry {
    pub event_number: usize,
    pub kind: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionTimeline {
    pub descriptor: SessionDescriptor,
    pub entries: Vec<TimelineEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TimelineWindowRequest {
    pub workflow_key: String,
    pub workflow_label: Option<String>,
    pub note: String,
    pub start_event: usize,
    pub end_event: usize,
}

pub fn discover_session(session: &Session) -> Result<SessionDescriptor> {
    let raw_bytes = if session.path.exists() {
        std::fs::metadata(&session.path)
            .map(|metadata| metadata.len())
            .unwrap_or(0)
    } else {
        0
    };
    let cwd = discover_session_cwd(session).ok().flatten();
    let project = derive_project(session, cwd.as_deref());
    let cohort_key = match &project {
        Some(project) => format!("{}:{project}", session.agent),
        None => format!("{}:{}", session.agent, session.timestamp.format("%Y-%m-%d")),
    };

    Ok(SessionDescriptor {
        session: session.clone(),
        raw_bytes,
        cwd,
        project,
        cohort_key,
    })
}

pub fn build_session_timeline(descriptor: &SessionDescriptor) -> Result<SessionTimeline> {
    let entries = match descriptor.session.agent {
        AgentKind::Claude | AgentKind::Codex => {
            build_jsonl_timeline(&descriptor.session.path, descriptor.session.agent)?
        }
        AgentKind::OpenCode => {
            let raw = read_session_source(&descriptor.session)?;
            build_opencode_timeline(&raw)
        }
    };

    Ok(SessionTimeline {
        descriptor: descriptor.clone(),
        entries,
    })
}

pub fn render_timeline(timeline: &SessionTimeline) -> String {
    let mut lines = vec![
        "# Staged Session Timeline".to_string(),
        format!("Agent: {}", timeline.descriptor.session.agent),
        format!(
            "Timestamp: {}",
            timeline.descriptor.session.timestamp.to_rfc3339()
        ),
        format!("Original path: {}", timeline.descriptor.session.path.display()),
        format!("Raw bytes: {}", timeline.descriptor.raw_bytes),
    ];

    if let Some(cwd) = &timeline.descriptor.cwd {
        lines.push(format!("CWD: {cwd}"));
    }
    if let Some(project) = &timeline.descriptor.project {
        lines.push(format!("Project: {project}"));
    }
    lines.push(format!("Cohort: {}", timeline.descriptor.cohort_key));
    lines.push(String::new());
    lines.push(
        "This file is an ordered compact timeline extracted from the raw session log. It keeps user messages, assistant messages, tool calls, command executions, and other meaningful events from the whole session."
            .to_string(),
    );
    lines.push(String::new());
    lines.push("## Events".to_string());

    if timeline.entries.is_empty() {
        lines.push("- [0] No meaningful events extracted.".to_string());
    } else {
        for entry in &timeline.entries {
            lines.push(format!(
                "- [{}] {}: {}",
                entry.event_number, entry.kind, entry.detail
            ));
        }
    }

    lines.push(String::new());
    lines.join("\n")
}

pub fn render_timeline_window(
    descriptor: &SessionDescriptor,
    request: &TimelineWindowRequest,
) -> Result<String> {
    let timeline = build_session_timeline(descriptor)?;
    let start = request
        .start_event
        .saturating_sub(DEFAULT_WINDOW_BEFORE_EVENTS)
        .max(1);
    let end = request.end_event + DEFAULT_WINDOW_AFTER_EVENTS;
    let entries = timeline
        .entries
        .iter()
        .filter(|entry| entry.event_number >= start && entry.event_number <= end)
        .cloned()
        .collect::<Vec<_>>();

    let mut lines = vec![
        "# Staged Session Workflow Window".to_string(),
        format!("Agent: {}", descriptor.session.agent),
        format!("Original path: {}", descriptor.session.path.display()),
        format!("Workflow key: {}", request.workflow_key),
    ];
    if let Some(label) = &request.workflow_label {
        lines.push(format!("Workflow label: {label}"));
    }
    lines.push(format!("Detection note: {}", request.note));
    lines.push(format!(
        "Target event range: {}..{}",
        request.start_event, request.end_event
    ));
    lines.push(String::new());
    lines.push(
        "This file is a focused window re-extracted from the raw session log around the detected repeated workflow."
            .to_string(),
    );
    lines.push(String::new());
    lines.push("## Events".to_string());

    if entries.is_empty() {
        lines.push("- [0] No events matched the requested window.".to_string());
    } else {
        for entry in entries {
            lines.push(format!(
                "- [{}] {}: {}",
                entry.event_number, entry.kind, entry.detail
            ));
        }
    }

    lines.push(String::new());
    Ok(lines.join("\n"))
}

fn build_jsonl_timeline(path: &Path, agent: AgentKind) -> Result<Vec<TimelineEntry>> {
    let file =
        File::open(path).with_context(|| format!("Failed to read session {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut entries = Vec::new();

    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        collect_jsonl_entries(agent, &value, &mut entries);
    }

    Ok(entries)
}

fn collect_jsonl_entries(_agent: AgentKind, value: &Value, entries: &mut Vec<TimelineEntry>) {
    let Some(event_type) = value.get("type").and_then(Value::as_str) else {
        return;
    };

    match event_type {
        "response_item" => {
            let Some(payload) = value.get("payload") else {
                return;
            };
            match payload.get("type").and_then(Value::as_str) {
                Some("message") => {
                    let role = payload.get("role").and_then(Value::as_str).unwrap_or("");
                    if matches!(role, "user" | "assistant") {
                        let text = extract_response_message_text(payload);
                        if !text.is_empty() {
                            push_entry(
                                entries,
                                format!("{} message", role.to_ascii_uppercase()),
                                text,
                            );
                        }
                    }
                }
                Some("function_call") => {
                    let name = payload
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown");
                    let arguments = payload
                        .get("arguments")
                        .and_then(Value::as_str)
                        .map(normalize_excerpt)
                        .filter(|value| !value.is_empty());
                    let detail = match arguments {
                        Some(arguments) => format!("{name} {arguments}"),
                        None => name.to_string(),
                    };
                    push_entry(entries, "TOOL call".to_string(), detail);
                }
                Some("web_search_call") => {
                    let action = payload.get("action").unwrap_or(payload);
                    let detail = [
                        action.get("type").and_then(Value::as_str),
                        action.get("query").and_then(Value::as_str),
                        action.get("url").and_then(Value::as_str),
                        action.get("pattern").and_then(Value::as_str),
                    ]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" | ");
                    if !detail.is_empty() {
                        push_entry(entries, "WEB".to_string(), detail);
                    }
                }
                _ => {}
            }
        }
        "event_msg" => {
            let Some(payload) = value.get("payload") else {
                return;
            };
            match payload.get("type").and_then(Value::as_str) {
                Some("user_message") => {
                    if let Some(message) = payload.get("message").and_then(Value::as_str) {
                        push_entry(entries, "USER message".to_string(), message.to_string());
                    }
                }
                Some("agent_message") => {
                    if let Some(message) = payload.get("message").and_then(Value::as_str) {
                        push_entry(entries, "ASSISTANT message".to_string(), message.to_string());
                    }
                }
                Some("task_complete") => {
                    if let Some(message) =
                        payload.get("last_agent_message").and_then(Value::as_str)
                    {
                        push_entry(entries, "ASSISTANT outcome".to_string(), message.to_string());
                    }
                }
                _ => {}
            }
        }
        "item.completed" => {
            let Some(item) = value.get("item") else {
                return;
            };
            if item.get("type").and_then(Value::as_str) == Some("command_execution") {
                if let Some(command) = item.get("command").and_then(Value::as_str) {
                    push_entry(entries, "COMMAND".to_string(), command.to_string());
                }
            } else if let Some(path) = item.get("path").and_then(Value::as_str) {
                let kind = item
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("tool_result");
                push_entry(entries, format!("TOOL {kind}"), path.to_string());
            }
        }
        _ => {}
    }
}

fn build_opencode_timeline(raw: &str) -> Vec<TimelineEntry> {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return Vec::new();
    };

    let mut entries = Vec::new();
    collect_opencode_entries(&value, &mut entries, 0);
    entries
}

fn collect_opencode_entries(value: &Value, entries: &mut Vec<TimelineEntry>, depth: usize) {
    const MAX_DEPTH: usize = 8;
    if depth > MAX_DEPTH {
        return;
    }

    match value {
        Value::Array(items) => {
            for item in items {
                collect_opencode_entries(item, entries, depth + 1);
            }
        }
        Value::Object(map) => {
            if let Some(role) = map
                .get("role")
                .and_then(Value::as_str)
                .or_else(|| {
                    map.get("info")
                        .and_then(|info| info.get("role"))
                        .and_then(Value::as_str)
                })
            {
                let text = extract_opencode_message_text(
                    map.get("content")
                        .or_else(|| map.get("parts"))
                        .or_else(|| map.get("text"))
                        .or_else(|| map.get("message"))
                        .unwrap_or(value),
                );
                if !text.is_empty() && matches!(role, "user" | "assistant") {
                    push_entry(
                        entries,
                        format!("{} message", role.to_ascii_uppercase()),
                        text,
                    );
                }
            }

            if let Some(tool_name) = extract_opencode_tool_name(map) {
                push_entry(entries, "TOOL call".to_string(), tool_name);
            }

            for item in map.values() {
                collect_opencode_entries(item, entries, depth + 1);
            }
        }
        _ => {}
    }
}

fn push_entry(entries: &mut Vec<TimelineEntry>, kind: String, detail: String) {
    let detail = normalize_excerpt(&detail);
    if detail.is_empty() {
        return;
    }
    let event_number = entries.len() + 1;
    entries.push(TimelineEntry {
        event_number,
        kind,
        detail,
    });
}

fn normalize_excerpt(input: &str) -> String {
    let normalized = input.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= MAX_TIMELINE_TEXT_CHARS {
        return normalized;
    }

    let head_len = MAX_TIMELINE_TEXT_CHARS.saturating_sub(24);
    let head: String = normalized.chars().take(head_len).collect();
    let omitted = normalized.chars().count().saturating_sub(head_len);
    format!("{head} [... omitted {omitted} chars ...]")
}

fn extract_response_message_text(payload: &Value) -> String {
    payload
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(extract_content_part_text)
        .collect::<Vec<_>>()
        .join("\n")
}

fn extract_content_part_text(value: &Value) -> Option<String> {
    if let Some(text) = value.get("text").and_then(Value::as_str) {
        return Some(text.to_string());
    }
    if let Some(text) = value.get("input_text").and_then(Value::as_str) {
        return Some(text.to_string());
    }
    if let Some(text) = value.get("output_text").and_then(Value::as_str) {
        return Some(text.to_string());
    }
    if let Some(array) = value.as_array() {
        let joined = array
            .iter()
            .filter_map(extract_content_part_text)
            .collect::<Vec<_>>()
            .join("\n");
        return (!joined.is_empty()).then_some(joined);
    }
    None
}

fn extract_opencode_tool_name(map: &serde_json::Map<String, Value>) -> Option<String> {
    let type_name = map.get("type").and_then(Value::as_str).unwrap_or("");
    if !type_name.to_ascii_lowercase().contains("tool")
        && !map.contains_key("tool")
        && !map.contains_key("name")
    {
        return None;
    }

    map.get("tool")
        .and_then(Value::as_str)
        .or_else(|| map.get("name").and_then(Value::as_str))
        .map(ToOwned::to_owned)
}

fn extract_opencode_message_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(items) => items
            .iter()
            .map(extract_opencode_message_text)
            .filter(|text| !text.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Object(map) => {
            let mut parts = Vec::new();
            for key in [
                "text", "message", "content", "parts", "summary", "input", "output",
            ] {
                if let Some(item) = map.get(key) {
                    let text = extract_opencode_message_text(item);
                    if !text.trim().is_empty() {
                        parts.push(text);
                    }
                }
            }
            parts.join("\n")
        }
        _ => String::new(),
    }
}

fn discover_session_cwd(session: &Session) -> Result<Option<String>> {
    if !session.path.exists() || !matches!(session.agent, AgentKind::Claude | AgentKind::Codex) {
        return Ok(None);
    }

    let file = File::open(&session.path)
        .with_context(|| format!("Failed to read session {}", session.path.display()))?;
    let reader = BufReader::new(file);

    for (index, line) in reader.lines().enumerate() {
        if index >= SESSION_META_SCAN_LINES {
            break;
        }
        let line = line?;
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if value.get("type").and_then(Value::as_str) != Some("session_meta") {
            continue;
        }
        if let Some(cwd) = value
            .get("payload")
            .and_then(|payload| payload.get("cwd"))
            .and_then(Value::as_str)
        {
            return Ok(Some(cwd.to_string()));
        }
    }

    Ok(None)
}

fn derive_project(session: &Session, cwd: Option<&str>) -> Option<String> {
    derive_project_from_cwd(cwd)
        .or_else(|| derive_project_from_path(&session.path, session.agent))
}

fn derive_project_from_cwd(cwd: Option<&str>) -> Option<String> {
    cwd.and_then(|cwd| {
        Path::new(cwd)
            .file_name()
            .and_then(|name| name.to_str())
            .map(sanitize_project_name)
    })
}

fn derive_project_from_path(path: &Path, agent: AgentKind) -> Option<String> {
    match agent {
        AgentKind::Claude => derive_claude_project_from_path(path),
        AgentKind::Codex | AgentKind::OpenCode => None,
    }
}

fn derive_claude_project_from_path(path: &Path) -> Option<String> {
    let components = path
        .components()
        .map(|component| component.as_os_str().to_string_lossy().to_string())
        .collect::<Vec<_>>();
    let index = components.iter().position(|component| component == "projects")?;
    components
        .get(index + 1)
        .map(|value| sanitize_project_name(value))
}

fn sanitize_project_name(raw: &str) -> String {
    raw.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::DateTime;
    use std::fs;
    use std::path::PathBuf;

    fn write_session(path: &Path, lines: &[String]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, format!("{}\n", lines.join("\n"))).unwrap();
    }

    fn sample_session(path: PathBuf, agent: AgentKind) -> Session {
        Session {
            id: path.to_string_lossy().to_string(),
            agent,
            path,
            timestamp: DateTime::parse_from_rfc3339("2026-03-12T08:00:00Z")
                .unwrap()
                .to_utc(),
            content: String::new(),
        }
    }

    #[test]
    fn test_codex_timeline_keeps_middle_command_execution() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".codex/sessions/2026/03/12/rollout-demo.jsonl");
        write_session(
            &path,
            &[
                serde_json::json!({
                    "type": "session_meta",
                    "payload": { "cwd": "/Users/me/code/atlas" }
                })
                .to_string(),
                serde_json::json!({
                    "type": "response_item",
                    "payload": {
                        "type": "message",
                        "role": "user",
                        "content": [{ "type": "input_text", "text": "Start implementing a large feature" }]
                    }
                })
                .to_string(),
                serde_json::json!({
                    "type": "item.completed",
                    "item": {
                        "type": "command_execution",
                        "command": "jj land && cargo test"
                    }
                })
                .to_string(),
                serde_json::json!({
                    "type": "response_item",
                    "payload": {
                        "type": "message",
                        "role": "assistant",
                        "content": [{ "type": "output_text", "text": "Feature landed and tests passed" }]
                    }
                })
                .to_string(),
            ],
        );

        let descriptor = discover_session(&sample_session(path, AgentKind::Codex)).unwrap();
        let timeline = build_session_timeline(&descriptor).unwrap();
        let rendered = render_timeline(&timeline);

        assert!(rendered.contains("Project: atlas"));
        assert!(rendered.contains("COMMAND: jj land && cargo test"));
        assert!(rendered.contains("USER message: Start implementing a large feature"));
        assert!(rendered.contains("ASSISTANT message: Feature landed and tests passed"));
    }

    #[test]
    fn test_render_timeline_window_keeps_requested_middle_range() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".codex/sessions/demo.jsonl");
        write_session(
            &path,
            &[
                serde_json::json!({
                    "type": "response_item",
                    "payload": {
                        "type": "message",
                        "role": "user",
                        "content": [{ "type": "input_text", "text": "feature intro" }]
                    }
                })
                .to_string(),
                serde_json::json!({
                    "type": "item.completed",
                    "item": {
                        "type": "command_execution",
                        "command": "jj land"
                    }
                })
                .to_string(),
                serde_json::json!({
                    "type": "item.completed",
                    "item": {
                        "type": "command_execution",
                        "command": "cargo test -p atlas"
                    }
                })
                .to_string(),
                serde_json::json!({
                    "type": "response_item",
                    "payload": {
                        "type": "message",
                        "role": "assistant",
                        "content": [{ "type": "output_text", "text": "done" }]
                    }
                })
                .to_string(),
            ],
        );

        let descriptor = discover_session(&sample_session(path, AgentKind::Codex)).unwrap();
        let window = render_timeline_window(
            &descriptor,
            &TimelineWindowRequest {
                workflow_key: "jj-land-run-tests".to_string(),
                workflow_label: Some("jj land and run tests".to_string()),
                note: "Repeated landing workflow".to_string(),
                start_event: 2,
                end_event: 3,
            },
        )
        .unwrap();

        assert!(window.contains("Workflow key: jj-land-run-tests"));
        assert!(window.contains("COMMAND: jj land"));
        assert!(window.contains("COMMAND: cargo test -p atlas"));
    }
}
