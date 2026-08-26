#![cfg(target_os = "linux")]

use std::{
    process::{Child, Command},
    thread,
    time::Duration,
};

use resource_guard::{
    adapters::{PidfdTerminationPort, SysinfoProcessSource, current_user_id},
    application::{ProcessSource, TerminationPort},
};

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn sends_sigterm_to_the_exact_child_process() {
    let child = Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("sleep should start");
    let mut child = ChildGuard(child);
    let pid = child.0.id();
    let mut source = SysinfoProcessSource::new();
    let process = source
        .find(pid)
        .expect("process lookup should succeed")
        .expect("child should be visible");
    assert_eq!(process.identity().uid(), current_user_id());

    PidfdTerminationPort
        .terminate(process.identity())
        .expect("SIGTERM should be delivered through pidfd");

    let exited = (0..20).any(|_| {
        if child.0.try_wait().unwrap().is_some() {
            true
        } else {
            thread::sleep(Duration::from_millis(10));
            false
        }
    });
    assert!(exited, "child should exit after SIGTERM");
}
