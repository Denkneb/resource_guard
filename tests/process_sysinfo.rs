#![cfg(target_os = "linux")]

use std::{
    os::unix::fs::MetadataExt,
    process::{Child, Command},
    thread,
    time::Duration,
};

use resource_guard::{adapters::SysinfoProcessSource, application::ProcessSource};

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
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
}
