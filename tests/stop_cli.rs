#![cfg(target_os = "linux")]

use std::process::{Child, Command};

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
fn kill_flag_is_rejected_before_process_inspection() {
    let output = resource_guard()
        .args(["stop", "4294967295", "--kill", "--yes"])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("SIGKILL is not implemented yet; no signal was sent")
    );
}
