use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;

// ---------------------------------------------------------------------------
// Helper
// ---------------------------------------------------------------------------

/// Build a distill command with HOME set to a temp dir so tests
/// don't interact with the real ~/.distill config.
fn distill_cmd(home: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("distill").unwrap();
    cmd.env("HOME", home);
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
            "No new sessions found since last scan.",
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
            "No new sessions found since last scan.",
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
saw_exec=0
while [ $# -gt 0 ]; do
  case "$1" in
    exec)
      saw_exec=1
      shift
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
[ -n "$schema_file" ] || exit 32
[ -f "$schema_file" ] || exit 33
grep -q '"proposals"' "$schema_file" || exit 34
[ -n "$last_message_file" ] || exit 35
printf '%s' '{"proposals":[{"type":"new","confidence":"high","target_skill":null,"evidence":[{"session":"mock-session","pattern":"repeated shell workflow"}],"body":"# Codex Skill\n\nUse codex scanner defaults."}]}' > "$last_message_file"
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

    let watermark = fs::read_to_string(dir.path().join(".distill").join("last-scan.json")).unwrap();
    assert!(watermark.contains("session-1"));
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
