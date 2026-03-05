use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;

/// Build a distill command with HOME set to a temp dir so tests
/// don't interact with the real ~/.distill config.
fn distill_cmd(home: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("distill").unwrap();
    cmd.env("HOME", home);
    cmd.env("DISTILL_SYSTEMCTL_PATH", "true");
    cmd.env("DISTILL_LAUNCHCTL_PATH", "true");
    cmd
}

#[test]
fn test_no_args_without_config() {
    let dir = tempfile::tempdir().unwrap();
    distill_cmd(dir.path())
        .assert()
        .failure()
        .stdout(predicate::str::contains("Welcome to distill"))
        .stderr(predicate::str::contains("interactive terminal"));
}

#[test]
fn test_help_flag() {
    let dir = tempfile::tempdir().unwrap();
    distill_cmd(dir.path())
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Monitor AI agent sessions"));
}

#[test]
fn test_version_flag() {
    let dir = tempfile::tempdir().unwrap();
    distill_cmd(dir.path())
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains("distill"));
}

#[test]
fn test_status_without_config() {
    let dir = tempfile::tempdir().unwrap();
    distill_cmd(dir.path())
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("not configured"));
}

#[test]
fn test_scan_now_without_config() {
    let dir = tempfile::tempdir().unwrap();
    distill_cmd(dir.path())
        .args(["scan", "--now"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("No config found"));
}

#[test]
fn test_scan_without_now_flag() {
    let dir = tempfile::tempdir().unwrap();
    // Seed a minimal config so plain `scan` can execute the scheduled path.
    let distill_dir = dir.path().join(".distill");
    std::fs::create_dir_all(&distill_dir).unwrap();
    std::fs::write(
        distill_dir.join("config.yaml"),
        "agents:\n  - name: claude\n    enabled: true\nscan_interval: weekly\nproposal_agent: claude\nshell: zsh\nnotifications: both\n",
    ).unwrap();

    distill_cmd(dir.path())
        .arg("scan")
        .assert()
        .success()
        .stdout(predicate::str::contains("running scheduled scan"))
        .stdout(predicate::str::contains(
            "No new sessions found since last scan.",
        ));
}

#[test]
fn test_review_no_proposals() {
    // With an empty proposals directory the command exits cleanly with a
    // human-friendly message and a zero exit code.
    let dir = tempfile::tempdir().unwrap();
    distill_cmd(dir.path())
        .arg("review")
        .assert()
        .success()
        .stdout(predicate::str::contains("No pending proposals to review."));
}

#[test]
fn test_onboard_write_json_template() {
    let dir = tempfile::tempdir().unwrap();
    let output_path = dir.path().join("onboarding.json");

    distill_cmd(dir.path())
        .args(["onboard", "--write-json"])
        .arg(&output_path)
        .assert()
        .success()
        .stdout(predicate::str::contains("Wrote onboarding JSON template"));

    let written = fs::read_to_string(&output_path).unwrap();
    assert!(written.contains("\"format_version\": 1"));
    assert!(written.contains("\"agents\""));
    assert!(written.contains("\"install_shell_hook\""));
}

#[test]
fn test_onboard_write_json_stdout() {
    let dir = tempfile::tempdir().unwrap();

    distill_cmd(dir.path())
        .args(["onboard", "--write-json", "-"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"format_version\": 1"))
        .stdout(predicate::str::contains("\"agents\""))
        .stdout(predicate::str::contains("\"install_shell_hook\""));
}

#[test]
fn test_onboard_apply_json() {
    let dir = tempfile::tempdir().unwrap();
    let input_path = dir.path().join("onboarding-input.json");
    fs::write(
        &input_path,
        r#"{
  "format_version": 1,
  "agents": [
    { "name": "claude", "enabled": true },
    { "name": "codex", "enabled": false }
  ],
  "scan_interval": "daily",
  "proposal_agent": "claude",
  "shell": "zsh",
  "notifications": "both",
  "notification_icon": null,
  "install_shell_hook": false
}"#,
    )
    .unwrap();

    distill_cmd(dir.path())
        .args(["onboard", "--apply-json"])
        .arg(&input_path)
        .assert()
        .success()
        .stdout(predicate::str::contains("Onboarding applied from JSON."))
        .stdout(predicate::str::contains("Scan interval    : daily"));

    let config_path = dir.path().join(".distill").join("config.yaml");
    assert!(config_path.exists(), "config.yaml should be written");
    let config = fs::read_to_string(config_path).unwrap();
    assert!(config.contains("scan_interval: daily"));
    assert!(config.contains("proposal_agent: claude"));
}

#[test]
fn test_onboard_apply_json_stdin() {
    let dir = tempfile::tempdir().unwrap();

    distill_cmd(dir.path())
        .args(["onboard", "--apply-json", "-"])
        .write_stdin(
            r#"{
  "format_version": 1,
  "agents": [
    { "name": "claude", "enabled": true },
    { "name": "codex", "enabled": false }
  ],
  "scan_interval": "monthly",
  "proposal_agent": "claude",
  "shell": "zsh",
  "notifications": "terminal",
  "notification_icon": null,
  "install_shell_hook": false
}"#,
        )
        .assert()
        .success()
        .stdout(predicate::str::contains("Onboarding applied from JSON."))
        .stdout(predicate::str::contains("Scan interval    : monthly"));

    let config_path = dir.path().join(".distill").join("config.yaml");
    assert!(config_path.exists(), "config.yaml should be written");
    let config = fs::read_to_string(config_path).unwrap();
    assert!(config.contains("scan_interval: monthly"));
    assert!(config.contains("notifications: terminal"));
}

#[test]
fn test_review_write_json() {
    let dir = tempfile::tempdir().unwrap();
    let proposals_dir = dir.path().join(".distill").join("proposals");
    fs::create_dir_all(&proposals_dir).unwrap();
    fs::write(
        proposals_dir.join("proposal-1.md"),
        "---\ntype: new\nconfidence: high\ntarget_skill: null\nevidence: []\ncreated: 2026-03-02T00:00:00Z\n---\n\n# Skill 1\n\nBody 1.\n",
    )
    .unwrap();

    let output_path = dir.path().join("review.json");
    distill_cmd(dir.path())
        .args(["review", "--write-json"])
        .arg(&output_path)
        .assert()
        .success()
        .stdout(predicate::str::contains("Wrote 1 pending proposal(s)"));

    let written = fs::read_to_string(&output_path).unwrap();
    assert!(written.contains("\"format_version\": 1"));
    assert!(written.contains("\"filename\": \"proposal-1.md\""));
    assert!(written.contains("\"decision\": null"));
}

#[test]
fn test_review_write_json_stdout() {
    let dir = tempfile::tempdir().unwrap();
    let proposals_dir = dir.path().join(".distill").join("proposals");
    fs::create_dir_all(&proposals_dir).unwrap();
    fs::write(
        proposals_dir.join("proposal-stdout.md"),
        "---\ntype: new\nconfidence: high\ntarget_skill: null\nevidence: []\ncreated: 2026-03-02T00:00:00Z\n---\n\n# Skill Stdout\n\nBody stdout.\n",
    )
    .unwrap();

    distill_cmd(dir.path())
        .args(["review", "--write-json", "-"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"format_version\": 1"))
        .stdout(predicate::str::contains(
            "\"filename\": \"proposal-stdout.md\"",
        ))
        .stdout(predicate::str::contains("\"decision\": null"));
}

#[test]
fn test_review_apply_json() {
    let dir = tempfile::tempdir().unwrap();
    let proposals_dir = dir.path().join(".distill").join("proposals");
    fs::create_dir_all(&proposals_dir).unwrap();
    fs::write(
        proposals_dir.join("proposal-1.md"),
        "---\ntype: new\nconfidence: high\ntarget_skill: null\nevidence: []\ncreated: 2026-03-02T00:00:00Z\n---\n\n# Skill 1\n\nBody 1.\n",
    )
    .unwrap();

    let review_json = dir.path().join("review-apply.json");
    fs::write(
        &review_json,
        r##"{
  "format_version": 1,
  "generated_at": "2026-03-04T00:00:00Z",
  "proposals": [
    {
      "filename": "proposal-1.md",
      "type": "new",
      "confidence": "high",
      "target_skill": null,
      "created": "2026-03-02T00:00:00Z",
      "evidence": [],
      "body": "# Skill 1\n\nBody 1.",
      "decision": "accept"
    }
  ]
}"##,
    )
    .unwrap();

    distill_cmd(dir.path())
        .args(["review", "--apply-json"])
        .arg(&review_json)
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Review decisions applied from JSON.",
        ))
        .stdout(predicate::str::contains("Accepted : 1"));

    assert!(
        !proposals_dir.join("proposal-1.md").exists(),
        "proposal should be removed after accept"
    );
    assert!(
        dir.path()
            .join(".distill")
            .join("skills")
            .join("proposal-1.md")
            .exists(),
        "skill should be written after accept"
    );
}

#[test]
fn test_review_apply_json_stdin() {
    let dir = tempfile::tempdir().unwrap();
    let proposals_dir = dir.path().join(".distill").join("proposals");
    fs::create_dir_all(&proposals_dir).unwrap();
    fs::write(
        proposals_dir.join("proposal-stdin.md"),
        "---\ntype: new\nconfidence: high\ntarget_skill: null\nevidence: []\ncreated: 2026-03-02T00:00:00Z\n---\n\n# Skill Stdin\n\nBody stdin.\n",
    )
    .unwrap();

    distill_cmd(dir.path())
        .args(["review", "--apply-json", "-"])
        .write_stdin(
            r##"{
  "format_version": 1,
  "generated_at": "2026-03-04T00:00:00Z",
  "proposals": [
    {
      "filename": "proposal-stdin.md",
      "type": "new",
      "confidence": "high",
      "target_skill": null,
      "created": "2026-03-02T00:00:00Z",
      "evidence": [],
      "body": "# Skill Stdin\n\nBody stdin.",
      "decision": "accept"
    }
  ]
}"##,
        )
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Review decisions applied from JSON.",
        ))
        .stdout(predicate::str::contains("Accepted : 1"));

    assert!(
        !proposals_dir.join("proposal-stdin.md").exists(),
        "proposal should be removed after accept"
    );
    assert!(
        dir.path()
            .join(".distill")
            .join("skills")
            .join("proposal-stdin.md")
            .exists(),
        "skill should be written after accept"
    );
}

#[test]
fn test_watch_install_stub() {
    let dir = tempfile::tempdir().unwrap();
    // Seed a minimal config so that `watch --install` can load it.
    let distill_dir = dir.path().join(".distill");
    std::fs::create_dir_all(&distill_dir).unwrap();
    std::fs::write(
        distill_dir.join("config.yaml"),
        "agents:\n  - name: claude\n    enabled: true\nscan_interval: weekly\nproposal_agent: claude\nshell: zsh\nnotifications: both\n",
    ).unwrap();

    distill_cmd(dir.path())
        .args(["watch", "--install"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Scheduler installed."));
}

#[test]
fn test_watch_uninstall_stub() {
    let dir = tempfile::tempdir().unwrap();
    distill_cmd(dir.path())
        .args(["watch", "--uninstall"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Scheduler removed."));
}

#[test]
fn test_notify_check() {
    let dir = tempfile::tempdir().unwrap();
    distill_cmd(dir.path())
        .args(["notify", "--check"])
        .assert()
        .success();
}

#[test]
fn test_invalid_subcommand() {
    let dir = tempfile::tempdir().unwrap();
    distill_cmd(dir.path())
        .arg("nonexistent")
        .assert()
        .failure();
}

#[test]
fn test_watch_mutual_exclusion() {
    let dir = tempfile::tempdir().unwrap();
    distill_cmd(dir.path())
        .args(["watch", "--install", "--uninstall"])
        .assert()
        .failure();
}

#[test]
fn test_status_with_config() {
    let dir = tempfile::tempdir().unwrap();
    let distill_dir = dir.path().join(".distill");
    std::fs::create_dir_all(&distill_dir).unwrap();
    std::fs::write(
        distill_dir.join("config.yaml"),
        "agents:\n  - name: claude\n    enabled: true\nscan_interval: weekly\nproposal_agent: claude\nshell: zsh\nnotifications: both\n",
    ).unwrap();

    distill_cmd(dir.path())
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("distill status"))
        .stdout(predicate::str::contains("claude"));
}

#[test]
fn test_dedupe_dry_run_does_not_write_proposals() {
    let dir = tempfile::tempdir().unwrap();
    let skills_dir = dir.path().join(".distill").join("skills");
    std::fs::create_dir_all(&skills_dir).unwrap();
    std::fs::write(skills_dir.join("alpha.md"), "# Same\nContent\n").unwrap();
    std::fs::write(skills_dir.join("beta.md"), "# Same\nContent\n").unwrap();

    distill_cmd(dir.path())
        .args(["dedupe", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Found 1 duplicate skill file(s)"))
        .stdout(predicate::str::contains("Dry run"));

    let proposals_dir = dir.path().join(".distill").join("proposals");
    assert!(
        !proposals_dir.exists() || std::fs::read_dir(&proposals_dir).unwrap().next().is_none(),
        "dry-run should not write proposal files"
    );
}

#[test]
fn test_dedupe_writes_remove_proposal() {
    let dir = tempfile::tempdir().unwrap();
    let skills_dir = dir.path().join(".distill").join("skills");
    std::fs::create_dir_all(&skills_dir).unwrap();
    std::fs::write(skills_dir.join("alpha.md"), "# Same\nContent\n").unwrap();
    std::fs::write(skills_dir.join("beta.md"), "# Same\nContent\n").unwrap();

    distill_cmd(dir.path())
        .arg("dedupe")
        .assert()
        .success()
        .stdout(predicate::str::contains("Wrote 1 remove proposal(s)"));

    let proposals_dir = dir.path().join(".distill").join("proposals");
    let proposals = std::fs::read_dir(&proposals_dir)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .collect::<Vec<_>>();
    assert_eq!(proposals.len(), 1);

    let content = std::fs::read_to_string(proposals[0].path()).unwrap();
    assert!(content.contains("type: remove"));
    assert!(content.contains("target_skill: beta.md"));
}

#[test]
fn test_convert_list_json_from_custom_config() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("mcp.json");
    std::fs::write(
        &config_path,
        r#"{
  "mcpServers": {
    "playwright": {
      "command": "npx",
      "args": ["-y", "@playwright/mcp"],
      "description": "Browser automation workflows",
      "tools": ["navigate", "click", "fill"]
    }
  }
}"#,
    )
    .unwrap();

    distill_cmd(dir.path())
        .args(["convert", "list", "--json", "--config"])
        .arg(&config_path)
        .assert()
        .success()
        .stdout(predicate::str::contains("\"name\": \"playwright\""))
        .stdout(predicate::str::contains("\"declared_tool_count\": 3"));
}

#[test]
fn test_convert_inspect_and_plan_json() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("mcp.json");
    std::fs::write(
        &config_path,
        r#"{
  "mcpServers": {
    "infra-admin": {
      "command": "terraform",
      "description": "Apply and destroy infra",
      "permissions": ["write"]
    }
  }
}"#,
    )
    .unwrap();

    distill_cmd(dir.path())
        .args(["convert", "inspect", "infra-admin", "--json", "--config"])
        .arg(&config_path)
        .assert()
        .success()
        .stdout(predicate::str::contains("\"name\": \"infra-admin\""))
        .stdout(predicate::str::contains("\"recommendation\": \"keep-mcp\""));

    distill_cmd(dir.path())
        .args([
            "convert",
            "plan",
            "infra-admin",
            "--mode",
            "replace",
            "--json",
            "--config",
        ])
        .arg(&config_path)
        .assert()
        .success()
        .stdout(predicate::str::contains("\"blocked\": true"))
        .stdout(predicate::str::contains("\"effective_mode\": \"replace\""));
}

#[test]
fn test_convert_list_detects_claude_settings_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let claude_dir = dir.path().join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    std::fs::write(
        claude_dir.join("settings.json"),
        r#"{
  "mcpServers": {
    "XcodeBuildMCP": {
      "command": "npx",
      "args": ["-y", "xcodebuildmcp@latest", "mcp"]
    }
  }
}"#,
    )
    .unwrap();

    distill_cmd(dir.path())
        .args(["convert", "list", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"name\": \"XcodeBuildMCP\""));
}

#[test]
fn test_convert_apply_hybrid_writes_skill_file() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("mcp.json");
    let mock_mcp = dir.path().join("mock-mcp.sh");
    std::fs::write(
        &mock_mcp,
        "#!/bin/sh\nread _\nread _\nprintf '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":\"2025-03-26\",\"capabilities\":{}}}\\n'\nprintf '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"tools\":[{\"name\":\"build_run_sim\"},{\"name\":\"launch_app_sim\"},{\"name\":\"list_sims\"}]}}\\n'\n",
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
    "XcodeBuildMCP": {{
      "command": "{}",
      "args": ["-y", "xcodebuildmcp@latest", "mcp"]
    }}
  }}
}}"#,
            mock_mcp.display()
        ),
    )
    .unwrap();

    distill_cmd(dir.path())
        .args([
            "convert",
            "apply",
            "XcodeBuildMCP",
            "--mode",
            "auto",
            "--json",
            "--config",
        ])
        .arg(&config_path)
        .assert()
        .success()
        .stdout(predicate::str::contains("\"effective_mode\": \"hybrid\""))
        .stdout(predicate::str::contains("\"mcp_config_updated\": false"));

    assert!(
        dir.path()
            .join(".distill")
            .join("skills")
            .join("mcp-xcodebuildmcp.md")
            .exists(),
        "hybrid apply should write a generated skill file"
    );

    let skill_path = dir
        .path()
        .join(".distill")
        .join("skills")
        .join("mcp-xcodebuildmcp.md");
    let skill = std::fs::read_to_string(&skill_path).unwrap();
    assert!(!skill.contains("## Server Metadata"));
    assert!(!skill.contains("mcp__"));

    let manifest_path = dir
        .path()
        .join(".distill")
        .join("skills")
        .join(".distill-manifests")
        .join("mcp-xcodebuildmcp.json");
    assert!(
        manifest_path.exists(),
        "hybrid apply should write parity manifest"
    );
}

#[test]
fn test_convert_apply_replace_removes_server_and_creates_backup() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("settings.json");
    let mock_mcp = dir.path().join("mock-mcp.sh");
    std::fs::write(
        &mock_mcp,
        "#!/bin/sh\nread _\nread _\nprintf '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":\"2025-03-26\",\"capabilities\":{}}}\\n'\nprintf '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"tools\":[{\"name\":\"navigate\"},{\"name\":\"click\"}]}}\\n'\n",
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
      "description": "Read-only browser helpers",
      "readOnly": true
    }}
  }}
}}"#,
            mock_mcp.display()
        ),
    )
    .unwrap();

    distill_cmd(dir.path())
        .args([
            "convert",
            "apply",
            "playwright",
            "--mode",
            "replace",
            "--yes",
            "--json",
            "--config",
        ])
        .arg(&config_path)
        .assert()
        .success()
        .stdout(predicate::str::contains("\"mcp_config_updated\": true"))
        .stdout(predicate::str::contains("\"effective_mode\": \"replace\""));

    let updated = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        !updated.contains("playwright"),
        "replace mode should remove the server from MCP config"
    );

    let backup_count = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("settings.json.bak-")
        })
        .count();
    assert_eq!(backup_count, 1, "replace mode should create one backup");
}

#[test]
fn test_convert_verify_reports_passed_for_generated_skill() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("settings.json");
    let skills_dir = dir.path().join("skills");
    let mock_mcp = dir.path().join("mock-mcp.sh");
    std::fs::write(
        &mock_mcp,
        "#!/bin/sh\nread _\nread _\nprintf '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":\"2025-03-26\",\"capabilities\":{}}}\\n'\nprintf '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"tools\":[{\"name\":\"navigate\"},{\"name\":\"click\"}]}}\\n'\n",
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
      "description": "Read-only browser helpers",
      "readOnly": true
    }}
  }}
}}"#,
            mock_mcp.display()
        ),
    )
    .unwrap();

    distill_cmd(dir.path())
        .args([
            "convert",
            "apply",
            "playwright",
            "--mode",
            "hybrid",
            "--json",
            "--config",
        ])
        .arg(&config_path)
        .args(["--skills-dir"])
        .arg(&skills_dir)
        .assert()
        .success();

    distill_cmd(dir.path())
        .args(["convert", "verify", "playwright", "--json", "--config"])
        .arg(&config_path)
        .args(["--skills-dir"])
        .arg(&skills_dir)
        .assert()
        .success()
        .stdout(predicate::str::contains("\"passed\": true"))
        .stdout(predicate::str::contains("\"introspection_ok\": true"));
}

#[test]
fn test_convert_one_shot_defaults_to_hybrid_for_replace_candidates() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("settings.json");
    let skills_dir = dir.path().join("skills");
    let mock_mcp = dir.path().join("mock-mcp.sh");
    std::fs::write(
        &mock_mcp,
        "#!/bin/sh\nread _\nread _\nprintf '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":\"2025-03-26\",\"capabilities\":{}}}\\n'\nprintf '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"tools\":[{\"name\":\"execute\"}]}}\\n'\n",
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
      "description": "Read-only browser helpers",
      "readOnly": true
    }}
  }}
}}"#,
            mock_mcp.display()
        ),
    )
    .unwrap();

    distill_cmd(dir.path())
        .args(["convert", "playwright", "--json", "--config"])
        .arg(&config_path)
        .args(["--skills-dir"])
        .arg(&skills_dir)
        .assert()
        .success()
        .stdout(predicate::str::contains("\"safe_mode_downgrade\": true"))
        .stdout(predicate::str::contains("\"applied_mode\": \"hybrid\""))
        .stdout(predicate::str::contains("\"verify_passed\": true"));

    let updated = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        updated.contains("playwright"),
        "one-shot default mode must not mutate MCP config"
    );
}

#[test]
fn test_convert_one_shot_replace_requires_yes() {
    let dir = tempfile::tempdir().unwrap();
    distill_cmd(dir.path())
        .args(["convert", "playwright", "--replace"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--replace requires --yes"));
}

#[test]
fn test_convert_one_shot_replace_mutates_config_when_confirmed() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("settings.json");
    let skills_dir = dir.path().join("skills");
    let mock_mcp = dir.path().join("mock-mcp.sh");
    std::fs::write(
        &mock_mcp,
        "#!/bin/sh\nread _\nread _\nprintf '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":\"2025-03-26\",\"capabilities\":{}}}\\n'\nprintf '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"tools\":[{\"name\":\"execute\"}]}}\\n'\n",
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
      "description": "Read-only browser helpers",
      "readOnly": true
    }}
  }}
}}"#,
            mock_mcp.display()
        ),
    )
    .unwrap();

    distill_cmd(dir.path())
        .args([
            "convert",
            "playwright",
            "--replace",
            "--yes",
            "--json",
            "--config",
        ])
        .arg(&config_path)
        .args(["--skills-dir"])
        .arg(&skills_dir)
        .assert()
        .success()
        .stdout(predicate::str::contains("\"applied_mode\": \"replace\""))
        .stdout(predicate::str::contains("\"verified_in_apply\": true"));

    let updated = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        !updated.contains("playwright"),
        "one-shot replace should remove server from config"
    );

    let backup_count = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("settings.json.bak-")
        })
        .count();
    assert_eq!(backup_count, 1, "one-shot replace should create one backup");
}
