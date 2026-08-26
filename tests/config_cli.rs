use std::{fs, process::Command};

use tempfile::tempdir;

fn resource_guard() -> Command {
    Command::new(env!("CARGO_BIN_EXE_resource-guard"))
}

#[test]
fn config_path_uses_explicit_override() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("config.toml");
    let output = resource_guard()
        .args(["config", "path"])
        .env("RESOURCE_GUARD_CONFIG", &path)
        .output()
        .unwrap();

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        path.to_string_lossy()
    );
}

#[test]
fn config_show_prints_effective_defaults() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("config.toml");
    let output = resource_guard()
        .arg("config")
        .env("RESOURCE_GUARD_CONFIG", path)
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("poll_interval_seconds = 5"));
    assert!(stdout.contains("protected_names = [\"resource-guard\"]"));
}

#[test]
fn config_init_and_check_work_with_an_isolated_file() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("nested/config.toml");
    let initialized = resource_guard()
        .args(["config", "init"])
        .env("RESOURCE_GUARD_CONFIG", &path)
        .output()
        .unwrap();

    assert!(initialized.status.success());
    assert!(path.exists());

    let checked = resource_guard()
        .args(["config", "check"])
        .env("RESOURCE_GUARD_CONFIG", &path)
        .output()
        .unwrap();
    assert!(checked.status.success());
    assert!(
        String::from_utf8(checked.stdout)
            .unwrap()
            .contains("configuration is valid")
    );
}

#[test]
fn config_check_rejects_an_invalid_file() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("config.toml");
    fs::write(&path, "[monitor]\npoll_interval_seconds = 0\n").unwrap();

    let output = resource_guard()
        .args(["config", "check"])
        .env("RESOURCE_GUARD_CONFIG", path)
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("poll interval must be greater than zero")
    );
}
