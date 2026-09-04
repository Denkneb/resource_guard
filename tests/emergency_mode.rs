#![cfg(target_os = "linux")]

use std::{collections::HashSet, process::Command, time::Duration};

use resource_guard::{
    adapters::{PidfdTerminationPort, SysinfoProcessSource, current_user_id},
    application::{
        EmergencyService, MemoryPressureMonitor, MemoryPressureSource, PortError, ProcessSource,
        StopProcess,
    },
    domain::{
        EmergencyAction, EmergencyActivationPolicy, EmergencyPolicy, MemoryPressureLevel,
        MemoryPressurePolicy, MemoryPressureSample, MemoryPsi, ProtectionPolicy, SystemResources,
    },
};

struct ChildGuard(std::process::Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct FakePressureSource(MemoryPressureSample);

impl MemoryPressureSource for FakePressureSource {
    fn sample(&mut self) -> Result<MemoryPressureSample, PortError> {
        Ok(self.0)
    }
}

#[test]
fn artificial_critical_pressure_terminates_an_allowlisted_child() {
    let child = Command::new("sleep").arg("30").spawn().unwrap();
    let child = ChildGuard(child);
    let child_pid = child.0.id();
    let mut source = SysinfoProcessSource::new();
    let snapshot = source.snapshot().unwrap();
    let observed = snapshot
        .processes
        .into_iter()
        .find(|process| process.descriptor.identity().pid() == child_pid)
        .unwrap();
    let pressure_sample = MemoryPressureSample {
        system: SystemResources {
            total_memory_bytes: 32 * 1_024 * 1_024 * 1_024,
            available_memory_bytes: 256 * 1_024 * 1_024,
            total_swap_bytes: 2 * 1_024 * 1_024 * 1_024,
            used_swap_bytes: 2 * 1_024 * 1_024 * 1_024,
        },
        psi: MemoryPsi {
            some_avg10: 20.0,
            full_avg10: 10.0,
        },
    };
    let mut pressure = MemoryPressureMonitor::new(
        FakePressureSource(pressure_sample),
        MemoryPressurePolicy {
            enabled: true,
            warning_available_percent: 15.0,
            critical_available_percent: 8.0,
            emergency_available_bytes: 512 * 1_024 * 1_024,
            critical_swap_used_percent: 90.0,
            critical_psi_full_avg10: 5.0,
            critical_samples: 2,
            recovery_available_percent: 20.0,
        },
    );
    let evaluation = pressure.poll().unwrap();
    assert_eq!(evaluation.current, MemoryPressureLevel::Critical);
    let activation = EmergencyActivationPolicy {
        action_available_bytes: 1_024 * 1_024 * 1_024,
        action_psi_full_avg10: 5.0,
    };
    assert!(activation.permits(evaluation));

    let protection = ProtectionPolicy::default();
    let mut emergency = EmergencyService::new(
        current_user_id(),
        protection.clone(),
        EmergencyPolicy {
            action: EmergencyAction::TerminateAllowlisted,
            allowed_names: HashSet::from(["sleep".to_owned()]),
            ..EmergencyPolicy::default()
        },
        Duration::from_secs(30),
    );
    let candidate = emergency
        .consider(
            activation.permits(evaluation),
            &[observed],
            Duration::ZERO,
            false,
        )
        .unwrap();
    let mut terminator = PidfdTerminationPort;

    StopProcess::new(&mut source, &mut terminator, current_user_id(), &protection)
        .execute(candidate.process.identity())
        .unwrap();

    let mut child = child;
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while std::time::Instant::now() < deadline {
        if child.0.try_wait().unwrap().is_some() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("allowlisted child did not exit after emergency SIGTERM");
}

#[test]
fn full_swap_without_psi_does_not_authorize_termination() {
    let child = Command::new("sleep").arg("30").spawn().unwrap();
    let mut child = ChildGuard(child);
    let child_pid = child.0.id();
    let mut source = SysinfoProcessSource::new();
    let snapshot = source.snapshot().unwrap();
    let observed = snapshot
        .processes
        .into_iter()
        .find(|process| process.descriptor.identity().pid() == child_pid)
        .unwrap();
    let pressure_sample = MemoryPressureSample {
        system: SystemResources {
            total_memory_bytes: 32 * 1_024 * 1_024 * 1_024,
            available_memory_bytes: 2 * 1_024 * 1_024 * 1_024,
            total_swap_bytes: 2 * 1_024 * 1_024 * 1_024,
            used_swap_bytes: 2 * 1_024 * 1_024 * 1_024,
        },
        psi: MemoryPsi::default(),
    };
    let mut pressure = MemoryPressureMonitor::new(
        FakePressureSource(pressure_sample),
        MemoryPressurePolicy {
            enabled: true,
            warning_available_percent: 15.0,
            critical_available_percent: 8.0,
            emergency_available_bytes: 512 * 1_024 * 1_024,
            critical_swap_used_percent: 90.0,
            critical_psi_full_avg10: 5.0,
            critical_samples: 2,
            recovery_available_percent: 20.0,
        },
    );
    assert_eq!(
        pressure.poll().unwrap().current,
        MemoryPressureLevel::Warning
    );
    let evaluation = pressure.poll().unwrap();
    assert_eq!(evaluation.current, MemoryPressureLevel::Critical);

    let activation = EmergencyActivationPolicy {
        action_available_bytes: 1_024 * 1_024 * 1_024,
        action_psi_full_avg10: 5.0,
    };
    assert!(!activation.permits(evaluation));

    let mut emergency = EmergencyService::new(
        current_user_id(),
        ProtectionPolicy::default(),
        EmergencyPolicy {
            action: EmergencyAction::TerminateAllowlisted,
            allowed_names: HashSet::from(["sleep".to_owned()]),
            ..EmergencyPolicy::default()
        },
        Duration::from_secs(30),
    );
    assert!(
        emergency
            .consider(
                activation.permits(evaluation),
                &[observed],
                Duration::ZERO,
                false,
            )
            .is_none()
    );
    assert!(child.0.try_wait().unwrap().is_none());
}
