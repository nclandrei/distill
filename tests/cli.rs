use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::path::PathBuf;

/// Build a distill command with HOME set to a temp dir so tests
/// don't interact with the real ~/.distill config.
fn distill_cmd(home: &std::path::Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_distill"));
    cmd.env("HOME", home);
    cmd.env("DISTILL_SYSTEMCTL_PATH", "true");
    cmd.env("DISTILL_LAUNCHCTL_PATH", "true");
    cmd
}

fn write_minimal_config(home: &std::path::Path, proposal_agent: &str) {
    let distill_dir = home.join(".distill");
    std::fs::create_dir_all(&distill_dir).unwrap();
    std::fs::write(
        distill_dir.join("config.yaml"),
        format!(
            "agents:\n  - name: claude\n    enabled: false\n  - name: codex\n    enabled: false\nscan_interval: weekly\nproposal_agent: {proposal_agent}\nshell: zsh\nnotifications: both\n"
        ),
    )
    .unwrap();
}

fn init_git_repo(path: &std::path::Path) {
    std::fs::create_dir_all(path).unwrap();
    let status = std::process::Command::new("git")
        .arg("init")
        .arg(path)
        .status()
        .unwrap();
    assert!(status.success(), "failed to init git repo");
}

fn write_agent_script(path: &std::path::Path, body: &str) {
    std::fs::write(path, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).unwrap();
    }
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
    assert!(content.contains("kind: skill"));
    assert!(content.contains("name: beta.md"));
}

#[test]
fn test_sync_agents_list_configured_empty() {
    let dir = tempfile::tempdir().unwrap();
    write_minimal_config(dir.path(), "true");

    distill_cmd(dir.path())
        .args(["sync-agents", "--list-configured"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "No configured sync-agents projects",
        ));
}

#[test]
fn test_sync_agents_all_configured_errors_when_empty() {
    let dir = tempfile::tempdir().unwrap();
    write_minimal_config(dir.path(), "true");

    distill_cmd(dir.path())
        .args(["sync-agents", "--all-configured"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "No configured sync-agents projects",
        ));
}

#[test]
fn test_sync_agents_save_projects_persists_allowlist() {
    let dir = tempfile::tempdir().unwrap();
    let script_path = dir.path().join("fake-agent.sh");
    write_agent_script(
        &script_path,
        "#!/bin/sh\ncat > /dev/null\nprintf '%s' '{\"proposals\":[]}'\n",
    );
    write_minimal_config(dir.path(), script_path.to_string_lossy().as_ref());

    let project = dir.path().join("repo-one");
    init_git_repo(&project);

    distill_cmd(dir.path())
        .args(["sync-agents", "--projects"])
        .arg(project.display().to_string())
        .arg("--save-projects")
        .arg("--dry-run")
        .assert()
        .success()
        .stdout(predicate::str::contains("Saved 1 project(s)"));

    let config = std::fs::read_to_string(dir.path().join(".distill").join("config.yaml")).unwrap();
    assert!(config.contains("sync_agents:"));
    assert!(config.contains(project.display().to_string().as_str()));
}

#[test]
fn test_sync_agents_dry_run_does_not_write_proposals() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("repo-two");
    init_git_repo(&project);

    let agents_path = project.join("AGENTS.md");
    let script_path = dir.path().join("fake-agent-proposal.sh");
    write_agent_script(
        &script_path,
        &format!(
            "#!/bin/sh\ncat > /dev/null\nprintf '%s' '{{\"proposals\":[{{\"type\":\"edit\",\"confidence\":\"high\",\"target\":{{\"kind\":\"file\",\"path\":\"{}\"}},\"evidence\":[{{\"session\":\"s1\",\"pattern\":\"p1\"}}],\"body\":\"# AGENTS\\\\n\\\\nUpdated\"}}]}}'\n",
            agents_path.display()
        ),
    );
    write_minimal_config(dir.path(), script_path.to_string_lossy().as_ref());

    distill_cmd(dir.path())
        .args(["sync-agents", "--projects"])
        .arg(project.display().to_string())
        .arg("--dry-run")
        .assert()
        .success()
        .stdout(predicate::str::contains("Dry run"));

    let proposals_dir = dir.path().join(".distill").join("proposals");
    assert!(!proposals_dir.exists() || std::fs::read_dir(proposals_dir).unwrap().next().is_none());
}

#[test]
fn test_watch_install_writes_scheduled_run_command() {
    let dir = tempfile::tempdir().unwrap();
    write_minimal_config(dir.path(), "true");

    distill_cmd(dir.path())
        .args(["watch", "--install"])
        .assert()
        .success();

    let scheduler_path: PathBuf = if cfg!(target_os = "linux") {
        dir.path()
            .join(".config")
            .join("systemd")
            .join("user")
            .join("distill.service")
    } else {
        dir.path()
            .join("Library")
            .join("LaunchAgents")
            .join("com.distill.agent.plist")
    };

    let content = std::fs::read_to_string(&scheduler_path).unwrap();
    assert!(
        content.contains("scheduled-run"),
        "scheduler should run scheduled-run command"
    );
}
