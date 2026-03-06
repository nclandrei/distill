use super::*;
use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

const DOSSIER_FORMAT_VERSION: u32 = 4;
const DEFAULT_BACKEND_TIMEOUT_SECONDS: u64 = 90;
const DEFAULT_BACKEND_CHUNK_SIZE: usize = 8;
const DEFAULT_PROBE_TIMEOUT_SECONDS: u64 = 30;
const DEFAULT_PROBE_RETRIES: u32 = 0;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ConvertBackendPreference {
    #[default]
    Auto,
    Codex,
    Claude,
}

impl std::fmt::Display for ConvertBackendPreference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConvertBackendPreference::Auto => write!(f, "auto"),
            ConvertBackendPreference::Codex => write!(f, "codex"),
            ConvertBackendPreference::Claude => write!(f, "claude"),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum ConvertBackendName {
    Codex,
    Claude,
}

impl std::fmt::Display for ConvertBackendName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConvertBackendName::Codex => write!(f, "codex"),
            ConvertBackendName::Claude => write!(f, "claude"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConvertBackendConfig {
    #[serde(default)]
    pub preference: ConvertBackendPreference,
    #[serde(default = "default_backend_timeout_seconds")]
    pub timeout_seconds: u64,
    #[serde(default = "default_backend_chunk_size")]
    pub chunk_size: usize,
}

impl Default for ConvertBackendConfig {
    fn default() -> Self {
        Self {
            preference: ConvertBackendPreference::Auto,
            timeout_seconds: DEFAULT_BACKEND_TIMEOUT_SECONDS,
            chunk_size: DEFAULT_BACKEND_CHUNK_SIZE,
        }
    }
}

fn default_backend_timeout_seconds() -> u64 {
    DEFAULT_BACKEND_TIMEOUT_SECONDS
}

fn default_backend_chunk_size() -> usize {
    DEFAULT_BACKEND_CHUNK_SIZE
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConvertProbeConfig {
    #[serde(default = "default_probe_timeout_seconds")]
    pub timeout_seconds: u64,
    #[serde(default = "default_probe_retries")]
    pub retries: u32,
    #[serde(default)]
    pub allow_side_effect_probes: bool,
}

impl Default for ConvertProbeConfig {
    fn default() -> Self {
        Self {
            timeout_seconds: DEFAULT_PROBE_TIMEOUT_SECONDS,
            retries: DEFAULT_PROBE_RETRIES,
            allow_side_effect_probes: false,
        }
    }
}

fn default_probe_timeout_seconds() -> u64 {
    DEFAULT_PROBE_TIMEOUT_SECONDS
}

fn default_probe_retries() -> u32 {
    DEFAULT_PROBE_RETRIES
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConvertV3Options {
    #[serde(default)]
    pub backend: Option<ConvertBackendName>,
    #[serde(default = "default_backend_auto")]
    pub backend_auto: bool,
    #[serde(default)]
    pub backend_config: ConvertBackendConfig,
}

impl Default for ConvertV3Options {
    fn default() -> Self {
        Self {
            backend: None,
            backend_auto: true,
            backend_config: ConvertBackendConfig::default(),
        }
    }
}

fn default_backend_auto() -> bool {
    true
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContractTestOptions {
    #[serde(default)]
    pub allow_side_effects: bool,
    #[serde(default = "default_probe_timeout_seconds")]
    pub probe_timeout_seconds: u64,
    #[serde(default = "default_probe_retries")]
    pub probe_retries: u32,
}

impl Default for ContractTestOptions {
    fn default() -> Self {
        Self {
            allow_side_effects: false,
            probe_timeout_seconds: DEFAULT_PROBE_TIMEOUT_SECONDS,
            probe_retries: DEFAULT_PROBE_RETRIES,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackendHealthStatus {
    pub backend: ConvertBackendName,
    pub available: bool,
    pub diagnostics: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConvertBackendHealthReport {
    pub checked_at: DateTime<Utc>,
    pub statuses: Vec<BackendHealthStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RuntimeTool {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolContractTest {
    pub probe: String,
    pub expected: String,
    pub method: String,
    #[serde(default = "default_contract_applicable")]
    pub applicable: bool,
}

fn default_contract_applicable() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolDossier {
    pub name: String,
    pub explanation: String,
    pub recipe: Vec<String>,
    pub evidence: Vec<String>,
    pub confidence: f32,
    pub contract_tests: Vec<ToolContractTest>,
    #[serde(default, skip_serializing_if = "probe_inputs_is_empty")]
    pub probe_inputs: ProbeInputs,
    #[serde(default)]
    pub probe_input_source: ProbeInputSource,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ProbeInputs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub happy_path: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invalid_input: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side_effect_safety: Option<Value>,
}

fn probe_inputs_is_empty(inputs: &ProbeInputs) -> bool {
    inputs.happy_path.is_none()
        && inputs.invalid_input.is_none()
        && inputs.side_effect_safety.is_none()
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ProbeInputSource {
    Backend,
    #[default]
    Synthesized,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ServerGate {
    Ready,
    Blocked,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ServerDossier {
    pub generated_at: DateTime<Utc>,
    pub format_version: u32,
    pub server: MCPServerProfile,
    pub runtime_tools: Vec<RuntimeTool>,
    pub tool_dossiers: Vec<ToolDossier>,
    pub server_gate: ServerGate,
    pub gate_reasons: Vec<String>,
    pub backend_used: String,
    pub backend_fallback_used: bool,
    pub backend_diagnostics: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DossierBundle {
    pub format_version: u32,
    pub generated_at: DateTime<Utc>,
    pub dossiers: Vec<ServerDossier>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BuildServerResult {
    pub server_id: String,
    pub orchestrator_skill_path: PathBuf,
    pub tool_skill_paths: Vec<PathBuf>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BuildResult {
    pub generated_at: DateTime<Utc>,
    pub skills_dir: PathBuf,
    pub servers: Vec<BuildServerResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ContractProbeResult {
    pub probe: String,
    pub passed: bool,
    pub skipped: bool,
    #[serde(default)]
    pub executed: bool,
    pub details: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_args_preview: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_preview: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_kind: Option<ProbeErrorKind>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ProbeErrorKind {
    Timeout,
    McpError,
    Transport,
    SchemaGap,
    Unsafe,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ContractToolResult {
    pub tool: String,
    pub passed: bool,
    pub probes: Vec<ContractProbeResult>,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ContractServerReport {
    pub server_id: String,
    pub passed: bool,
    pub introspection_ok: bool,
    pub missing_runtime_tools: Vec<String>,
    pub tools: Vec<ContractToolResult>,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ContractTestReport {
    pub generated_at: DateTime<Utc>,
    pub passed: bool,
    pub servers: Vec<ContractServerReport>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ApplyServerResult {
    pub server_id: String,
    pub applied: bool,
    pub orchestrator_skill_path: PathBuf,
    pub tool_skill_paths: Vec<PathBuf>,
    pub mcp_config_backup: Option<PathBuf>,
    pub mcp_config_updated: bool,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ApplyResultV3 {
    pub generated_at: DateTime<Utc>,
    pub skills_dir: PathBuf,
    pub servers: Vec<ApplyServerResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OneShotV3Result {
    pub generated_at: DateTime<Utc>,
    pub dossier: DossierBundle,
    pub contract_test: ContractTestReport,
    pub apply: ApplyResultV3,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackendSelection {
    pub selected: ConvertBackendName,
    pub fallback: Option<ConvertBackendName>,
    pub auto_mode: bool,
    pub diagnostics: Vec<String>,
}

#[derive(Debug, Clone)]
struct BackendContext {
    selection: BackendSelection,
    health: ConvertBackendHealthReport,
}

pub fn backend_health_report(_config: &ConvertBackendConfig) -> ConvertBackendHealthReport {
    ConvertBackendHealthReport {
        checked_at: Utc::now(),
        statuses: vec![
            codex_backend().health_check(),
            claude_backend().health_check(),
        ],
    }
}

pub fn discover_v3(
    selector: Option<&str>,
    all: bool,
    additional_paths: &[PathBuf],
    options: &ConvertV3Options,
) -> Result<DossierBundle> {
    let inventory = discover(additional_paths)?;
    let targets = select_servers(&inventory, selector, all)?;
    if targets.is_empty() {
        bail!("No MCP servers selected for conversion discovery.");
    }

    let backend_ctx = prepare_backend_context(options)?;
    let mut dossiers = Vec::with_capacity(targets.len());

    for server in targets {
        let dossier = discover_server_dossier(&server, options, &backend_ctx)?;
        dossiers.push(dossier);
    }

    Ok(DossierBundle {
        format_version: DOSSIER_FORMAT_VERSION,
        generated_at: Utc::now(),
        dossiers,
    })
}

pub fn discover_v3_to_path(
    selector: Option<&str>,
    all: bool,
    additional_paths: &[PathBuf],
    options: &ConvertV3Options,
    out_path: &Path,
) -> Result<DossierBundle> {
    let bundle = discover_v3(selector, all, additional_paths, options)?;
    write_dossier_bundle(out_path, &bundle)?;
    Ok(bundle)
}

pub fn build_from_dossier_path(
    dossier_path: &Path,
    skills_dir: Option<PathBuf>,
) -> Result<BuildResult> {
    let bundle = load_dossier_bundle(dossier_path)?;
    build_from_bundle(&bundle, skills_dir)
}

pub fn contract_test_from_dossier_path(
    dossier_path: &Path,
    report_path: Option<&Path>,
    options: ContractTestOptions,
) -> Result<ContractTestReport> {
    let bundle = load_dossier_bundle(dossier_path)?;
    let report = contract_test_bundle(&bundle, options)?;
    if let Some(path) = report_path {
        let body = serde_json::to_string_pretty(&report)
            .context("Failed to serialize contract test report")?;
        fs::write(path, format!("{body}\n"))
            .with_context(|| format!("Failed to write {}", path.display()))?;
    }
    Ok(report)
}

pub fn apply_from_dossier_path(
    dossier_path: &Path,
    yes: bool,
    skills_dir: Option<PathBuf>,
    contract_options: ContractTestOptions,
) -> Result<ApplyResultV3> {
    let bundle = load_dossier_bundle(dossier_path)?;
    apply_from_bundle(&bundle, yes, skills_dir, contract_options)
}

pub fn run_one_shot_v3(
    selector: &str,
    additional_paths: &[PathBuf],
    options: &ConvertV3Options,
    skills_dir: Option<PathBuf>,
    contract_options: ContractTestOptions,
) -> Result<OneShotV3Result> {
    let bundle = discover_v3(Some(selector), false, additional_paths, options)?;
    let contract = contract_test_bundle(&bundle, contract_options)?;
    if !contract.passed {
        bail!(
            "Conversion blocked: contract tests failed. Run 'distill convert contract-test --from-dossier ...' for details."
        );
    }
    let apply = apply_from_bundle(&bundle, true, skills_dir, contract_options)?;
    Ok(OneShotV3Result {
        generated_at: Utc::now(),
        dossier: bundle,
        contract_test: contract,
        apply,
    })
}

pub fn write_dossier_bundle(path: &Path, bundle: &DossierBundle) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create {}", parent.display()))?;
    }
    let body =
        serde_json::to_string_pretty(bundle).context("Failed to serialize dossier bundle")?;
    fs::write(path, format!("{body}\n"))
        .with_context(|| format!("Failed to write {}", path.display()))?;
    Ok(())
}

pub fn load_dossier_bundle(path: &Path) -> Result<DossierBundle> {
    let raw =
        fs::read_to_string(path).with_context(|| format!("Failed to read {}", path.display()))?;

    if let Ok(bundle) = serde_json::from_str::<DossierBundle>(&raw) {
        return Ok(upgrade_bundle_from_legacy(bundle));
    }

    if let Ok(single) = serde_json::from_str::<ServerDossier>(&raw) {
        let bundle = DossierBundle {
            format_version: DOSSIER_FORMAT_VERSION,
            generated_at: Utc::now(),
            dossiers: vec![single],
        };
        return Ok(upgrade_bundle_from_legacy(bundle));
    }

    bail!(
        "Invalid dossier JSON in {}. Expected either a DossierBundle or a ServerDossier.",
        path.display()
    )
}

fn upgrade_bundle_from_legacy(mut bundle: DossierBundle) -> DossierBundle {
    if bundle.format_version < DOSSIER_FORMAT_VERSION {
        bundle.format_version = DOSSIER_FORMAT_VERSION;
    }
    for dossier in &mut bundle.dossiers {
        if dossier.format_version < DOSSIER_FORMAT_VERSION {
            dossier.format_version = DOSSIER_FORMAT_VERSION;
        }
        for tool in &mut dossier.tool_dossiers {
            if tool.probe_input_source == ProbeInputSource::Synthesized
                && has_any_probe_inputs(&tool.probe_inputs)
            {
                tool.probe_input_source = ProbeInputSource::Backend;
            }
        }
    }
    bundle
}

pub fn build_from_bundle(
    bundle: &DossierBundle,
    skills_dir: Option<PathBuf>,
) -> Result<BuildResult> {
    let skills_root = skills_dir.unwrap_or_else(default_agents_skills_dir);
    fs::create_dir_all(&skills_root)
        .with_context(|| format!("Failed to create skills dir {}", skills_root.display()))?;

    let mut servers = Vec::with_capacity(bundle.dossiers.len());
    for dossier in &bundle.dossiers {
        let (orchestrator, tool_paths, mut notes) = write_server_skills(dossier, &skills_root)?;
        if dossier.server_gate == ServerGate::Blocked {
            notes.push("Server gate is blocked; generated skills are draft-only until discover/contract-test gates pass.".to_string());
        }
        servers.push(BuildServerResult {
            server_id: dossier.server.id.clone(),
            orchestrator_skill_path: orchestrator,
            tool_skill_paths: tool_paths,
            notes,
        });
    }

    Ok(BuildResult {
        generated_at: Utc::now(),
        skills_dir: skills_root,
        servers,
    })
}

pub fn contract_test_bundle(
    bundle: &DossierBundle,
    options: ContractTestOptions,
) -> Result<ContractTestReport> {
    let mut servers = Vec::with_capacity(bundle.dossiers.len());
    let mut all_passed = true;

    for dossier in &bundle.dossiers {
        let runtime_specs = introspect_tool_specs(&dossier.server);
        let (introspection_ok, runtime_map, mut reasons) = match runtime_specs {
            Ok(specs) => {
                let map = specs
                    .into_iter()
                    .map(|spec| {
                        let normalized = normalize_tool_name(&spec.name);
                        (
                            normalized.clone(),
                            RuntimeTool {
                                name: normalized,
                                description: spec.description,
                                input_schema: spec.input_schema,
                            },
                        )
                    })
                    .collect::<BTreeMap<_, _>>();
                (true, map, vec![])
            }
            Err(err) => {
                let map = dossier
                    .runtime_tools
                    .iter()
                    .map(|tool| (normalize_tool_name(&tool.name), tool.clone()))
                    .collect::<BTreeMap<_, _>>();
                (
                    false,
                    map,
                    vec![format!(
                        "Runtime introspection unavailable; falling back to dossier runtime metadata: {err}"
                    )],
                )
            }
        };
        let runtime_names = runtime_map.keys().cloned().collect::<BTreeSet<_>>();

        let mut missing_runtime_tools = vec![];
        let mut tools = Vec::with_capacity(dossier.tool_dossiers.len());
        let mut server_passed = dossier.server_gate == ServerGate::Ready;
        if dossier.server_gate == ServerGate::Blocked {
            reasons.push(format!(
                "Server gate is blocked: {}",
                dossier.gate_reasons.join(" | ")
            ));
        }

        for tool in &dossier.tool_dossiers {
            let normalized = normalize_tool_name(&tool.name);
            if !runtime_names.contains(&normalized) {
                missing_runtime_tools.push(normalized.clone());
            }
            let runtime_spec = runtime_map.get(&normalized);
            let result =
                evaluate_tool_contract(tool, &dossier.server, runtime_spec, &runtime_map, options);
            if !result.passed {
                server_passed = false;
            }
            tools.push(result);
        }

        missing_runtime_tools.sort();
        missing_runtime_tools.dedup();
        if !missing_runtime_tools.is_empty() {
            server_passed = false;
            reasons.push(format!(
                "Missing runtime tools for contract coverage: {}",
                missing_runtime_tools.join(", ")
            ));
        }

        if !server_passed {
            all_passed = false;
        }

        servers.push(ContractServerReport {
            server_id: dossier.server.id.clone(),
            passed: server_passed,
            introspection_ok,
            missing_runtime_tools,
            tools,
            reasons,
        });
    }

    Ok(ContractTestReport {
        generated_at: Utc::now(),
        passed: all_passed,
        servers,
    })
}

pub fn apply_from_bundle(
    bundle: &DossierBundle,
    yes: bool,
    skills_dir: Option<PathBuf>,
    contract_options: ContractTestOptions,
) -> Result<ApplyResultV3> {
    if !yes {
        bail!("apply requires --yes because it mutates MCP config entries.");
    }

    if bundle.dossiers.is_empty() {
        bail!("No dossiers found to apply.");
    }

    let contract = contract_test_bundle(bundle, contract_options)?;
    if !contract.passed {
        bail!(
            "Conversion blocked: one or more contract tests failed. No files were applied or MCP configs mutated."
        );
    }

    let skills_root = skills_dir.unwrap_or_else(default_agents_skills_dir);
    fs::create_dir_all(&skills_root)
        .with_context(|| format!("Failed to create skills dir {}", skills_root.display()))?;

    let mut servers = Vec::with_capacity(bundle.dossiers.len());
    for dossier in &bundle.dossiers {
        if dossier.server_gate != ServerGate::Ready {
            bail!(
                "Server '{}' is blocked and cannot be applied: {}",
                dossier.server.id,
                dossier.gate_reasons.join(" | ")
            );
        }

        let (orchestrator, tool_paths, notes) = write_server_skills(dossier, &skills_root)?;

        let remove_result =
            remove_server_from_config(&dossier.server.source_path, &dossier.server.name);
        let (backup, updated) = match remove_result {
            Ok((backup, updated)) if updated => (backup, updated),
            Ok((_backup, _updated)) => {
                rollback_server_skill_files(&orchestrator, &tool_paths);
                bail!(
                    "MCP config entry '{}' not found in {}. Rolled back generated skills to keep conversion atomic.",
                    dossier.server.name,
                    dossier.server.source_path.display()
                );
            }
            Err(err) => {
                rollback_server_skill_files(&orchestrator, &tool_paths);
                return Err(err).with_context(|| {
                    format!(
                        "Failed to mutate MCP config for '{}'; generated skill files were rolled back.",
                        dossier.server.id
                    )
                });
            }
        };

        servers.push(ApplyServerResult {
            server_id: dossier.server.id.clone(),
            applied: true,
            orchestrator_skill_path: orchestrator,
            tool_skill_paths: tool_paths,
            mcp_config_backup: backup,
            mcp_config_updated: updated,
            notes,
        });
    }

    Ok(ApplyResultV3 {
        generated_at: Utc::now(),
        skills_dir: skills_root,
        servers,
    })
}

fn rollback_server_skill_files(orchestrator: &Path, tool_paths: &[PathBuf]) {
    for path in tool_paths {
        let _ = fs::remove_file(path);
    }
    let _ = fs::remove_file(orchestrator);
}

fn default_agents_skills_dir() -> PathBuf {
    let home = std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."));
    home.join(".agents").join("skills")
}

fn write_server_skills(
    dossier: &ServerDossier,
    root: &Path,
) -> Result<(PathBuf, Vec<PathBuf>, Vec<String>)> {
    let server_slug = sanitize_slug(&dossier.server.name);
    let orchestrator_path = root.join(format!("{server_slug}.md"));

    let mut tool_skill_paths = vec![];
    let mut tool_refs = vec![];
    for tool in &dossier.tool_dossiers {
        let tool_slug = sanitize_slug(&tool.name);
        let file_name = format!("{server_slug}--{tool_slug}.md");
        let path = root.join(&file_name);
        let body = render_tool_skill_markdown(dossier, tool);
        fs::write(&path, body)
            .with_context(|| format!("Failed to write tool skill {}", path.display()))?;
        tool_skill_paths.push(path.clone());
        tool_refs.push((tool.name.clone(), file_name));
    }

    let orchestrator = render_orchestrator_v3_markdown(dossier, &tool_refs);
    fs::write(&orchestrator_path, orchestrator).with_context(|| {
        format!(
            "Failed to write orchestrator skill {}",
            orchestrator_path.display()
        )
    })?;

    let mut notes = vec![format!(
        "Generated 1 orchestrator skill and {} tool skills.",
        tool_skill_paths.len()
    )];
    if dossier.backend_fallback_used {
        notes.push("Backend fallback was used during dossier discovery.".to_string());
    }
    Ok((orchestrator_path, tool_skill_paths, notes))
}

fn render_orchestrator_v3_markdown(
    dossier: &ServerDossier,
    tool_refs: &[(String, String)],
) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Workflow: {}\n\n", dossier.server.name.trim()));
    out.push_str("## Purpose\n\n");
    out.push_str(&format!("{}\n\n", dossier.server.purpose.trim()));

    out.push_str("## Capability Skills\n\n");
    if tool_refs.is_empty() {
        out.push_str("- No tool skills available.\n\n");
    } else {
        for (tool, file) in tool_refs {
            let skill_name = file.trim_end_matches(".md");
            out.push_str(&format!("- `${skill_name}` for `{tool}`.\n"));
        }
        out.push('\n');
    }

    out.push_str("## Flow\n\n");
    out.push_str("1. Confirm user intent and choose the minimum capability skills needed.\n");
    out.push_str("2. Execute one tool skill at a time and keep outputs deterministic.\n");
    out.push_str("3. Validate outcomes after each step; stop on mismatch and report root cause.\n");
    out.push_str(
        "4. Ask for explicit confirmation before destructive or irreversible operations.\n\n",
    );

    if dossier.server_gate == ServerGate::Blocked {
        out.push_str("## Gate Status\n\n");
        out.push_str("Server conversion is currently blocked:\n");
        for reason in &dossier.gate_reasons {
            out.push_str(&format!("- {}\n", reason));
        }
    }

    out
}

fn render_tool_skill_markdown(dossier: &ServerDossier, tool: &ToolDossier) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Capability: {}\n\n", tool.name));
    out.push_str("## What It Does\n\n");
    out.push_str(&format!("{}\n\n", tool.explanation.trim()));

    out.push_str("## Recipe\n\n");
    if tool.recipe.is_empty() {
        out.push_str("1. Validate inputs and preconditions.\n");
        out.push_str("2. Execute the operation and capture output.\n");
        out.push_str("3. Validate result and report outcome.\n\n");
    } else {
        for (idx, step) in tool.recipe.iter().enumerate() {
            out.push_str(&format!("{}. {}\n", idx + 1, step));
        }
        out.push('\n');
    }

    out.push_str("## Contract Tests\n\n");
    for test in &tool.contract_tests {
        let applicability = if test.applicable {
            "required"
        } else {
            "optional"
        };
        out.push_str(&format!(
            "- `{}` ({applicability}): {}. Method: {}\n",
            test.probe, test.expected, test.method
        ));
    }
    out.push('\n');

    out.push_str("## Evidence\n\n");
    if tool.evidence.is_empty() {
        out.push_str("- Runtime metadata + contract checks (source not available).\n");
    } else {
        for evidence in &tool.evidence {
            out.push_str(&format!("- {}\n", evidence));
        }
    }
    out.push('\n');

    out.push_str("## Confidence\n\n");
    out.push_str(&format!("- {:.2}\n\n", tool.confidence.clamp(0.0, 1.0)));

    out.push_str("## Scope\n\n");
    out.push_str(&format!(
        "- Generated from `{}` dossier entry.\n",
        dossier.server.id
    ));

    out
}

fn select_servers(
    inventory: &ConvertInventory,
    selector: Option<&str>,
    all: bool,
) -> Result<Vec<MCPServerProfile>> {
    if all {
        return Ok(inventory.servers.clone());
    }
    if let Some(selector) = selector {
        return Ok(vec![resolve_server(&inventory.servers, selector)?]);
    }

    if inventory.servers.len() == 1 {
        return Ok(inventory.servers.clone());
    }

    bail!(
        "Specify a server name/id or pass --all. Found {} discoverable server(s).",
        inventory.servers.len()
    )
}

fn discover_server_dossier(
    server: &MCPServerProfile,
    options: &ConvertV3Options,
    backend_ctx: &BackendContext,
) -> Result<ServerDossier> {
    let mut gate_reasons = vec![];
    let mut diagnostics = backend_ctx.selection.diagnostics.clone();

    let runtime_specs = introspect_tool_specs(server)
        .with_context(|| format!("Runtime MCP introspection failed for '{}'.", server.id));

    let runtime_tools = match runtime_specs {
        Ok(specs) => specs
            .iter()
            .map(|spec| RuntimeTool {
                name: normalize_tool_name(&spec.name),
                description: spec.description.clone(),
                input_schema: spec.input_schema.clone(),
            })
            .collect::<Vec<_>>(),
        Err(err) => {
            gate_reasons.push(format!("Runtime tools/list introspection failed: {err}"));
            vec![]
        }
    };

    if runtime_tools.is_empty() {
        gate_reasons.push(
            "No runtime tools available; cannot build deterministic per-tool recipes.".to_string(),
        );
    }

    let mut selected_backend = backend_ctx.selection.selected;
    let mut fallback_used = false;

    let mut dossiers = vec![];
    if !runtime_tools.is_empty() {
        match generate_tool_dossiers(
            selected_backend,
            server,
            &runtime_tools,
            options.backend_config.chunk_size.max(1),
            options.backend_config.timeout_seconds,
        ) {
            Ok(generated) => dossiers = generated,
            Err(err) => {
                diagnostics.push(format!(
                    "Primary backend '{}' failed: {err}",
                    selected_backend
                ));
                if backend_ctx.selection.auto_mode {
                    if let Some(fallback) = backend_ctx.selection.fallback {
                        fallback_used = true;
                        selected_backend = fallback;
                        diagnostics.push(format!(
                            "Retrying dossier generation with fallback backend '{}'.",
                            fallback
                        ));
                        match generate_tool_dossiers(
                            fallback,
                            server,
                            &runtime_tools,
                            options.backend_config.chunk_size.max(1),
                            options.backend_config.timeout_seconds,
                        ) {
                            Ok(generated) => dossiers = generated,
                            Err(fallback_err) => {
                                diagnostics.push(format!(
                                    "Warning: backend dossier generation failed on primary and fallback; using runtime fallback dossiers. fallback_error={fallback_err}"
                                ));
                            }
                        }
                    } else {
                        diagnostics.push(format!(
                            "Warning: backend dossier generation failed and no fallback backend is available; using runtime fallback dossiers. error={err}"
                        ));
                    }
                } else {
                    diagnostics.push(format!(
                        "Warning: backend dossier generation failed; using runtime fallback dossiers. error={err}"
                    ));
                }
            }
        }
    }

    let runtime_map = runtime_tools
        .iter()
        .map(|tool| (normalize_tool_name(&tool.name), tool.clone()))
        .collect::<BTreeMap<_, _>>();

    let mut dossier_map = dossiers
        .into_iter()
        .map(|tool| (normalize_tool_name(&tool.name), tool))
        .collect::<BTreeMap<_, _>>();

    let mut tool_dossiers = vec![];
    for name in runtime_map.keys() {
        let mut dossier = dossier_map
            .remove(name)
            .unwrap_or_else(|| fallback_tool_dossier(server, runtime_map.get(name).unwrap()));
        normalize_contract_tests(&mut dossier, server);
        if dossier.recipe.is_empty() {
            gate_reasons.push(format!("Tool '{}' has no executable recipe.", name));
        }
        if !has_required_probes(&dossier.contract_tests) {
            gate_reasons.push(format!(
                "Tool '{}' is missing required contract tests (happy-path, invalid-input, side-effect-safety).",
                name
            ));
        }
        tool_dossiers.push(dossier);
    }

    let mut backend_diagnostics = diagnostics;
    backend_diagnostics.extend(backend_ctx.health.statuses.iter().map(|status| {
        format!(
            "backend={} available={} diagnostics={}",
            status.backend,
            status.available,
            status.diagnostics.join(" | ")
        )
    }));

    let server_gate = if gate_reasons.is_empty() {
        ServerGate::Ready
    } else {
        ServerGate::Blocked
    };

    Ok(ServerDossier {
        generated_at: Utc::now(),
        format_version: DOSSIER_FORMAT_VERSION,
        server: server.clone(),
        runtime_tools,
        tool_dossiers,
        server_gate,
        gate_reasons,
        backend_used: selected_backend.to_string(),
        backend_fallback_used: fallback_used,
        backend_diagnostics,
    })
}

fn fallback_tool_dossier(server: &MCPServerProfile, runtime_tool: &RuntimeTool) -> ToolDossier {
    let mut evidence = source_ground_evidence(server, runtime_tool);
    evidence.push("fallback: runtime metadata + deterministic defaults".to_string());

    ToolDossier {
        name: normalize_tool_name(&runtime_tool.name),
        explanation: runtime_tool.description.clone().unwrap_or_else(|| {
            format!(
                "Execute '{}' actions for {}.",
                runtime_tool.name, server.purpose
            )
        }),
        recipe: vec![
            "Validate required inputs against runtime tool schema before execution.".to_string(),
            "Run the tool once with deterministic arguments and capture raw output.".to_string(),
            "Validate status/output shape and return concise structured result.".to_string(),
        ],
        evidence,
        confidence: 0.5,
        contract_tests: default_contract_tests(server),
        probe_inputs: ProbeInputs::default(),
        probe_input_source: ProbeInputSource::Synthesized,
    }
}

fn source_ground_evidence(server: &MCPServerProfile, runtime_tool: &RuntimeTool) -> Vec<String> {
    let mut evidence = vec![];

    if let Some(url) = &server.url {
        evidence.push(format!("runtime-url: {url}"));
    }

    if let Some(command) = &server.command {
        let mut cmd = command.clone();
        if !server.args.is_empty() {
            cmd.push(' ');
            cmd.push_str(&server.args.join(" "));
        }
        evidence.push(format!("runtime-command: {cmd}"));
    }

    if let Some(description) = &runtime_tool.description {
        evidence.push(format!("runtime-tool-description: {description}"));
    }

    for arg in &server.args {
        if arg.contains("github.com") {
            evidence.push(format!("source-candidate: {arg}"));
        }
        if arg.contains('/') && !arg.starts_with('-') {
            evidence.push(format!("package-candidate: {arg}"));
        }
    }

    if evidence.is_empty() {
        evidence.push("runtime metadata + contract test fallback".to_string());
    }

    evidence.sort();
    evidence.dedup();
    evidence
}

fn default_contract_tests(server: &MCPServerProfile) -> Vec<ToolContractTest> {
    let side_effect_optional = matches!(
        server.inferred_permission,
        PermissionLevel::ReadOnly | PermissionLevel::Unknown
    );

    vec![
        ToolContractTest {
            probe: "happy-path".to_string(),
            expected: "Produces valid output for a representative valid request.".to_string(),
            method: "Run with canonical valid inputs and assert output schema/fields.".to_string(),
            applicable: true,
        },
        ToolContractTest {
            probe: "invalid-input".to_string(),
            expected: "Returns deterministic validation or error response for malformed input."
                .to_string(),
            method: "Run with malformed/unsupported inputs and assert predictable failure path."
                .to_string(),
            applicable: true,
        },
        ToolContractTest {
            probe: "side-effect-safety".to_string(),
            expected: "Confirms dry-run or explicit confirmation guard before destructive actions."
                .to_string(),
            method: "Run safety check path first; ensure mutations are gated behind confirmation."
                .to_string(),
            applicable: !side_effect_optional,
        },
    ]
}

fn normalize_contract_tests(dossier: &mut ToolDossier, server: &MCPServerProfile) {
    for test in &mut dossier.contract_tests {
        test.probe = test.probe.trim().to_ascii_lowercase();
        test.expected = test.expected.trim().to_string();
        test.method = test.method.trim().to_string();
        if test.expected.is_empty() {
            test.expected = "Expected behavior not provided by backend.".to_string();
        }
        if test.method.is_empty() {
            test.method = "Method not provided by backend.".to_string();
        }
    }

    if !has_required_probes(&dossier.contract_tests) {
        let mut defaults = default_contract_tests(server);
        let existing = dossier
            .contract_tests
            .iter()
            .map(|test| test.probe.clone())
            .collect::<BTreeSet<_>>();
        for default in defaults.drain(..) {
            if !existing.contains(&default.probe) {
                dossier.contract_tests.push(default);
            }
        }
    }

    dossier.confidence = dossier.confidence.clamp(0.0, 1.0);
    dossier.recipe.retain(|step| !step.trim().is_empty());
    dossier.evidence.retain(|item| !item.trim().is_empty());
    dossier
        .recipe
        .iter_mut()
        .for_each(|step| *step = step.trim().to_string());
    dossier
        .evidence
        .iter_mut()
        .for_each(|item| *item = item.trim().to_string());

    if dossier.probe_input_source == ProbeInputSource::Synthesized
        && has_any_probe_inputs(&dossier.probe_inputs)
    {
        dossier.probe_input_source = ProbeInputSource::Backend;
    }
}

fn has_required_probes(tests: &[ToolContractTest]) -> bool {
    let probes = tests
        .iter()
        .map(|test| test.probe.trim().to_ascii_lowercase())
        .collect::<BTreeSet<_>>();
    probes.contains("happy-path")
        && probes.contains("invalid-input")
        && probes.contains("side-effect-safety")
}

fn has_any_probe_inputs(probe_inputs: &ProbeInputs) -> bool {
    probe_inputs.happy_path.is_some()
        || probe_inputs.invalid_input.is_some()
        || probe_inputs.side_effect_safety.is_some()
}

#[derive(Debug, Clone)]
struct ProbeFailure {
    kind: ProbeErrorKind,
    message: String,
    response_preview: Option<String>,
}

#[derive(Debug, Clone)]
struct McpToolCallOutcome {
    is_error: bool,
    details: String,
    response_preview: String,
    duration_ms: u64,
}

fn evaluate_tool_contract(
    tool: &ToolDossier,
    server: &MCPServerProfile,
    runtime_tool: Option<&RuntimeTool>,
    runtime_tools: &BTreeMap<String, RuntimeTool>,
    options: ContractTestOptions,
) -> ContractToolResult {
    let mut probes = vec![];
    let mut reasons = vec![];
    let tool_name = normalize_tool_name(&tool.name);

    let probe_map = tool
        .contract_tests
        .iter()
        .map(|test| (test.probe.trim().to_ascii_lowercase(), test))
        .collect::<BTreeMap<_, _>>();

    let required = ["happy-path", "invalid-input", "side-effect-safety"];
    let Some(runtime_tool) = runtime_tool else {
        for probe in required {
            probes.push(ContractProbeResult {
                probe: probe.to_string(),
                passed: false,
                skipped: false,
                executed: false,
                details: "Runtime tool is missing from tools/list introspection.".to_string(),
                request_args_preview: None,
                response_preview: None,
                duration_ms: None,
                error_kind: Some(ProbeErrorKind::Transport),
            });
        }
        reasons.push("Runtime tool is missing from tools/list introspection.".to_string());
        return ContractToolResult {
            tool: tool_name,
            passed: false,
            probes,
            reasons,
        };
    };

    let happy_probe_args = resolve_happy_probe_args(tool, runtime_tool);

    for probe in required {
        match probe_map.get(probe) {
            Some(test) if !test.applicable => {
                probes.push(ContractProbeResult {
                    probe: probe.to_string(),
                    passed: true,
                    skipped: true,
                    executed: false,
                    details: "Probe marked optional for this tool.".to_string(),
                    request_args_preview: None,
                    response_preview: None,
                    duration_ms: None,
                    error_kind: None,
                });
            }
            Some(test) => {
                let content_ok = !test.expected.trim().is_empty() && !test.method.trim().is_empty();
                if !content_ok {
                    probes.push(ContractProbeResult {
                        probe: probe.to_string(),
                        passed: false,
                        skipped: false,
                        executed: false,
                        details: "Probe is missing expected behavior or execution method."
                            .to_string(),
                        request_args_preview: None,
                        response_preview: None,
                        duration_ms: None,
                        error_kind: Some(ProbeErrorKind::SchemaGap),
                    });
                    reasons.push(format!("Probe '{probe}' is missing required details."));
                    continue;
                }

                let result = match probe {
                    "happy-path" => run_happy_probe(
                        server,
                        runtime_tools,
                        &tool_name,
                        &tool.name,
                        happy_probe_args.clone(),
                        options,
                    ),
                    "invalid-input" => run_invalid_probe(
                        server,
                        runtime_tool,
                        &tool_name,
                        tool,
                        &happy_probe_args,
                        options,
                    ),
                    "side-effect-safety" => run_side_effect_probe(
                        server,
                        runtime_tool,
                        &tool_name,
                        tool,
                        &happy_probe_args,
                        options,
                    ),
                    _ => unreachable!("required probe list is fixed"),
                };
                if !result.passed {
                    reasons.push(format!("Probe '{probe}' failed: {}", result.details));
                }
                probes.push(result);
            }
            None => {
                probes.push(ContractProbeResult {
                    probe: probe.to_string(),
                    passed: false,
                    skipped: false,
                    executed: false,
                    details: "Probe missing from tool dossier.".to_string(),
                    request_args_preview: None,
                    response_preview: None,
                    duration_ms: None,
                    error_kind: Some(ProbeErrorKind::SchemaGap),
                });
                reasons.push(format!("Probe '{probe}' is missing."));
            }
        }
    }

    let passed = probes.iter().all(|probe| probe.passed);

    ContractToolResult {
        tool: tool_name,
        passed,
        probes,
        reasons,
    }
}

fn run_happy_probe(
    server: &MCPServerProfile,
    runtime_tools: &BTreeMap<String, RuntimeTool>,
    tool_name: &str,
    original_tool_name: &str,
    happy_probe_args: std::result::Result<(Value, ProbeInputSource), ProbeFailure>,
    options: ContractTestOptions,
) -> ContractProbeResult {
    match happy_probe_args {
        Ok((args, source)) => {
            let baseline = execute_probe_expect_success(
                server,
                tool_name,
                "happy-path",
                args.clone(),
                source,
                options,
                false,
                &[],
            );
            if baseline.passed {
                return baseline;
            }

            let retryable_not_found = baseline.error_kind == Some(ProbeErrorKind::McpError)
                && baseline
                    .response_preview
                    .as_deref()
                    .unwrap_or_default()
                    .to_ascii_lowercase()
                    .contains("not found");
            if !retryable_not_found {
                return baseline;
            }

            let setup_calls = derive_setup_calls(runtime_tools, original_tool_name, &args);
            if setup_calls.is_empty() {
                return baseline;
            }

            let retried = execute_probe_expect_success(
                server,
                tool_name,
                "happy-path",
                args,
                source,
                options,
                false,
                &setup_calls,
            );
            if retried.passed {
                return ContractProbeResult {
                    details: format!("{} (after deterministic setup prelude).", retried.details),
                    ..retried
                };
            }
            baseline
        }
        Err(err) => probe_failure_result("happy-path", false, None, err),
    }
}

fn run_invalid_probe(
    server: &MCPServerProfile,
    runtime_tool: &RuntimeTool,
    tool_name: &str,
    tool: &ToolDossier,
    happy_probe_args: &std::result::Result<(Value, ProbeInputSource), ProbeFailure>,
    options: ContractTestOptions,
) -> ContractProbeResult {
    let resolved = resolve_invalid_probe_args(tool, runtime_tool, happy_probe_args);
    match resolved {
        Ok((args, source)) => {
            execute_probe_expect_error(server, tool_name, "invalid-input", args, source, options)
        }
        Err(err) => probe_failure_result("invalid-input", false, None, err),
    }
}

fn run_side_effect_probe(
    server: &MCPServerProfile,
    runtime_tool: &RuntimeTool,
    tool_name: &str,
    tool: &ToolDossier,
    happy_probe_args: &std::result::Result<(Value, ProbeInputSource), ProbeFailure>,
    options: ContractTestOptions,
) -> ContractProbeResult {
    if !is_likely_side_effect_tool(server, tool, runtime_tool) {
        return ContractProbeResult {
            probe: "side-effect-safety".to_string(),
            passed: true,
            skipped: true,
            executed: false,
            details: "Tool classified as read-only/unknown; side-effect probe safely skipped."
                .to_string(),
            request_args_preview: None,
            response_preview: None,
            duration_ms: None,
            error_kind: None,
        };
    }

    if options.allow_side_effects {
        let explicit = tool
            .probe_inputs
            .side_effect_safety
            .clone()
            .map(|args| (args, ProbeInputSource::Backend))
            .or_else(|| {
                happy_probe_args
                    .as_ref()
                    .ok()
                    .map(|(args, source)| (args.clone(), *source))
            });
        let Some((args, source)) = explicit else {
            return probe_failure_result(
                "side-effect-safety",
                false,
                None,
                ProbeFailure {
                    kind: ProbeErrorKind::SchemaGap,
                    message:
                        "No explicit side-effect probe args were provided and happy-path args are unavailable."
                            .to_string(),
                    response_preview: None,
                },
            );
        };
        return execute_probe_expect_success(
            server,
            tool_name,
            "side-effect-safety",
            args,
            source,
            options,
            true,
            &[],
        );
    }

    let guarded = resolve_guarded_side_effect_args(tool, runtime_tool, happy_probe_args);
    match guarded {
        Ok((args, source)) => execute_probe_expect_success(
            server,
            tool_name,
            "side-effect-safety",
            args,
            source,
            options,
            false,
            &[],
        ),
        Err(err) => probe_failure_result("side-effect-safety", false, None, err),
    }
}

#[allow(clippy::too_many_arguments)]
fn execute_probe_expect_success(
    server: &MCPServerProfile,
    tool_name: &str,
    probe: &str,
    args: Value,
    source: ProbeInputSource,
    options: ContractTestOptions,
    side_effectful: bool,
    setup_calls: &[(String, Value)],
) -> ContractProbeResult {
    let request_preview = Some(clipped_preview(&value_to_compact_json(&args), 220));
    match execute_mcp_tool_probe(server, tool_name, &args, options, setup_calls) {
        Ok(call) if !call.is_error => ContractProbeResult {
            probe: probe.to_string(),
            passed: true,
            skipped: false,
            executed: true,
            details: if side_effectful {
                format!(
                    "Probe executed successfully (source={}, side-effects allowed).",
                    probe_input_source_label(source)
                )
            } else {
                format!(
                    "Probe executed successfully (source={}).",
                    probe_input_source_label(source)
                )
            },
            request_args_preview: request_preview,
            response_preview: Some(call.response_preview),
            duration_ms: Some(call.duration_ms),
            error_kind: None,
        },
        Ok(call) => ContractProbeResult {
            probe: probe.to_string(),
            passed: false,
            skipped: false,
            executed: true,
            details: format!("Tool returned an error path: {}", call.details),
            request_args_preview: request_preview,
            response_preview: Some(call.response_preview),
            duration_ms: Some(call.duration_ms),
            error_kind: Some(ProbeErrorKind::McpError),
        },
        Err(err) => probe_failure_result(probe, true, request_preview, err),
    }
}

fn execute_probe_expect_error(
    server: &MCPServerProfile,
    tool_name: &str,
    probe: &str,
    args: Value,
    source: ProbeInputSource,
    options: ContractTestOptions,
) -> ContractProbeResult {
    let request_preview = Some(clipped_preview(&value_to_compact_json(&args), 220));
    match execute_mcp_tool_probe(server, tool_name, &args, options, &[]) {
        Ok(call) if call.is_error => ContractProbeResult {
            probe: probe.to_string(),
            passed: true,
            skipped: false,
            executed: true,
            details: format!(
                "Probe produced expected error path (source={}).",
                probe_input_source_label(source)
            ),
            request_args_preview: request_preview,
            response_preview: Some(call.response_preview),
            duration_ms: Some(call.duration_ms),
            error_kind: None,
        },
        Ok(call) => ContractProbeResult {
            probe: probe.to_string(),
            passed: false,
            skipped: false,
            executed: true,
            details: "Expected invalid-input probe to fail, but it succeeded.".to_string(),
            request_args_preview: request_preview,
            response_preview: Some(call.response_preview),
            duration_ms: Some(call.duration_ms),
            error_kind: Some(ProbeErrorKind::McpError),
        },
        Err(err) => probe_failure_result(probe, true, request_preview, err),
    }
}

fn probe_failure_result(
    probe: &str,
    executed: bool,
    request_preview: Option<String>,
    err: ProbeFailure,
) -> ContractProbeResult {
    ContractProbeResult {
        probe: probe.to_string(),
        passed: false,
        skipped: false,
        executed,
        details: err.message,
        request_args_preview: request_preview,
        response_preview: err.response_preview,
        duration_ms: None,
        error_kind: Some(err.kind),
    }
}

fn probe_input_source_label(source: ProbeInputSource) -> &'static str {
    match source {
        ProbeInputSource::Backend => "backend",
        ProbeInputSource::Synthesized => "synthesized",
    }
}

fn resolve_happy_probe_args(
    tool: &ToolDossier,
    runtime_tool: &RuntimeTool,
) -> std::result::Result<(Value, ProbeInputSource), ProbeFailure> {
    if let Some(args) = &tool.probe_inputs.happy_path {
        return Ok((args.clone(), ProbeInputSource::Backend));
    }
    let schema = runtime_tool
        .input_schema
        .as_ref()
        .ok_or_else(|| ProbeFailure {
            kind: ProbeErrorKind::SchemaGap,
            message: "Missing runtime input schema and no backend happy-path args available."
                .to_string(),
            response_preview: None,
        })?;
    let args = synthesize_happy_args(schema).ok_or_else(|| ProbeFailure {
        kind: ProbeErrorKind::SchemaGap,
        message: "Unable to synthesize deterministic happy-path args from runtime input schema."
            .to_string(),
        response_preview: None,
    })?;
    Ok((args, ProbeInputSource::Synthesized))
}

fn resolve_invalid_probe_args(
    tool: &ToolDossier,
    runtime_tool: &RuntimeTool,
    happy_probe_args: &std::result::Result<(Value, ProbeInputSource), ProbeFailure>,
) -> std::result::Result<(Value, ProbeInputSource), ProbeFailure> {
    if let Some(args) = &tool.probe_inputs.invalid_input {
        return Ok((args.clone(), ProbeInputSource::Backend));
    }
    let schema = runtime_tool
        .input_schema
        .as_ref()
        .ok_or_else(|| ProbeFailure {
            kind: ProbeErrorKind::SchemaGap,
            message: "Missing runtime input schema and no backend invalid-input args available."
                .to_string(),
            response_preview: None,
        })?;
    let fallback_happy = happy_probe_args
        .as_ref()
        .ok()
        .map(|(args, _)| args.clone())
        .or_else(|| synthesize_happy_args(schema))
        .ok_or_else(|| ProbeFailure {
            kind: ProbeErrorKind::SchemaGap,
            message: "Unable to derive base args for invalid-input synthesis.".to_string(),
            response_preview: None,
        })?;
    let invalid = synthesize_invalid_args(schema, &fallback_happy).ok_or_else(|| ProbeFailure {
        kind: ProbeErrorKind::SchemaGap,
        message: "Unable to synthesize deterministic invalid-input args from runtime schema."
            .to_string(),
        response_preview: None,
    })?;
    Ok((invalid, ProbeInputSource::Synthesized))
}

fn resolve_guarded_side_effect_args(
    tool: &ToolDossier,
    runtime_tool: &RuntimeTool,
    happy_probe_args: &std::result::Result<(Value, ProbeInputSource), ProbeFailure>,
) -> std::result::Result<(Value, ProbeInputSource), ProbeFailure> {
    if let Some(args) = &tool.probe_inputs.side_effect_safety {
        if has_non_mutating_guard(args) {
            return Ok((args.clone(), ProbeInputSource::Backend));
        }
        return Err(ProbeFailure {
            kind: ProbeErrorKind::Unsafe,
            message:
                "Provided side-effect-safety probe args do not include required non-mutating guards."
                    .to_string(),
            response_preview: None,
        });
    }

    if let Ok((happy_args, source)) = happy_probe_args
        && let Some(guarded) = apply_non_mutating_guards(happy_args)
    {
        return Ok((guarded, *source));
    }

    if let Some(schema) = &runtime_tool.input_schema
        && let Some(mut guarded) = synthesize_happy_args(schema)
        && let Some(with_guards) = apply_non_mutating_guards(&guarded)
    {
        guarded = with_guards;
        return Ok((guarded, ProbeInputSource::Synthesized));
    }

    Err(ProbeFailure {
        kind: ProbeErrorKind::Unsafe,
        message: "No safe guard path found for side-effect probe (dry_run/check/preview/noop/confirm=false/force=false).".to_string(),
        response_preview: None,
    })
}

fn derive_setup_calls(
    runtime_tools: &BTreeMap<String, RuntimeTool>,
    tool_name: &str,
    args: &Value,
) -> Vec<(String, Value)> {
    let normalized = normalize_tool_name(tool_name).to_ascii_lowercase();
    let mut calls = vec![];

    if runtime_tools.contains_key("create_entities") {
        let entity_names = collect_entity_names(args);
        if !entity_names.is_empty()
            && (normalized.contains("observation")
                || normalized.contains("relation")
                || normalized.contains("entity"))
        {
            let entities = entity_names
                .into_iter()
                .map(|name| {
                    serde_json::json!({
                        "name": name,
                        "entityType": "sample",
                        "observations": ["sample"]
                    })
                })
                .collect::<Vec<_>>();
            calls.push((
                "create_entities".to_string(),
                serde_json::json!({ "entities": entities }),
            ));
        }
    }

    calls
}

fn collect_entity_names(root: &Value) -> Vec<String> {
    let mut out = BTreeSet::new();
    collect_entity_names_recursive(root, &mut out);
    out.into_iter().collect()
}

fn collect_entity_names_recursive(value: &Value, out: &mut BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            for (key, entry) in map {
                let key_lower = key.to_ascii_lowercase();
                if matches!(key_lower.as_str(), "entityname" | "name" | "from" | "to")
                    && let Some(name) = entry.as_str()
                {
                    let trimmed = name.trim();
                    if !trimmed.is_empty() {
                        out.insert(trimmed.to_string());
                    }
                }
                collect_entity_names_recursive(entry, out);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_entity_names_recursive(item, out);
            }
        }
        _ => {}
    }
}

fn is_likely_side_effect_tool(
    server: &MCPServerProfile,
    tool: &ToolDossier,
    runtime_tool: &RuntimeTool,
) -> bool {
    if matches!(
        server.inferred_permission,
        PermissionLevel::ReadOnly | PermissionLevel::Unknown
    ) {
        return false;
    }

    let mut text = tool.name.to_ascii_lowercase();
    text.push(' ');
    text.push_str(&tool.explanation.to_ascii_lowercase());
    if let Some(description) = &runtime_tool.description {
        text.push(' ');
        text.push_str(&description.to_ascii_lowercase());
    }

    const MUTATING: &[&str] = &[
        "create",
        "update",
        "delete",
        "remove",
        "write",
        "set",
        "apply",
        "install",
        "uninstall",
        "push",
        "commit",
        "merge",
        "stop",
        "start",
        "run",
        "execute",
        "launch",
    ];
    const READ_ONLY: &[&str] = &[
        "list",
        "get",
        "read",
        "fetch",
        "query",
        "search",
        "find",
        "inspect",
        "show",
        "status",
        "snapshot",
        "screenshot",
    ];

    let mutating = MUTATING.iter().any(|token| text.contains(token));
    let read_only = READ_ONLY.iter().any(|token| text.contains(token));
    if mutating {
        return true;
    }

    match server.inferred_permission {
        PermissionLevel::Destructive => true,
        PermissionLevel::Write => !read_only,
        PermissionLevel::ReadOnly | PermissionLevel::Unknown => false,
    }
}

fn has_non_mutating_guard(args: &Value) -> bool {
    let Some(map) = args.as_object() else {
        return false;
    };

    let true_guards = ["dry_run", "dryRun", "check", "preview", "noop", "no_op"];
    if true_guards
        .iter()
        .any(|key| map.get(*key).and_then(Value::as_bool) == Some(true))
    {
        return true;
    }

    let false_guards = ["confirm", "force"];
    false_guards
        .iter()
        .any(|key| map.get(*key).and_then(Value::as_bool) == Some(false))
}

fn apply_non_mutating_guards(args: &Value) -> Option<Value> {
    let mut map = args.as_object()?.clone();
    let mut touched = false;

    for key in ["dry_run", "dryRun", "check", "preview", "noop", "no_op"] {
        if map.contains_key(key) {
            map.insert(key.to_string(), Value::Bool(true));
            touched = true;
        }
    }
    for key in ["confirm", "force"] {
        if map.contains_key(key) {
            map.insert(key.to_string(), Value::Bool(false));
            touched = true;
        }
    }

    touched.then_some(Value::Object(map))
}

fn synthesize_happy_args(schema: &Value) -> Option<Value> {
    if let Some(constant) = schema.get("const") {
        return Some(constant.clone());
    }
    if let Some(values) = schema.get("enum").and_then(Value::as_array)
        && let Some(first) = values.first()
    {
        return Some(first.clone());
    }
    if let Some(default) = schema.get("default") {
        return Some(default.clone());
    }
    if let Some(branch) = schema
        .get("anyOf")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .or_else(|| {
            schema
                .get("oneOf")
                .and_then(Value::as_array)
                .and_then(|items| items.first())
        })
        .or_else(|| {
            schema
                .get("allOf")
                .and_then(Value::as_array)
                .and_then(|items| items.first())
        })
    {
        return synthesize_happy_args(branch);
    }

    let kinds = schema_type_kinds(schema);
    if kinds.iter().any(|kind| kind == "object") || schema.get("properties").is_some() {
        let mut out = serde_json::Map::new();
        let required = schema
            .get("required")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let properties = schema
            .get("properties")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();

        for field in required {
            let Some(name) = field.as_str() else {
                continue;
            };
            let value = properties
                .get(name)
                .and_then(synthesize_happy_args)
                .unwrap_or_else(|| Value::String("sample".to_string()));
            out.insert(name.to_string(), value);
        }
        return Some(Value::Object(out));
    }

    if kinds.iter().any(|kind| kind == "array") {
        if let Some(items) = schema.get("items") {
            let value = synthesize_happy_args(items)?;
            return Some(Value::Array(vec![value]));
        }
        return Some(Value::Array(vec![]));
    }
    if kinds.iter().any(|kind| kind == "string") {
        return Some(Value::String("sample".to_string()));
    }
    if kinds.iter().any(|kind| kind == "integer") {
        return Some(Value::Number(1_i64.into()));
    }
    if kinds.iter().any(|kind| kind == "number") {
        return serde_json::Number::from_f64(1.0).map(Value::Number);
    }
    if kinds.iter().any(|kind| kind == "boolean") {
        return Some(Value::Bool(true));
    }
    if kinds.iter().any(|kind| kind == "null") {
        return Some(Value::Null);
    }

    None
}

fn synthesize_invalid_args(schema: &Value, happy_args: &Value) -> Option<Value> {
    let kinds = schema_type_kinds(schema);

    if (kinds.iter().any(|kind| kind == "object") || schema.get("properties").is_some())
        && let Some(mut obj) = happy_args.as_object().cloned()
    {
        if let Some(required) = schema.get("required").and_then(Value::as_array)
            && let Some(first_required) = required.first().and_then(Value::as_str)
            && obj.remove(first_required).is_some()
        {
            return Some(Value::Object(obj));
        }

        if let Some((name, property_schema)) = schema
            .get("properties")
            .and_then(Value::as_object)
            .and_then(|properties| {
                properties
                    .iter()
                    .next()
                    .map(|(name, schema)| (name.as_str(), schema))
            })
        {
            let wrong = wrong_type_value(schema_type_kinds(property_schema).as_slice());
            obj.insert(name.to_string(), wrong);
            return Some(Value::Object(obj));
        }
    }

    Some(wrong_type_value(kinds.as_slice()))
}

fn wrong_type_value(expected: &[String]) -> Value {
    if expected.iter().any(|kind| kind == "string") {
        return Value::Bool(true);
    }
    if expected
        .iter()
        .any(|kind| kind == "integer" || kind == "number")
    {
        return Value::String("invalid".to_string());
    }
    if expected.iter().any(|kind| kind == "boolean") {
        return Value::String("invalid".to_string());
    }
    if expected.iter().any(|kind| kind == "array") {
        return Value::Object(serde_json::Map::new());
    }
    if expected.iter().any(|kind| kind == "object") {
        return Value::String("invalid".to_string());
    }
    Value::Null
}

fn schema_type_kinds(schema: &Value) -> Vec<String> {
    match schema.get("type") {
        Some(Value::String(kind)) => vec![kind.to_ascii_lowercase()],
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .map(|kind| kind.to_ascii_lowercase())
            .collect::<Vec<_>>(),
        _ => vec![],
    }
}

fn execute_mcp_tool_probe(
    server: &MCPServerProfile,
    tool_name: &str,
    args: &Value,
    options: ContractTestOptions,
    setup_calls: &[(String, Value)],
) -> std::result::Result<McpToolCallOutcome, ProbeFailure> {
    let mut last_err: Option<ProbeFailure> = None;
    for _attempt in 0..=options.probe_retries {
        match execute_mcp_tool_probe_once(
            server,
            tool_name,
            args,
            options.probe_timeout_seconds,
            setup_calls,
        ) {
            Ok(outcome) => return Ok(outcome),
            Err(err) => last_err = Some(err),
        }
    }
    Err(last_err.unwrap_or(ProbeFailure {
        kind: ProbeErrorKind::Transport,
        message: "Probe execution failed without detailed error.".to_string(),
        response_preview: None,
    }))
}

fn execute_mcp_tool_probe_once(
    server: &MCPServerProfile,
    tool_name: &str,
    args: &Value,
    timeout_seconds: u64,
    setup_calls: &[(String, Value)],
) -> std::result::Result<McpToolCallOutcome, ProbeFailure> {
    let command = server.command.as_deref().ok_or_else(|| ProbeFailure {
        kind: ProbeErrorKind::Transport,
        message: "MCP server has no executable command.".to_string(),
        response_preview: None,
    })?;

    let mut child = Command::new(command)
        .args(&server.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| ProbeFailure {
            kind: ProbeErrorKind::Transport,
            message: format!("Failed to spawn MCP command '{command}': {err}"),
            response_preview: None,
        })?;

    let mut stdin = child.stdin.take().ok_or_else(|| ProbeFailure {
        kind: ProbeErrorKind::Transport,
        message: "Failed to open MCP stdin.".to_string(),
        response_preview: None,
    })?;
    let stdout = child.stdout.take().ok_or_else(|| ProbeFailure {
        kind: ProbeErrorKind::Transport,
        message: "Failed to open MCP stdout.".to_string(),
        response_preview: None,
    })?;
    let mut stderr = child.stderr.take().ok_or_else(|| ProbeFailure {
        kind: ProbeErrorKind::Transport,
        message: "Failed to open MCP stderr.".to_string(),
        response_preview: None,
    })?;

    let (tx, rx) = mpsc::channel::<String>();
    let reader_handle = thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    let _ = tx.send(line.trim_end().to_string());
                }
                Err(_) => break,
            }
        }
    });

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
    let started = Instant::now();
    let deadline = started + Duration::from_secs(timeout_seconds.max(1));
    let mut buffered = BTreeMap::<i64, Value>::new();

    writeln!(stdin, "{init}").map_err(|err| ProbeFailure {
        kind: ProbeErrorKind::Transport,
        message: format!("Failed to write MCP initialize request: {err}"),
        response_preview: None,
    })?;
    let _ = wait_for_jsonrpc_response(1, &rx, &mut buffered, deadline);

    let mut next_setup_id = 100_i64;
    for (setup_tool, setup_args) in setup_calls {
        let setup_id = next_setup_id;
        next_setup_id += 1;
        let setup_call = serde_json::json!({
            "jsonrpc":"2.0",
            "id":setup_id,
            "method":"tools/call",
            "params":{"name":setup_tool,"arguments":setup_args}
        });
        writeln!(stdin, "{setup_call}").map_err(|err| ProbeFailure {
            kind: ProbeErrorKind::Transport,
            message: format!(
                "Failed to write setup tools/call for '{}': {err}",
                setup_tool
            ),
            response_preview: None,
        })?;
        let setup_response = wait_for_jsonrpc_response(setup_id, &rx, &mut buffered, deadline)
            .map_err(|mut err| {
                err.message = format!(
                    "Setup call '{}' failed before '{}' execution: {}",
                    setup_tool, tool_name, err.message
                );
                err
            })?;
        if setup_response.get("error").is_some()
            || setup_response
                .get("result")
                .and_then(|v| v.get("isError"))
                .and_then(Value::as_bool)
                .unwrap_or(false)
            || setup_response
                .get("result")
                .and_then(|v| v.get("is_error"))
                .and_then(Value::as_bool)
                .unwrap_or(false)
        {
            drop(stdin);
            let _ = child.kill();
            let _ = child.wait();
            return Err(ProbeFailure {
                kind: ProbeErrorKind::McpError,
                message: format!(
                    "Setup probe call for tool '{}' failed before '{}' execution.",
                    setup_tool, tool_name
                ),
                response_preview: Some(clipped_preview(
                    &value_to_compact_json(&setup_response),
                    260,
                )),
            });
        }
    }

    let target_call_id = 2_i64;
    let call = serde_json::json!({
        "jsonrpc":"2.0",
        "id":target_call_id,
        "method":"tools/call",
        "params":{"name":tool_name,"arguments":args}
    });
    writeln!(stdin, "{call}").map_err(|err| ProbeFailure {
        kind: ProbeErrorKind::Transport,
        message: format!("Failed to write MCP tools/call request: {err}"),
        response_preview: None,
    })?;
    let response = wait_for_jsonrpc_response(target_call_id, &rx, &mut buffered, deadline)?;

    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();
    let _ = reader_handle.join();
    let mut _stderr_text = String::new();
    let _ = stderr.read_to_string(&mut _stderr_text);
    let duration_ms = started.elapsed().as_millis().min(u64::MAX as u128) as u64;

    let response_preview = clipped_preview(&value_to_compact_json(&response), 260);
    if let Some(error) = response.get("error") {
        return Ok(McpToolCallOutcome {
            is_error: true,
            details: format!(
                "JSON-RPC error: {}",
                clipped_preview(&value_to_compact_json(error), 180)
            ),
            response_preview,
            duration_ms,
        });
    }

    let Some(result) = response.get("result") else {
        return Err(ProbeFailure {
            kind: ProbeErrorKind::Transport,
            message: "MCP response missing `result` payload for tools/call.".to_string(),
            response_preview: Some(response_preview),
        });
    };

    let is_error = result
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || result
            .get("is_error")
            .and_then(Value::as_bool)
            .unwrap_or(false);
    let details = if is_error {
        "Tool returned result.isError=true".to_string()
    } else {
        "Tool returned success result.".to_string()
    };

    Ok(McpToolCallOutcome {
        is_error,
        details,
        response_preview,
        duration_ms,
    })
}

fn wait_for_jsonrpc_response(
    expected_id: i64,
    rx: &mpsc::Receiver<String>,
    buffered: &mut BTreeMap<i64, Value>,
    deadline: Instant,
) -> std::result::Result<Value, ProbeFailure> {
    if let Some(found) = buffered.remove(&expected_id) {
        return Ok(found);
    }

    loop {
        let now = Instant::now();
        if now >= deadline {
            return Err(ProbeFailure {
                kind: ProbeErrorKind::Timeout,
                message: format!("Timed out waiting for MCP response id={expected_id}."),
                response_preview: None,
            });
        }
        let remaining = deadline.saturating_duration_since(now);
        let wait = remaining.min(Duration::from_millis(200));
        match rx.recv_timeout(wait) {
            Ok(line) => {
                let Ok(value) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                let Some(id) = value.get("id").and_then(Value::as_i64) else {
                    continue;
                };
                if id == expected_id {
                    return Ok(value);
                }
                buffered.insert(id, value);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(ProbeFailure {
                    kind: ProbeErrorKind::Transport,
                    message: format!(
                        "MCP process closed before response id={expected_id} was received."
                    ),
                    response_preview: None,
                });
            }
        }
    }
}

fn value_to_compact_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "<non-serializable>".to_string())
}

fn generate_tool_dossiers(
    backend_name: ConvertBackendName,
    server: &MCPServerProfile,
    runtime_tools: &[RuntimeTool],
    chunk_size: usize,
    timeout_seconds: u64,
) -> Result<Vec<ToolDossier>> {
    let chunks = runtime_tools
        .chunks(chunk_size.max(1))
        .map(|chunk| chunk.to_vec())
        .collect::<Vec<_>>();

    let backend = backend_by_name(backend_name, timeout_seconds);
    let schema = tool_dossier_chunk_schema();

    let mut out = vec![];
    for chunk in chunks {
        let prompt = build_tool_chunk_prompt(server, &chunk);
        let raw = backend.explain_tool_chunk(&prompt, &schema)?;
        let parsed = parse_backend_chunk_response(&raw)?;

        for mut dossier in parsed.tool_dossiers {
            dossier.name = normalize_tool_name(&dossier.name);
            if dossier.recipe.is_empty() {
                dossier.recipe = vec![
                    "Validate inputs against runtime schema.".to_string(),
                    "Execute tool with deterministic arguments.".to_string(),
                    "Verify output/error contract and summarize outcome.".to_string(),
                ];
            }
            if dossier.contract_tests.is_empty() {
                dossier.contract_tests = default_contract_tests(server);
            }
            if dossier.evidence.is_empty() {
                let runtime_tool = RuntimeTool {
                    name: dossier.name.clone(),
                    description: None,
                    input_schema: None,
                };
                dossier.evidence = source_ground_evidence(server, &runtime_tool);
            }
            dossier.probe_input_source = if has_any_probe_inputs(&dossier.probe_inputs) {
                ProbeInputSource::Backend
            } else {
                ProbeInputSource::Synthesized
            };
            out.push(dossier);
        }
    }

    Ok(out)
}

fn build_tool_chunk_prompt(server: &MCPServerProfile, tools: &[RuntimeTool]) -> String {
    let tools_json = serde_json::to_string_pretty(tools).unwrap_or_else(|_| "[]".to_string());
    format!(
        "You are generating deterministic tool dossiers for MCP -> skill conversion.\n\
Return only JSON matching the schema.\n\
Do not invent tool names. Use names exactly from runtime_tools.\n\
\n\
Server name: {}\n\
Server purpose: {}\n\
Runtime tools:\n{}\n\
\n\
Requirements per tool:\n\
- explanation: concise behavior summary\n\
- recipe: deterministic steps to execute and verify the tool\n\
- evidence: runtime/source grounding references\n\
- confidence: float between 0 and 1\n\
- contract_tests must include probes: happy-path, invalid-input, side-effect-safety\n\
- probe_inputs: optional deterministic request args for happy-path/invalid-input/side-effect-safety\n\
- probe_input_source: backend if probe_inputs came from backend, otherwise synthesized\n",
        server.name, server.purpose, tools_json
    )
}

fn tool_dossier_chunk_schema() -> String {
    r#"{
  \"type\": \"object\",
  \"additionalProperties\": false,
  \"required\": [\"tool_dossiers\"],
  \"properties\": {
    \"tool_dossiers\": {
      \"type\": \"array\",
      \"items\": {
        \"type\": \"object\",
        \"additionalProperties\": false,
        \"required\": [\"name\", \"explanation\", \"recipe\", \"evidence\", \"confidence\", \"contract_tests\"],
        \"properties\": {
          \"name\": { \"type\": \"string\" },
          \"explanation\": { \"type\": \"string\" },
          \"recipe\": { \"type\": \"array\", \"items\": { \"type\": \"string\" } },
          \"evidence\": { \"type\": \"array\", \"items\": { \"type\": \"string\" } },
          \"confidence\": { \"type\": \"number\" },
          \"probe_inputs\": {
            \"type\": \"object\",
            \"additionalProperties\": false,
            \"properties\": {
              \"happy_path\": { \"type\": \"object\" },
              \"invalid_input\": { \"type\": \"object\" },
              \"side_effect_safety\": { \"type\": \"object\" }
            }
          },
          \"probe_input_source\": { \"type\": \"string\", \"enum\": [\"backend\", \"synthesized\"] },
          \"contract_tests\": {
            \"type\": \"array\",
            \"items\": {
              \"type\": \"object\",
              \"additionalProperties\": false,
              \"required\": [\"probe\", \"expected\", \"method\", \"applicable\"],
              \"properties\": {
                \"probe\": { \"type\": \"string\" },
                \"expected\": { \"type\": \"string\" },
                \"method\": { \"type\": \"string\" },
                \"applicable\": { \"type\": \"boolean\" }
              }
            }
          }
        }
      }
    }
  }
}"#
        .to_string()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct BackendChunkResponse {
    tool_dossiers: Vec<ToolDossier>,
}

fn parse_backend_chunk_response(raw: &str) -> Result<BackendChunkResponse> {
    let trimmed = raw.trim();
    let response: BackendChunkResponse = serde_json::from_str(trimmed).with_context(|| {
        format!(
            "Backend response is not valid dossier JSON: {}",
            clipped_preview(trimmed, 300)
        )
    })?;
    if response.tool_dossiers.is_empty() {
        bail!("Backend response contained no tool_dossiers.");
    }
    Ok(response)
}

trait AgentBackend {
    #[allow(dead_code)]
    fn discover_tools_dossier(&self, prompt: &str, schema_json: &str) -> Result<String>;
    fn explain_tool_chunk(&self, prompt: &str, schema_json: &str) -> Result<String>;
    fn health_check(&self) -> BackendHealthStatus;
    fn backend_name(&self) -> ConvertBackendName;
}

#[derive(Debug, Clone)]
struct CodexBackend {
    command: String,
    timeout_seconds: u64,
}

impl AgentBackend for CodexBackend {
    fn discover_tools_dossier(&self, prompt: &str, schema_json: &str) -> Result<String> {
        invoke_codex_structured_with_timeout(
            &self.command,
            prompt,
            schema_json,
            self.timeout_seconds,
        )
    }

    fn explain_tool_chunk(&self, prompt: &str, schema_json: &str) -> Result<String> {
        invoke_codex_structured_with_timeout(
            &self.command,
            prompt,
            schema_json,
            self.timeout_seconds,
        )
    }

    fn health_check(&self) -> BackendHealthStatus {
        command_health(self.backend_name(), &self.command)
    }

    fn backend_name(&self) -> ConvertBackendName {
        ConvertBackendName::Codex
    }
}

#[derive(Debug, Clone)]
struct ClaudeBackend {
    command: String,
    timeout_seconds: u64,
}

impl AgentBackend for ClaudeBackend {
    fn discover_tools_dossier(&self, prompt: &str, schema_json: &str) -> Result<String> {
        invoke_claude_structured_with_timeout(
            &self.command,
            prompt,
            schema_json,
            self.timeout_seconds,
        )
    }

    fn explain_tool_chunk(&self, prompt: &str, schema_json: &str) -> Result<String> {
        invoke_claude_structured_with_timeout(
            &self.command,
            prompt,
            schema_json,
            self.timeout_seconds,
        )
    }

    fn health_check(&self) -> BackendHealthStatus {
        command_health(self.backend_name(), &self.command)
    }

    fn backend_name(&self) -> ConvertBackendName {
        ConvertBackendName::Claude
    }
}

fn codex_backend() -> CodexBackend {
    CodexBackend {
        command: std::env::var("DISTILL_CONVERT_CODEX_COMMAND")
            .unwrap_or_else(|_| "codex".to_string()),
        timeout_seconds: DEFAULT_BACKEND_TIMEOUT_SECONDS,
    }
}

fn claude_backend() -> ClaudeBackend {
    ClaudeBackend {
        command: std::env::var("DISTILL_CONVERT_CLAUDE_COMMAND")
            .unwrap_or_else(|_| "claude".to_string()),
        timeout_seconds: DEFAULT_BACKEND_TIMEOUT_SECONDS,
    }
}

fn backend_by_name(name: ConvertBackendName, timeout_seconds: u64) -> Box<dyn AgentBackend> {
    match name {
        ConvertBackendName::Codex => Box::new(CodexBackend {
            command: std::env::var("DISTILL_CONVERT_CODEX_COMMAND")
                .unwrap_or_else(|_| "codex".to_string()),
            timeout_seconds,
        }),
        ConvertBackendName::Claude => Box::new(ClaudeBackend {
            command: std::env::var("DISTILL_CONVERT_CLAUDE_COMMAND")
                .unwrap_or_else(|_| "claude".to_string()),
            timeout_seconds,
        }),
    }
}

fn command_health(name: ConvertBackendName, command: &str) -> BackendHealthStatus {
    let checks = [["--version"], ["-v"], ["version"]];
    let mut diagnostics = vec![];

    for args in checks {
        match Command::new(command)
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
        {
            Ok(status) if status.success() => {
                diagnostics.push(format!("{} responded to '{}'.", command, args.join(" ")));
                return BackendHealthStatus {
                    backend: name,
                    available: true,
                    diagnostics,
                };
            }
            Ok(status) => {
                diagnostics.push(format!(
                    "{} '{}' exited with status {}.",
                    command,
                    args.join(" "),
                    status
                ));
            }
            Err(err) => {
                diagnostics.push(format!("{} '{}' failed: {err}", command, args.join(" ")));
            }
        }
    }

    BackendHealthStatus {
        backend: name,
        available: false,
        diagnostics,
    }
}

fn prepare_backend_context(options: &ConvertV3Options) -> Result<BackendContext> {
    let health = backend_health_report(&options.backend_config);
    let selection = select_backend(&health, options)?;
    Ok(BackendContext { selection, health })
}

fn select_backend(
    health: &ConvertBackendHealthReport,
    options: &ConvertV3Options,
) -> Result<BackendSelection> {
    let by_backend = health
        .statuses
        .iter()
        .map(|status| (status.backend, status.available))
        .collect::<BTreeMap<_, _>>();
    let codex_available = *by_backend.get(&ConvertBackendName::Codex).unwrap_or(&false);
    let claude_available = *by_backend
        .get(&ConvertBackendName::Claude)
        .unwrap_or(&false);

    let available_ordered = [ConvertBackendName::Codex, ConvertBackendName::Claude]
        .into_iter()
        .filter(|backend| *by_backend.get(backend).unwrap_or(&false))
        .collect::<Vec<_>>();

    if let Some(explicit) = options.backend {
        if !*by_backend.get(&explicit).unwrap_or(&false) {
            bail!(
                "Requested backend '{}' is unavailable. Install/configure it, or rerun with --backend-auto. Diagnostics: {}",
                explicit,
                health
                    .statuses
                    .iter()
                    .map(|status| format!("{}: {}", status.backend, status.diagnostics.join(" | ")))
                    .collect::<Vec<_>>()
                    .join(" || ")
            );
        }
        return Ok(BackendSelection {
            selected: explicit,
            fallback: None,
            auto_mode: false,
            diagnostics: vec![format!("Using explicit backend override '{}'.", explicit)],
        });
    }

    let preferred = match options.backend_config.preference {
        ConvertBackendPreference::Codex => Some(ConvertBackendName::Codex),
        ConvertBackendPreference::Claude => Some(ConvertBackendName::Claude),
        ConvertBackendPreference::Auto => None,
    };

    if let Some(pref) = preferred
        && *by_backend.get(&pref).unwrap_or(&false)
    {
        return Ok(BackendSelection {
            selected: pref,
            fallback: None,
            auto_mode: false,
            diagnostics: vec![format!("Using configured backend preference '{}'.", pref)],
        });
    }

    if let Some(selected) = available_ordered.first().copied() {
        let fallback = available_ordered
            .iter()
            .copied()
            .find(|backend| *backend != selected);
        let reason = if options.backend_config.preference == ConvertBackendPreference::Auto {
            "Auto-selected first available backend (codex, then claude)."
        } else {
            "Configured backend preference unavailable; auto-selected first available backend."
        };
        return Ok(BackendSelection {
            selected,
            fallback: if options.backend_auto { fallback } else { None },
            auto_mode: options.backend_auto,
            diagnostics: vec![reason.to_string()],
        });
    }

    let mut guidance = vec![
        "No supported backend is installed. Install Codex CLI (`codex`) or Claude Code CLI (`claude`)."
            .to_string(),
        "Then rerun with --backend codex|claude or keep --backend-auto.".to_string(),
    ];
    if !codex_available {
        guidance.push("Codex backend check failed.".to_string());
    }
    if !claude_available {
        guidance.push("Claude backend check failed.".to_string());
    }
    guidance.extend(health.statuses.iter().map(|status| {
        format!(
            "{} diagnostics: {}",
            status.backend,
            status.diagnostics.join(" | ")
        )
    }));

    bail!(guidance.join(" "))
}

fn invoke_codex_structured_with_timeout(
    command: &str,
    prompt: &str,
    schema_json: &str,
    timeout_seconds: u64,
) -> Result<String> {
    let schema_path = create_temp_file_path("distill-convert-v3-codex-schema", "json")?;
    let output_path = create_temp_file_path("distill-convert-v3-codex-output", "txt")?;
    fs::write(&schema_path, schema_json)
        .with_context(|| format!("Failed to write {}", schema_path.display()))?;

    let output = run_command_with_timeout(
        Command::new(command)
            .args([
                "exec",
                "--ephemeral",
                "--output-schema",
                schema_path.to_string_lossy().as_ref(),
                "--output-last-message",
                output_path.to_string_lossy().as_ref(),
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("Failed to spawn `{command} exec`"))?,
        prompt.as_bytes(),
        timeout_seconds,
    );

    let result = match output {
        Ok(output) if output.status.success() => {
            let stdout = String::from_utf8(output.stdout).unwrap_or_default();
            fs::read_to_string(&output_path)
                .ok()
                .filter(|text| !text.trim().is_empty())
                .unwrap_or(stdout)
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let message = format!(
                "Codex backend failed with status {}: {}",
                output.status,
                clipped_preview(stderr.trim(), 220)
            );
            cleanup_temp_files(&[schema_path, output_path]);
            bail!(message);
        }
        Err(err) => {
            cleanup_temp_files(&[schema_path, output_path]);
            return Err(err);
        }
    };

    cleanup_temp_files(&[schema_path, output_path]);
    Ok(result)
}

fn invoke_claude_structured_with_timeout(
    command: &str,
    prompt: &str,
    schema_json: &str,
    timeout_seconds: u64,
) -> Result<String> {
    let full_prompt = format!(
        "Return ONLY JSON matching this schema:\n{}\n\nPrompt:\n{}",
        schema_json, prompt
    );

    let output = run_command_with_timeout(
        Command::new(command)
            .args([
                "--print",
                "--no-session-persistence",
                "--output-format",
                "json",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("Failed to spawn `{command}`"))?,
        full_prompt.as_bytes(),
        timeout_seconds,
    )?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "Claude backend failed with status {}: {}",
            output.status,
            clipped_preview(stderr.trim(), 220)
        );
    }

    let stdout = String::from_utf8(output.stdout).unwrap_or_default();
    extract_claude_json_payload(&stdout).with_context(|| {
        format!(
            "Claude response did not contain valid JSON payload. Output preview: {}",
            clipped_preview(stdout.trim(), 260)
        )
    })
}

fn run_command_with_timeout(
    mut child: std::process::Child,
    stdin_payload: &[u8],
    timeout_seconds: u64,
) -> Result<std::process::Output> {
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(stdin_payload)
            .context("Failed to write backend prompt to stdin")?;
    }

    let timeout = Duration::from_secs(timeout_seconds.max(1));
    let deadline = Instant::now() + timeout;

    loop {
        if let Some(status) = child
            .try_wait()
            .context("Failed while waiting for backend process")?
        {
            let mut stdout = vec![];
            let mut stderr = vec![];
            if let Some(mut out) = child.stdout.take() {
                let _ = std::io::Read::read_to_end(&mut out, &mut stdout);
            }
            if let Some(mut err) = child.stderr.take() {
                let _ = std::io::Read::read_to_end(&mut err, &mut stderr);
            }
            return Ok(std::process::Output {
                status,
                stdout,
                stderr,
            });
        }

        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!(
                "Backend command timed out after {} seconds",
                timeout_seconds.max(1)
            );
        }

        thread::sleep(Duration::from_millis(30));
    }
}

fn extract_claude_json_payload(stdout: &str) -> Result<String> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        bail!("Claude output is empty.");
    }

    if let Ok(value) = serde_json::from_str::<Value>(trimmed)
        && value.get("tool_dossiers").is_some()
    {
        return Ok(trimmed.to_string());
    }

    let envelope: Value =
        serde_json::from_str(trimmed).context("Claude output is not valid JSON envelope")?;

    if let Some(text) = envelope
        .get("output")
        .and_then(Value::as_str)
        .or_else(|| envelope.get("text").and_then(Value::as_str))
        .or_else(|| envelope.get("completion").and_then(Value::as_str))
        && let Some(json_payload) = extract_embedded_json(text)
    {
        return Ok(json_payload);
    }

    if let Some(message) = envelope.get("message") {
        if let Some(text) = message.get("text").and_then(Value::as_str)
            && let Some(json_payload) = extract_embedded_json(text)
        {
            return Ok(json_payload);
        }

        if let Some(content) = message.get("content") {
            if let Some(text) = content.as_str()
                && let Some(json_payload) = extract_embedded_json(text)
            {
                return Ok(json_payload);
            }
            if let Some(items) = content.as_array() {
                for item in items {
                    if let Some(text) = item.get("text").and_then(Value::as_str)
                        && let Some(json_payload) = extract_embedded_json(text)
                    {
                        return Ok(json_payload);
                    }
                }
            }
        }
    }

    if let Some(items) = envelope.get("content").and_then(Value::as_array) {
        for item in items {
            if let Some(text) = item.get("text").and_then(Value::as_str)
                && let Some(json_payload) = extract_embedded_json(text)
            {
                return Ok(json_payload);
            }
        }
    }

    bail!("No JSON payload found in Claude envelope.")
}

fn extract_embedded_json(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if serde_json::from_str::<Value>(trimmed).is_ok() {
        return Some(trimmed.to_string());
    }

    let no_fence = trimmed
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();

    if serde_json::from_str::<Value>(no_fence).is_ok() {
        return Some(no_fence.to_string());
    }

    let start = no_fence.find('{')?;
    let end = no_fence.rfind('}')?;
    if start >= end {
        return None;
    }
    let candidate = &no_fence[start..=end];
    if serde_json::from_str::<Value>(candidate).is_ok() {
        return Some(candidate.to_string());
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn write_executable(path: &Path, body: &str) {
        fs::write(path, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    fn health_report(codex: bool, claude: bool) -> ConvertBackendHealthReport {
        ConvertBackendHealthReport {
            checked_at: Utc::now(),
            statuses: vec![
                BackendHealthStatus {
                    backend: ConvertBackendName::Codex,
                    available: codex,
                    diagnostics: vec![],
                },
                BackendHealthStatus {
                    backend: ConvertBackendName::Claude,
                    available: claude,
                    diagnostics: vec![],
                },
            ],
        }
    }

    #[test]
    fn selector_uses_explicit_backend_when_available() {
        let options = ConvertV3Options {
            backend: Some(ConvertBackendName::Claude),
            backend_auto: true,
            backend_config: ConvertBackendConfig::default(),
        };
        let selection = select_backend(&health_report(true, true), &options).unwrap();
        assert_eq!(selection.selected, ConvertBackendName::Claude);
        assert_eq!(selection.fallback, None);
        assert!(!selection.auto_mode);
    }

    #[test]
    fn selector_prefers_config_backend_when_installed() {
        let options = ConvertV3Options {
            backend: None,
            backend_auto: true,
            backend_config: ConvertBackendConfig {
                preference: ConvertBackendPreference::Claude,
                timeout_seconds: 10,
                chunk_size: 2,
            },
        };
        let selection = select_backend(&health_report(true, true), &options).unwrap();
        assert_eq!(selection.selected, ConvertBackendName::Claude);
        assert_eq!(selection.fallback, None);
        assert!(!selection.auto_mode);
    }

    #[test]
    fn selector_auto_picks_codex_then_claude() {
        let options = ConvertV3Options::default();
        let selection = select_backend(&health_report(true, true), &options).unwrap();
        assert_eq!(selection.selected, ConvertBackendName::Codex);
        assert_eq!(selection.fallback, Some(ConvertBackendName::Claude));
        assert!(selection.auto_mode);
    }

    #[test]
    fn selector_fails_when_no_backend_is_available() {
        let options = ConvertV3Options::default();
        let err = select_backend(&health_report(false, false), &options).unwrap_err();
        let message = format!("{err:#}");
        assert!(message.contains("No supported backend is installed"));
    }

    #[test]
    fn parse_backend_chunk_response_is_strict() {
        let raw = r#"{
  "tool_dossiers": [
    {
      "name": "build_run_sim",
      "explanation": "Build and run",
      "recipe": ["step one"],
      "evidence": ["runtime"],
      "confidence": 0.9,
      "contract_tests": [
        {
          "probe": "happy-path",
          "expected": "works",
          "method": "run",
          "applicable": true
        }
      ]
    }
  ]
}"#;
        let parsed = parse_backend_chunk_response(raw).unwrap();
        assert_eq!(parsed.tool_dossiers.len(), 1);
        assert_eq!(parsed.tool_dossiers[0].name, "build_run_sim");
    }

    #[test]
    fn gate_blocks_when_required_tool_dossier_missing() {
        let server = MCPServerProfile {
            id: "fixture:xcode".to_string(),
            name: "xcode".to_string(),
            source_label: "fixture".to_string(),
            source_path: PathBuf::from("/tmp/settings.json"),
            purpose: "Xcode workflows".to_string(),
            command: Some("mock".to_string()),
            args: vec![],
            url: None,
            env_keys: vec![],
            declared_tool_count: 1,
            permission_hints: vec![],
            inferred_permission: PermissionLevel::Write,
            recommendation: ConversionRecommendation::Hybrid,
            recommendation_reason: "write".to_string(),
        };
        let runtime = RuntimeTool {
            name: "build".to_string(),
            description: None,
        };
        let mut fallback = fallback_tool_dossier(&server, &runtime);
        fallback.recipe.clear();
        normalize_contract_tests(&mut fallback, &server);
        assert!(fallback.recipe.is_empty());
        assert!(has_required_probes(&fallback.contract_tests));
    }

    #[test]
    fn extract_claude_payload_from_envelope_content() {
        let envelope = r#"{
  "message": {
    "content": [
      {
        "type": "text",
        "text": "```json\\n{\"tool_dossiers\":[{\"name\":\"navigate\",\"explanation\":\"Open\",\"recipe\":[\"a\"],\"evidence\":[\"runtime\"],\"confidence\":0.7,\"contract_tests\":[{\"probe\":\"happy-path\",\"expected\":\"ok\",\"method\":\"run\",\"applicable\":true},{\"probe\":\"invalid-input\",\"expected\":\"error\",\"method\":\"run\",\"applicable\":true},{\"probe\":\"side-effect-safety\",\"expected\":\"confirm\",\"method\":\"run\",\"applicable\":true}]}]}\\n```"
      }
    ]
  }
}"#;
        let payload = extract_claude_json_payload(envelope).unwrap();
        assert!(payload.contains("tool_dossiers"));
        let parsed = parse_backend_chunk_response(&payload).unwrap();
        assert_eq!(parsed.tool_dossiers.len(), 1);
    }

    #[test]
    fn contract_test_reports_missing_probe() {
        let tool = ToolDossier {
            name: "navigate".to_string(),
            explanation: "Open url".to_string(),
            recipe: vec!["run".to_string()],
            evidence: vec!["runtime".to_string()],
            confidence: 0.9,
            contract_tests: vec![ToolContractTest {
                probe: "happy-path".to_string(),
                expected: "ok".to_string(),
                method: "run".to_string(),
                applicable: true,
            }],
            probe_inputs: ProbeInputs::default(),
            probe_input_source: ProbeInputSource::Synthesized,
        };

        let server = MCPServerProfile {
            id: "custom:playwright".to_string(),
            name: "playwright".to_string(),
            source_label: "custom".to_string(),
            source_path: PathBuf::from("/tmp/settings.json"),
            purpose: "browser".to_string(),
            command: None,
            args: vec![],
            url: None,
            env_keys: vec![],
            declared_tool_count: 1,
            permission_hints: vec![],
            inferred_permission: PermissionLevel::ReadOnly,
            recommendation: ConversionRecommendation::Hybrid,
            recommendation_reason: "read-only".to_string(),
        };
        let runtime_tool = RuntimeTool {
            name: "navigate".to_string(),
            description: Some("navigate".to_string()),
            input_schema: Some(serde_json::json!({
                "type":"object",
                "required":["query"],
                "properties":{"query":{"type":"string"}}
            })),
        };
        let mut runtime_tools = BTreeMap::new();
        runtime_tools.insert("navigate".to_string(), runtime_tool.clone());

        let result = evaluate_tool_contract(
            &tool,
            &server,
            Some(&runtime_tool),
            &runtime_tools,
            ContractTestOptions::default(),
        );
        assert!(!result.passed);
        assert!(
            result
                .reasons
                .iter()
                .any(|reason| reason.contains("invalid-input"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn codex_backend_reports_failure_status() {
        let dir = tempfile::tempdir().unwrap();
        let codex = dir.path().join("mock-codex-fail.sh");
        write_executable(
            &codex,
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo v; exit 0; fi\nexit 42\n",
        );
        let err = invoke_codex_structured_with_timeout(
            codex.to_str().unwrap(),
            "prompt",
            &tool_dossier_chunk_schema(),
            3,
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("failed with status"));
    }

    #[cfg(unix)]
    #[test]
    fn codex_backend_invalid_json_is_rejected_by_parser() {
        let dir = tempfile::tempdir().unwrap();
        let codex = dir.path().join("mock-codex-invalid.sh");
        write_executable(
            &codex,
            r#"#!/bin/sh
if [ "$1" = "--version" ] || [ "$1" = "-v" ] || [ "$1" = "version" ]; then
  echo "mock-codex"
  exit 0
fi
last=""
while [ $# -gt 0 ]; do
  case "$1" in
    --output-last-message|-o) last="$2"; shift 2 ;;
    *) shift ;;
  esac
done
cat > /dev/null
printf '%s' 'not-json' > "$last"
"#,
        );
        let raw = invoke_codex_structured_with_timeout(
            codex.to_str().unwrap(),
            "prompt",
            &tool_dossier_chunk_schema(),
            3,
        )
        .unwrap();
        let err = parse_backend_chunk_response(&raw).unwrap_err();
        assert!(format!("{err:#}").contains("not valid dossier JSON"));
    }

    #[cfg(unix)]
    #[test]
    fn claude_backend_successfully_extracts_envelope_payload() {
        let dir = tempfile::tempdir().unwrap();
        let claude = dir.path().join("mock-claude-success.sh");
        write_executable(
            &claude,
            r#"#!/bin/sh
if [ "$1" = "--version" ] || [ "$1" = "-v" ] || [ "$1" = "version" ]; then
  echo "mock-claude"
  exit 0
fi
cat > /dev/null
cat <<'JSON'
{"message":{"content":[{"type":"text","text":"{\"tool_dossiers\":[{\"name\":\"execute\",\"explanation\":\"Run execute\",\"recipe\":[\"validate\",\"run\",\"verify\"],\"evidence\":[\"runtime\"],\"confidence\":0.9,\"contract_tests\":[{\"probe\":\"happy-path\",\"expected\":\"ok\",\"method\":\"run\",\"applicable\":true},{\"probe\":\"invalid-input\",\"expected\":\"error\",\"method\":\"run\",\"applicable\":true},{\"probe\":\"side-effect-safety\",\"expected\":\"confirm\",\"method\":\"run\",\"applicable\":true}]}]}"}]}}
JSON
"#,
        );

        let raw = invoke_claude_structured_with_timeout(
            claude.to_str().unwrap(),
            "prompt",
            &tool_dossier_chunk_schema(),
            3,
        )
        .unwrap();
        let parsed = parse_backend_chunk_response(&raw).unwrap();
        assert_eq!(parsed.tool_dossiers.len(), 1);
        assert_eq!(parsed.tool_dossiers[0].name, "execute");
    }

    #[cfg(unix)]
    #[test]
    fn claude_backend_invalid_envelope_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let claude = dir.path().join("mock-claude-invalid.sh");
        write_executable(
            &claude,
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo v; exit 0; fi\ncat > /dev/null\necho '{\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"no-json-here\"}]}}'\n",
        );
        let err = invoke_claude_structured_with_timeout(
            claude.to_str().unwrap(),
            "prompt",
            &tool_dossier_chunk_schema(),
            3,
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("did not contain valid JSON payload"));
    }

    #[cfg(unix)]
    #[test]
    fn discover_v3_falls_back_from_codex_to_claude_in_auto_mode() {
        let _guard = env_lock().lock().unwrap();

        let dir = tempfile::tempdir().unwrap();
        let mcp = dir.path().join("mock-mcp.sh");
        let codex = dir.path().join("mock-codex-fail.sh");
        let claude = dir.path().join("mock-claude-success.sh");
        let config_path = dir.path().join("settings.json");

        write_executable(
            &mcp,
            "#!/bin/sh\nread _\nread _\nprintf '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":\"2025-03-26\",\"capabilities\":{}}}\\n'\nprintf '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"tools\":[{\"name\":\"execute\"}]}}\\n'\n",
        );
        write_executable(
            &codex,
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ] || [ \"$1\" = \"-v\" ] || [ \"$1\" = \"version\" ]; then echo v; exit 0; fi\nexit 31\n",
        );
        write_executable(
            &claude,
            r#"#!/bin/sh
if [ "$1" = "--version" ] || [ "$1" = "-v" ] || [ "$1" = "version" ]; then
  echo "mock-claude"
  exit 0
fi
cat > /dev/null
cat <<'JSON'
{"message":{"content":[{"type":"text","text":"{\"tool_dossiers\":[{\"name\":\"execute\",\"explanation\":\"Run execute\",\"recipe\":[\"validate\",\"run\",\"verify\"],\"evidence\":[\"runtime\"],\"confidence\":0.8,\"contract_tests\":[{\"probe\":\"happy-path\",\"expected\":\"ok\",\"method\":\"run\",\"applicable\":true},{\"probe\":\"invalid-input\",\"expected\":\"error\",\"method\":\"run\",\"applicable\":true},{\"probe\":\"side-effect-safety\",\"expected\":\"confirm\",\"method\":\"run\",\"applicable\":true}]}]}"}]}}
JSON
"#,
        );
        fs::write(
            &config_path,
            format!(
                r#"{{
  "mcpServers": {{
    "playwright": {{
      "command": "{}",
      "readOnly": true
    }}
  }}
}}"#,
                mcp.display()
            ),
        )
        .unwrap();

        // SAFETY: Tests serialize env mutation using env_lock().
        unsafe {
            std::env::set_var("DISTILL_CONVERT_CODEX_COMMAND", codex.as_os_str());
            std::env::set_var("DISTILL_CONVERT_CLAUDE_COMMAND", claude.as_os_str());
        }

        let options = ConvertV3Options::default();
        let bundle = discover_v3(Some("playwright"), false, &[config_path], &options).unwrap();
        assert_eq!(bundle.dossiers.len(), 1);
        let dossier = &bundle.dossiers[0];
        assert_eq!(dossier.backend_used, "claude");
        assert!(dossier.backend_fallback_used);
        assert_eq!(dossier.server_gate, ServerGate::Ready);

        // SAFETY: Tests serialize env mutation using env_lock().
        unsafe {
            std::env::remove_var("DISTILL_CONVERT_CODEX_COMMAND");
            std::env::remove_var("DISTILL_CONVERT_CLAUDE_COMMAND");
        }
    }

    #[cfg(unix)]
    #[test]
    fn discover_v3_uses_runtime_fallback_when_all_backends_fail() {
        let _guard = env_lock().lock().unwrap();

        let dir = tempfile::tempdir().unwrap();
        let mcp = dir.path().join("mock-mcp.sh");
        let codex = dir.path().join("mock-codex-fail.sh");
        let claude = dir.path().join("mock-claude-fail.sh");
        let config_path = dir.path().join("settings.json");

        write_executable(
            &mcp,
            "#!/bin/sh\nread _\nread _\nprintf '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":\"2025-03-26\",\"capabilities\":{}}}\\n'\nprintf '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"tools\":[{\"name\":\"execute\",\"inputSchema\":{\"type\":\"object\",\"required\":[\"query\"],\"properties\":{\"query\":{\"type\":\"string\"}}}}]}}\\n'\n",
        );
        write_executable(
            &codex,
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ] || [ \"$1\" = \"-v\" ] || [ \"$1\" = \"version\" ]; then echo v; exit 0; fi\nexit 31\n",
        );
        write_executable(
            &claude,
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ] || [ \"$1\" = \"-v\" ] || [ \"$1\" = \"version\" ]; then echo v; exit 0; fi\nexit 32\n",
        );
        fs::write(
            &config_path,
            format!(
                r#"{{
  "mcpServers": {{
    "playwright": {{
      "command": "{}",
      "readOnly": true
    }}
  }}
}}"#,
                mcp.display()
            ),
        )
        .unwrap();

        // SAFETY: Tests serialize env mutation using env_lock().
        unsafe {
            std::env::set_var("DISTILL_CONVERT_CODEX_COMMAND", codex.as_os_str());
            std::env::set_var("DISTILL_CONVERT_CLAUDE_COMMAND", claude.as_os_str());
        }

        let options = ConvertV3Options::default();
        let bundle = discover_v3(Some("playwright"), false, &[config_path], &options).unwrap();
        let dossier = &bundle.dossiers[0];
        assert_eq!(dossier.server_gate, ServerGate::Ready);
        assert!(dossier.gate_reasons.is_empty());
        assert_eq!(dossier.tool_dossiers.len(), 1);
        assert!(dossier.backend_fallback_used);

        // SAFETY: Tests serialize env mutation using env_lock().
        unsafe {
            std::env::remove_var("DISTILL_CONVERT_CODEX_COMMAND");
            std::env::remove_var("DISTILL_CONVERT_CLAUDE_COMMAND");
        }
    }

    #[cfg(unix)]
    #[test]
    fn execute_mcp_tool_probe_supports_sequential_setup_calls() {
        let dir = tempfile::tempdir().unwrap();
        let mcp = dir.path().join("mock-memory-state.sh");
        write_executable(
            &mcp,
            r#"#!/bin/sh
created=0
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      printf '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-03-26","capabilities":{}}}\n'
      ;;
    *'"method":"tools/call"'*)
      if echo "$line" | grep -q '"name":"create_entities"'; then
        created=1
        printf '{"jsonrpc":"2.0","id":100,"result":{"content":[{"type":"text","text":"created"}],"isError":false}}\n'
      elif echo "$line" | grep -q '"name":"add_observations"'; then
        if [ "$created" -eq 1 ]; then
          printf '{"jsonrpc":"2.0","id":2,"result":{"content":[{"type":"text","text":"ok"}],"isError":false}}\n'
        else
          printf '{"jsonrpc":"2.0","id":2,"result":{"content":[{"type":"text","text":"Entity with name sample not found"}],"isError":true}}\n'
        fi
      fi
      ;;
  esac
done
"#,
        );

        let server = MCPServerProfile {
            id: "fixture:memory".to_string(),
            name: "memory".to_string(),
            source_label: "fixture".to_string(),
            source_path: PathBuf::from("/tmp/settings.json"),
            purpose: "memory".to_string(),
            command: Some(mcp.to_string_lossy().to_string()),
            args: vec![],
            url: None,
            env_keys: vec![],
            declared_tool_count: 2,
            permission_hints: vec![],
            inferred_permission: PermissionLevel::Write,
            recommendation: ConversionRecommendation::Hybrid,
            recommendation_reason: "write".to_string(),
        };
        let args = serde_json::json!({
            "observations":[{"entityName":"sample","contents":["sample"]}]
        });
        let options = ContractTestOptions::default();

        let no_setup = execute_mcp_tool_probe(&server, "add_observations", &args, options, &[])
            .expect("probe without setup should return response");
        assert!(no_setup.is_error);

        let setup = vec![(
            "create_entities".to_string(),
            serde_json::json!({
                "entities":[{"name":"sample","entityType":"sample","observations":["sample"]}]
            }),
        )];
        let with_setup =
            execute_mcp_tool_probe(&server, "add_observations", &args, options, &setup)
                .expect("probe with setup should return response");
        assert!(!with_setup.is_error);
    }
}
