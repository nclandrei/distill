use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum PermissionLevel {
    ReadOnly,
    Write,
    Destructive,
    Unknown,
}

impl std::fmt::Display for PermissionLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PermissionLevel::ReadOnly => write!(f, "read-only"),
            PermissionLevel::Write => write!(f, "write"),
            PermissionLevel::Destructive => write!(f, "destructive"),
            PermissionLevel::Unknown => write!(f, "unknown"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ConversionRecommendation {
    KeepMcp,
    Hybrid,
    ReplaceCandidate,
}

impl std::fmt::Display for ConversionRecommendation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConversionRecommendation::KeepMcp => write!(f, "keep-mcp"),
            ConversionRecommendation::Hybrid => write!(f, "hybrid"),
            ConversionRecommendation::ReplaceCandidate => write!(f, "replace-candidate"),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PlanMode {
    Auto,
    Hybrid,
    Replace,
}

impl std::fmt::Display for PlanMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlanMode::Auto => write!(f, "auto"),
            PlanMode::Hybrid => write!(f, "hybrid"),
            PlanMode::Replace => write!(f, "replace"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MCPServerProfile {
    pub id: String,
    pub name: String,
    pub source_label: String,
    pub source_path: PathBuf,
    pub purpose: String,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub url: Option<String>,
    pub env_keys: Vec<String>,
    pub declared_tool_count: usize,
    pub permission_hints: Vec<String>,
    pub inferred_permission: PermissionLevel,
    pub recommendation: ConversionRecommendation,
    pub recommendation_reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConvertInventory {
    pub generated_at: DateTime<Utc>,
    pub searched_paths: Vec<PathBuf>,
    pub servers: Vec<MCPServerProfile>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConvertPlan {
    pub generated_at: DateTime<Utc>,
    pub server: MCPServerProfile,
    pub requested_mode: PlanMode,
    pub recommended_mode: PlanMode,
    pub effective_mode: PlanMode,
    pub blocked: bool,
    pub actions: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone)]
struct ConfigSource {
    label: String,
    path: PathBuf,
}

pub fn discover(additional_paths: &[PathBuf]) -> Result<ConvertInventory> {
    let home = std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."));
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let sources = default_sources(&home, &cwd, additional_paths);
    discover_from_sources(&sources)
}

pub fn inspect(server_selector: &str, additional_paths: &[PathBuf]) -> Result<MCPServerProfile> {
    let inventory = discover(additional_paths)?;
    resolve_server(&inventory.servers, server_selector)
}

pub fn plan(
    server_selector: &str,
    requested_mode: PlanMode,
    additional_paths: &[PathBuf],
) -> Result<ConvertPlan> {
    let server = inspect(server_selector, additional_paths)?;

    let recommended_mode = match server.recommendation {
        ConversionRecommendation::ReplaceCandidate => PlanMode::Replace,
        ConversionRecommendation::Hybrid | ConversionRecommendation::KeepMcp => PlanMode::Hybrid,
    };

    let effective_mode = if requested_mode == PlanMode::Auto {
        recommended_mode
    } else {
        requested_mode
    };

    let mut actions = vec![
        "Capture the current MCP server config and create a rollback backup.".to_string(),
        "Generate skill drafts from server purpose, command metadata, and permission profile."
            .to_string(),
    ];
    let mut warnings = vec![];
    let mut blocked = false;

    match effective_mode {
        PlanMode::Hybrid => {
            actions.push(
                "Keep MCP enabled and emit workflow-oriented skills that orchestrate MCP usage."
                    .to_string(),
            );
            actions.push(
                "Validate skill instructions against a task corpus while MCP remains fallback."
                    .to_string(),
            );
        }
        PlanMode::Replace => {
            actions.push(
                "Generate replacement skills and parity checks for baseline MCP behaviors."
                    .to_string(),
            );
            actions.push(
                "Disable MCP config entry only after parity checks pass and user confirms apply."
                    .to_string(),
            );
        }
        PlanMode::Auto => unreachable!("effective mode resolves away from auto"),
    }

    if effective_mode == PlanMode::Replace
        && server.recommendation != ConversionRecommendation::ReplaceCandidate
    {
        blocked = true;
        warnings.push(
            "Replace mode is blocked because this server is not a safe replace candidate."
                .to_string(),
        );
        warnings.push(format!(
            "Recommendation is '{}' ({})",
            server.recommendation, server.recommendation_reason
        ));
    }

    if server.inferred_permission == PermissionLevel::Destructive {
        warnings.push(
            "Destructive capability detected; keep MCP with explicit human review gates."
                .to_string(),
        );
    }

    if server.url.is_some() {
        warnings.push(
            "Remote URL-backed MCP servers are typically dynamic; replacement is usually unsafe."
                .to_string(),
        );
    }

    Ok(ConvertPlan {
        generated_at: Utc::now(),
        server,
        requested_mode,
        recommended_mode,
        effective_mode,
        blocked,
        actions,
        warnings,
    })
}

fn resolve_server(servers: &[MCPServerProfile], server_selector: &str) -> Result<MCPServerProfile> {
    let selector = server_selector.trim();
    if selector.is_empty() {
        bail!("Server selector must be non-empty.");
    }

    if let Some(found) = servers
        .iter()
        .find(|s| s.id.eq_ignore_ascii_case(selector))
        .cloned()
    {
        return Ok(found);
    }

    let mut by_name = servers
        .iter()
        .filter(|s| s.name.eq_ignore_ascii_case(selector))
        .cloned()
        .collect::<Vec<_>>();

    if by_name.is_empty() {
        let known = servers.iter().map(|s| s.id.clone()).collect::<Vec<_>>();
        if known.is_empty() {
            bail!(
                "No MCP servers discovered. Run 'distill convert list' to inspect searched paths."
            );
        }
        bail!(
            "No MCP server matched '{selector}'. Known server ids: {}",
            known.join(", ")
        );
    }

    by_name.sort_by(|a, b| a.id.cmp(&b.id));
    if by_name.len() > 1 {
        let ids = by_name.iter().map(|s| s.id.clone()).collect::<Vec<_>>();
        bail!(
            "Server name '{selector}' is ambiguous. Use one of: {}",
            ids.join(", ")
        );
    }

    Ok(by_name.remove(0))
}

fn discover_from_sources(sources: &[ConfigSource]) -> Result<ConvertInventory> {
    let mut servers = Vec::new();

    for source in sources {
        if !source.path.exists() {
            continue;
        }

        let raw = std::fs::read_to_string(&source.path)
            .with_context(|| format!("Failed to read {}", source.path.display()))?;
        let root: Value = serde_json::from_str(&raw)
            .with_context(|| format!("Failed to parse JSON in {}", source.path.display()))?;

        for (name, entry) in extract_server_entries(&root) {
            let Some(obj) = entry.as_object() else {
                continue;
            };

            let permission_hints = collect_permission_hints(obj);
            let command = obj
                .get("command")
                .and_then(Value::as_str)
                .map(ToString::to_string);
            let args = obj
                .get("args")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let url = obj
                .get("url")
                .and_then(Value::as_str)
                .map(ToString::to_string)
                .or_else(|| {
                    obj.get("endpoint")
                        .and_then(Value::as_str)
                        .map(ToString::to_string)
                });
            let env_keys = obj
                .get("env")
                .and_then(Value::as_object)
                .map(|env| {
                    let mut keys = env.keys().cloned().collect::<Vec<_>>();
                    keys.sort();
                    keys
                })
                .unwrap_or_default();
            let description = obj
                .get("description")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(ToString::to_string)
                .or_else(|| {
                    obj.get("purpose")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|v| !v.is_empty())
                        .map(ToString::to_string)
                });
            let declared_tool_count = declared_tool_count(obj);
            let inferred_permission = infer_permission(
                &name,
                description.as_deref(),
                command.as_deref(),
                &args,
                &permission_hints,
            );
            let purpose = infer_purpose(
                &name,
                description.as_deref(),
                command.as_deref(),
                url.as_deref(),
                &args,
            );
            let (recommendation, recommendation_reason) = recommend_conversion(
                inferred_permission.clone(),
                url.as_deref(),
                &env_keys,
                declared_tool_count,
            );

            servers.push(MCPServerProfile {
                id: format!("{}:{}", source.label, name),
                name,
                source_label: source.label.clone(),
                source_path: source.path.clone(),
                purpose,
                command,
                args,
                url,
                env_keys,
                declared_tool_count,
                permission_hints,
                inferred_permission,
                recommendation,
                recommendation_reason,
            });
        }
    }

    servers.sort_by(|a, b| a.id.cmp(&b.id));

    Ok(ConvertInventory {
        generated_at: Utc::now(),
        searched_paths: sources.iter().map(|s| s.path.clone()).collect(),
        servers,
    })
}

fn default_sources(home: &Path, cwd: &Path, additional_paths: &[PathBuf]) -> Vec<ConfigSource> {
    let mut sources = vec![
        ConfigSource {
            label: "claude-global".to_string(),
            path: home.join(".claude").join("mcp.json"),
        },
        ConfigSource {
            label: "claude-project".to_string(),
            path: cwd.join(".claude").join("mcp.json"),
        },
        ConfigSource {
            label: "codex-global".to_string(),
            path: home.join(".codex").join("mcp.json"),
        },
        ConfigSource {
            label: "codex-project".to_string(),
            path: cwd.join(".codex").join("mcp.json"),
        },
        ConfigSource {
            label: "shared-global".to_string(),
            path: home.join(".config").join("mcp").join("servers.json"),
        },
    ];

    for (idx, path) in additional_paths.iter().enumerate() {
        sources.push(ConfigSource {
            label: format!("custom-{}", idx + 1),
            path: path.clone(),
        });
    }

    dedupe_sources(sources)
}

fn dedupe_sources(sources: Vec<ConfigSource>) -> Vec<ConfigSource> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for source in sources {
        let key = source.path.to_string_lossy().to_lowercase();
        if seen.insert(key) {
            out.push(source);
        }
    }
    out
}

fn extract_server_entries(root: &Value) -> Vec<(String, Value)> {
    if let Some(obj) = root.get("mcpServers").and_then(Value::as_object) {
        return obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    }
    if let Some(obj) = root.get("servers").and_then(Value::as_object) {
        return obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    }
    if let Some(obj) = root.as_object() {
        return obj
            .iter()
            .filter(|(_, value)| likely_server_object(value))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
    }
    Vec::new()
}

fn likely_server_object(value: &Value) -> bool {
    let Some(obj) = value.as_object() else {
        return false;
    };
    let keys = [
        "command",
        "args",
        "url",
        "endpoint",
        "env",
        "description",
        "purpose",
        "permissions",
        "scopes",
        "capabilities",
        "tools",
    ];
    keys.iter().any(|key| obj.contains_key(*key))
}

fn declared_tool_count(obj: &serde_json::Map<String, Value>) -> usize {
    if let Some(arr) = obj.get("tools").and_then(Value::as_array) {
        return arr.len();
    }
    if let Some(num) = obj.get("tool_count").and_then(Value::as_u64) {
        return num as usize;
    }
    if let Some(cap) = obj.get("capabilities").and_then(Value::as_object) {
        if let Some(arr) = cap.get("tools").and_then(Value::as_array) {
            return arr.len();
        }
        if let Some(num) = cap.get("tool_count").and_then(Value::as_u64) {
            return num as usize;
        }
    }
    0
}

fn collect_permission_hints(obj: &serde_json::Map<String, Value>) -> Vec<String> {
    let mut hints = BTreeSet::new();

    for key in ["permissions", "scopes"] {
        match obj.get(key) {
            Some(Value::String(s)) => {
                hints.insert(s.trim().to_lowercase());
            }
            Some(Value::Array(items)) => {
                for item in items {
                    if let Some(s) = item.as_str() {
                        hints.insert(s.trim().to_lowercase());
                    }
                }
            }
            _ => {}
        }
    }

    for key in ["readOnly", "read_only", "readonly"] {
        if obj.get(key).and_then(Value::as_bool) == Some(true) {
            hints.insert("read-only".to_string());
        }
    }

    if let Some(cap) = obj.get("capabilities").and_then(Value::as_object) {
        for (k, v) in cap {
            if v.as_bool() == Some(true) {
                hints.insert(k.to_lowercase());
            }
        }
    }

    hints.into_iter().collect()
}

fn infer_purpose(
    name: &str,
    description: Option<&str>,
    command: Option<&str>,
    url: Option<&str>,
    args: &[String],
) -> String {
    if let Some(desc) = description {
        return desc.to_string();
    }

    let mut haystack = vec![name.to_lowercase()];
    if let Some(cmd) = command {
        haystack.push(cmd.to_lowercase());
    }
    if let Some(endpoint) = url {
        haystack.push(endpoint.to_lowercase());
    }
    haystack.extend(args.iter().map(|arg| arg.to_lowercase()));
    let corpus = haystack.join(" ");

    if contains_any(&corpus, &["playwright", "browser", "puppeteer", "selenium"]) {
        return "Browser automation and interactive web workflows".to_string();
    }
    if contains_any(
        &corpus,
        &["jira", "linear", "github", "gitlab", "issue", "pr"],
    ) {
        return "Project and issue management workflows".to_string();
    }
    if contains_any(
        &corpus,
        &["k8s", "kubectl", "helm", "terraform", "aws", "gcloud"],
    ) {
        return "Infrastructure and platform operations".to_string();
    }
    if contains_any(&corpus, &["sql", "postgres", "mysql", "database", "db"]) {
        return "Database querying and administration".to_string();
    }
    if contains_any(&corpus, &["file", "filesystem", "fs", "local", "shell"]) {
        return "Local automation and filesystem tasks".to_string();
    }

    "General-purpose MCP integration".to_string()
}

fn infer_permission(
    name: &str,
    description: Option<&str>,
    command: Option<&str>,
    args: &[String],
    permission_hints: &[String],
) -> PermissionLevel {
    let mut parts = vec![name.to_lowercase()];
    if let Some(desc) = description {
        parts.push(desc.to_lowercase());
    }
    if let Some(cmd) = command {
        parts.push(cmd.to_lowercase());
    }
    parts.extend(args.iter().map(|arg| arg.to_lowercase()));
    parts.extend(permission_hints.iter().map(|hint| hint.to_lowercase()));
    let corpus = parts.join(" ");

    let destructive = [
        "delete",
        "destroy",
        "drop",
        "rm -rf",
        "truncate",
        "uninstall",
        "terminate",
        "shutdown",
    ];
    let write = [
        "write", "create", "update", "insert", "upsert", "apply", "deploy", "commit", "push",
        "exec", "execute", "mutation", "admin",
    ];
    let read = [
        "read", "list", "get", "search", "query", "fetch", "inspect", "browse",
    ];

    if contains_any(&corpus, &destructive) {
        return PermissionLevel::Destructive;
    }
    if contains_any(&corpus, &write) {
        return PermissionLevel::Write;
    }

    let read_only_hint = permission_hints
        .iter()
        .any(|hint| hint.contains("read") && !hint.contains("write"));
    if read_only_hint || contains_any(&corpus, &read) {
        return PermissionLevel::ReadOnly;
    }

    PermissionLevel::Unknown
}

fn recommend_conversion(
    permission: PermissionLevel,
    url: Option<&str>,
    env_keys: &[String],
    declared_tool_count: usize,
) -> (ConversionRecommendation, String) {
    if url.is_some() {
        return (
            ConversionRecommendation::KeepMcp,
            "Remote URL-based servers are typically dynamic and better kept as MCP integrations."
                .to_string(),
        );
    }

    if permission == PermissionLevel::Destructive {
        return (
            ConversionRecommendation::KeepMcp,
            "Destructive actions detected; keep MCP for explicit execution controls.".to_string(),
        );
    }

    if permission == PermissionLevel::Write {
        return (
            ConversionRecommendation::Hybrid,
            "Write-oriented capabilities are safer as MCP with skills for orchestration."
                .to_string(),
        );
    }

    if permission == PermissionLevel::ReadOnly {
        if env_keys.is_empty() {
            return (
                ConversionRecommendation::ReplaceCandidate,
                "Read-only and no credential requirements; good candidate for skill replacement."
                    .to_string(),
            );
        }
        return (
            ConversionRecommendation::Hybrid,
            "Read-only but credential-backed; prefer hybrid conversion with MCP fallback."
                .to_string(),
        );
    }

    if declared_tool_count > 10 {
        return (
            ConversionRecommendation::Hybrid,
            "Large tool surface detected; start with hybrid conversion and verify incrementally."
                .to_string(),
        );
    }

    (
        ConversionRecommendation::Hybrid,
        "Insufficient metadata for safe replacement; defaulting to hybrid conversion.".to_string(),
    )
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_server_entries_supports_mcp_servers_key() {
        let value: Value = serde_json::from_str(
            r#"{
  "mcpServers": {
    "playwright": {
      "command": "npx",
      "args": ["-y", "@playwright/mcp"]
    }
  }
}"#,
        )
        .unwrap();

        let entries = extract_server_entries(&value);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, "playwright");
    }

    #[test]
    fn test_recommend_conversion_keeps_remote_url_servers() {
        let (recommendation, reason) = recommend_conversion(
            PermissionLevel::ReadOnly,
            Some("https://example.com/mcp"),
            &[],
            0,
        );
        assert_eq!(recommendation, ConversionRecommendation::KeepMcp);
        assert!(reason.contains("Remote URL"));
    }

    #[test]
    fn test_infer_permission_detects_destructive_keywords() {
        let permission = infer_permission(
            "terraform-admin",
            Some("Delete and destroy cluster resources"),
            Some("terraform"),
            &["apply".to_string()],
            &[],
        );
        assert_eq!(permission, PermissionLevel::Destructive);
    }

    #[test]
    fn test_plan_blocks_replace_for_non_replace_candidate() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("mcp.json");
        std::fs::write(
            &config_path,
            r#"{
  "mcpServers": {
    "danger": {
      "command": "terraform",
      "description": "Apply and destroy infra"
    }
  }
}"#,
        )
        .unwrap();

        let plan = plan("danger", PlanMode::Replace, &[config_path]).unwrap();
        assert!(plan.blocked);
        assert!(
            plan.warnings
                .iter()
                .any(|warning| warning.contains("blocked"))
        );
    }

    #[test]
    fn test_inspect_reports_ambiguous_name() {
        let dir = tempfile::tempdir().unwrap();
        let one = dir.path().join("one.json");
        let two = dir.path().join("two.json");

        std::fs::write(
            &one,
            r#"{
  "mcpServers": {
    "shared": { "command": "npx", "description": "Read list" }
  }
}"#,
        )
        .unwrap();
        std::fs::write(
            &two,
            r#"{
  "mcpServers": {
    "shared": { "command": "uvx", "description": "Read list" }
  }
}"#,
        )
        .unwrap();

        let err = inspect("shared", &[one, two]).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("ambiguous"));
    }
}
