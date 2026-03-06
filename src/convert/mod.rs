use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::config::Config;

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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConvertApplyResult {
    pub generated_at: DateTime<Utc>,
    pub server: MCPServerProfile,
    pub requested_mode: PlanMode,
    pub effective_mode: PlanMode,
    pub orchestrator_skill_path: PathBuf,
    pub skill_path: PathBuf,
    pub tool_skill_paths: Vec<PathBuf>,
    pub mcp_config_backup: Option<PathBuf>,
    pub mcp_config_updated: bool,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConvertVerifyReport {
    pub generated_at: DateTime<Utc>,
    pub server: MCPServerProfile,
    pub orchestrator_skill_path: PathBuf,
    pub skill_path: PathBuf,
    pub tool_skill_paths: Vec<PathBuf>,
    pub introspection_ok: bool,
    pub introspected_tool_count: usize,
    pub required_tools: Vec<String>,
    pub missing_in_server: Vec<String>,
    pub missing_in_skill: Vec<String>,
    pub missing_skill_files: Vec<String>,
    pub passed: bool,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ManifestToolSkill {
    tool_name: String,
    skill_file: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct SkillParityManifest {
    format_version: u32,
    generated_at: DateTime<Utc>,
    server_id: String,
    server_name: String,
    #[serde(default)]
    orchestrator_skill: Option<String>,
    #[serde(default)]
    required_tools: Vec<String>,
    #[serde(default)]
    tool_skills: Vec<ManifestToolSkill>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    required_tool_hints: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ToolSpec {
    name: String,
    description: Option<String>,
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
        "Generate one orchestrator skill and a capability skill set mapped to tool behaviors."
            .to_string(),
    ];
    let mut warnings = vec![];
    let mut blocked = false;

    match effective_mode {
        PlanMode::Hybrid => {
            actions.push(
                "Keep MCP enabled and use the generated skill set as the default orchestration layer."
                    .to_string(),
            );
            actions.push(
                "Validate skill instructions against a task corpus while MCP remains fallback."
                    .to_string(),
            );
        }
        PlanMode::Replace => {
            actions.push(
                "Generate replacement skill set and parity checks for tool-level behaviors."
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

pub fn apply(
    server_selector: &str,
    requested_mode: PlanMode,
    confirm_replace: bool,
    additional_paths: &[PathBuf],
    output_dir: Option<PathBuf>,
) -> Result<ConvertApplyResult> {
    let conversion_plan = plan(server_selector, requested_mode, additional_paths)?;

    if conversion_plan.blocked {
        bail!(
            "Conversion plan is blocked for '{}': {}",
            conversion_plan.server.id,
            conversion_plan.warnings.join(" | ")
        );
    }

    if conversion_plan.effective_mode == PlanMode::Replace && !confirm_replace {
        bail!("Replace mode requires explicit confirmation via --yes.");
    }

    let skills_dir = output_dir.unwrap_or_else(Config::skills_dir);
    std::fs::create_dir_all(&skills_dir)
        .with_context(|| format!("Failed to create skills directory {}", skills_dir.display()))?;

    let introspected_specs = introspect_tool_specs(&conversion_plan.server).ok();
    let introspected_tools = introspected_specs.as_ref().map(|items| {
        items
            .iter()
            .map(|item| item.name.clone())
            .collect::<Vec<String>>()
    });

    let mut required_tools =
        required_tool_names(&conversion_plan.server, introspected_tools.as_deref());
    if required_tools.is_empty() {
        required_tools.push("general-orchestration".to_string());
    }

    let spec_by_name = introspected_specs
        .as_ref()
        .map(|items| {
            items
                .iter()
                .cloned()
                .map(|item| (item.name.clone(), item))
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();

    let server_slug = sanitize_slug(&conversion_plan.server.name);
    let mut slug_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut tool_skills = Vec::new();
    let mut tool_skill_paths = Vec::new();
    for tool_name in &required_tools {
        let base_slug = sanitize_slug(tool_name);
        let counter = slug_counts.entry(base_slug.clone()).or_insert(0);
        let tool_slug = if *counter == 0 {
            base_slug.clone()
        } else {
            format!("{base_slug}-{}", *counter + 1)
        };
        *counter += 1;

        let file_name = format!("mcp-{server_slug}-tool-{tool_slug}.md");
        let skill_path = skills_dir.join(&file_name);
        let description = spec_by_name
            .get(tool_name)
            .and_then(|item| item.description.as_deref());
        std::fs::write(
            &skill_path,
            render_capability_skill_markdown(&conversion_plan.server, tool_name, description),
        )
        .with_context(|| format!("Failed to write capability skill {}", skill_path.display()))?;
        tool_skills.push(ManifestToolSkill {
            tool_name: tool_name.clone(),
            skill_file: file_name,
        });
        tool_skill_paths.push(skill_path);
    }

    let orchestrator_filename = format!("mcp-{server_slug}.md");
    let skill_path = skills_dir.join(&orchestrator_filename);
    std::fs::write(
        &skill_path,
        render_orchestrator_skill_markdown(&conversion_plan, &tool_skills),
    )
    .with_context(|| {
        format!(
            "Failed to write converted orchestrator skill {}",
            skill_path.display()
        )
    })?;
    let manifest = SkillParityManifest {
        format_version: 2,
        generated_at: Utc::now(),
        server_id: conversion_plan.server.id.clone(),
        server_name: conversion_plan.server.name.clone(),
        orchestrator_skill: Some(orchestrator_filename),
        required_tools: required_tools.clone(),
        tool_skills: tool_skills.clone(),
        required_tool_hints: vec![],
    };
    write_skill_manifest(&skill_path, &manifest)?;

    let mut notes = vec![format!(
        "Generated 1 orchestrator skill and {} capability skills.",
        tool_skills.len()
    )];
    notes.push("Wrote internal parity manifest for verify checks.".to_string());
    let mut mcp_config_backup = None;
    let mut mcp_config_updated = false;

    match conversion_plan.effective_mode {
        PlanMode::Hybrid => {
            notes.push("MCP config left unchanged (hybrid mode).".to_string());
        }
        PlanMode::Replace => {
            if introspected_tools.is_none() {
                bail!(
                    "Replace mode requires successful live MCP tool introspection before config mutation."
                );
            }
            let verify = verify_with_server_and_path(
                &conversion_plan.server,
                &skill_path,
                introspected_tools.as_deref(),
            )?;
            if !verify.passed {
                bail!(
                    "Replace mode verification failed for '{}': missing_in_server=[{}], missing_in_skill=[{}], missing_skill_files=[{}]",
                    conversion_plan.server.id,
                    verify.missing_in_server.join(", "),
                    verify.missing_in_skill.join(", "),
                    verify.missing_skill_files.join(", ")
                );
            }
            let (backup, updated) = remove_server_from_config(
                &conversion_plan.server.source_path,
                &conversion_plan.server.name,
            )?;
            mcp_config_backup = backup;
            mcp_config_updated = updated;
            if updated {
                notes.push("Removed MCP server entry from config (replace mode).".to_string());
            } else {
                notes.push(
                    "No MCP server entry was removed (server key not found in config).".to_string(),
                );
            }
        }
        PlanMode::Auto => unreachable!("effective mode resolves away from auto"),
    }

    Ok(ConvertApplyResult {
        generated_at: Utc::now(),
        server: conversion_plan.server,
        requested_mode,
        effective_mode: conversion_plan.effective_mode,
        orchestrator_skill_path: skill_path.clone(),
        skill_path,
        tool_skill_paths,
        mcp_config_backup,
        mcp_config_updated,
        notes,
    })
}

pub fn verify(
    server_selector: &str,
    additional_paths: &[PathBuf],
    skills_dir: Option<PathBuf>,
) -> Result<ConvertVerifyReport> {
    let server = inspect(server_selector, additional_paths)?;
    let skills_dir = skills_dir.unwrap_or_else(Config::skills_dir);
    let skill_path = skills_dir.join(format!("mcp-{}.md", sanitize_slug(&server.name)));

    let introspected = introspect_tools(&server).ok();
    verify_with_server_and_path(&server, &skill_path, introspected.as_deref())
}

fn verify_with_server_and_path(
    server: &MCPServerProfile,
    skill_path: &Path,
    introspected_tools: Option<&[String]>,
) -> Result<ConvertVerifyReport> {
    if !skill_path.exists() {
        bail!(
            "Generated skill file not found for verification: {}",
            skill_path.display()
        );
    }

    let skill_content = std::fs::read_to_string(skill_path)
        .with_context(|| format!("Failed to read {}", skill_path.display()))?;
    let mut required_tools = required_tool_names(server, introspected_tools);

    let mut notes = vec![];
    let introspected_set = introspected_tools
        .map(|items| {
            items
                .iter()
                .map(|item| normalize_tool_name(item))
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    let introspection_ok = !introspected_set.is_empty();
    if introspection_ok {
        notes.push(format!(
            "Live MCP introspection returned {} tools.",
            introspected_set.len()
        ));
    } else {
        notes.push(
            "Live MCP introspection unavailable; verification is heuristic-only.".to_string(),
        );
    }

    let manifest_path = manifest_path_for_skill(skill_path)?;
    let mut manifest_tool_skills = vec![];
    let mut orchestrator_skill_path = skill_path.to_path_buf();
    if manifest_path.exists() {
        let manifest_raw = std::fs::read_to_string(&manifest_path)
            .with_context(|| format!("Failed to read {}", manifest_path.display()))?;
        let manifest: SkillParityManifest =
            serde_json::from_str(&manifest_raw).with_context(|| {
                format!(
                    "Failed to parse parity manifest {}",
                    manifest_path.display()
                )
            })?;
        notes.push(format!(
            "Loaded parity manifest from {}.",
            manifest_path.display()
        ));
        if let Some(orchestrator) = manifest.orchestrator_skill {
            if let Some(parent) = skill_path.parent() {
                orchestrator_skill_path = parent.join(orchestrator);
            }
        }
        if !manifest.required_tools.is_empty() {
            required_tools = manifest
                .required_tools
                .iter()
                .map(|tool| normalize_tool_name(tool))
                .collect();
        } else if !manifest.required_tool_hints.is_empty() {
            required_tools = manifest
                .required_tool_hints
                .iter()
                .map(|hint| hint_to_tool_name(hint))
                .collect();
        }
        manifest_tool_skills = manifest.tool_skills;
    } else {
        let legacy = extract_tool_hints_from_skill(&skill_content)
            .iter()
            .map(|hint| hint_to_tool_name(hint))
            .collect::<Vec<_>>();
        notes.push(
            "Parity manifest missing; falling back to legacy tool-hint parsing from markdown."
                .to_string(),
        );
        if required_tools.is_empty() {
            required_tools = legacy.clone();
        }
        if !legacy.is_empty() {
            let current_file = skill_path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("skill.md")
                .to_string();
            manifest_tool_skills = legacy
                .iter()
                .map(|tool_name| ManifestToolSkill {
                    tool_name: tool_name.clone(),
                    skill_file: current_file.clone(),
                })
                .collect();
        }
    }

    required_tools.sort();
    required_tools.dedup();

    let parent_dir = skill_path
        .parent()
        .context("Skill path has no parent directory for verification")?;
    let tool_skill_paths = manifest_tool_skills
        .iter()
        .map(|tool| parent_dir.join(&tool.skill_file))
        .collect::<Vec<_>>();
    let mapped_tool_names = manifest_tool_skills
        .iter()
        .map(|tool| normalize_tool_name(&tool.tool_name))
        .collect::<BTreeSet<_>>();

    let missing_in_skill = required_tools
        .iter()
        .filter(|tool| !mapped_tool_names.contains(*tool))
        .cloned()
        .collect::<Vec<_>>();

    let required_tool_names = required_tools.iter().cloned().collect::<BTreeSet<_>>();
    let missing_in_server = if introspection_ok {
        required_tool_names
            .iter()
            .filter(|name| !introspected_set.contains(*name))
            .cloned()
            .collect::<Vec<_>>()
    } else {
        vec![]
    };

    let mut missing_skill_files = manifest_tool_skills
        .iter()
        .zip(tool_skill_paths.iter())
        .filter_map(|(tool, path)| {
            if path.exists() {
                None
            } else {
                Some(tool.skill_file.clone())
            }
        })
        .collect::<Vec<_>>();
    if !orchestrator_skill_path.exists() {
        missing_skill_files.push(
            orchestrator_skill_path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("orchestrator-skill")
                .to_string(),
        );
    }

    let passed = missing_in_skill.is_empty()
        && missing_skill_files.is_empty()
        && (missing_in_server.is_empty() || !introspection_ok)
        && !tool_skill_paths.is_empty();

    Ok(ConvertVerifyReport {
        generated_at: Utc::now(),
        server: server.clone(),
        orchestrator_skill_path: orchestrator_skill_path.clone(),
        skill_path: skill_path.to_path_buf(),
        tool_skill_paths,
        introspection_ok,
        introspected_tool_count: introspected_set.len(),
        required_tools,
        missing_in_server,
        missing_in_skill,
        missing_skill_files,
        passed,
        notes,
    })
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
struct CapabilityPlaybook {
    title: String,
    goal: String,
    tool_hints: Vec<String>,
    steps: Vec<String>,
}

fn render_orchestrator_skill_markdown(
    plan: &ConvertPlan,
    tool_skills: &[ManifestToolSkill],
) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Workflow: {}\n\n", plan.server.name.trim()));
    out.push_str("## Purpose\n\n");
    out.push_str(&format!("{}\n\n", plan.server.purpose));

    out.push_str("## Capability Skills\n\n");
    if tool_skills.is_empty() {
        out.push_str("- No capability skills were generated.\n\n");
    } else {
        for skill in tool_skills {
            let skill_name = skill.skill_file.trim_end_matches(".md");
            out.push_str(&format!(
                "- `${skill_name}`: Executes `{}` operations.\n",
                skill.tool_name
            ));
        }
        out.push('\n');
    }

    out.push_str("## Orchestration\n\n");
    out.push_str("1. Clarify the user goal and select the minimum capability skills needed.\n");
    out.push_str(
        "2. Execute capability skills in dependency order, one focused action at a time.\n",
    );
    out.push_str("3. After each capability run, validate output and decide next step.\n");
    out.push_str("4. Stop immediately on errors, report root cause, and suggest recovery.\n\n");

    out.push_str("## Guardrails\n\n");
    out.push_str(
        "- Keep explicit user confirmation before destructive or production-impacting steps.\n",
    );
    out.push_str(
        "- When behavior is unclear, inspect tool schemas or run a dry-run/check command first.\n",
    );
    if !plan.warnings.is_empty() {
        for warning in &plan.warnings {
            out.push_str(&format!("- {}\n", warning));
        }
    }

    out
}

fn render_capability_skill_markdown(
    server: &MCPServerProfile,
    tool_name: &str,
    description: Option<&str>,
) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Capability: {tool_name}\n\n"));
    out.push_str("## Purpose\n\n");
    if let Some(text) = description.map(str::trim).filter(|text| !text.is_empty()) {
        out.push_str(text);
        out.push_str("\n\n");
    } else {
        out.push_str(&format!(
            "Execute `{tool_name}` tasks for {}.\n\n",
            server.purpose.to_lowercase()
        ));
    }

    out.push_str("## Execution\n\n");
    out.push_str("1. Confirm prerequisites and collect only required inputs.\n");
    out.push_str(&format!(
        "2. Run the `{tool_name}` capability and capture raw output.\n"
    ));
    out.push_str("3. Validate errors, status, and key fields before continuing.\n");
    out.push_str("4. Return a concise result and next-step recommendation.\n\n");

    out.push_str("## Safety\n\n");
    out.push_str("- Ask for confirmation before destructive or irreversible actions.\n");
    out.push_str("- If arguments are unclear, run a non-destructive check first.\n");

    out
}

fn capability_playbooks(
    server: &MCPServerProfile,
    fallback_actions: &[String],
    introspected_tools: Option<&[String]>,
) -> Vec<CapabilityPlaybook> {
    let name = server.name.to_lowercase();
    let purpose = server.purpose.clone();
    let introspected_set = introspected_tools
        .map(|items| {
            items
                .iter()
                .map(|item| normalize_tool_name(item))
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();

    if name.contains("xcodebuildmcp") || name.contains("xcode") {
        return vec![
            CapabilityPlaybook {
                title: "Build and launch in simulator".to_string(),
                goal: "Compile and run iOS code paths quickly during iteration.".to_string(),
                tool_hints: filter_hints_by_introspection(
                    vec![
                        "mcp__XcodeBuildMCP__build_run_sim".to_string(),
                        "mcp__XcodeBuildMCP__launch_app_sim".to_string(),
                        "mcp__XcodeBuildMCP__list_sims".to_string(),
                    ],
                    &introspected_set,
                ),
                steps: vec![
                    "List available simulators and choose target device/OS.".to_string(),
                    "Build and run in simulator with project defaults.".to_string(),
                    "Capture immediate app behavior and regressions before deeper debugging."
                        .to_string(),
                ],
            },
            CapabilityPlaybook {
                title: "UI interaction and visual checks".to_string(),
                goal: "Drive screens deterministically and confirm UI state.".to_string(),
                tool_hints: filter_hints_by_introspection(
                    vec![
                        "mcp__XcodeBuildMCP__snapshot_ui".to_string(),
                        "mcp__XcodeBuildMCP__tap".to_string(),
                        "mcp__XcodeBuildMCP__type_text".to_string(),
                        "mcp__XcodeBuildMCP__screenshot".to_string(),
                    ],
                    &introspected_set,
                ),
                steps: vec![
                    "Take a UI snapshot to identify accessible targets.".to_string(),
                    "Trigger interactions by accessibility id/label first, coordinates last."
                        .to_string(),
                    "Capture screenshots for before/after evidence of state transitions."
                        .to_string(),
                ],
            },
            CapabilityPlaybook {
                title: "Attach debugger and inspect failures".to_string(),
                goal: "Investigate crashes, stuck flows, and state mismatches.".to_string(),
                tool_hints: filter_hints_by_introspection(
                    vec![
                        "mcp__XcodeBuildMCP__debug_attach_sim".to_string(),
                        "mcp__XcodeBuildMCP__debug_stack".to_string(),
                        "mcp__XcodeBuildMCP__debug_variables".to_string(),
                        "mcp__XcodeBuildMCP__debug_lldb_command".to_string(),
                    ],
                    &introspected_set,
                ),
                steps: vec![
                    "Attach debugger to running app process.".to_string(),
                    "Collect backtrace and inspect frame variables at failure point.".to_string(),
                    "Apply fix and rerun the same flow to confirm closure.".to_string(),
                ],
            },
        ];
    }

    if name.contains("chrome-devtools") || name.contains("devtools") || name.contains("chrome") {
        return vec![
            CapabilityPlaybook {
                title: "Navigate and inspect page state".to_string(),
                goal: "Understand DOM/accessibility state before automation actions.".to_string(),
                tool_hints: filter_hints_by_introspection(
                    vec![
                        "mcp__chrome-devtools__navigate_page".to_string(),
                        "mcp__chrome-devtools__take_snapshot".to_string(),
                        "mcp__chrome-devtools__click".to_string(),
                        "mcp__chrome-devtools__fill".to_string(),
                    ],
                    &introspected_set,
                ),
                steps: vec![
                    "Open target URL and wait for primary content.".to_string(),
                    "Capture a text snapshot and locate stable element identifiers.".to_string(),
                    "Perform interactions and re-snapshot to validate results.".to_string(),
                ],
            },
            CapabilityPlaybook {
                title: "Trace network and console failures".to_string(),
                goal: "Root-cause runtime errors and bad responses.".to_string(),
                tool_hints: filter_hints_by_introspection(
                    vec![
                        "mcp__chrome-devtools__list_network_requests".to_string(),
                        "mcp__chrome-devtools__get_network_request".to_string(),
                        "mcp__chrome-devtools__list_console_messages".to_string(),
                        "mcp__chrome-devtools__get_console_message".to_string(),
                    ],
                    &introspected_set,
                ),
                steps: vec![
                    "List recent network requests and inspect failing responses.".to_string(),
                    "Collect console errors and correlate them with failing endpoints.".to_string(),
                    "Re-run interaction after fix to verify errors disappear.".to_string(),
                ],
            },
            CapabilityPlaybook {
                title: "Run performance diagnostics".to_string(),
                goal: "Capture page performance issues and actionable insights.".to_string(),
                tool_hints: filter_hints_by_introspection(
                    vec![
                        "mcp__chrome-devtools__performance_start_trace".to_string(),
                        "mcp__chrome-devtools__performance_stop_trace".to_string(),
                        "mcp__chrome-devtools__performance_analyze_insight".to_string(),
                    ],
                    &introspected_set,
                ),
                steps: vec![
                    "Start trace recording for the target journey.".to_string(),
                    "Stop trace and inspect key insights (latency, LCP breakdown, etc.)."
                        .to_string(),
                    "Prioritize fixes and rerun trace to measure impact.".to_string(),
                ],
            },
        ];
    }

    let mut steps = vec![
        "Confirm MCP server availability and auth prerequisites.".to_string(),
        format!(
            "Use MCP tools to execute {} tasks with explicit checks.",
            purpose
        ),
    ];
    steps.extend(fallback_actions.iter().cloned());

    let fallback_hints = if introspected_set.is_empty() {
        vec![]
    } else {
        let mut names = introspected_set.iter().cloned().collect::<Vec<_>>();
        names.sort();
        names
            .into_iter()
            .take(6)
            .map(|tool| format!("{}{}", tool_hint_prefix(server), tool))
            .collect::<Vec<_>>()
    };

    vec![CapabilityPlaybook {
        title: "General orchestration".to_string(),
        goal: format!(
            "Perform {} with reproducible sequencing.",
            purpose.to_lowercase()
        ),
        tool_hints: fallback_hints,
        steps,
    }]
}

fn filter_hints_by_introspection(
    hints: Vec<String>,
    introspected: &BTreeSet<String>,
) -> Vec<String> {
    if introspected.is_empty() {
        return hints;
    }
    hints
        .iter()
        .filter(|hint| introspected.contains(&hint_to_tool_name(hint)))
        .cloned()
        .collect::<Vec<_>>()
}

fn required_tool_hints(
    server: &MCPServerProfile,
    introspected_tools: Option<&[String]>,
) -> Vec<String> {
    let mut hints = capability_playbooks(server, &[], introspected_tools)
        .into_iter()
        .flat_map(|playbook| playbook.tool_hints)
        .collect::<Vec<_>>();
    hints.sort();
    hints.dedup();
    hints
}

fn required_tool_names(
    server: &MCPServerProfile,
    introspected_tools: Option<&[String]>,
) -> Vec<String> {
    if let Some(items) = introspected_tools {
        let mut names = items
            .iter()
            .map(|item| normalize_tool_name(item))
            .collect::<Vec<_>>();
        names.sort();
        names.dedup();
        if !names.is_empty() {
            return names;
        }
    }

    let mut fallback = required_tool_hints(server, None)
        .iter()
        .map(|hint| hint_to_tool_name(hint))
        .collect::<Vec<_>>();
    fallback.sort();
    fallback.dedup();
    fallback
}

fn extract_tool_hints_from_skill(content: &str) -> Vec<String> {
    let mut hints = vec![];
    for line in content.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with("- `") || !trimmed.ends_with('`') {
            continue;
        }
        let value = trimmed.trim_start_matches("- `").trim_end_matches('`');
        if value.starts_with("mcp__") {
            hints.push(value.to_string());
        }
    }
    hints.sort();
    hints.dedup();
    hints
}

fn hint_to_tool_name(hint: &str) -> String {
    hint.rsplit("__").next().unwrap_or(hint).trim().to_string()
}

fn normalize_tool_name(name: &str) -> String {
    let trimmed = name.trim();
    if trimmed.starts_with("mcp__") {
        hint_to_tool_name(trimmed)
    } else {
        trimmed.to_string()
    }
}

fn tool_hint_prefix(server: &MCPServerProfile) -> String {
    let lower = server.name.to_lowercase();
    if lower.contains("xcodebuildmcp") {
        return "mcp__XcodeBuildMCP__".to_string();
    }
    if lower.contains("chrome-devtools") || lower.contains("devtools") {
        return "mcp__chrome-devtools__".to_string();
    }
    format!("mcp__{}__", server.name)
}

fn introspect_tools(server: &MCPServerProfile) -> Result<Vec<String>> {
    let mut tools = introspect_tool_specs(server)?
        .into_iter()
        .map(|tool| tool.name)
        .collect::<Vec<_>>();
    tools.sort();
    tools.dedup();
    if tools.is_empty() {
        bail!("MCP introspection returned no tools for '{}'.", server.id);
    }
    Ok(tools)
}

fn introspect_tool_specs(server: &MCPServerProfile) -> Result<Vec<ToolSpec>> {
    let command = server
        .command
        .as_deref()
        .context("MCP server has no command to introspect")?;

    let mut child = Command::new(command)
        .args(&server.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("Failed to spawn MCP command: {command}"))?;

    let init = serde_json::json!({
        "jsonrpc":"2.0",
        "id":1,
        "method":"initialize",
        "params":{
            "protocolVersion":"2025-03-26",
            "capabilities":{},
            "clientInfo":{"name":"distill","version":"0.1"}
        }
    });
    let list = serde_json::json!({
        "jsonrpc":"2.0",
        "id":2,
        "method":"tools/list",
        "params":{}
    });

    {
        let mut stdin = child.stdin.take().context("Failed to open MCP stdin")?;
        writeln!(stdin, "{init}").context("Failed to write MCP initialize request")?;
        writeln!(stdin, "{list}").context("Failed to write MCP tools/list request")?;
    }

    let output = child
        .wait_with_output()
        .context("Failed while waiting for MCP introspection output")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut tools = vec![];
    for line in stdout.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(id) = value.get("id").and_then(Value::as_i64) else {
            continue;
        };
        if id != 2 {
            continue;
        }
        let Some(items) = value
            .get("result")
            .and_then(|result| result.get("tools"))
            .and_then(Value::as_array)
        else {
            continue;
        };
        tools = items
            .iter()
            .filter_map(|item| {
                let name = item.get("name").and_then(Value::as_str)?;
                let description = item
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                    .map(ToString::to_string);
                Some(ToolSpec {
                    name: normalize_tool_name(name),
                    description,
                })
            })
            .collect::<Vec<_>>();
        break;
    }

    let mut deduped: BTreeMap<String, ToolSpec> = BTreeMap::new();
    for tool in tools {
        deduped.entry(tool.name.clone()).or_insert(tool);
    }
    let tools = deduped.into_values().collect::<Vec<_>>();

    if tools.is_empty() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "MCP introspection returned no tools for '{}'. stderr: {}",
            server.id,
            stderr.lines().take(5).collect::<Vec<_>>().join(" | ")
        );
    }

    Ok(tools)
}

fn remove_server_from_config(path: &Path, server_name: &str) -> Result<(Option<PathBuf>, bool)> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read config {}", path.display()))?;

    if let Ok(mut root) = serde_json::from_str::<Value>(&raw) {
        let removed = remove_server_from_json(&mut root, server_name);
        if !removed {
            return Ok((None, false));
        }

        let backup = backup_file(path)?;
        let body = serde_json::to_string_pretty(&root)
            .context("Failed to serialize updated JSON MCP config")?;
        std::fs::write(path, format!("{body}\n"))
            .with_context(|| format!("Failed to write config {}", path.display()))?;
        return Ok((Some(backup), true));
    }

    if let Ok(mut root) = toml::from_str::<toml::Value>(&raw) {
        let removed = remove_server_from_toml(&mut root, server_name);
        if !removed {
            return Ok((None, false));
        }

        let backup = backup_file(path)?;
        let body =
            toml::to_string_pretty(&root).context("Failed to serialize updated TOML MCP config")?;
        std::fs::write(path, body)
            .with_context(|| format!("Failed to write config {}", path.display()))?;
        return Ok((Some(backup), true));
    }

    bail!(
        "Failed to parse {} as JSON or TOML for replace mode update.",
        path.display()
    )
}

fn remove_server_from_json(root: &mut Value, server_name: &str) -> bool {
    let mut removed = false;

    if let Some(obj) = root.get_mut("mcpServers").and_then(Value::as_object_mut) {
        removed |= obj.remove(server_name).is_some();
    }
    if let Some(obj) = root.get_mut("mcp_servers").and_then(Value::as_object_mut) {
        removed |= obj.remove(server_name).is_some();
    }
    if let Some(obj) = root.get_mut("servers").and_then(Value::as_object_mut) {
        removed |= obj.remove(server_name).is_some();
    }
    if let Some(obj) = root
        .get_mut("amp.mcpServers")
        .and_then(Value::as_object_mut)
    {
        removed |= obj.remove(server_name).is_some();
    }

    if let Some(amp) = root.get_mut("amp").and_then(Value::as_object_mut)
        && let Some(obj) = amp.get_mut("mcpServers").and_then(Value::as_object_mut)
    {
        removed |= obj.remove(server_name).is_some();
    }

    if let Some(obj) = root.as_object_mut() {
        let should_remove = obj.get(server_name).is_some_and(likely_server_object);
        if should_remove {
            removed |= obj.remove(server_name).is_some();
        }
    }

    removed
}

fn remove_server_from_toml(root: &mut toml::Value, server_name: &str) -> bool {
    let mut removed = false;

    if let Some(table) = root.as_table_mut() {
        if let Some(mcp_servers) = table
            .get_mut("mcp_servers")
            .and_then(toml::Value::as_table_mut)
        {
            removed |= mcp_servers.remove(server_name).is_some();
        }

        if let Some(amp_mcp) = table
            .get_mut("amp.mcpServers")
            .and_then(toml::Value::as_table_mut)
        {
            removed |= amp_mcp.remove(server_name).is_some();
        }

        if let Some(amp) = table.get_mut("amp").and_then(toml::Value::as_table_mut)
            && let Some(mcp) = amp
                .get_mut("mcpServers")
                .and_then(toml::Value::as_table_mut)
        {
            removed |= mcp.remove(server_name).is_some();
        }
    }

    removed
}

fn backup_file(path: &Path) -> Result<PathBuf> {
    let filename = path
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or("mcp-config")
        .to_string();
    let backup_name = format!("{}.bak-{}", filename, Utc::now().format("%Y%m%d-%H%M%S"));
    let backup_path = path.with_file_name(backup_name);
    std::fs::copy(path, &backup_path).with_context(|| {
        format!(
            "Failed to create backup from {} to {}",
            path.display(),
            backup_path.display()
        )
    })?;
    Ok(backup_path)
}

fn manifest_path_for_skill(skill_path: &Path) -> Result<PathBuf> {
    let parent = skill_path
        .parent()
        .context("Skill path has no parent directory for manifest")?;
    let stem = skill_path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("skill");
    Ok(parent
        .join(".distill-manifests")
        .join(format!("{stem}.json")))
}

fn write_skill_manifest(skill_path: &Path, manifest: &SkillParityManifest) -> Result<PathBuf> {
    let manifest_path = manifest_path_for_skill(skill_path)?;
    if let Some(dir) = manifest_path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("Failed to create {}", dir.display()))?;
    }
    let body = serde_json::to_string_pretty(manifest).context("Failed to serialize manifest")?;
    std::fs::write(&manifest_path, format!("{body}\n"))
        .with_context(|| format!("Failed to write {}", manifest_path.display()))?;
    Ok(manifest_path)
}

fn sanitize_slug(input: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in input.chars() {
        let lower = ch.to_ascii_lowercase();
        if lower.is_ascii_alphanumeric() {
            out.push(lower);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "mcp-server".to_string()
    } else {
        trimmed
    }
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
        let root = parse_source_root(&raw, &source.path)?;

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

fn parse_source_root(raw: &str, path: &Path) -> Result<Value> {
    if let Ok(root) = serde_json::from_str::<Value>(raw) {
        return Ok(root);
    }

    if let Ok(toml_root) = toml::from_str::<toml::Value>(raw) {
        return serde_json::to_value(toml_root)
            .with_context(|| format!("Failed to convert TOML in {}", path.display()));
    }

    bail!(
        "Failed to parse {} as JSON or TOML MCP config.",
        path.display()
    )
}

fn default_sources(home: &Path, cwd: &Path, additional_paths: &[PathBuf]) -> Vec<ConfigSource> {
    let mut sources = vec![
        ConfigSource {
            label: "claude-global-json".to_string(),
            path: home.join(".claude").join("mcp.json"),
        },
        ConfigSource {
            label: "claude-global-settings".to_string(),
            path: home.join(".claude").join("settings.json"),
        },
        ConfigSource {
            label: "claude-project-json".to_string(),
            path: cwd.join(".claude").join("mcp.json"),
        },
        ConfigSource {
            label: "claude-project-settings".to_string(),
            path: cwd.join(".claude").join("settings.json"),
        },
        ConfigSource {
            label: "codex-global-json".to_string(),
            path: home.join(".codex").join("mcp.json"),
        },
        ConfigSource {
            label: "codex-global-toml".to_string(),
            path: home.join(".codex").join("config.toml"),
        },
        ConfigSource {
            label: "codex-project-json".to_string(),
            path: cwd.join(".codex").join("mcp.json"),
        },
        ConfigSource {
            label: "codex-project-toml".to_string(),
            path: cwd.join(".codex").join("config.toml"),
        },
        ConfigSource {
            label: "shared-global".to_string(),
            path: home.join(".config").join("mcp").join("servers.json"),
        },
        ConfigSource {
            label: "amp-settings".to_string(),
            path: home.join(".config").join("amp").join("settings.json"),
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
    if let Some(obj) = root.get("mcp_servers").and_then(Value::as_object) {
        return obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    }
    if let Some(obj) = root.get("servers").and_then(Value::as_object) {
        return obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    }
    if let Some(obj) = root.get("amp.mcpServers").and_then(Value::as_object) {
        return obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    }
    if let Some(amp_obj) = root.get("amp").and_then(Value::as_object)
        && let Some(obj) = amp_obj.get("mcpServers").and_then(Value::as_object)
    {
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
    if contains_any(&corpus, &["xcode", "simulator", "ios", "xcodebuildmcp"]) {
        return "Xcode build, simulator, and iOS debug workflows".to_string();
    }
    if contains_any(&corpus, &["chrome-devtools", "devtools", "chrome"]) {
        return "Browser inspection and debugging workflows".to_string();
    }
    if contains_any(
        &corpus,
        &["memory", "knowledge graph", "read_graph", "search_nodes"],
    ) {
        return "Memory and knowledge graph workflows".to_string();
    }
    if contains_any(
        &corpus,
        &[
            "jira",
            "linear",
            "github",
            "gitlab",
            "issue",
            "pull request",
            "merge request",
        ],
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
        "write",
        "create",
        "update",
        "insert",
        "upsert",
        "apply",
        "deploy",
        "commit",
        "push",
        "exec",
        "execute",
        "mutation",
        "admin",
        "xcodebuildmcp",
        "xcode",
        "simulator",
        "debug",
        "chrome-devtools",
        "devtools",
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
    fn test_parse_source_root_supports_toml() {
        let raw = r#"
[mcp_servers.chrome-devtools]
command = "npx"
args = ["-y", "chrome-devtools-mcp@latest"]
"#;
        let root = parse_source_root(raw, Path::new("config.toml")).unwrap();
        let entries = extract_server_entries(&root);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, "chrome-devtools");
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
    fn test_infer_purpose_memory_server_is_memory_workflow() {
        let purpose = infer_purpose(
            "memory",
            None,
            Some("npx"),
            None,
            &[
                "-y".to_string(),
                "@modelcontextprotocol/server-memory".to_string(),
            ],
        );
        assert_eq!(purpose, "Memory and knowledge graph workflows");
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

    #[test]
    fn test_apply_replace_writes_skill_and_updates_config() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("settings.json");
        let skills_dir = dir.path().join("skills");
        let mock_mcp = dir.path().join("mock-mcp.sh");
        std::fs::write(
            &mock_mcp,
            "#!/bin/sh\nread _\nread _\nprintf '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":\"2025-03-26\",\"capabilities\":{}}}\\n'\nprintf '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"tools\":[{\"name\":\"navigate\"},{\"name\":\"click\"},{\"name\":\"fill\"}]}}\\n'\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&mock_mcp, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::write(
            &config_path,
            &format!(
                r#"{{
  "mcpServers": {{
    "playwright": {{
      "command": "{}",
      "description": "Read list",
      "readOnly": true
    }}
  }}
}}"#,
                mock_mcp.display()
            ),
        )
        .unwrap();

        let result = apply(
            "playwright",
            PlanMode::Replace,
            true,
            &[config_path.clone()],
            Some(skills_dir.clone()),
        )
        .unwrap();

        assert!(result.skill_path.exists());
        assert!(result.orchestrator_skill_path.exists());
        assert_eq!(result.tool_skill_paths.len(), 3);
        assert!(result.tool_skill_paths.iter().all(|path| path.exists()));
        assert!(result.mcp_config_updated);
        assert!(result.mcp_config_backup.is_some());

        let updated = std::fs::read_to_string(&config_path).unwrap();
        assert!(!updated.contains("playwright"));

        let skill = std::fs::read_to_string(&result.skill_path).unwrap();
        assert!(skill.contains("## Capability Skills"));
        assert!(!skill.contains("## Server Metadata"));
        assert!(!skill.contains("mcp__"));

        let manifest_path = manifest_path_for_skill(&result.skill_path).unwrap();
        assert!(manifest_path.exists());
        let manifest_raw = std::fs::read_to_string(&manifest_path).unwrap();
        assert!(!manifest_raw.contains("\"required_tool_hints\""));
        let manifest: SkillParityManifest = serde_json::from_str(&manifest_raw).unwrap();
        assert_eq!(manifest.required_tools.len(), 3);
        assert_eq!(manifest.tool_skills.len(), 3);
    }

    #[test]
    fn test_apply_replace_requires_confirmation() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("settings.json");
        let mock_mcp = dir.path().join("mock-mcp.sh");
        std::fs::write(
            &mock_mcp,
            "#!/bin/sh\nread _\nread _\nprintf '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":\"2025-03-26\",\"capabilities\":{}}}\\n'\nprintf '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"tools\":[{\"name\":\"navigate\"}]}}\\n'\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&mock_mcp, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::write(
            &config_path,
            &format!(
                r#"{{
  "mcpServers": {{
    "playwright": {{
      "command": "{}",
      "description": "Read list",
      "readOnly": true
    }}
  }}
}}"#,
                mock_mcp.display()
            ),
        )
        .unwrap();

        let err = apply(
            "playwright",
            PlanMode::Replace,
            false,
            &[config_path],
            Some(dir.path().join("skills")),
        )
        .unwrap_err();

        assert!(format!("{err:#}").contains("--yes"));
    }

    #[test]
    fn test_apply_replace_requires_live_introspection() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("settings.json");
        std::fs::write(
            &config_path,
            r#"{
  "mcpServers": {
    "playwright": {
      "command": "/bin/echo",
      "args": ["not-json"],
      "description": "Read list",
      "readOnly": true
    }
  }
}"#,
        )
        .unwrap();

        let err = apply(
            "playwright",
            PlanMode::Replace,
            true,
            &[config_path],
            Some(dir.path().join("skills")),
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("live MCP tool introspection"));
    }

    #[test]
    fn test_apply_dedupes_duplicate_tool_names() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("settings.json");
        let skills_dir = dir.path().join("skills");
        let mock_mcp = dir.path().join("mock-mcp.sh");
        std::fs::write(
            &mock_mcp,
            "#!/bin/sh\nread _\nread _\nprintf '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":\"2025-03-26\",\"capabilities\":{}}}\\n'\nprintf '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"tools\":[{\"name\":\"navigate\"},{\"name\":\"navigate\"},{\"name\":\"click\"}]}}\\n'\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&mock_mcp, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::write(
            &config_path,
            &format!(
                r#"{{
  "mcpServers": {{
    "playwright": {{
      "command": "{}",
      "description": "Read list",
      "readOnly": true
    }}
  }}
}}"#,
                mock_mcp.display()
            ),
        )
        .unwrap();

        let result = apply(
            "playwright",
            PlanMode::Hybrid,
            false,
            &[config_path],
            Some(skills_dir),
        )
        .unwrap();

        assert_eq!(result.tool_skill_paths.len(), 2);
    }

    #[test]
    fn test_verify_uses_manifest_with_clean_skill_markdown() {
        let dir = tempfile::tempdir().unwrap();
        let skill_path = dir.path().join("mcp-playwright.md");
        std::fs::write(
            &skill_path,
            r#"# MCP Workflow: playwright

## Capability Playbooks

### 1. Generic

Goal: Run browser automation workflows.

Steps:
1. Open browser.
"#,
        )
        .unwrap();

        let server = MCPServerProfile {
            id: "fixture:playwright".to_string(),
            name: "playwright".to_string(),
            source_label: "fixture".to_string(),
            source_path: dir.path().join("settings.json"),
            purpose: "Browser automation".to_string(),
            command: Some("npx".to_string()),
            args: vec!["-y".to_string(), "@playwright/mcp@latest".to_string()],
            url: None,
            env_keys: vec![],
            declared_tool_count: 0,
            permission_hints: vec![],
            inferred_permission: PermissionLevel::ReadOnly,
            recommendation: ConversionRecommendation::ReplaceCandidate,
            recommendation_reason: "read-only".to_string(),
        };

        let manifest = SkillParityManifest {
            format_version: 2,
            generated_at: Utc::now(),
            server_id: server.id.clone(),
            server_name: server.name.clone(),
            orchestrator_skill: Some("mcp-playwright.md".to_string()),
            required_tools: vec!["execute".to_string()],
            tool_skills: vec![ManifestToolSkill {
                tool_name: "execute".to_string(),
                skill_file: "mcp-playwright-tool-execute.md".to_string(),
            }],
            required_tool_hints: vec!["mcp__playwright__execute".to_string()],
        };
        std::fs::write(
            dir.path().join("mcp-playwright-tool-execute.md"),
            "# Capability: execute\n",
        )
        .unwrap();
        write_skill_manifest(&skill_path, &manifest).unwrap();

        let introspected = vec!["mcp__playwright__execute".to_string()];
        let report =
            verify_with_server_and_path(&server, &skill_path, Some(&introspected)).unwrap();

        assert!(report.introspection_ok);
        assert!(report.missing_in_server.is_empty());
        assert!(report.missing_in_skill.is_empty());
        assert!(report.missing_skill_files.is_empty());
        assert!(report.passed);
    }
}
