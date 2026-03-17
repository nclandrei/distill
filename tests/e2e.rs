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
    seed_config_with_all(home, proposal_agent, claude_enabled, codex_enabled, false);
}

fn seed_config_with_all(
    home: &std::path::Path,
    proposal_agent: &str,
    claude_enabled: bool,
    codex_enabled: bool,
    opencode_enabled: bool,
) {
    let distill_dir = home.join(".distill");
    fs::create_dir_all(&distill_dir).unwrap();
    fs::write(
        distill_dir.join("config.yaml"),
        format!(
            "agents:\n  - name: claude\n    enabled: {claude_enabled}\n  - name: codex\n    \
             enabled: {codex_enabled}\n  - name: opencode\n    enabled: {opencode_enabled}\nscan_interval: weekly\nproposal_agent: \
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

#[cfg(unix)]
fn init_git_repo(path: &std::path::Path) {
    fs::create_dir_all(path).unwrap();
    let status = std::process::Command::new("git")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .arg("init")
        .arg(path)
        .status()
        .unwrap();
    assert!(status.success(), "failed to init git repo");

    for args in [
        [
            "-C",
            path.to_str().unwrap(),
            "config",
            "user.name",
            "Distill Test",
        ],
        [
            "-C",
            path.to_str().unwrap(),
            "config",
            "user.email",
            "distill@example.com",
        ],
    ] {
        let status = std::process::Command::new("git")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "failed to configure git repo");
    }
}

#[cfg(unix)]
fn commit_all(path: &std::path::Path, message: &str) {
    let add_status = std::process::Command::new("git")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .args(["-C", path.to_str().unwrap(), "add", "."])
        .status()
        .unwrap();
    assert!(add_status.success(), "failed to stage git changes");

    let commit_status = std::process::Command::new("git")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .args(["-C", path.to_str().unwrap(), "commit", "-m", message])
        .status()
        .unwrap();
    assert!(commit_status.success(), "failed to create git commit");
}

#[cfg(unix)]
fn codex_message_line(role: &str, text: &str) -> String {
    serde_json::json!({
        "type": "response_item",
        "payload": {
            "type": "message",
            "role": role,
            "content": [{"type": if role == "user" { "input_text" } else { "output_text" }, "text": text}]
        }
    })
    .to_string()
}

#[cfg(unix)]
fn codex_command_line(command: &str) -> String {
    serde_json::json!({
        "type": "item.completed",
        "item": {
            "type": "command_execution",
            "command": command
        }
    })
    .to_string()
}

#[cfg(unix)]
fn write_codex_session(
    home: &std::path::Path,
    session_name: &str,
    project: &str,
    touch_time: &str,
    filler_before: usize,
    filler_after: usize,
    filler_width: usize,
    include_workflow: bool,
) -> std::path::PathBuf {
    let sessions_dir = home
        .join(".codex")
        .join("sessions")
        .join("2026")
        .join("03")
        .join("12");
    fs::create_dir_all(&sessions_dir).unwrap();
    let path = sessions_dir.join(format!("{session_name}.jsonl"));

    let mut lines = vec![
        serde_json::json!({
            "type": "session_meta",
            "payload": { "cwd": format!("/Users/test/code/{project}") }
        })
        .to_string(),
    ];

    for index in 0..filler_before {
        let filler = format!(
            "{project} before {index} {}",
            "x".repeat(filler_width.max(1))
        );
        lines.push(codex_message_line("user", &filler));
        lines.push(codex_message_line(
            "assistant",
            &format!("Investigating {project} before {index}"),
        ));
    }

    if include_workflow {
        lines.push(codex_message_line(
            "user",
            &format!("Wrap this up in {project} and land it cleanly."),
        ));
        lines.push(codex_command_line("jj land"));
        lines.push(codex_command_line(&format!("cargo test -p {project}")));
        lines.push(codex_message_line(
            "assistant",
            &format!("Landed {project} changes and verified tests."),
        ));
    } else {
        lines.push(codex_message_line(
            "user",
            &format!("Keep exploring unrelated {project} copy tweaks."),
        ));
        lines.push(codex_command_line("sed -n '1,40p' README.md"));
        lines.push(codex_message_line(
            "assistant",
            &format!("Reviewed unrelated {project} docs."),
        ));
    }

    for index in 0..filler_after {
        let filler = format!(
            "{project} after {index} {}",
            "y".repeat(filler_width.max(1))
        );
        lines.push(codex_message_line("user", &filler));
        lines.push(codex_message_line(
            "assistant",
            &format!("Following up on {project} after {index}"),
        ));
    }

    fs::write(&path, format!("{}\n", lines.join("\n"))).unwrap();
    let status = std::process::Command::new("touch")
        .args(["-t", touch_time, path.to_str().unwrap()])
        .status()
        .unwrap();
    assert!(status.success(), "failed to set session timestamp");
    path
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
        .stdout(predicate::str::contains("opencode"))
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

/// Full Codex detection-agent path:
///  1. Configure `proposal_agent: codex`
///  2. Seed one fake Codex session file
///  3. Put a mock `codex` executable at the front of PATH
///  4. Run `scan --now` and verify workflow findings are persisted
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
grep -q '"session_findings"' "$schema_file" || exit 34
[ -n "$last_message_file" ] || exit 35
staged_session="$(find "$cd_dir/sessions" -name '*.jsonl' | head -n 1)"
[ -n "$staged_session" ] || exit 36
printf '%s\n' "{\"type\":\"item.completed\",\"item\":{\"type\":\"command_execution\",\"command\":\"sed -n '1,40p' $staged_session\"}}"
printf '%s' "{\"inspected_files\":[\"$staged_session\"],\"session_findings\":[{\"session\":\"$staged_session\",\"summary\":\"repeated shell workflow\",\"candidates\":[{\"workflow_key\":\"jj-land-run-tests\",\"workflow_label\":\"land and test\",\"note\":\"Repeated landing and test workflow in the middle of the session.\",\"start_event\":1,\"end_event\":1}]}]}" > "$last_message_file"
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
            "Inspecting 1 staged session timeline file(s)",
        ))
        .stdout(predicate::str::contains("Agent proposed 0 skill(s)."));

    let scan_state = fs::read_to_string(dir.path().join(".distill/scan-state.json")).unwrap();
    assert!(scan_state.contains("jj-land-run-tests"));
    assert!(scan_state.contains("session-1.jsonl"));

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
printf '%s' "{\"inspected_files\":[\"$staged_session\"],\"session_findings\":[{\"session\":\"$staged_session\",\"summary\":\"No reusable signal.\",\"candidates\":[]}]}"
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
            "Inspecting 1 staged session timeline file(s)",
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
printf '%s\n' "{\"type\":\"result\",\"structured_output\":{\"inspected_files\":[\"$staged_session\"],\"session_findings\":[{\"session\":\"$staged_session\",\"summary\":\"Repeated release checklist.\",\"candidates\":[{\"workflow_key\":\"release-checklist\",\"workflow_label\":\"release checklist\",\"note\":\"Reusable release checklist workflow.\",\"start_event\":1,\"end_event\":1}]}]}}"
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
            "Inspecting 1 staged session timeline file(s)",
        ))
        .stdout(predicate::str::contains("Agent proposed 0 skill(s)."));

    let scan_state = fs::read_to_string(dir.path().join(".distill/scan-state.json")).unwrap();
    assert!(scan_state.contains("release-checklist"));
    assert!(scan_state.contains("session-1.jsonl"));
}

#[cfg(unix)]
#[test]
fn test_e2e_scan_codex_middle_workflow_across_projects_writes_proposal() {
    let dir = tempfile::tempdir().unwrap();
    seed_config_with(dir.path(), "fake-proposal-agent.sh", false, true);

    write_codex_session(
        dir.path(),
        "atlas-short",
        "atlas",
        "202603121100",
        1,
        1,
        16,
        true,
    );
    write_codex_session(
        dir.path(),
        "ios-long",
        "ios-app",
        "202603121200",
        12,
        10,
        180,
        true,
    );
    write_codex_session(
        dir.path(),
        "web-medium",
        "web-ui",
        "202603121300",
        4,
        4,
        80,
        true,
    );
    write_codex_session(
        dir.path(),
        "docs-noise",
        "docs-site",
        "202603121400",
        6,
        6,
        90,
        false,
    );
    write_codex_session(
        dir.path(),
        "copy-noise",
        "marketing",
        "202603121500",
        3,
        2,
        120,
        false,
    );

    let bin_dir = dir.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let fake_agent = bin_dir.join("fake-proposal-agent.sh");
    write_executable_script(
        &fake_agent,
        r##"#!/bin/sh
prompt="$(cat)"
sessions="$(find "$PWD/sessions" -name '*.jsonl' | sort)"
[ -n "$sessions" ] || exit 71

if printf '%s' "$prompt" | grep -q "workflow detection engine"; then
  printf '{"inspected_files":['
  first=1
  for file in $sessions; do
    [ $first -eq 0 ] && printf ','
    printf '"%s"' "$file"
    first=0
  done
  printf '],"session_findings":['
  first=1
  for file in $sessions; do
    [ $first -eq 0 ] && printf ','
    if grep -q "COMMAND: jj land" "$file" && grep -q "COMMAND: cargo test" "$file"; then
      candidates='[{"workflow_key":"jj-land-run-tests","workflow_label":"land and run tests","note":"Repeated landing and verification workflow.","start_event":1,"end_event":999}]'
      summary='Landing workflow detected.'
    else
      candidates='[]'
      summary='Noise-only session.'
    fi
    printf '{"session":"%s","summary":"%s","candidates":%s}' "$file" "$summary" "$candidates"
    first=0
  done
  printf ']}'
else
  printf '{"inspected_files":['
  first=1
  for file in $sessions; do
    [ $first -eq 0 ] && printf ','
    printf '"%s"' "$file"
    first=0
  done
  printf '],"file_findings":['
  first=1
  for file in $sessions; do
    [ $first -eq 0 ] && printf ','
    printf '{"session":"%s","summary":"Repeated landing and verification workflow."}' "$file"
    first=0
  done
  printf '],"proposals":[{"type":"new","confidence":"high","target_skill":null,"evidence":['
  first=1
  for file in $sessions; do
    [ $first -eq 0 ] && printf ','
    printf '{"session":"%s","pattern":"Repeated landing and verification workflow."}' "$file"
    first=0
  done
  printf '],"body":"# Land And Test\\n\\n## When to use\\nUse when wrapping up work by landing changes and running targeted verification.\\n\\n## Steps\\n1. Finish the change.\\n2. Run `jj land`.\\n3. Run the relevant test command.\\n4. Confirm the result.\\n\\n## Verification\\nCheck that the land command and tests both succeeded.\\n\\n## Pitfalls\\nDo not skip the test step after landing."}]}'
fi
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
            "workflow group(s) reached the proposal threshold",
        ))
        .stdout(predicate::str::contains("Agent proposed 1 skill(s)."));

    let proposals_dir = dir.path().join(".distill").join("proposals");
    let proposal_paths: Vec<_> = fs::read_dir(&proposals_dir)
        .unwrap()
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .collect();
    assert_eq!(proposal_paths.len(), 1);
    let proposal_text = fs::read_to_string(&proposal_paths[0]).unwrap();
    assert!(proposal_text.contains("Land And Test"));
    assert!(proposal_text.contains("atlas-short"));
    assert!(proposal_text.contains("ios-long"));
    assert!(proposal_text.contains("web-medium"));
    assert!(!proposal_text.contains("docs-noise"));
    assert!(!proposal_text.contains("copy-noise"));

    let scan_state = fs::read_to_string(dir.path().join(".distill/scan-state.json")).unwrap();
    assert!(scan_state.contains("jj-land-run-tests"));
}

#[cfg(unix)]
#[test]
fn test_e2e_scan_large_codex_backlog_respects_byte_cap_and_preserves_remaining_sessions() {
    let dir = tempfile::tempdir().unwrap();
    seed_config_with(dir.path(), "fake-proposal-agent.sh", false, true);

    for index in 0..18 {
        write_codex_session(
            dir.path(),
            &format!("bulk-{index:02}"),
            &format!("project-{index:02}"),
            &format!("20260311{:02}00", index + 1),
            4,
            4,
            220,
            false,
        );
    }

    let bin_dir = dir.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let fake_agent = bin_dir.join("fake-proposal-agent.sh");
    write_executable_script(
        &fake_agent,
        r#"#!/bin/sh
cat > /dev/null
sessions="$(find "$PWD/sessions" -name '*.jsonl' | sort)"
[ -n "$sessions" ] || exit 81
for file in $sessions; do
  basename "$file" >> "$HOME/.distill/seen-batches.txt"
done
printf '{"inspected_files":['
first=1
for file in $sessions; do
  [ $first -eq 0 ] && printf ','
  printf '"%s"' "$file"
  first=0
done
printf '],"session_findings":['
first=1
for file in $sessions; do
  [ $first -eq 0 ] && printf ','
  printf '{"session":"%s","summary":"Noise-only session.","candidates":[]}' "$file"
  first=0
done
printf ']}'
"#,
    );

    let path_env = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    distill_cmd(dir.path())
        .env("PATH", &path_env)
        .env("DISTILL_SCAN_BATCH_SIZE", "50")
        .env("DISTILL_SCAN_MAX_RAW_BYTES", "12000")
        .args(["scan", "--now"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Capped this scan to"));

    let seen_after_first =
        fs::read_to_string(dir.path().join(".distill/seen-batches.txt")).unwrap();
    let first_count = seen_after_first.lines().count();
    assert!(first_count > 0);
    assert!(first_count < 18);

    let backlog_after_first =
        fs::read_to_string(dir.path().join(".distill/scan-backlog.json")).unwrap();
    let backlog_count_first = serde_json::from_str::<serde_json::Value>(&backlog_after_first)
        .unwrap()["sessions"]
        .as_array()
        .unwrap()
        .len();
    assert!(backlog_count_first > 0);

    distill_cmd(dir.path())
        .env("PATH", &path_env)
        .env("DISTILL_SCAN_BATCH_SIZE", "50")
        .env("DISTILL_SCAN_MAX_RAW_BYTES", "12000")
        .args(["scan", "--now"])
        .assert()
        .success();

    let seen_after_second =
        fs::read_to_string(dir.path().join(".distill/seen-batches.txt")).unwrap();
    assert!(seen_after_second.lines().count() > first_count);

    let backlog_after_second =
        fs::read_to_string(dir.path().join(".distill/scan-backlog.json")).unwrap();
    let backlog_count_second = serde_json::from_str::<serde_json::Value>(&backlog_after_second)
        .unwrap()["sessions"]
        .as_array()
        .unwrap()
        .len();
    assert!(backlog_count_second < backlog_count_first);
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
fn test_e2e_scan_ignores_stale_backlog_session_paths() {
    let dir = tempfile::tempdir().unwrap();
    seed_config_with(dir.path(), "fake-proposal-agent.sh", false, true);

    let live_session_path = write_codex_session(
        dir.path(),
        "live-session",
        "demo",
        "202603121200",
        1,
        1,
        40,
        false,
    );
    let stale_session_path = dir
        .path()
        .join(".codex/sessions/2026/03/13/rollout-missing.jsonl");

    fs::write(
        dir.path().join(".distill/scan-backlog.json"),
        serde_json::json!({
            "sessions": [
                {
                    "id": stale_session_path.to_string_lossy(),
                    "agent": "codex",
                    "path": stale_session_path,
                    "timestamp": "2026-03-13T10:00:00Z",
                    "content": ""
                },
                {
                    "id": live_session_path.to_string_lossy(),
                    "agent": "codex",
                    "path": live_session_path,
                    "timestamp": "2026-03-12T12:00:00Z",
                    "content": ""
                }
            ]
        })
        .to_string(),
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
[ -n "$staged_session" ] || exit 71
printf '%s' "{\"inspected_files\":[\"$staged_session\"],\"session_findings\":[{\"session\":\"$staged_session\",\"summary\":\"Covered.\",\"candidates\":[]}]}"
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
        .success();

    assert!(
        !dir.path().join(".distill/scan-backlog.json").exists(),
        "stale backlog entries should be pruned once scan completes"
    );
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
fn test_e2e_scheduled_run_drains_multiple_batches_until_budget() {
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
        .env("DISTILL_SCHEDULED_RUN_MAX_BATCHES", "2")
        .args(["scheduled-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "continuing automatic backlog catch-up",
        ))
        .stdout(predicate::str::contains(
            "stopping automatic catch-up after 2 batch(es)",
        ));

    let seen = fs::read_to_string(dir.path().join(".distill/seen-batches.txt")).unwrap();
    let lines = seen.lines().collect::<Vec<_>>();
    assert_eq!(
        lines,
        vec!["0001-session-new.jsonl", "0001-session-mid.jsonl"]
    );

    let backlog = fs::read_to_string(dir.path().join(".distill/scan-backlog.json")).unwrap();
    let backlog_count = serde_json::from_str::<serde_json::Value>(&backlog).unwrap()["sessions"]
        .as_array()
        .unwrap()
        .len();
    assert_eq!(backlog_count, 1);
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
printf '%s' "{\"inspected_files\":[\"$staged_session\"],\"session_findings\":[{\"session\":\"$staged_session\",\"summary\":\"Covered.\",\"candidates\":[]}]}"
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
    assert!(run_dir.join("parsed-workflow-response.json").is_file());
    assert!(run_dir.join("scan-status.json").is_file());
    assert!(run_dir.join("workspace/manifest.json").is_file());

    let scan_status = serde_json::from_str::<serde_json::Value>(
        &fs::read_to_string(run_dir.join("scan-status.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(scan_status["phase"], "finalize");
    assert!(scan_status["selected_raw_bytes"].as_u64().unwrap() > 0);
    assert!(scan_status["staged_bytes"].as_u64().unwrap() > 0);
    assert_eq!(scan_status["discovered_sessions"], 1);
    assert!(scan_status["durations_ms"]["discovery"].as_u64().is_some());
}

#[cfg(unix)]
#[test]
fn test_e2e_scan_opencode_monitored_sessions_with_generic_proposal_agent() {
    let dir = tempfile::tempdir().unwrap();
    seed_config_with_all(dir.path(), "fake-proposal-agent.sh", false, false, true);

    let bin_dir = dir.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();

    let opencode = bin_dir.join("opencode");
    write_executable_script(
        &opencode,
        r#"#!/bin/sh
log_file="$HOME/opencode-calls.log"
printf '%s\n' "$*" >> "$log_file"
if [ "$1" = "session" ] && [ "$2" = "list" ]; then
  printf '%s' '[{"id":"sess-1","updatedAt":"2026-03-10T12:00:00Z"}]'
  exit 0
fi
if [ "$1" = "export" ]; then
  [ "$2" = "sess-1" ] || exit 41
  printf '%s' '{"messages":[{"role":"user","content":[{"text":"Create a review checklist"}]},{"role":"assistant","content":[{"text":"I inspected the repo and drafted a plan."}]},{"type":"tool_call","tool":"read"}]}'
  exit 0
fi
exit 42
"#,
    );

    let fake_agent = bin_dir.join("fake-proposal-agent.sh");
    write_executable_script(
        &fake_agent,
        r##"#!/bin/sh
cat > /dev/null
staged_session="$(find "$PWD/sessions/opencode" -type f | head -n 1)"
[ -n "$staged_session" ] || exit 51
printf '%s' "{\"inspected_files\":[\"$staged_session\"],\"file_findings\":[{\"session\":\"$staged_session\",\"summary\":\"Repeated review workflow.\"}],\"proposals\":[{\"type\":\"new\",\"confidence\":\"high\",\"target_skill\":\"review-checklist\",\"evidence\":[{\"session\":\"$staged_session\",\"pattern\":\"Repeated review workflow.\"}],\"body\":\"# Review Checklist\\n\\n## When to use\\nUse when reviewing repo changes.\\n\\n## Steps\\n1. Inspect the diff.\\n\\n## Verification\\nConfirm tests pass.\\n\\n## Pitfalls\\nDo not skip failing tests.\"}]}"
"##,
    );

    let path_env = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    distill_cmd(dir.path())
        .env("PATH", &path_env)
        .args(["scan", "--now"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Scanning agents: opencode"))
        .stdout(predicate::str::contains("Found 1 session(s) to analyze."))
        .stdout(predicate::str::contains("Agent proposed 1 skill(s)."));

    let proposal_dir = dir.path().join(".distill/proposals");
    let proposal_paths: Vec<_> = fs::read_dir(&proposal_dir)
        .unwrap()
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .collect();
    assert_eq!(proposal_paths.len(), 1);
    let proposal = fs::read_to_string(&proposal_paths[0]).unwrap();
    assert!(proposal.contains("Review Checklist"));
    assert!(proposal.contains(".local/share/opencode/sessions/sess-1.json"));

    let calls = fs::read_to_string(dir.path().join("opencode-calls.log")).unwrap();
    assert!(calls.contains("session list --format json"));
    assert!(calls.contains("export sess-1 --format json"));
}

#[cfg(unix)]
#[test]
fn test_e2e_scan_opencode_proposal_agent_uses_isolated_home_and_inline_permissions() {
    let dir = tempfile::tempdir().unwrap();
    seed_config_with_all(dir.path(), "opencode", false, false, true);

    fs::create_dir_all(dir.path().join(".config/opencode")).unwrap();
    fs::create_dir_all(dir.path().join(".local/share/opencode")).unwrap();
    fs::write(
        dir.path().join(".config/opencode/opencode.json"),
        "{\"theme\":\"dark\"}",
    )
    .unwrap();
    fs::write(
        dir.path().join(".local/share/opencode/auth.json"),
        "{\"token\":\"secret\"}",
    )
    .unwrap();

    let bin_dir = dir.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let opencode = bin_dir.join("opencode");
    write_executable_script(
        &opencode,
        &format!(
            r##"#!/bin/sh
if [ "$1" = "session" ] && [ "$2" = "list" ]; then
  printf '%s' '[{{"id":"sess-1","updatedAt":"2026-03-10T12:00:00Z"}}]'
  exit 0
fi
if [ "$1" = "export" ]; then
  printf '%s' '{{"messages":[{{"role":"user","content":[{{"text":"Document the release workflow"}}]}},{{"role":"assistant","content":[{{"text":"I will inspect the release scripts."}}]}}]}}'
  exit 0
fi
[ "$1" = "run" ] || exit 61
[ "$HOME" != "{home}" ] || exit 62
[ -f "$HOME/.config/opencode/opencode.json" ] || exit 63
[ -f "$HOME/.local/share/opencode/auth.json" ] || exit 64
printf '%s' "$OPENCODE_CONFIG_CONTENT" | grep -q '"edit":"deny"' || exit 65
printf '%s' "$OPENCODE_CONFIG_CONTENT" | grep -q '"bash":"deny"' || exit 66
printf '%s' "$OPENCODE_CONFIG_CONTENT" | grep -q '"webfetch":"deny"' || exit 67
staged_session="$(find "$PWD/sessions/opencode" -type f | head -n 1)"
[ -n "$staged_session" ] || exit 68
cat > /dev/null
printf '%s\n' "{{\"type\":\"tool\",\"name\":\"read\",\"path\":\"$staged_session\"}}"
printf '%s' "{{\"output\":\"{{\\\"inspected_files\\\":[\\\"$staged_session\\\"],\\\"file_findings\\\":[{{\\\"session\\\":\\\"$staged_session\\\",\\\"summary\\\":\\\"Repeated release workflow.\\\"}}],\\\"proposals\\\":[{{\\\"type\\\":\\\"new\\\",\\\"confidence\\\":\\\"high\\\",\\\"target_skill\\\":\\\"release-checklist\\\",\\\"evidence\\\":[{{\\\"session\\\":\\\"$staged_session\\\",\\\"pattern\\\":\\\"Repeated release workflow.\\\"}}],\\\"body\\\":\\\"# Release Checklist\\\\n\\\\n## When to use\\\\nUse when preparing a release.\\\\n\\\\n## Steps\\\\n1. Inspect the release scripts.\\\\n\\\\n## Verification\\\\nConfirm package output.\\\\n\\\\n## Pitfalls\\\\nDo not skip smoke tests.\\\"}}]}}\"}}"
"##,
            home = dir.path().display()
        ),
    );

    let path_env = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    distill_cmd(dir.path())
        .env("PATH", &path_env)
        .args(["scan", "--now"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Scanning agents: opencode"))
        .stdout(predicate::str::contains("Agent proposed 1 skill(s)."));
}

#[cfg(unix)]
#[test]
fn test_e2e_sync_agents_opencode_proposal_agent_writes_proposal() {
    let dir = tempfile::tempdir().unwrap();
    seed_config_with_all(dir.path(), "opencode", false, false, false);

    fs::create_dir_all(dir.path().join(".config/opencode")).unwrap();
    fs::create_dir_all(dir.path().join(".local/share/opencode")).unwrap();
    fs::write(
        dir.path().join(".config/opencode/opencode.json"),
        "{\"theme\":\"dark\"}",
    )
    .unwrap();
    fs::write(
        dir.path().join(".local/share/opencode/auth.json"),
        "{\"token\":\"secret\"}",
    )
    .unwrap();

    let project = dir.path().join("demo-project");
    init_git_repo(&project);
    fs::write(project.join("README.md"), "# Demo\n").unwrap();
    commit_all(&project, "Add README");
    let canonical_project = fs::canonicalize(&project).unwrap();

    let bin_dir = dir.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let opencode = bin_dir.join("opencode");
    write_executable_script(
        &opencode,
        &format!(
            r##"#!/bin/sh
[ "$1" = "run" ] || exit 71
[ "$HOME" != "{home}" ] || exit 72
cat > "{prompt_path}"
printf '%s' "{{\"proposals\":[{{\"type\":\"edit\",\"confidence\":\"high\",\"target\":{{\"kind\":\"file\",\"path\":\"{agents_path}\"}},\"evidence\":[{{\"session\":\"/tmp/session-1.jsonl\",\"pattern\":\"Repeated AGENTS drift.\"}}],\"body\":\"# AGENTS\\n\\nKeep repo instructions current.\"}}]}}"
"##,
            home = dir.path().display(),
            prompt_path = dir.path().join("sync-agents-prompt.txt").display(),
            agents_path = canonical_project.join("AGENTS.md").display()
        ),
    );

    let path_env = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    distill_cmd(dir.path())
        .env("PATH", &path_env)
        .args(["sync-agents", "--projects"])
        .arg(canonical_project.display().to_string())
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "sync-agents: evaluated 1 project(s)",
        ))
        .stdout(predicate::str::contains("Wrote 1 proposal(s)"));

    let proposals: Vec<_> = fs::read_dir(dir.path().join(".distill/proposals"))
        .unwrap()
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .collect();
    assert_eq!(proposals.len(), 1);
    let proposal = fs::read_to_string(&proposals[0]).unwrap();
    assert!(
        proposal.contains(
            canonical_project
                .join("AGENTS.md")
                .to_string_lossy()
                .as_ref()
        )
    );
    assert!(proposal.contains("type: edit"));
    assert!(dir.path().join("sync-agents-prompt.txt").is_file());
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
