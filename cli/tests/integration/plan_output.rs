//! Plan/output/profile contract tests for pcp CLI.

use assert_cmd::cargo::cargo_bin_cmd;
use serde_json::Value;
use std::fs;
use tempfile::TempDir;

#[test]
fn test_plan_json_contract_and_no_mutation() {
    let src = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();

    let source_file = src.path().join("a.txt");
    let destination_file = dst.path().join("a.txt");
    fs::write(&source_file, "hello plan").unwrap();

    let mut cmd = cargo_bin_cmd!("pcp");
    let output = cmd
        .arg("--plan")
        .arg("--output")
        .arg("json")
        .arg(&source_file)
        .arg(&destination_file)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let payload: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(payload["schema_version"], "1.0");
    assert_eq!(payload["mode"], "plan");
    assert!(payload["effective_config"].is_object());
    assert_eq!(payload["effective_config"]["profile"], "modern");
    assert_eq!(payload["effective_config"]["output_mode"], "json");

    let items = payload["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["source"], source_file.display().to_string());
    assert_eq!(
        items[0]["destination"],
        destination_file.display().to_string()
    );
    assert_eq!(items[0]["action"], "copy");
    assert_eq!(items[0]["reason"], "not_exists");

    assert!(
        !destination_file.exists(),
        "plan mode must not mutate filesystem"
    );
}

#[test]
fn test_plan_jsonl_effective_config_first() {
    let src = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();

    let source_file = src.path().join("exists.txt");
    let destination_file = dst.path().join("exists.txt");
    fs::write(&source_file, "new content").unwrap();
    fs::write(&destination_file, "old content").unwrap();

    let mut cmd = cargo_bin_cmd!("pcp");
    let stdout = cmd
        .arg("--plan")
        .arg("--output")
        .arg("jsonl")
        .arg(&source_file)
        .arg(&destination_file)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let text = String::from_utf8(stdout).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert!(
        lines.len() >= 2,
        "jsonl output must include effective_config and at least one item"
    );

    let first: Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(first["schema_version"], "1.0");
    assert_eq!(first["record_type"], "effective_config");
    assert_eq!(first["mode"], "plan");

    let second: Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(second["schema_version"], "1.0");
    assert_eq!(second["record_type"], "plan_item");
    assert_eq!(second["action"], "skip");
    assert_eq!(second["reason"], "exists");
}

#[test]
fn test_execute_json_contract() {
    let src = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();

    let source_file = src.path().join("exec.txt");
    let destination_file = dst.path().join("exec.txt");
    fs::write(&source_file, "execute content").unwrap();

    let mut cmd = cargo_bin_cmd!("pcp");
    let output = cmd
        .arg("--output")
        .arg("json")
        .arg(&source_file)
        .arg(&destination_file)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let payload: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(payload["schema_version"], "1.0");
    assert_eq!(payload["mode"], "execute");
    assert!(payload["effective_config"].is_object());

    let items = payload["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["outcome"], "copied");
    assert!(items[0]["bytes_copied"].as_u64().unwrap() > 0);

    assert!(destination_file.exists());
    assert_eq!(
        fs::read_to_string(destination_file).unwrap(),
        "execute content"
    );
}

#[test]
fn test_fast_profile_effective_config() {
    let src = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();

    let source_file = src.path().join("fast.txt");
    let destination_file = dst.path().join("fast.txt");
    fs::write(&source_file, "fast profile").unwrap();

    let mut cmd = cargo_bin_cmd!("pcp");
    let output = cmd
        .arg("--plan")
        .arg("--profile")
        .arg("fast")
        .arg("--output")
        .arg("json")
        .arg(&source_file)
        .arg(&destination_file)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let payload: Value = serde_json::from_slice(&output).unwrap();
    let effective = &payload["effective_config"];
    assert_eq!(effective["profile"], "fast");
    assert_eq!(effective["preserve_timestamps"], false);
    assert_eq!(effective["preserve_permissions"], false);
    assert_eq!(effective["fsync"], false);
    assert_eq!(effective["output_mode"], "json");
}
