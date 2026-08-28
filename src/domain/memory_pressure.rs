use super::SystemResources;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MemoryPsi {
    pub some_avg10: f32,
    pub full_avg10: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MemoryPressureSample {
    pub system: SystemResources,
    pub psi: MemoryPsi,
}

impl MemoryPressureSample {
    #[must_use]
    pub fn available_percent(self) -> f32 {
        percent(
            self.system.available_memory_bytes,
            self.system.total_memory_bytes,
        )
    }

    #[must_use]
    pub fn swap_used_percent(self) -> f32 {
        percent(self.system.used_swap_bytes, self.system.total_swap_bytes)
    }
}

fn percent(value: u64, total: u64) -> f32 {
    if total == 0 {
        0.0
    } else {
        let basis_points = (u128::from(value) * 10_000 / u128::from(total)).min(10_000);
        let basis_points = u16::try_from(basis_points).unwrap_or(10_000);
        f32::from(basis_points) / 100.0
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum MemoryPressureLevel {
    #[default]
    Normal,
    Warning,
    Critical,
    Recovery,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MemoryPressurePolicy {
    pub enabled: bool,
    pub warning_available_percent: f32,
    pub critical_available_percent: f32,
    pub emergency_available_bytes: u64,
    pub critical_swap_used_percent: f32,
    pub critical_psi_full_avg10: f32,
    pub critical_samples: u32,
    pub recovery_available_percent: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MemoryPressureEvaluation {
    pub previous: MemoryPressureLevel,
    pub current: MemoryPressureLevel,
    pub sample: MemoryPressureSample,
}

impl MemoryPressureEvaluation {
    #[must_use]
    pub fn changed(self) -> bool {
        self.previous != self.current
    }
}

#[derive(Debug)]
pub struct MemoryPressureTracker {
    policy: MemoryPressurePolicy,
    level: MemoryPressureLevel,
    critical_samples: u32,
}

impl MemoryPressureTracker {
    #[must_use]
    pub const fn new(policy: MemoryPressurePolicy) -> Self {
        Self {
            policy,
            level: MemoryPressureLevel::Normal,
            critical_samples: 0,
        }
    }

    #[must_use]
    pub const fn level(&self) -> MemoryPressureLevel {
        self.level
    }

    pub fn evaluate(&mut self, sample: MemoryPressureSample) -> MemoryPressureEvaluation {
        let previous = self.level;
        self.level = self.next_level(sample);
        MemoryPressureEvaluation {
            previous,
            current: self.level,
            sample,
        }
    }

    fn next_level(&mut self, sample: MemoryPressureSample) -> MemoryPressureLevel {
        if !self.policy.enabled {
            self.critical_samples = 0;
            return MemoryPressureLevel::Normal;
        }

        let available_percent = sample.available_percent();
        let swap_critical = sample.system.total_swap_bytes > 0
            && sample.swap_used_percent() >= self.policy.critical_swap_used_percent;
        let psi_critical = sample.psi.full_avg10 >= self.policy.critical_psi_full_avg10;
        let emergency_floor =
            sample.system.available_memory_bytes <= self.policy.emergency_available_bytes;
        let critical_signal = emergency_floor
            || (available_percent <= self.policy.critical_available_percent
                && (swap_critical || psi_critical));

        if critical_signal {
            self.critical_samples = self.critical_samples.saturating_add(1);
        } else {
            self.critical_samples = 0;
        }

        if self.level == MemoryPressureLevel::Critical {
            if available_percent >= self.policy.recovery_available_percent && !psi_critical {
                return MemoryPressureLevel::Recovery;
            }
            return MemoryPressureLevel::Critical;
        }

        if emergency_floor || self.critical_samples >= self.policy.critical_samples {
            return MemoryPressureLevel::Critical;
        }

        if critical_signal
            || available_percent <= self.policy.warning_available_percent
            || psi_critical
        {
            return MemoryPressureLevel::Warning;
        }

        MemoryPressureLevel::Normal
    }
}

#[cfg(test)]
mod tests {
    use super::{
        MemoryPressureLevel, MemoryPressurePolicy, MemoryPressureSample, MemoryPressureTracker,
        MemoryPsi,
    };
    use crate::domain::SystemResources;

    const GIB: u64 = 1_024 * 1_024 * 1_024;

    fn policy() -> MemoryPressurePolicy {
        MemoryPressurePolicy {
            enabled: true,
            warning_available_percent: 15.0,
            critical_available_percent: 8.0,
            emergency_available_bytes: 512 * 1_024 * 1_024,
            critical_swap_used_percent: 90.0,
            critical_psi_full_avg10: 5.0,
            critical_samples: 2,
            recovery_available_percent: 20.0,
        }
    }

    fn sample(available_gib: u64, swap_used_gib: u64, full_avg10: f32) -> MemoryPressureSample {
        MemoryPressureSample {
            system: SystemResources {
                total_memory_bytes: 32 * GIB,
                available_memory_bytes: available_gib * GIB,
                total_swap_bytes: 10 * GIB,
                used_swap_bytes: swap_used_gib * GIB,
            },
            psi: MemoryPsi {
                some_avg10: full_avg10,
                full_avg10,
            },
        }
    }

    #[test]
    fn requires_repeated_critical_samples() {
        let mut tracker = MemoryPressureTracker::new(policy());

        assert_eq!(
            tracker.evaluate(sample(2, 9, 0.0)).current,
            MemoryPressureLevel::Warning
        );
        assert_eq!(
            tracker.evaluate(sample(2, 9, 0.0)).current,
            MemoryPressureLevel::Critical
        );
    }

    #[test]
    fn full_swap_without_active_memory_pressure_stays_normal() {
        let mut tracker = MemoryPressureTracker::new(policy());

        assert_eq!(
            tracker.evaluate(sample(16, 10, 0.0)).current,
            MemoryPressureLevel::Normal
        );
    }

    #[test]
    fn emergency_floor_is_immediately_critical() {
        let mut tracker = MemoryPressureTracker::new(policy());
        let mut urgent = sample(1, 0, 0.0);
        urgent.system.available_memory_bytes = 256 * 1_024 * 1_024;

        assert_eq!(
            tracker.evaluate(urgent).current,
            MemoryPressureLevel::Critical
        );
    }

    #[test]
    fn critical_state_uses_recovery_hysteresis() {
        let mut tracker = MemoryPressureTracker::new(policy());
        tracker.evaluate(sample(2, 9, 0.0));
        tracker.evaluate(sample(2, 9, 0.0));

        assert_eq!(
            tracker.evaluate(sample(5, 0, 0.0)).current,
            MemoryPressureLevel::Critical
        );
        assert_eq!(
            tracker.evaluate(sample(8, 0, 0.0)).current,
            MemoryPressureLevel::Recovery
        );
        assert_eq!(
            tracker.evaluate(sample(8, 0, 0.0)).current,
            MemoryPressureLevel::Normal
        );
    }

    #[test]
    fn recovered_memory_leaves_critical_even_when_swap_remains_full() {
        let mut tracker = MemoryPressureTracker::new(policy());
        tracker.evaluate(sample(2, 10, 0.0));
        tracker.evaluate(sample(2, 10, 0.0));

        assert_eq!(
            tracker.evaluate(sample(8, 10, 0.0)).current,
            MemoryPressureLevel::Recovery
        );
        assert_eq!(
            tracker.evaluate(sample(8, 10, 0.0)).current,
            MemoryPressureLevel::Normal
        );
    }
}
