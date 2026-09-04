use std::{cmp::Ordering, collections::HashSet, path::PathBuf};

use super::{
    MemoryPressureEvaluation, MemoryPressureLevel, ProcessDescriptor, ProcessResources,
    ProtectionPolicy,
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EmergencyActivationPolicy {
    pub action_available_bytes: u64,
    pub action_psi_full_avg10: f32,
}

impl EmergencyActivationPolicy {
    #[must_use]
    pub fn permits(self, evaluation: MemoryPressureEvaluation) -> bool {
        (evaluation.current != MemoryPressureLevel::Normal
            && evaluation.sample.system.available_memory_bytes <= self.action_available_bytes)
            || (evaluation.current == MemoryPressureLevel::Critical
                && evaluation.signals.available_critical
                && evaluation.sample.psi.full_avg10 >= self.action_psi_full_avg10)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum EmergencyAction {
    #[default]
    NotifyOnly,
    TerminateAllowlisted,
    TerminateLargestUnprotected,
}

#[derive(Clone, Debug, Default)]
pub struct EmergencyPolicy {
    pub action: EmergencyAction,
    pub allowed_names: HashSet<String>,
    pub allowed_executables: HashSet<PathBuf>,
    pub exempt_names: HashSet<String>,
    pub exempt_executables: HashSet<PathBuf>,
}

impl EmergencyPolicy {
    #[must_use]
    pub fn is_exempt(&self, process: &ProcessDescriptor) -> bool {
        self.exempt_names.contains(process.name())
            || process
                .executable()
                .is_some_and(|path| self.exempt_executables.contains(path))
    }

    #[must_use]
    pub fn is_allowed(&self, process: &ProcessDescriptor) -> bool {
        self.allowed_names.contains(process.name())
            || process
                .executable()
                .is_some_and(|path| self.allowed_executables.contains(path))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct EmergencyCandidate {
    pub process: ProcessDescriptor,
    pub resources: ProcessResources,
    pub memory_growth_bytes: u64,
}

#[must_use]
pub fn select_emergency_victim<'a>(
    candidates: &'a [EmergencyCandidate],
    current_uid: u32,
    protection: &ProtectionPolicy,
    emergency: &EmergencyPolicy,
) -> Option<&'a EmergencyCandidate> {
    if emergency.action == EmergencyAction::NotifyOnly {
        return None;
    }

    candidates
        .iter()
        .filter(|candidate| candidate.process.identity().uid() == current_uid)
        .filter(|candidate| !protection.is_protected(&candidate.process))
        .filter(|candidate| !emergency.is_exempt(&candidate.process))
        .filter(|candidate| {
            emergency.action == EmergencyAction::TerminateLargestUnprotected
                || emergency.is_allowed(&candidate.process)
        })
        .max_by(|left, right| compare_candidates(left, right))
}

#[must_use]
pub const fn force_termination_permitted(
    automatic_action_permitted: bool,
    allow_sigkill: bool,
) -> bool {
    automatic_action_permitted && allow_sigkill
}

fn compare_candidates(left: &EmergencyCandidate, right: &EmergencyCandidate) -> Ordering {
    left.resources
        .resident_memory_bytes
        .cmp(&right.resources.resident_memory_bytes)
        .then_with(|| left.memory_growth_bytes.cmp(&right.memory_growth_bytes))
        .then_with(|| {
            right
                .process
                .identity()
                .pid()
                .cmp(&left.process.identity().pid())
        })
}

#[cfg(test)]
mod tests {
    use std::{collections::HashSet, path::PathBuf, time::Duration};

    use super::{
        EmergencyAction, EmergencyActivationPolicy, EmergencyCandidate, EmergencyPolicy,
        force_termination_permitted, select_emergency_victim,
    };
    use crate::domain::{
        MemoryPressureEvaluation, MemoryPressureLevel, MemoryPressureSample, MemoryPressureSignals,
        MemoryPsi, ProcessDescriptor, ProcessIdentity, ProcessResources, ProtectionPolicy,
        SystemResources,
    };

    fn candidate(pid: u32, uid: u32, name: &str, memory: u64, growth: u64) -> EmergencyCandidate {
        EmergencyCandidate {
            process: ProcessDescriptor::new(
                ProcessIdentity::new(pid, uid, 100),
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
            memory_growth_bytes: growth,
        }
    }

    fn policy(action: EmergencyAction) -> EmergencyPolicy {
        EmergencyPolicy {
            action,
            allowed_names: HashSet::from(["compiler".to_owned(), "worker".to_owned()]),
            exempt_names: HashSet::from(["desktop".to_owned()]),
            ..EmergencyPolicy::default()
        }
    }

    fn pressure(available: u64, psi_full_avg10: f32) -> MemoryPressureEvaluation {
        MemoryPressureEvaluation {
            previous: MemoryPressureLevel::Warning,
            current: MemoryPressureLevel::Critical,
            sample: MemoryPressureSample {
                system: SystemResources {
                    total_memory_bytes: 32 * 1_024 * 1_024 * 1_024,
                    available_memory_bytes: available,
                    total_swap_bytes: 2 * 1_024 * 1_024 * 1_024,
                    used_swap_bytes: 2 * 1_024 * 1_024 * 1_024,
                },
                psi: MemoryPsi {
                    some_avg10: psi_full_avg10,
                    full_avg10: psi_full_avg10,
                },
            },
            signals: MemoryPressureSignals {
                available_warning: true,
                available_critical: true,
                available_recovered: false,
                swap_critical: true,
                psi_critical: psi_full_avg10 >= 5.0,
                emergency_floor: available <= 512 * 1_024 * 1_024,
            },
        }
    }

    #[test]
    fn activation_requires_configured_floor_or_low_memory_with_psi() {
        let policy = EmergencyActivationPolicy {
            action_available_bytes: 1_024 * 1_024 * 1_024,
            action_psi_full_avg10: 5.0,
        };

        assert!(!policy.permits(pressure(2 * 1_024 * 1_024 * 1_024, 0.0)));
        assert!(policy.permits(pressure(2 * 1_024 * 1_024 * 1_024, 5.0)));
        assert!(policy.permits(pressure(1_024 * 1_024 * 1_024, 0.0)));

        let mut disabled = pressure(512 * 1_024 * 1_024, 10.0);
        disabled.current = MemoryPressureLevel::Normal;
        assert!(!policy.permits(disabled));
    }

    #[test]
    fn allowlisted_mode_selects_the_largest_allowed_current_user_process() {
        let candidates = vec![
            candidate(10, 1_000, "worker", 2_000, 100),
            candidate(11, 1_000, "browser", 9_000, 500),
            candidate(12, 1_000, "compiler", 4_000, 50),
            candidate(13, 2_000, "compiler", 8_000, 500),
        ];

        let selected = select_emergency_victim(
            &candidates,
            1_000,
            &ProtectionPolicy::default(),
            &policy(EmergencyAction::TerminateAllowlisted),
        )
        .unwrap();

        assert_eq!(selected.process.identity().pid(), 12);
    }

    #[test]
    fn largest_mode_excludes_protected_and_emergency_exempt_processes() {
        let candidates = vec![
            candidate(10, 1_000, "desktop", 10_000, 100),
            candidate(11, 1_000, "shell", 9_000, 100),
            candidate(12, 1_000, "worker", 4_000, 100),
        ];
        let protection = ProtectionPolicy::new(["shell".to_owned()], [], [], []);

        let selected = select_emergency_victim(
            &candidates,
            1_000,
            &protection,
            &policy(EmergencyAction::TerminateLargestUnprotected),
        )
        .unwrap();

        assert_eq!(selected.process.identity().pid(), 12);
    }

    #[test]
    fn notify_only_never_selects_a_victim() {
        assert!(
            select_emergency_victim(
                &[candidate(10, 1_000, "worker", 2_000, 100)],
                1_000,
                &ProtectionPolicy::default(),
                &policy(EmergencyAction::NotifyOnly),
            )
            .is_none()
        );
    }

    #[test]
    fn force_termination_requires_activation_and_opt_in() {
        assert!(!force_termination_permitted(true, false));
        assert!(!force_termination_permitted(false, true));
        assert!(force_termination_permitted(true, true));
    }
}
