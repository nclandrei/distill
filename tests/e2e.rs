use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;

// ---------------------------------------------------------------------------
// Helper
// ---------------------------------------------------------------------------

/// Build a distill command with HOME set to a temp dir so tests
/// don't interact with the real ~/.distill config.
fn distill_cmd(home: &std::path::Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_distill"));
    cmd.env("HOME", home);
    cmd.env("DISTILL_SYSTEMCTL_PATH", "true");
    cmd.env("DISTILL_LAUNCHCTL_PATH", "true");
    cmd
}

/// Write a minimal but valid config.yaml under `<home>/.distill/config.yaml`.
fn seed_config(home: &std::path::Path) {
    seed_config_with(home, "claude", true, true);
}

fn seed_config_with(
    home: &std::path::Path,
    proposal_agent: &str,
    claude_enabled: bool,
    codex_enabled: bool,
) {
    let distill_dir = home.join(".distill");
    fs::create_dir_all(&distill_dir).unwrap();
    fs::write(
        distill_dir.join("config.yaml"),
        format!(
            "agents:\n  - name: claude\n    enabled: {claude_enabled}\n  - name: codex\n    \
             enabled: {codex_enabled}\nscan_interval: weekly\nproposal_agent: \
             {proposal_agent}\nshell: zsh\nnotifications: both\n"
        ),
    )
    .unwrap();
}

/// Seed N fake proposal `.md` files under `<home>/.distill/proposals/`.
fn seed_proposals(home: &std::path::Path, count: usize) {
    let proposals_dir = home.join(".distill").join("proposals");
    fs::create_dir_all(&proposals_dir).unwrap();
    for i in 0..count {
        fs::write(
            proposals_dir.join(format!("proposal-{i}.md")),
            format!(
                "---\ntype: new\nconfidence: high\ntarget_skill: null\nevidence: []\ncreated: 2026-03-02T00:00:00Z\n---\n\n# Skill {i}\n\nProposal body {i}.\n"
            ),
        )
        .unwrap();
    }
}

fn seed_preference_proposals(home: &std::path::Path, count: usize) {
    let proposals_dir = home.join(".distill").join("proposals");
    fs::create_dir_all(&proposals_dir).unwrap();
    for i in 0..count {
        fs::write(
            proposals_dir.join(format!("git-pref-{i}.md")),
            format!(
                "---\ntype: new\nconfidence: high\ntarget_skill: null\nevidence:\n  - session: /tmp/session-{i}.jsonl\n    pattern: Repeated git rebase workflow\ncreated: 2026-03-02T00:00:00Z\n---\n\n# Git Rebase Workflow {i}\n\n## When to use\nWhen cleaning feature branches.\n\n## Steps\n1. Fetch latest main.\n2. Rebase your branch.\n\n## Verification\nEnsure branch history looks linear.\n\n## Pitfalls\nAvoid rebasing shared branches.\n"
            ),
        )
        .unwrap();
    }
}

#[cfg(unix)]
fn write_executable_script(path: &std::path::Path, body: &str) {
    fs::write(path, body).unwrap();
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).unwrap();
}

// ---------------------------------------------------------------------------
// Test 1 — status output includes configured agents and scan interval
// ---------------------------------------------------------------------------

/// Seed a config.yaml, run `distill status`, and verify the output contains
/// the agent names, scan interval, and the status header.
#[test]
fn test_e2e_status_shows_config() {
    let dir = tempfile::tempdir().unwrap();
    seed_config(dir.path());

    distill_cmd(dir.path())
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("distill status"))
        .stdout(predicate::str::contains("claude"))
        .stdout(predicate::str::contains("codex"))
        .stdout(predicate::str::contains("weekly"));
}

// ---------------------------------------------------------------------------
// Test 2 — notify --check reports pending proposals
// ---------------------------------------------------------------------------

/// Seed two proposal files, run `distill notify --check`, and verify that the
/// output mentions the correct count.
#[test]
fn test_e2e_notify_with_proposals() {
    let dir = tempfile::tempdir().unwrap();
    seed_proposals(dir.path(), 2);

    distill_cmd(dir.path())
        .args(["notify", "--check"])
        .assert()
        .success()
        .stdout(predicate::str::contains("2 new proposals ready"))
        .stdout(predicate::str::contains("distill review"));
}

/// A single proposal should use the singular form "proposal" (no trailing 's').
#[test]
fn test_e2e_notify_single_proposal() {
    let dir = tempfile::tempdir().unwrap();
    seed_proposals(dir.path(), 1);

    distill_cmd(dir.path())
        .args(["notify", "--check"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 new proposal ready"));
}

// ---------------------------------------------------------------------------
// Test 3 — notify --check is silent when there are no proposals
// ---------------------------------------------------------------------------

/// With no proposals directory present, `notify --check` must exit 0 and
/// produce no output.
#[test]
fn test_e2e_notify_no_proposals() {
    let dir = tempfile::tempdir().unwrap();
    // Deliberately do NOT create the proposals directory.

    distill_cmd(dir.path())
        .args(["notify", "--check"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
}

/// With an empty proposals directory, `notify --check` must also be silent.
#[test]
fn test_e2e_notify_empty_proposals_dir() {
    let dir = tempfile::tempdir().unwrap();
    // Create the directory but leave it empty.
    fs::create_dir_all(dir.path().join(".distill").join("proposals")).unwrap();

    distill_cmd(dir.path())
        .args(["notify", "--check"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
}

// ---------------------------------------------------------------------------
// Test 4 — scan --now attempts to run and fails on config, not agent
// ---------------------------------------------------------------------------

/// With no config file present, `scan --now` must fail and the error message
/// must mention config (not an unexpected panic or missing binary).
#[test]
fn test_e2e_scan_no_config_error_mentions_config() {
    let dir = tempfile::tempdir().unwrap();
    // Deliberately do NOT seed a config.

    distill_cmd(dir.path())
        .args(["scan", "--now"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("No config found"));
}

/// With a valid config but no actual agent session files in the temp HOME,
/// `scan --now` exits successfully with a "no sessions" message (the scan
/// engine short-circuits before invoking the real agent binary).
#[test]
fn test_e2e_scan_creates_proposals_dir() {
    let dir = tempfile::tempdir().unwrap();
    seed_config(dir.path());

    // No real ~/.claude/projects/ or ~/.codex/sessions/ exist in the temp
    // HOME so both adapters return empty session lists.  The scan engine
    // records a watermark and exits cleanly without invoking the agent binary.
    distill_cmd(dir.path())
        .args(["scan", "--now"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "No pending sessions found for scan.",
        ));

    // The .distill directory must have been created by Config::ensure_dirs().
    assert!(
        dir.path().join(".distill").is_dir(),
        ".distill directory was not created"
    );
}

/// With a valid config, plain `scan` (without `--now`) should execute the
/// scheduled scan path and still run the scan engine.
#[test]
fn test_e2e_scan_without_now_runs_scheduled_path() {
    let dir = tempfile::tempdir().unwrap();
    seed_config(dir.path());

    distill_cmd(dir.path())
        .arg("scan")
        .assert()
        .success()
        .stdout(predicate::str::contains("running scheduled scan"))
        .stdout(predicate::str::contains(
            "No pending sessions found for scan.",
        ));

    assert!(
        dir.path().join(".distill").join("last-scan.json").is_file(),
        "last-scan.json should be written after scheduled scan path"
    );
}

/// Full Codex proposal-agent path:
///  1. Configure `proposal_agent: codex`
///  2. Seed one fake Codex session file
///  3. Put a mock `codex` executable at the front of PATH
///  4. Run `scan --now` and verify a proposal is written
#[cfg(unix)]
#[test]
fn test_e2e_scan_codex_proposal_agent_with_schema_enforcement() {
    let dir = tempfile::tempdir().unwrap();
    seed_config_with(dir.path(), "codex", false, true);

    let sessions_dir = dir.path().join(".codex").join("sessions");
    fs::create_dir_all(&sessions_dir).unwrap();
    fs::write(
        sessions_dir.join("session-1.jsonl"),
        r#"{"type":"message","role":"user","content":"extract workflow"}"#,
    )
    .unwrap();

    let bin_dir = dir.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let mock_codex = bin_dir.join("codex");
    let script = r##"#!/bin/sh
schema_file=""
last_message_file=""
sandbox=""
cd_dir=""
saw_json=0
saw_exec=0
while [ $# -gt 0 ]; do
  case "$1" in
    exec)
      saw_exec=1
      shift
      ;;
    --json)
      saw_json=1
      shift
      ;;
    --sandbox)
      sandbox="$2"
      shift 2
      ;;
    -C|--cd)
      cd_dir="$2"
      shift 2
      ;;
    --output-schema)
      schema_file="$2"
      shift 2
      ;;
    --output-last-message|-o)
      last_message_file="$2"
      shift 2
      ;;
    *)
      shift
      ;;
  esac
done
cat > /dev/null
[ "$saw_exec" -eq 1 ] || exit 31
[ "$saw_json" -eq 1 ] || exit 32
[ "$sandbox" = "read-only" ] || exit 33
[ -n "$cd_dir" ] || exit 34
[ -n "$schema_file" ] || exit 32
[ -f "$schema_file" ] || exit 33
grep -q '"proposals"' "$schema_file" || exit 34
[ -n "$last_message_file" ] || exit 35
staged_session="$(find "$cd_dir/sessions" -name '*.jsonl' | head -n 1)"
[ -n "$staged_session" ] || exit 36
printf '%s\n' "{\"type\":\"item.completed\",\"item\":{\"type\":\"command_execution\",\"command\":\"sed -n '1,40p' $staged_session\"}}"
printf '%s' "{\"inspected_files\":[\"$staged_session\"],\"file_findings\":[{\"session\":\"$staged_session\",\"summary\":\"repeated shell workflow\"}],\"proposals\":[{\"type\":\"new\",\"confidence\":\"high\",\"target_skill\":null,\"evidence\":[{\"session\":\"$staged_session\",\"pattern\":\"repeated shell workflow\"}],\"body\":\"# Codex Skill\\n\\nUse codex scanner defaults.\"}]}" > "$last_message_file"
"##;
    fs::write(&mock_codex, script).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&mock_codex, fs::Permissions::from_mode(0o755)).unwrap();
    }

    let path_env = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    distill_cmd(dir.path())
        .env("PATH", path_env)
        .args(["scan", "--now"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Found 1 session(s) to analyze."))
        .stdout(predicate::str::contains(
            "Inspecting 1 staged session file(s)",
        ))
        .stdout(predicate::str::contains("Agent proposed 1 skill(s)."));

    let proposals_dir = dir.path().join(".distill").join("proposals");
    let proposal_paths: Vec<_> = fs::read_dir(&proposals_dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|entry| entry.path()))
        .collect();
    assert_eq!(
        proposal_paths.len(),
        1,
        "expected one generated proposal file"
    );

    let proposal_text = fs::read_to_string(&proposal_paths[0]).unwrap();
    assert!(proposal_text.contains("Codex Skill"));
    assert!(proposal_text.contains("type: new"));
    assert!(proposal_text.contains("session-1.jsonl"));

    let watermark = fs::read_to_string(dir.path().join(".distill").join("last-scan.json")).unwrap();
    assert!(watermark.contains("timestamp"));
    assert!(
        !dir.path()
            .join(".distill")
            .join("scan-backlog.json")
            .exists()
    );
}

#[cfg(unix)]
#[test]
fn test_e2e_scan_reports_existing_shared_skills_and_manifest_flow() {
    let dir = tempfile::tempdir().unwrap();
    seed_config_with(dir.path(), "fake-proposal-agent.sh", true, false);

    let sessions_dir = dir.path().join(".claude").join("projects").join("demo");
    fs::create_dir_all(&sessions_dir).unwrap();
    fs::write(
        sessions_dir.join("session-1.jsonl"),
        r#"{"timestamp":"2026-03-10T12:00:00Z","role":"user","content":"extract workflow"}"#,
    )
    .unwrap();

    let distill_skills_dir = dir.path().join(".distill").join("skills");
    fs::create_dir_all(&distill_skills_dir).unwrap();
    fs::write(
        distill_skills_dir.join("debugging.md"),
        "# Debugging\nRead the error carefully.",
    )
    .unwrap();

    let shared_skill_dir = dir.path().join(".agents").join("skills").join("review");
    fs::create_dir_all(&shared_skill_dir).unwrap();
    fs::write(
        shared_skill_dir.join("SKILL.md"),
        "# Review\nLook for regressions first.",
    )
    .unwrap();

    let bin_dir = dir.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let fake_agent = bin_dir.join("fake-proposal-agent.sh");
    write_executable_script(
        &fake_agent,
        r#"#!/bin/sh
prompt_file="$HOME/.distill/last-scan-prompt.txt"
cat > "$prompt_file"
staged_session="$(find "$PWD/sessions" -name '*.jsonl' | head -n 1)"
[ -n "$staged_session" ] || exit 21
printf '%s' "{\"inspected_files\":[\"$staged_session\"],\"file_findings\":[{\"session\":\"$staged_session\",\"summary\":\"No reusable signal.\"}],\"proposals\":[]}"
"#,
    );

    let path_env = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    distill_cmd(dir.path())
        .env("PATH", path_env)
        .args(["scan", "--now"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Found 1 session(s) to analyze."))
        .stdout(predicate::str::contains("Loaded 2 existing skill(s)."))
        .stdout(predicate::str::contains(
            "Inspecting 1 staged session file(s)",
        ))
        .stdout(predicate::str::contains("Agent proposed 0 skill(s)."));

    let prompt = fs::read_to_string(dir.path().join(".distill/last-scan-prompt.txt")).unwrap();
    assert!(prompt.contains("Candidate Session Files"));
    assert!(prompt.contains("manifest.json"));
    assert!(prompt.contains("review"));
    assert!(!prompt.contains("Session Excerpts"));
    assert!(!prompt.contains("extract workflow"));
}

#[cfg(unix)]
#[test]
fn test_e2e_preference_learning_roundtrip_review_to_scan_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let bin_dir = dir.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();

    let fake_agent = bin_dir.join("fake-proposal-agent.sh");
    write_executable_script(
        &fake_agent,
        r#"#!/bin/sh
prompt_file="$HOME/.distill/last-scan-prompt.txt"
cat > "$prompt_file"
staged_session="$(find "$PWD/sessions" -name '*.jsonl' | head -n 1)"
[ -n "$staged_session" ] || exit 22
printf '%s' "{\"inspected_files\":[\"$staged_session\"],\"file_findings\":[{\"session\":\"$staged_session\",\"summary\":\"Repeated git workflow.\"}],\"proposals\":[]}"
"#,
    );

    seed_config_with(
        dir.path(),
        fake_agent.to_string_lossy().as_ref(),
        true,
        false,
    );
    seed_preference_proposals(dir.path(), 3);

    let review_json = dir.path().join("review.json");
    distill_cmd(dir.path())
        .args(["review", "--write-json"])
        .arg(&review_json)
        .assert()
        .success();

    let mut spec: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&review_json).unwrap()).unwrap();
    let proposals = spec["proposals"]
        .as_array_mut()
        .expect("review json must contain proposals array");
    for proposal in proposals {
        proposal["decision"] = serde_json::Value::String("accept".to_string());
    }
    fs::write(&review_json, serde_json::to_string_pretty(&spec).unwrap()).unwrap();

    distill_cmd(dir.path())
        .args(["review", "--apply-json"])
        .arg(&review_json)
        .assert()
        .success()
        .stdout(predicate::str::contains("Accepted : 3"));

    let history = fs::read_to_string(dir.path().join(".distill/history/decisions.jsonl")).unwrap();
    assert!(history.contains("\"proposal_type\":\"new\""));
    assert!(history.contains("\"target_kind\":\"skill\""));
    assert!(history.contains("\"git\""));

    let sessions_dir = dir
        .path()
        .join(".claude")
        .join("projects")
        .join("project-a");
    fs::create_dir_all(&sessions_dir).unwrap();
    fs::write(
        sessions_dir.join("session-1.jsonl"),
        r#"{"timestamp":"2026-03-06T10:00:00Z","role":"user","text":"reuse git rebase flow"}"#,
    )
    .unwrap();

    distill_cmd(dir.path())
        .args(["scan", "--now"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Loaded preference history: 3 reviewed decision(s)",
        ));

    let prompt = fs::read_to_string(dir.path().join(".distill/last-scan-prompt.txt")).unwrap();
    assert!(prompt.contains("Learned Preferences From Past Reviews"));
    assert!(prompt.contains("Prioritize categories the user usually accepts"));
    assert!(prompt.contains("git (accepted 3, rejected 0)"));
    assert!(prompt.contains("Candidate Session Files"));
    assert!(!prompt.contains("reuse git rebase flow"));
}

#[cfg(unix)]
#[test]
fn test_e2e_scan_claude_stream_json_with_coverage() {
    let dir = tempfile::tempdir().unwrap();
    seed_config_with(dir.path(), "claude", true, false);

    let sessions_dir = dir.path().join(".claude").join("projects").join("demo");
    fs::create_dir_all(&sessions_dir).unwrap();
    fs::write(
        sessions_dir.join("session-1.jsonl"),
        r#"{"timestamp":"2026-03-10T12:00:00Z","role":"user","content":"repeat the release checklist"}"#,
    )
    .unwrap();

    let bin_dir = dir.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let mock_claude = bin_dir.join("claude");
    write_executable_script(
        &mock_claude,
        r##"#!/bin/sh
cat > /dev/null
staged_session="$(find "$PWD/sessions" -name '*.jsonl' | head -n 1)"
[ -n "$staged_session" ] || exit 41
printf '%s\n' "{\"type\":\"tool_use\",\"tool\":\"Read\",\"path\":\"$staged_session\"}"
printf '%s\n' "{\"type\":\"result\",\"structured_output\":{\"inspected_files\":[\"$staged_session\"],\"file_findings\":[{\"session\":\"$staged_session\",\"summary\":\"Repeated release checklist.\"}],\"proposals\":[{\"type\":\"new\",\"confidence\":\"medium\",\"target_skill\":null,\"evidence\":[{\"session\":\"$staged_session\",\"pattern\":\"Repeated release checklist.\"}],\"body\":\"# Release Checklist\\n\\nUse a consistent release checklist.\"}]}}"
"##,
    );

    let path_env = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    distill_cmd(dir.path())
        .env("PATH", path_env)
        .args(["scan", "--now"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Inspecting 1 staged session file(s)",
        ))
        .stdout(predicate::str::contains("Agent proposed 1 skill(s)."));

    let proposals_dir = dir.path().join(".distill").join("proposals");
    let proposal_paths: Vec<_> = fs::read_dir(&proposals_dir)
        .unwrap()
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .collect();
    assert_eq!(proposal_paths.len(), 1);
    let proposal_text = fs::read_to_string(&proposal_paths[0]).unwrap();
    assert!(proposal_text.contains("Release Checklist"));
    assert!(proposal_text.contains("session-1.jsonl"));
}

#[cfg(unix)]
#[test]
fn test_e2e_scan_rejects_missing_coverage_and_keeps_backlog() {
    let dir = tempfile::tempdir().unwrap();
    seed_config_with(dir.path(), "fake-proposal-agent.sh", true, false);

    let sessions_dir = dir.path().join(".claude").join("projects").join("demo");
    fs::create_dir_all(&sessions_dir).unwrap();
    fs::write(
        sessions_dir.join("session-1.jsonl"),
        r#"{"timestamp":"2026-03-10T12:00:00Z","role":"user","content":"extract workflow"}"#,
    )
    .unwrap();

    let bin_dir = dir.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let fake_agent = bin_dir.join("fake-proposal-agent.sh");
    write_executable_script(
        &fake_agent,
        r#"#!/bin/sh
cat > /dev/null
printf '%s' '{"inspected_files":[],"file_findings":[],"proposals":[]}'
"#,
    );

    let path_env = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    distill_cmd(dir.path())
        .env("PATH", path_env)
        .args(["scan", "--now"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("inspected_files"));

    let backlog = fs::read_to_string(dir.path().join(".distill/scan-backlog.json")).unwrap();
    assert!(backlog.contains("session-1.jsonl"));
}

#[cfg(unix)]
#[test]
fn test_e2e_scan_first_run_uses_newest_batch_then_drains_backlog() {
    let dir = tempfile::tempdir().unwrap();
    seed_config_with(dir.path(), "fake-proposal-agent.sh", true, false);

    let sessions_dir = dir.path().join(".claude").join("projects").join("demo");
    fs::create_dir_all(&sessions_dir).unwrap();
    for (name, touch_time) in [
        ("session-old.jsonl", "202603101200"),
        ("session-mid.jsonl", "202603101300"),
        ("session-new.jsonl", "202603101400"),
    ] {
        let path = sessions_dir.join(name);
        fs::write(&path, r#"{"role":"user","content":"repeat workflow"}"#).unwrap();
        let status = std::process::Command::new("touch")
            .args(["-t", touch_time, path.to_str().unwrap()])
            .status()
            .unwrap();
        assert!(status.success());
    }

    let bin_dir = dir.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let fake_agent = bin_dir.join("fake-proposal-agent.sh");
    write_executable_script(
        &fake_agent,
        r#"#!/bin/sh
cat > /dev/null
staged_session="$(find "$PWD/sessions" -name '*.jsonl' | head -n 1)"
[ -n "$staged_session" ] || exit 51
basename "$staged_session" >> "$HOME/.distill/seen-batches.txt"
printf '%s' "{\"inspected_files\":[\"$staged_session\"],\"file_findings\":[{\"session\":\"$staged_session\",\"summary\":\"Covered.\"}],\"proposals\":[]}"
"#,
    );

    let path_env = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    distill_cmd(dir.path())
        .env("PATH", &path_env)
        .env("DISTILL_SCAN_BATCH_SIZE", "1")
        .args(["scan", "--now"])
        .assert()
        .success();

    distill_cmd(dir.path())
        .env("PATH", &path_env)
        .env("DISTILL_SCAN_BATCH_SIZE", "1")
        .args(["scan", "--now"])
        .assert()
        .success();

    let seen = fs::read_to_string(dir.path().join(".distill/seen-batches.txt")).unwrap();
    let lines = seen.lines().collect::<Vec<_>>();
    assert_eq!(
        lines,
        vec!["0001-session-new.jsonl", "0001-session-mid.jsonl"]
    );
}

#[cfg(unix)]
#[test]
fn test_e2e_scan_debug_dir_captures_workspace_and_outputs() {
    let dir = tempfile::tempdir().unwrap();
    seed_config_with(dir.path(), "fake-proposal-agent.sh", true, false);

    let sessions_dir = dir.path().join(".claude").join("projects").join("demo");
    fs::create_dir_all(&sessions_dir).unwrap();
    fs::write(
        sessions_dir.join("session-1.jsonl"),
        r#"{"timestamp":"2026-03-10T12:00:00Z","role":"user","content":"extract workflow"}"#,
    )
    .unwrap();

    let bin_dir = dir.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let fake_agent = bin_dir.join("fake-proposal-agent.sh");
    write_executable_script(
        &fake_agent,
        r#"#!/bin/sh
cat > /dev/null
staged_session="$(find "$PWD/sessions" -name '*.jsonl' | head -n 1)"
[ -n "$staged_session" ] || exit 61
printf '%s' "{\"inspected_files\":[\"$staged_session\"],\"file_findings\":[{\"session\":\"$staged_session\",\"summary\":\"Covered.\"}],\"proposals\":[]}"
"#,
    );

    let debug_dir = dir.path().join("scan-debug");
    let path_env = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    distill_cmd(dir.path())
        .env("PATH", path_env)
        .env("DISTILL_SCAN_DEBUG_DIR", &debug_dir)
        .args(["scan", "--now"])
        .assert()
        .success();

    let run_dirs: Vec<_> = fs::read_dir(&debug_dir)
        .unwrap()
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .collect();
    assert_eq!(run_dirs.len(), 1);
    let run_dir = &run_dirs[0];
    assert!(run_dir.join("prompt.txt").is_file());
    assert!(run_dir.join("agent-stdout.log").is_file());
    assert!(run_dir.join("parsed-response.json").is_file());
    assert!(run_dir.join("workspace/manifest.json").is_file());
}
// ---------------------------------------------------------------------------
// Test 5 — full flow: seed config → verify status → verify proposals → verify notify
// ---------------------------------------------------------------------------

/// End-to-end flow:
///  1. Seed config
///  2. `distill status` reports correct agent / interval
///  3. Seed 3 proposals
///  4. `distill status` reports 3 pending proposals
///  5. `distill notify --check` reports the same 3 proposals
#[test]
fn test_e2e_full_flow_status_after_config() {
    let dir = tempfile::tempdir().unwrap();
    seed_config(dir.path());

    // Step 1: status with config but no proposals
    distill_cmd(dir.path())
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("distill status"))
        .stdout(predicate::str::contains("Pending proposals: 0"))
        .stdout(predicate::str::contains("Last scan:         never"));

    // Step 2: seed proposals on disk
    seed_proposals(dir.path(), 3);

    // Step 3: status now reflects 3 pending proposals
    distill_cmd(dir.path())
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("Pending proposals: 3"));

    // Step 4: notify --check also sees the 3 proposals
    distill_cmd(dir.path())
        .args(["notify", "--check"])
        .assert()
        .success()
        .stdout(predicate::str::contains("3 new proposals ready"));
}
