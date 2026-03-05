use anyhow::{Result, bail};
use serde::Serialize;
use std::path::PathBuf;

use crate::convert::{
    self, ConvertApplyResult, ConvertInventory, ConvertPlan, ConvertVerifyReport, MCPServerProfile,
    PlanMode,
};

pub fn parse_mode(raw: &str) -> Result<PlanMode> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "auto" => Ok(PlanMode::Auto),
        "hybrid" => Ok(PlanMode::Hybrid),
        "replace" => Ok(PlanMode::Replace),
        other => bail!("Unsupported mode '{other}'. Expected: auto, hybrid, replace."),
    }
}

#[derive(Debug, Clone, Serialize)]
struct ConvertRunResult {
    server_id: String,
    requested_mode: PlanMode,
    planned_mode: PlanMode,
    applied_mode: PlanMode,
    safe_mode_downgrade: bool,
    verify_passed: bool,
    verified_in_apply: bool,
    apply: ConvertApplyResult,
    verify: Option<ConvertVerifyReport>,
    notes: Vec<String>,
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

pub fn run_apply(
    selector: &str,
    mode_raw: &str,
    yes: bool,
    json: bool,
    config_paths: &[PathBuf],
    output_dir: Option<PathBuf>,
) -> Result<()> {
    let mode = parse_mode(mode_raw)?;
    let result = convert::apply(selector, mode, yes, config_paths, output_dir)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(());
    }

    print_apply_result(&result);
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

pub fn run_one_shot(
    selector: &str,
    replace: bool,
    yes: bool,
    json: bool,
    config_paths: &[PathBuf],
    skills_dir: Option<PathBuf>,
) -> Result<()> {
    if replace && !yes {
        bail!("--replace requires --yes because this mutates MCP config.");
    }

    let requested_mode = if replace {
        PlanMode::Replace
    } else {
        PlanMode::Auto
    };
    let plan = convert::plan(selector, requested_mode, config_paths)?;

    let mut applied_mode = plan.effective_mode;
    let mut safe_mode_downgrade = false;
    let mut notes = vec![];
    if !replace && applied_mode == PlanMode::Replace {
        applied_mode = PlanMode::Hybrid;
        safe_mode_downgrade = true;
        notes.push(
            "Auto planning resolved to replace; one-shot defaulted to hybrid for safety. Use --replace --yes to mutate config."
                .to_string(),
        );
    }

    let apply = convert::apply(
        selector,
        applied_mode,
        yes,
        config_paths,
        skills_dir.clone(),
    )?;
    let (verify, verified_in_apply, verify_passed) = if apply.effective_mode == PlanMode::Replace {
        // replace-mode already gates config mutation on live verification inside apply()
        (None, true, true)
    } else {
        let verify = convert::verify(selector, config_paths, skills_dir)?;
        let passed = verify.passed;
        (Some(verify), false, passed)
    };

    let result = ConvertRunResult {
        server_id: apply.server.id.clone(),
        requested_mode,
        planned_mode: plan.effective_mode,
        applied_mode: apply.effective_mode,
        safe_mode_downgrade,
        verify_passed,
        verified_in_apply,
        apply,
        verify,
        notes,
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        print_run_result(&result);
    }

    if !result.verify_passed {
        bail!(
            "One-shot conversion completed but verification failed. Check missing_in_server/missing_in_skill in JSON output."
        );
    }

    Ok(())
}

pub fn run_overview(config_paths: &[PathBuf]) -> Result<()> {
    let inventory = convert::discover(config_paths)?;
    print_inventory(&inventory);
    println!();
    println!("Next steps:");
    println!("  distill convert <server-id|server-name>");
    println!("  distill convert inspect <server-id|server-name>");
    println!("  distill convert plan <server-id|server-name> --mode auto|hybrid|replace");
    println!("  distill convert apply <server-id|server-name> --mode auto|hybrid|replace");
    println!("  distill convert verify <server-id|server-name>");
    println!("Use --json for one-shot agent automation.");
    Ok(())
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

fn print_apply_result(result: &ConvertApplyResult) {
    println!("Applied conversion for {}", result.server.id);
    println!("  requested_mode    : {}", result.requested_mode);
    println!("  effective_mode    : {}", result.effective_mode);
    println!("  skill_path        : {}", result.skill_path.display());
    println!("  mcp_config_updated: {}", result.mcp_config_updated);
    if let Some(backup) = &result.mcp_config_backup {
        println!("  mcp_config_backup : {}", backup.display());
    }

    if !result.notes.is_empty() {
        println!("Notes:");
        for note in &result.notes {
            println!("  - {note}");
        }
    }
}

fn print_verify_report(report: &ConvertVerifyReport) {
    println!("Verification for {}", report.server.id);
    println!("  passed               : {}", report.passed);
    println!("  skill_path           : {}", report.skill_path.display());
    println!("  introspection_ok     : {}", report.introspection_ok);
    println!(
        "  introspected_tools   : {}",
        report.introspected_tool_count
    );
    println!(
        "  required_hint_count  : {}",
        report.required_tool_hints.len()
    );
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
    if !report.notes.is_empty() {
        println!("Notes:");
        for note in &report.notes {
            println!("  - {note}");
        }
    }
}

fn print_run_result(result: &ConvertRunResult) {
    println!("Converted {}", result.server_id);
    println!(
        "mode={} verify={} skill={}",
        result.applied_mode,
        if result.verify_passed {
            "passed"
        } else {
            "failed"
        },
        result.apply.skill_path.display()
    );
    if result.apply.mcp_config_updated {
        if let Some(backup) = &result.apply.mcp_config_backup {
            println!("mcp_config=updated backup={}", backup.display());
        } else {
            println!("mcp_config=updated");
        }
    } else {
        println!("mcp_config=unchanged");
    }
    if result.safe_mode_downgrade {
        println!("note=safe-default-used (auto->replace was downgraded to hybrid)");
    }
    if !result.verify_passed {
        if let Some(verify) = &result.verify {
            if !verify.missing_in_server.is_empty() {
                println!("missing_in_server={}", verify.missing_in_server.join(","));
            }
            if !verify.missing_in_skill.is_empty() {
                println!("missing_in_skill={}", verify.missing_in_skill.join(","));
            }
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
}
