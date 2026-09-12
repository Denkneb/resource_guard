#![cfg(target_os = "linux")]

use std::{
    os::unix::fs::MetadataExt,
    process::{Child, Command},
    thread,
    time::Duration,
};

use resource_guard::{
    adapters::SysinfoProcessSource, application::ProcessSource, domain::ProcessState,
};

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn controlled_child_in(directory: &std::path::Path, program: &str) -> ChildGuard {
    let child = Command::new(program)
        .arg("30")
        .current_dir(directory)
        .spawn()
        .expect("child should start");
    ChildGuard(child)
}

fn await_child(
    source: &mut SysinfoProcessSource,
    pid: u32,
) -> resource_guard::domain::ProcessDescriptor {
    (0..50)
        .find_map(|_| {
            let process = source.find(pid).expect("process lookup should succeed");
            if process.is_none() {
                thread::sleep(Duration::from_millis(10));
            }
            process
        })
        .expect("child process should be visible through sysinfo")
}

#[test]
fn finds_a_controlled_child_process_with_stable_identity() {
    let child = Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("sleep should start");
    let child = ChildGuard(child);
    let pid = child.0.id();
    let expected_uid = std::fs::metadata(format!("/proc/{pid}"))
        .expect("child proc directory should exist")
        .uid();
    let mut source = SysinfoProcessSource::new();

    let process = (0..20)
        .find_map(|_| {
            let process = source.find(pid).expect("process lookup should succeed");
            if process.is_none() {
                thread::sleep(Duration::from_millis(10));
            }
            process
        })
        .expect("child process should be visible through sysinfo");

    assert_eq!(process.identity().pid(), pid);
    assert_eq!(process.identity().uid(), expected_uid);
    assert_ne!(process.identity().started_at(), 0);

    let observed = source
        .snapshot()
        .expect("snapshot should succeed")
        .processes
        .into_iter()
        .find(|observed| observed.descriptor.identity().pid() == pid)
        .expect("child should be present in the process tree snapshot");
    assert_eq!(observed.descriptor.parent_pid(), Some(std::process::id()));
    assert_ne!(observed.descriptor.state(), ProcessState::Other);
    assert_ne!(observed.descriptor.state(), ProcessState::Zombie);
}

#[test]
fn resolves_the_working_directory_of_a_controlled_child() {
    let directory = tempfile::tempdir().unwrap();
    let child = controlled_child_in(directory.path(), "sleep");
    let pid = child.0.id();
    let mut source = SysinfoProcessSource::new();
    await_child(&mut source, pid);

    let observed = source
        .snapshot()
        .expect("snapshot should succeed")
        .processes
        .into_iter()
        .find(|observed| observed.descriptor.identity().pid() == pid)
        .expect("child should be present in the process tree snapshot");

    assert_eq!(
        observed.descriptor.working_directory(),
        Some(directory.path())
    );
    assert_eq!(observed.descriptor.identity().pid(), pid);
    assert_eq!(observed.descriptor.parent_pid(), Some(std::process::id()));
}

#[test]
fn a_deleted_child_working_directory_keeps_the_process_in_the_snapshot() {
    let directory = tempfile::tempdir().unwrap();
    let child = controlled_child_in(directory.path(), "sleep");
    let pid = child.0.id();
    let mut source = SysinfoProcessSource::new();
    await_child(&mut source, pid);

    std::fs::remove_dir_all(directory.path()).expect("temporary directory should be removed");

    let observed = source
        .snapshot()
        .expect("snapshot should succeed")
        .processes
        .into_iter()
        .find(|observed| observed.descriptor.identity().pid() == pid)
        .expect("child should stay in the process tree snapshot");

    assert_eq!(observed.descriptor.working_directory(), None);
}
