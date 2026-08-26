#![cfg(target_os = "linux")]

use std::{
    fs,
    os::unix::fs::{FileTypeExt, PermissionsExt},
    os::unix::net::UnixListener,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use rustix::process::{Pid, Signal, kill_process};
use tempfile::TempDir;

struct DaemonGuard {
    child: Child,
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn resource_guard() -> Command {
    Command::new(env!("CARGO_BIN_EXE_resource-guard"))
}

fn isolated_command(directory: &TempDir) -> Command {
    let mut command = resource_guard();
    command
        .env(
            "RESOURCE_GUARD_RUNTIME_DIR",
            directory.path().join("runtime"),
        )
        .env(
            "RESOURCE_GUARD_CONFIG",
            directory.path().join("config.toml"),
        );
    command
}

fn start_daemon(directory: &TempDir) -> DaemonGuard {
    let child = isolated_command(directory)
        .arg("daemon")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let guard = DaemonGuard { child };
    wait_until_ready(directory);
    guard
}

fn wait_until_ready(directory: &TempDir) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if isolated_command(directory)
            .arg("status")
            .output()
            .is_ok_and(|output| output.status.success())
        {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
    panic!("daemon did not become ready");
}

#[test]
fn status_reports_running_daemon_and_secure_socket_permissions() {
    let directory = tempfile::tempdir().unwrap();
    let _daemon = start_daemon(&directory);

    let output = isolated_command(&directory).arg("status").output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("daemon: running"));
    assert!(stdout.contains("processes:"));

    let runtime = directory.path().join("runtime");
    let socket = runtime.join("control.sock");
    assert_eq!(
        fs::metadata(runtime).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(socket).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn refuses_a_second_daemon() {
    let directory = tempfile::tempdir().unwrap();
    let _daemon = start_daemon(&directory);

    let output = isolated_command(&directory).arg("daemon").output().unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("already running")
    );
}

#[test]
fn removes_a_stale_socket_before_starting() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = directory.path().join("runtime");
    fs::create_dir(&runtime).unwrap();
    let stale_socket = runtime.join("control.sock");
    drop(UnixListener::bind(&stale_socket).unwrap());

    let _daemon = start_daemon(&directory);
    assert!(
        fs::metadata(runtime.join("control.sock"))
            .unwrap()
            .file_type()
            .is_socket()
    );
}

#[test]
fn sigterm_stops_daemon_and_removes_socket() {
    let directory = tempfile::tempdir().unwrap();
    let mut daemon = start_daemon(&directory);
    let pid = Pid::from_raw(i32::try_from(daemon.child.id()).unwrap()).unwrap();

    kill_process(pid, Signal::TERM).unwrap();
    let status = daemon.child.wait().unwrap();
    assert!(status.success());
    assert!(!directory.path().join("runtime/control.sock").exists());
}

#[test]
fn status_fails_when_daemon_is_absent() {
    let directory = tempfile::tempdir().unwrap();
    let output = isolated_command(&directory).arg("status").output().unwrap();

    assert!(!output.status.success());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("connect to daemon")
    );
}
