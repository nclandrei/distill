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
    let distill_dir = home.join(".distill");
    fs::create_dir_all(&distill_dir).unwrap();
    fs::write(
        distill_dir.join("config.yaml"),
        "agents:\n  - name: claude\n    enabled: true\n  - name: codex\n    enabled: true\n\
         scan_interval: weekly\nproposal_agent: claude\nshell: zsh\nnotifications: both\n",
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
        .stdout(predicate::str::contains("No new sessions found since last scan."));

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
        .stdout(predicate::str::contains("No new sessions found since last scan."));

    assert!(
        dir.path().join(".distill").join("last-scan.json").is_file(),
        "last-scan.json should be written after scheduled scan path"
    );
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
