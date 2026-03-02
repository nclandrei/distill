use assert_cmd::Command;
use predicates::prelude::*;

/// Build a distill command with HOME set to a temp dir so tests
/// don't interact with the real ~/.distill config.
fn distill_cmd(home: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("distill").unwrap();
    cmd.env("HOME", home);
    cmd
}

#[test]
fn test_no_args_without_config() {
    let dir = tempfile::tempdir().unwrap();
    distill_cmd(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Welcome to distill"));
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
    distill_cmd(dir.path())
        .arg("scan")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "scheduled scan not yet implemented",
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
