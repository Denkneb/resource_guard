#![cfg(target_os = "linux")]

use std::{
    process::{Child, Command},
    thread,
    time::Duration,
};

use resource_guard::{
    adapters::{PidfdTerminationPort, SysinfoProcessSource, current_user_id},
    application::{ProcessSource, StopWorkload, TerminationPort},
    domain::ProtectionPolicy,
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

fn wait_for_exit(child: &mut ChildGuard) -> bool {
    (0..20).any(|_| {
        if child.0.try_wait().unwrap().is_some() {
            true
        } else {
            thread::sleep(Duration::from_millis(10));
            false
        }
    })
}

#[test]
fn stop_workload_signals_only_the_listed_identity_and_spares_a_sibling() {
    let target = Command::new("sleep").arg("30").spawn().unwrap();
    let mut target = ChildGuard(target);
    let sibling = Command::new("sleep").arg("30").spawn().unwrap();
    let mut sibling = ChildGuard(sibling);

    let mut source = SysinfoProcessSource::new();
    let target_process = source
        .find(target.0.id())
        .expect("process lookup should succeed")
        .expect("target child should be visible");
    let target_identity = target_process.identity();
    let sibling_pid = sibling.0.id();

    let mut terminator = PidfdTerminationPort;
    let count = StopWorkload::new(
        &mut source,
        &mut terminator,
        current_user_id(),
        &ProtectionPolicy::default(),
    )
    .execute([target_identity])
    .expect("SIGTERM should be delivered through the workload stop use case");

    assert_eq!(count, 1);
    assert!(
        wait_for_exit(&mut target),
        "target child should exit after SIGTERM"
    );
    assert!(
        sibling.0.try_wait().unwrap().is_none(),
        "sibling child {sibling_pid} must remain alive"
    );
}
