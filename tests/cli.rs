use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn test_no_args_runs_without_error() {
    Command::cargo_bin("distill")
        .unwrap()
        .assert()
        .success();
}

#[test]
fn test_help_flag() {
    Command::cargo_bin("distill")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Monitor AI agent sessions"));
}

#[test]
fn test_version_flag() {
    Command::cargo_bin("distill")
        .unwrap()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains("distill"));
}

#[test]
fn test_status_without_config() {
    Command::cargo_bin("distill")
        .unwrap()
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("not configured"));
}

#[test]
fn test_scan_now() {
    Command::cargo_bin("distill")
        .unwrap()
        .args(["scan", "--now"])
        .assert()
        .success()
        .stdout(predicate::str::contains("scan"));
}

#[test]
fn test_review_stub() {
    Command::cargo_bin("distill")
        .unwrap()
        .arg("review")
        .assert()
        .success()
        .stdout(predicate::str::contains("not yet implemented"));
}

#[test]
fn test_watch_install_stub() {
    Command::cargo_bin("distill")
        .unwrap()
        .args(["watch", "--install"])
        .assert()
        .success();
}

#[test]
fn test_watch_uninstall_stub() {
    Command::cargo_bin("distill")
        .unwrap()
        .args(["watch", "--uninstall"])
        .assert()
        .success();
}

#[test]
fn test_notify_check() {
    Command::cargo_bin("distill")
        .unwrap()
        .args(["notify", "--check"])
        .assert()
        .success();
}

#[test]
fn test_invalid_subcommand() {
    Command::cargo_bin("distill")
        .unwrap()
        .arg("nonexistent")
        .assert()
        .failure();
}

#[test]
fn test_watch_mutual_exclusion() {
    Command::cargo_bin("distill")
        .unwrap()
        .args(["watch", "--install", "--uninstall"])
        .assert()
        .failure();
}
