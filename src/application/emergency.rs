use std::{collections::HashMap, time::Duration};

use crate::domain::{
    EmergencyCandidate, EmergencyPolicy, MemoryPressureLevel, ProcessIdentity, ProtectionPolicy,
    select_emergency_victim,
};

use super::ObservedProcess;

/// Application use case for deterministic, cooldown-aware emergency victim selection.
pub struct EmergencyService {
    current_uid: u32,
    protection: ProtectionPolicy,
    policy: EmergencyPolicy,
    action_cooldown: Duration,
    previous_memory: HashMap<ProcessIdentity, u64>,
    last_action_at: Option<Duration>,
}

impl EmergencyService {
    #[must_use]
    pub fn new(
        current_uid: u32,
        protection: ProtectionPolicy,
        policy: EmergencyPolicy,
        action_cooldown: Duration,
    ) -> Self {
        Self {
            current_uid,
            protection,
            policy,
            action_cooldown,
            previous_memory: HashMap::new(),
            last_action_at: None,
        }
    }

    #[must_use]
    pub fn consider(
        &mut self,
        level: MemoryPressureLevel,
        processes: &[ObservedProcess],
        now: Duration,
        action_pending: bool,
    ) -> Option<EmergencyCandidate> {
        let candidates = processes
            .iter()
            .map(|process| {
                let identity = process.descriptor.identity();
                let memory = process.resources.resident_memory_bytes;
                let previous = self
                    .previous_memory
                    .insert(identity, memory)
                    .unwrap_or(memory);
                EmergencyCandidate {
                    process: process.descriptor.clone(),
                    resources: process.resources,
                    memory_growth_bytes: memory.saturating_sub(previous),
                }
            })
            .collect::<Vec<_>>();
        self.previous_memory.retain(|identity, _| {
            candidates
                .iter()
                .any(|candidate| candidate.process.identity() == *identity)
        });

        if level != MemoryPressureLevel::Critical
            || action_pending
            || self
                .last_action_at
                .is_some_and(|last| now.saturating_sub(last) < self.action_cooldown)
        {
            return None;
        }

        let selected = select_emergency_victim(
            &candidates,
            self.current_uid,
            &self.protection,
            &self.policy,
        )?
        .clone();
        self.last_action_at = Some(now);
        Some(selected)
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashSet, path::PathBuf, time::Duration};

    use super::EmergencyService;
    use crate::{
        application::ObservedProcess,
        domain::{
            EmergencyAction, EmergencyPolicy, MemoryPressureLevel, ProcessDescriptor,
            ProcessIdentity, ProcessResources, ProtectionPolicy,
        },
    };

    fn process(pid: u32, name: &str, memory: u64) -> ObservedProcess {
        ObservedProcess {
            descriptor: ProcessDescriptor::new(
                ProcessIdentity::new(pid, 1_000, 100),
                name,
                Some(PathBuf::from(format!("/usr/bin/{name}"))),
            ),
            resources: ProcessResources {
                cpu_percent: 0.0,
                resident_memory_bytes: memory,
                virtual_memory_bytes: memory,
                running_for: Duration::ZERO,
                observed_at: Duration::ZERO,
            },
        }
    }

    #[test]
    fn selects_one_victim_and_enforces_the_action_cooldown() {
        let mut service = EmergencyService::new(
            1_000,
            ProtectionPolicy::default(),
            EmergencyPolicy {
                action: EmergencyAction::TerminateAllowlisted,
                allowed_names: HashSet::from(["worker".to_owned()]),
                ..EmergencyPolicy::default()
            },
            Duration::from_secs(30),
        );
        let processes = vec![process(42, "worker", 4_096)];

        assert!(
            service
                .consider(
                    MemoryPressureLevel::Critical,
                    &processes,
                    Duration::ZERO,
                    false,
                )
                .is_some()
        );
        assert!(
            service
                .consider(
                    MemoryPressureLevel::Critical,
                    &processes,
                    Duration::from_secs(10),
                    false,
                )
                .is_none()
        );
    }

    #[test]
    fn ordinary_ignore_does_not_make_a_process_emergency_exempt() {
        let mut service = EmergencyService::new(
            1_000,
            ProtectionPolicy::new([], [], ["worker".to_owned()], []),
            EmergencyPolicy {
                action: EmergencyAction::TerminateLargestUnprotected,
                ..EmergencyPolicy::default()
            },
            Duration::from_secs(30),
        );

        assert!(
            service
                .consider(
                    MemoryPressureLevel::Critical,
                    &[process(42, "worker", 4_096)],
                    Duration::ZERO,
                    false,
                )
                .is_some()
        );
    }
}
