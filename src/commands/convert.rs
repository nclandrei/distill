use crate::config::{Config as AppConfig, ConvertBackendPreference as AppBackendPreference};
use anyhow::{Result, bail};
use convert_core as convert;
use convert_core::{
    ContractTestOptions, ConvertBackendConfig, ConvertBackendHealthReport, ConvertBackendName,
    ConvertBackendPreference, ConvertInventory, ConvertPlan, ConvertV3Options, ConvertVerifyReport,
    MCPServerProfile, PlanMode,
};
use serde::Serialize;
use std::path::{Path, PathBuf};

pub fn parse_mode(raw: &str) -> Result<PlanMode> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "auto" => Ok(PlanMode::Auto),
        "hybrid" => Ok(PlanMode::Hybrid),
        "replace" => Ok(PlanMode::Replace),
        other => bail!("Unsupported mode '{other}'. Expected: auto, hybrid, replace."),
    }
}

pub fn parse_backend(raw: &str) -> Result<ConvertBackendName> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "codex" => Ok(ConvertBackendName::Codex),
        "claude" => Ok(ConvertBackendName::Claude),
        other => bail!("Unsupported backend '{other}'. Expected: codex or claude."),
    }
}

fn map_backend_preference(pref: &AppBackendPreference) -> ConvertBackendPreference {
    match pref {
        AppBackendPreference::Auto => ConvertBackendPreference::Auto,
        AppBackendPreference::Codex => ConvertBackendPreference::Codex,
        AppBackendPreference::Claude => ConvertBackendPreference::Claude,
    }
}

fn v3_options(
    backend: Option<&str>,
    backend_auto_flag: bool,
    app_config: &AppConfig,
) -> Result<ConvertV3Options> {
    let backend = backend.map(parse_backend).transpose()?;
    let backend_auto = backend_auto_flag || backend.is_none();

    Ok(ConvertV3Options {
        backend,
        backend_auto,
        backend_config: ConvertBackendConfig {
            preference: map_backend_preference(&app_config.convert.backend_preference),
            timeout_seconds: app_config.convert.backend_timeout_seconds,
            chunk_size: app_config.convert.backend_chunk_size,
        },
    })
}

fn contract_test_options(
    app_config: &AppConfig,
    allow_side_effects_override: bool,
    probe_timeout_seconds_override: Option<u64>,
    probe_retries_override: Option<u32>,
) -> ContractTestOptions {
    ContractTestOptions {
        allow_side_effects: allow_side_effects_override
            || app_config.convert.allow_side_effect_probes,
        probe_timeout_seconds: probe_timeout_seconds_override
            .unwrap_or(app_config.convert.probe_timeout_seconds),
        probe_retries: probe_retries_override.unwrap_or(app_config.convert.probe_retries),
    }
}

fn maybe_backend_health(
    enabled: bool,
    config: &ConvertBackendConfig,
) -> Option<ConvertBackendHealthReport> {
    enabled.then(|| convert::backend_health_report(config))
}

fn emit_with_optional_health<T: Serialize>(
    json: bool,
    health: Option<&ConvertBackendHealthReport>,
    result: &T,
    pretty: impl FnOnce(),
) -> Result<()> {
    if json {
        #[derive(Serialize)]
        struct Envelope<'a, T: Serialize> {
            #[serde(skip_serializing_if = "Option::is_none")]
            backend_health: Option<&'a ConvertBackendHealthReport>,
            result: &'a T,
        }
        let envelope = Envelope {
            backend_health: health,
            result,
        };
        println!("{}", serde_json::to_string_pretty(&envelope)?);
        return Ok(());
    }

    if let Some(health) = health {
        print_backend_health(health);
        println!();
    }
    pretty();
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn run_discover_v3(
    selector: Option<&str>,
    all: bool,
    json: bool,
    out: Option<PathBuf>,
    config_paths: &[PathBuf],
    backend: Option<&str>,
    backend_auto: bool,
    backend_health: bool,
    app_config: &AppConfig,
) -> Result<()> {
    let options = v3_options(backend, backend_auto, app_config)?;
    let health = maybe_backend_health(backend_health, &options.backend_config);

    let bundle = if let Some(path) = out.as_deref() {
        convert::discover_v3_to_path(selector, all, config_paths, &options, path)?
    } else {
        convert::discover_v3(selector, all, config_paths, &options)?
    };

    emit_with_optional_health(json, health.as_ref(), &bundle, || {
        print_discover_bundle(&bundle);
        if let Some(path) = out {
            println!("\nWrote dossier JSON: {}", path.display());
        }
    })
}

pub fn run_build_v3(
    from_dossier: &Path,
    skills_dir: Option<PathBuf>,
    json: bool,
    backend_health: bool,
    app_config: &AppConfig,
) -> Result<()> {
    let backend_config = ConvertBackendConfig {
        preference: map_backend_preference(&app_config.convert.backend_preference),
        timeout_seconds: app_config.convert.backend_timeout_seconds,
        chunk_size: app_config.convert.backend_chunk_size,
    };
    let health = maybe_backend_health(backend_health, &backend_config);

    let result = convert::build_from_dossier_path(from_dossier, skills_dir)?;

    emit_with_optional_health(json, health.as_ref(), &result, || {
        print_build_result(&result);
    })
}

#[allow(clippy::too_many_arguments)]
pub fn run_contract_test_v3(
    from_dossier: &Path,
    report: Option<&Path>,
    json: bool,
    backend_health: bool,
    allow_side_effects: bool,
    probe_timeout_seconds: Option<u64>,
    probe_retries: Option<u32>,
    app_config: &AppConfig,
) -> Result<()> {
    let backend_config = ConvertBackendConfig {
        preference: map_backend_preference(&app_config.convert.backend_preference),
        timeout_seconds: app_config.convert.backend_timeout_seconds,
        chunk_size: app_config.convert.backend_chunk_size,
    };
    let health = maybe_backend_health(backend_health, &backend_config);

    let contract_options = contract_test_options(
        app_config,
        allow_side_effects,
        probe_timeout_seconds,
        probe_retries,
    );
    let result = convert::contract_test_from_dossier_path(from_dossier, report, contract_options)?;

    emit_with_optional_health(json, health.as_ref(), &result, || {
        print_contract_result(&result);
        if let Some(path) = report {
            println!("Contract-test report: {}", path.display());
        }
    })
}

#[allow(clippy::too_many_arguments)]
pub fn run_apply_v3(
    from_dossier: &Path,
    yes: bool,
    skills_dir: Option<PathBuf>,
    json: bool,
    backend_health: bool,
    allow_side_effects: bool,
    probe_timeout_seconds: Option<u64>,
    probe_retries: Option<u32>,
    app_config: &AppConfig,
) -> Result<()> {
    let backend_config = ConvertBackendConfig {
        preference: map_backend_preference(&app_config.convert.backend_preference),
        timeout_seconds: app_config.convert.backend_timeout_seconds,
        chunk_size: app_config.convert.backend_chunk_size,
    };
    let health = maybe_backend_health(backend_health, &backend_config);

    let contract_options = contract_test_options(
        app_config,
        allow_side_effects,
        probe_timeout_seconds,
        probe_retries,
    );
    let result = convert::apply_from_dossier_path(from_dossier, yes, skills_dir, contract_options)?;

    emit_with_optional_health(json, health.as_ref(), &result, || {
        print_apply_v3_result(&result);
    })
}

#[allow(clippy::too_many_arguments)]
pub fn run_one_shot_v3(
    selector: &str,
    json: bool,
    config_paths: &[PathBuf],
    skills_dir: Option<PathBuf>,
    backend: Option<&str>,
    backend_auto: bool,
    backend_health: bool,
    allow_side_effects: bool,
    probe_timeout_seconds: Option<u64>,
    probe_retries: Option<u32>,
    app_config: &AppConfig,
) -> Result<()> {
    let options = v3_options(backend, backend_auto, app_config)?;
    let health = maybe_backend_health(backend_health, &options.backend_config);
    let contract_options = contract_test_options(
        app_config,
        allow_side_effects,
        probe_timeout_seconds,
        probe_retries,
    );

    let result = convert::run_one_shot_v3(
        selector,
        config_paths,
        &options,
        skills_dir,
        contract_options,
    )?;

    emit_with_optional_health(json, health.as_ref(), &result, || {
        print_one_shot_result(&result);
    })
}

pub fn run_overview_v3(config_paths: &[PathBuf]) -> Result<()> {
    let inventory = convert::discover(config_paths)?;
    print_inventory(&inventory);
    println!();
    println!("V4 conversion flow:");
    println!("  distill convert discover <server|--all> [--out dossier.json]");
    println!("  distill convert build --from-dossier dossier.json [--skills-dir ...]");
    println!(
        "  distill convert contract-test --from-dossier dossier.json [--report ...] [--allow-side-effects] [--probe-timeout-seconds N] [--probe-retries N]"
    );
    println!("  distill convert apply --from-dossier dossier.json --yes [--skills-dir ...]");
    println!();
    println!("One-shot:");
    println!("  distill convert <server>");
    println!("Use --backend codex|claude, --backend-auto, and --backend-health as needed.");
    Ok(())
}

pub fn run_list(json: bool, config_paths: &[PathBuf]) -> Result<()> {
    let inventory = convert::discover(config_paths)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&inventory)?);
        return Ok(());
    }

    print_inventory(&inventory);
    Ok(())
}

pub fn run_inspect(selector: &str, json: bool, config_paths: &[PathBuf]) -> Result<()> {
    let server = convert::inspect(selector, config_paths)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&server)?);
        return Ok(());
    }

    print_server(&server);
    Ok(())
}

pub fn run_plan(
    selector: &str,
    mode_raw: &str,
    dry_run: bool,
    json: bool,
    config_paths: &[PathBuf],
) -> Result<()> {
    let mode = parse_mode(mode_raw)?;
    let plan = convert::plan(selector, mode, config_paths)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&plan)?);
        return Ok(());
    }

    print_plan(&plan, dry_run);
    Ok(())
}

pub fn run_verify(
    selector: &str,
    json: bool,
    config_paths: &[PathBuf],
    skills_dir: Option<PathBuf>,
) -> Result<()> {
    let report = convert::verify(selector, config_paths, skills_dir)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    print_verify_report(&report);
    Ok(())
}

fn print_backend_health(health: &ConvertBackendHealthReport) {
    println!("Backend health ({}):", health.checked_at);
    for status in &health.statuses {
        println!(
            "  - {}: {}",
            status.backend,
            if status.available {
                "available"
            } else {
                "unavailable"
            }
        );
        for line in &status.diagnostics {
            println!("      {line}");
        }
    }
}

fn print_discover_bundle(bundle: &convert::DossierBundle) {
    println!(
        "Generated {} dossier(s) at {}",
        bundle.dossiers.len(),
        bundle.generated_at
    );
    for dossier in &bundle.dossiers {
        println!("- {}", dossier.server.id);
        println!("  gate      : {:?}", dossier.server_gate);
        println!("  backend   : {}", dossier.backend_used);
        println!("  tools     : {}", dossier.runtime_tools.len());
        if dossier.backend_fallback_used {
            println!("  fallback  : used");
        }
        if !dossier.gate_reasons.is_empty() {
            println!("  reasons   : {}", dossier.gate_reasons.join(" | "));
        }
    }
}

fn print_build_result(result: &convert::BuildResult) {
    println!("Built skills in {}", result.skills_dir.display());
    for server in &result.servers {
        println!("- {}", server.server_id);
        println!(
            "  orchestrator : {}",
            server.orchestrator_skill_path.display()
        );
        println!("  tool_skills  : {}", server.tool_skill_paths.len());
    }
}

fn print_contract_result(result: &convert::ContractTestReport) {
    println!(
        "Contract test {} ({} server dossier(s))",
        if result.passed { "passed" } else { "failed" },
        result.servers.len()
    );
    for server in &result.servers {
        println!(
            "- {}: {}",
            server.server_id,
            if server.passed { "pass" } else { "fail" }
        );
        if !server.reasons.is_empty() {
            println!("  reasons: {}", server.reasons.join(" | "));
        }
    }
}

fn print_apply_v3_result(result: &convert::ApplyResultV3) {
    println!("Applied conversion into {}", result.skills_dir.display());
    for server in &result.servers {
        println!("- {}", server.server_id);
        println!("  updated_config : {}", server.mcp_config_updated);
        println!(
            "  orchestrator   : {}",
            server.orchestrator_skill_path.display()
        );
        println!("  tool_skills    : {}", server.tool_skill_paths.len());
        if let Some(backup) = &server.mcp_config_backup {
            println!("  backup         : {}", backup.display());
        }
    }
}

fn print_one_shot_result(result: &convert::OneShotV3Result) {
    println!(
        "One-shot conversion complete: {} dossier(s), contract={}, applied={}",
        result.dossier.dossiers.len(),
        if result.contract_test.passed {
            "pass"
        } else {
            "fail"
        },
        result.apply.servers.len()
    );
    for server in &result.apply.servers {
        println!(
            "- {} -> {}",
            server.server_id,
            server.orchestrator_skill_path.display()
        );
    }
}

fn print_inventory(inventory: &ConvertInventory) {
    if inventory.servers.is_empty() {
        println!("No MCP servers found.");
        println!("Searched paths:");
        for path in &inventory.searched_paths {
            println!("  - {}", path.display());
        }
        return;
    }

    let existing_sources = inventory
        .servers
        .iter()
        .map(|server| server.source_path.clone())
        .collect::<std::collections::BTreeSet<_>>();

    println!(
        "Found {} MCP server(s) across {} config file(s).",
        inventory.servers.len(),
        existing_sources.len()
    );

    for server in &inventory.servers {
        println!("- {}", server.id);
        println!("  source         : {}", server.source_path.display());
        println!("  purpose        : {}", server.purpose);
        println!("  permissions    : {}", server.inferred_permission);
        println!("  recommendation : {}", server.recommendation);
    }
}

fn print_server(server: &MCPServerProfile) {
    println!("Server: {}", server.id);
    println!("  Name                : {}", server.name);
    println!("  Source              : {}", server.source_path.display());
    println!("  Purpose             : {}", server.purpose);
    println!(
        "  Command             : {}",
        display_option(&server.command)
    );
    println!("  URL                 : {}", display_option(&server.url));
    if server.args.is_empty() {
        println!("  Args                : (none)");
    } else {
        println!("  Args                : {}", server.args.join(" "));
    }
    if server.env_keys.is_empty() {
        println!("  Required env keys   : (none)");
    } else {
        println!("  Required env keys   : {}", server.env_keys.join(", "));
    }
    if server.permission_hints.is_empty() {
        println!("  Permission hints    : (none)");
    } else {
        println!(
            "  Permission hints    : {}",
            server.permission_hints.join(", ")
        );
    }
    println!("  Declared tool count : {}", server.declared_tool_count);
    println!("  Inferred permission : {}", server.inferred_permission);
    println!("  Recommendation      : {}", server.recommendation);
    println!("  Why                 : {}", server.recommendation_reason);
}

fn print_plan(plan: &ConvertPlan, dry_run: bool) {
    println!("Plan for {}", plan.server.id);
    println!("  requested_mode : {}", plan.requested_mode);
    println!("  recommended    : {}", plan.recommended_mode);
    println!("  effective_mode : {}", plan.effective_mode);
    println!("  blocked        : {}", plan.blocked);

    if dry_run {
        println!("  dry_run        : true (no files were changed)");
    }

    println!("Actions:");
    for action in &plan.actions {
        println!("  - {action}");
    }

    if !plan.warnings.is_empty() {
        println!("Warnings:");
        for warning in &plan.warnings {
            println!("  - {warning}");
        }
    }

    if plan.blocked {
        println!(
            "Apply step is blocked for this plan. Use hybrid mode or adjust server scope before replace."
        );
    }
}

fn print_verify_report(report: &ConvertVerifyReport) {
    println!("Verification for {}", report.server.id);
    println!("  passed               : {}", report.passed);
    println!(
        "  orchestrator         : {}",
        report.orchestrator_skill_path.display()
    );
    println!("  capability_skills    : {}", report.tool_skill_paths.len());
    println!("  introspection_ok     : {}", report.introspection_ok);
    println!(
        "  introspected_tools   : {}",
        report.introspected_tool_count
    );
    println!("  required_tool_count  : {}", report.required_tools.len());
    if !report.missing_in_server.is_empty() {
        println!(
            "  missing_in_server    : {}",
            report.missing_in_server.join(", ")
        );
    }
    if !report.missing_in_skill.is_empty() {
        println!(
            "  missing_in_skill     : {}",
            report.missing_in_skill.join(", ")
        );
    }
    if !report.missing_skill_files.is_empty() {
        println!(
            "  missing_skill_files  : {}",
            report.missing_skill_files.join(", ")
        );
    }
    if !report.notes.is_empty() {
        println!("Notes:");
        for note in &report.notes {
            println!("  - {note}");
        }
    }
}

fn display_option(value: &Option<String>) -> String {
    value.clone().unwrap_or_else(|| "(none)".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_mode() {
        assert_eq!(parse_mode("auto").unwrap(), PlanMode::Auto);
        assert_eq!(parse_mode("hybrid").unwrap(), PlanMode::Hybrid);
        assert_eq!(parse_mode("replace").unwrap(), PlanMode::Replace);
        assert!(parse_mode("invalid").is_err());
    }

    #[test]
    fn test_parse_backend() {
        assert_eq!(parse_backend("codex").unwrap(), ConvertBackendName::Codex);
        assert_eq!(parse_backend("claude").unwrap(), ConvertBackendName::Claude);
        assert!(parse_backend("other").is_err());
    }
}
