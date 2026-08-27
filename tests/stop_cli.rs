#![cfg(target_os = "linux")]

use std::{
    fs,
    io::Read,
    process::{Child, Command, Stdio},
};

use tempfile::tempdir;

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn resource_guard() -> Command {
    Command::new(env!("CARGO_BIN_EXE_resource-guard"))
}

fn term_ignoring_child() -> ChildGuard {
    let child = Command::new("sh")
        .args(["-c", "trap '' TERM; printf x; exec sleep 30"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut child = ChildGuard(child);
    let mut ready = [0_u8; 1];
    child
        .0
        .stdout
        .as_mut()
        .unwrap()
        .read_exact(&mut ready)
        .unwrap();
    child
}

fn short_grace_period_config() -> (tempfile::TempDir, std::path::PathBuf) {
    let directory = tempdir().unwrap();
    let config = directory.path().join("config.toml");
    fs::write(&config, "[termination]\ngrace_period_seconds = 1\n").unwrap();
    (directory, config)
}

#[test]
fn stop_gracefully_terminates_a_child_process() {
    let directory = tempdir().unwrap();
    let config = directory.path().join("config.toml");
    let child = Command::new("sleep").arg("30").spawn().unwrap();
    let mut child = ChildGuard(child);

    let output = resource_guard()
        .args(["stop", &child.0.id().to_string()])
        .env("RESOURCE_GUARD_CONFIG", config)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("process exited")
    );
    assert!(child.0.wait().unwrap().code().is_none());
}

#[test]
fn force_stop_still_requires_an_existing_process() {
    let output = resource_guard()
        .args(["stop", "4294967295", "--kill", "--yes"])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("process 4294967295 does not exist")
    );
}

#[test]
fn confirmed_force_stop_kills_a_child_that_ignores_sigterm() {
    let (_directory, config) = short_grace_period_config();
    let mut child = term_ignoring_child();
    let pid = child.0.id();

    let output = resource_guard()
        .args(["stop", &pid.to_string(), "--kill", "--yes"])
        .env("RESOURCE_GUARD_CONFIG", config)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("SIGKILL")
    );
    assert!(child.0.wait().unwrap().code().is_none());
}

#[test]
fn non_interactive_force_stop_requires_yes_and_does_not_send_sigkill() {
    let (_directory, config) = short_grace_period_config();
    let mut child = term_ignoring_child();
    let pid = child.0.id();

    let output = resource_guard()
        .args(["stop", &pid.to_string(), "--kill"])
        .env("RESOURCE_GUARD_CONFIG", config)
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("rerun with --kill --yes")
    );
    assert!(child.0.try_wait().unwrap().is_none());
}
