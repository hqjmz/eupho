#![cfg(unix)]

use std::fs;

use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn help_identifies_the_native_observe_only_cli() {
    Command::cargo_bin("eupho")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Phase 1 is observe-only"))
        .stdout(predicate::str::contains("instructions"));
}

#[test]
fn once_requires_an_explicit_repository() {
    Command::cargo_bin("eupho")
        .unwrap()
        .arg("once")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("--repo"));
}

#[test]
fn json_argument_errors_keep_stable_machine_codes() {
    let missing = Command::cargo_bin("eupho")
        .unwrap()
        .args(["once", "--json"])
        .assert()
        .code(2)
        .get_output()
        .stderr
        .clone();
    let missing: serde_json::Value = serde_json::from_slice(&missing).unwrap();
    assert_eq!(missing["error"]["code"], "missing_required_option");

    let unknown = Command::cargo_bin("eupho")
        .unwrap()
        .args(["doctro", "--json"])
        .assert()
        .code(2)
        .get_output()
        .stderr
        .clone();
    let unknown: serde_json::Value = serde_json::from_slice(&unknown).unwrap();
    assert_eq!(unknown["error"]["code"], "unknown_command");
}

#[test]
fn unknown_options_fail_instead_of_being_ignored() {
    Command::cargo_bin("eupho")
        .unwrap()
        .args(["status", "--mutate"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("--mutate"));
}

#[test]
fn human_errors_cannot_inject_terminal_control_sequences() {
    let output = Command::cargo_bin("eupho")
        .unwrap()
        .arg("--unknown-\u{1b}[31m-red")
        .assert()
        .code(2)
        .get_output()
        .stderr
        .clone();

    assert!(!output.contains(&0x1b));
    assert!(!output.contains(&0x07));
}

#[test]
fn instruction_link_command_creates_and_reports_a_relative_link() {
    let repository = tempfile::tempdir().unwrap();
    fs::create_dir(repository.path().join(".git")).unwrap();
    fs::write(repository.path().join("AGENTS.md"), "canonical\n").unwrap();

    let output = Command::cargo_bin("eupho")
        .unwrap()
        .args([
            "instructions",
            "link",
            "--path",
            repository.path().to_str().unwrap(),
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(value["action"], "created");
    assert_eq!(value["linkTarget"], "AGENTS.md");
    let metadata = fs::symlink_metadata(repository.path().join("CLAUDE.md")).unwrap();
    assert!(metadata.file_type().is_symlink());
    assert_eq!(
        fs::read_link(repository.path().join("CLAUDE.md")).unwrap(),
        std::path::Path::new("AGENTS.md")
    );
}

#[test]
fn instruction_link_command_never_overwrites_an_existing_file() {
    let repository = tempfile::tempdir().unwrap();
    fs::create_dir(repository.path().join(".git")).unwrap();
    fs::write(repository.path().join("AGENTS.md"), "canonical\n").unwrap();
    fs::write(repository.path().join("CLAUDE.md"), "independent\n").unwrap();

    Command::cargo_bin("eupho")
        .unwrap()
        .args([
            "instructions",
            "link",
            "--path",
            repository.path().to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("refusing to replace"));

    assert_eq!(
        fs::read_to_string(repository.path().join("CLAUDE.md")).unwrap(),
        "independent\n"
    );
    assert!(
        fs::symlink_metadata(repository.path().join("CLAUDE.md"))
            .unwrap()
            .file_type()
            .is_file()
    );
}

#[test]
fn status_json_has_a_stable_empty_snapshot_shape() {
    let state = tempfile::tempdir().unwrap();
    let output = Command::cargo_bin("eupho")
        .unwrap()
        .args([
            "status",
            "--state-root",
            state.path().join("state").to_str().unwrap(),
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert!(value["stateRoot"].as_str().unwrap().ends_with("/state"));
    assert_eq!(value["repositories"], serde_json::json!([]));
}
