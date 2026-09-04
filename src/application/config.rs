use std::{error::Error, fmt, path::PathBuf, time::Duration};

use crate::domain::{
    EmergencyAction, EmergencyActivationPolicy, EmergencyPolicy, IgnoreRule, MemoryPressurePolicy,
    ProtectionPolicy, StaleWorkloadPolicy, Thresholds, ViolationPolicy,
};

const BYTES_PER_MIB: u64 = 1_048_576;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Settings {
    pub monitor: MonitorSettings,
    pub memory_pressure: MemoryPressureSettings,
    pub emergency: EmergencySettings,
    pub stale_workloads: StaleWorkloadSettings,
    pub termination: TerminationSettings,
    pub processes: ProcessSettings,
    pub notifications: NotificationSettings,
}

impl Settings {
    /// Checks all application-level configuration invariants.
    ///
    /// # Errors
    ///
    /// Returns the first invalid setting.
    pub fn validate(&self) -> Result<(), ConfigValidationError> {
        if self.monitor.poll_interval.is_zero() {
            return Err(ConfigValidationError::ZeroPollInterval);
        }
        if self.monitor.consecutive_samples == 0 {
            return Err(ConfigValidationError::ZeroConsecutiveSamples);
        }
        if !self.monitor.max_cpu_percent.is_finite() || self.monitor.max_cpu_percent <= 0.0 {
            return Err(ConfigValidationError::InvalidCpuThreshold);
        }
        if self.monitor.max_memory_bytes == 0 {
            return Err(ConfigValidationError::ZeroMemoryThreshold);
        }
        if !valid_percent(self.memory_pressure.warning_available_percent)
            || !valid_percent(self.memory_pressure.critical_available_percent)
            || !valid_percent(self.memory_pressure.critical_swap_used_percent)
            || !valid_percent(self.memory_pressure.recovery_available_percent)
        {
            return Err(ConfigValidationError::InvalidPressurePercentage);
        }
        if self.memory_pressure.critical_available_percent
            >= self.memory_pressure.warning_available_percent
            || self.memory_pressure.recovery_available_percent
                <= self.memory_pressure.warning_available_percent
        {
            return Err(ConfigValidationError::InvalidPressureOrdering);
        }
        if self.memory_pressure.emergency_available_bytes == 0 {
            return Err(ConfigValidationError::ZeroEmergencyMemoryFloor);
        }
        if !self.memory_pressure.critical_psi_full_avg10.is_finite()
            || self.memory_pressure.critical_psi_full_avg10 < 0.0
            || self.memory_pressure.critical_psi_full_avg10 > 100.0
        {
            return Err(ConfigValidationError::InvalidPsiThreshold);
        }
        if self.memory_pressure.critical_samples == 0 {
            return Err(ConfigValidationError::ZeroCriticalSamples);
        }
        if self.memory_pressure.warning_poll_interval.is_zero()
            || self.memory_pressure.critical_poll_interval.is_zero()
        {
            return Err(ConfigValidationError::ZeroPressurePollInterval);
        }
        if self.emergency.term_grace_period.is_zero() {
            return Err(ConfigValidationError::ZeroEmergencyGracePeriod);
        }
        if self.emergency.action_cooldown.is_zero() {
            return Err(ConfigValidationError::ZeroEmergencyCooldown);
        }
        if self.emergency.action_available_bytes == 0 {
            return Err(ConfigValidationError::ZeroEmergencyActionMemory);
        }
        if !self.emergency.action_psi_full_avg10.is_finite()
            || self.emergency.action_psi_full_avg10 < 0.0
            || self.emergency.action_psi_full_avg10 > 100.0
        {
            return Err(ConfigValidationError::InvalidEmergencyPsiThreshold);
        }
        if self.termination.grace_period.is_zero() {
            return Err(ConfigValidationError::ZeroGracePeriod);
        }
        if self.stale_workloads.minimum_age.is_zero()
            || self.stale_workloads.minimum_tree_memory_bytes == 0
            || self.stale_workloads.consecutive_samples == 0
            || self.stale_workloads.notification_cooldown.is_zero()
            || !self.stale_workloads.maximum_cpu_percent.is_finite()
            || self.stale_workloads.maximum_cpu_percent < 0.0
        {
            return Err(ConfigValidationError::InvalidStaleWorkloadPolicy);
        }
        if self.notifications.timeout.is_zero() {
            return Err(ConfigValidationError::ZeroNotificationTimeout);
        }

        for path in self
            .processes
            .protected_executables
            .iter()
            .chain(&self.processes.ignored_executables)
        {
            if !path.is_absolute() {
                return Err(ConfigValidationError::RelativeExecutable(path.clone()));
            }
        }

        for path in self
            .emergency
            .allowed_executables
            .iter()
            .chain(&self.emergency.exempt_executables)
        {
            if !path.is_absolute() {
                return Err(ConfigValidationError::RelativeExecutable(path.clone()));
            }
        }

        Ok(())
    }

    #[must_use]
    pub const fn thresholds(&self) -> Thresholds {
        Thresholds {
            max_cpu_percent: Some(self.monitor.max_cpu_percent),
            max_resident_memory_bytes: Some(self.monitor.max_memory_bytes),
        }
    }

    #[must_use]
    pub const fn violation_policy(&self) -> ViolationPolicy {
        ViolationPolicy::new(
            self.monitor.consecutive_samples,
            self.monitor.minimum_duration,
            self.monitor.cooldown,
        )
    }

    #[must_use]
    pub fn protection_policy(&self) -> ProtectionPolicy {
        ProtectionPolicy::new(
            self.processes.protected_names.clone(),
            self.processes.protected_executables.clone(),
            self.processes.ignored_names.clone(),
            self.processes.ignored_executables.clone(),
        )
    }

    #[must_use]
    pub const fn memory_pressure_policy(&self) -> MemoryPressurePolicy {
        MemoryPressurePolicy {
            enabled: self.memory_pressure.enabled,
            warning_available_percent: self.memory_pressure.warning_available_percent,
            critical_available_percent: self.memory_pressure.critical_available_percent,
            emergency_available_bytes: self.memory_pressure.emergency_available_bytes,
            critical_swap_used_percent: self.memory_pressure.critical_swap_used_percent,
            critical_psi_full_avg10: self.memory_pressure.critical_psi_full_avg10,
            critical_samples: self.memory_pressure.critical_samples,
            recovery_available_percent: self.memory_pressure.recovery_available_percent,
        }
    }

    #[must_use]
    pub fn emergency_policy(&self) -> EmergencyPolicy {
        EmergencyPolicy {
            action: self.emergency.action,
            allowed_names: self.emergency.allowed_names.iter().cloned().collect(),
            allowed_executables: self.emergency.allowed_executables.iter().cloned().collect(),
            exempt_names: self.emergency.exempt_names.iter().cloned().collect(),
            exempt_executables: self.emergency.exempt_executables.iter().cloned().collect(),
        }
    }

    #[must_use]
    pub const fn emergency_activation_policy(&self) -> EmergencyActivationPolicy {
        EmergencyActivationPolicy {
            action_available_bytes: self.emergency.action_available_bytes,
            action_psi_full_avg10: self.emergency.action_psi_full_avg10,
        }
    }

    #[must_use]
    pub fn stale_workload_policy(&self) -> StaleWorkloadPolicy {
        StaleWorkloadPolicy {
            enabled: self.stale_workloads.enabled,
            only_under_memory_pressure: self.stale_workloads.only_under_memory_pressure,
            candidate_names: self
                .stale_workloads
                .candidate_names
                .iter()
                .cloned()
                .collect(),
            launcher_names: self
                .stale_workloads
                .launcher_names
                .iter()
                .cloned()
                .collect(),
            ignored_root_names: self
                .stale_workloads
                .ignored_root_names
                .iter()
                .cloned()
                .collect(),
            minimum_age: self.stale_workloads.minimum_age,
            minimum_tree_memory_bytes: self.stale_workloads.minimum_tree_memory_bytes,
            maximum_cpu_percent: self.stale_workloads.maximum_cpu_percent,
            consecutive_samples: self.stale_workloads.consecutive_samples,
            notification_cooldown: self.stale_workloads.notification_cooldown,
        }
    }

    pub fn add_ignore_rule(&mut self, rule: IgnoreRule) {
        match rule {
            IgnoreRule::Name(name) => {
                if !self.processes.ignored_names.contains(&name) {
                    self.processes.ignored_names.push(name);
                }
            }
            IgnoreRule::Executable(path) => {
                if !self.processes.ignored_executables.contains(&path) {
                    self.processes.ignored_executables.push(path);
                }
            }
        }
    }

    pub fn add_stale_workload_ignore(&mut self, name: String) {
        if !self.stale_workloads.ignored_root_names.contains(&name) {
            self.stale_workloads.ignored_root_names.push(name);
        }
    }
}

const fn valid_percent(value: f32) -> bool {
    value.is_finite() && value > 0.0 && value <= 100.0
}

#[derive(Clone, Debug, PartialEq)]
pub struct MemoryPressureSettings {
    pub enabled: bool,
    pub warning_available_percent: f32,
    pub critical_available_percent: f32,
    pub emergency_available_bytes: u64,
    pub critical_swap_used_percent: f32,
    pub critical_psi_full_avg10: f32,
    pub critical_samples: u32,
    pub warning_poll_interval: Duration,
    pub critical_poll_interval: Duration,
    pub recovery_available_percent: f32,
}

impl Default for MemoryPressureSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            warning_available_percent: 15.0,
            critical_available_percent: 8.0,
            emergency_available_bytes: 512 * BYTES_PER_MIB,
            critical_swap_used_percent: 90.0,
            critical_psi_full_avg10: 5.0,
            critical_samples: 2,
            warning_poll_interval: Duration::from_secs(1),
            critical_poll_interval: Duration::from_millis(500),
            recovery_available_percent: 20.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct EmergencySettings {
    pub action: EmergencyAction,
    pub allow_sigkill: bool,
    pub action_available_bytes: u64,
    pub action_psi_full_avg10: f32,
    pub term_grace_period: Duration,
    pub action_cooldown: Duration,
    pub allowed_names: Vec<String>,
    pub allowed_executables: Vec<PathBuf>,
    pub exempt_names: Vec<String>,
    pub exempt_executables: Vec<PathBuf>,
}

impl Default for EmergencySettings {
    fn default() -> Self {
        Self {
            action: EmergencyAction::NotifyOnly,
            allow_sigkill: false,
            action_available_bytes: 1_024 * BYTES_PER_MIB,
            action_psi_full_avg10: 5.0,
            term_grace_period: Duration::from_secs(3),
            action_cooldown: Duration::from_secs(30),
            allowed_names: Vec::new(),
            allowed_executables: Vec::new(),
            exempt_names: vec!["resource-guard".to_owned()],
            exempt_executables: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct StaleWorkloadSettings {
    pub enabled: bool,
    pub only_under_memory_pressure: bool,
    pub candidate_names: Vec<String>,
    pub launcher_names: Vec<String>,
    pub ignored_root_names: Vec<String>,
    pub minimum_age: Duration,
    pub minimum_tree_memory_bytes: u64,
    pub maximum_cpu_percent: f32,
    pub consecutive_samples: u32,
    pub notification_cooldown: Duration,
}

impl Default for StaleWorkloadSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            only_under_memory_pressure: true,
            candidate_names: vec!["pytest", "coverage", "black", "pre-commit"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            launcher_names: vec![
                "uv",
                "pytest",
                "coverage",
                "black",
                "pre-commit",
                "python",
                "python3",
                "xargs",
                "bash",
                "sh",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
            ignored_root_names: Vec::new(),
            minimum_age: Duration::from_hours(1),
            minimum_tree_memory_bytes: 256 * BYTES_PER_MIB,
            maximum_cpu_percent: 5.0,
            consecutive_samples: 3,
            notification_cooldown: Duration::from_mins(30),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct NotificationSettings {
    pub enabled: bool,
    pub timeout: Duration,
}

impl Default for NotificationSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            timeout: Duration::from_secs(15),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct MonitorSettings {
    pub poll_interval: Duration,
    pub consecutive_samples: u32,
    pub minimum_duration: Duration,
    pub cooldown: Duration,
    pub max_cpu_percent: f32,
    pub max_memory_bytes: u64,
}

impl Default for MonitorSettings {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(5),
            consecutive_samples: 3,
            minimum_duration: Duration::from_secs(10),
            cooldown: Duration::from_secs(600),
            max_cpu_percent: 90.0,
            max_memory_bytes: 2_048 * BYTES_PER_MIB,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TerminationSettings {
    pub grace_period: Duration,
}

impl Default for TerminationSettings {
    fn default() -> Self {
        Self {
            grace_period: Duration::from_secs(10),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProcessSettings {
    pub protected_names: Vec<String>,
    pub protected_executables: Vec<PathBuf>,
    pub ignored_names: Vec<String>,
    pub ignored_executables: Vec<PathBuf>,
}

impl Default for ProcessSettings {
    fn default() -> Self {
        Self {
            protected_names: vec!["resource-guard".to_owned()],
            protected_executables: Vec::new(),
            ignored_names: Vec::new(),
            ignored_executables: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConfigValidationError {
    ZeroPollInterval,
    ZeroConsecutiveSamples,
    InvalidCpuThreshold,
    ZeroMemoryThreshold,
    InvalidPressurePercentage,
    InvalidPressureOrdering,
    ZeroEmergencyMemoryFloor,
    InvalidPsiThreshold,
    ZeroCriticalSamples,
    ZeroPressurePollInterval,
    ZeroEmergencyGracePeriod,
    ZeroEmergencyCooldown,
    ZeroEmergencyActionMemory,
    InvalidEmergencyPsiThreshold,
    InvalidStaleWorkloadPolicy,
    ZeroGracePeriod,
    ZeroNotificationTimeout,
    RelativeExecutable(PathBuf),
}

impl fmt::Display for ConfigValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroPollInterval => write!(formatter, "poll interval must be greater than zero"),
            Self::ZeroConsecutiveSamples => {
                write!(formatter, "consecutive samples must be greater than zero")
            }
            Self::InvalidCpuThreshold => {
                write!(
                    formatter,
                    "CPU threshold must be finite and greater than zero"
                )
            }
            Self::ZeroMemoryThreshold => {
                write!(formatter, "memory threshold must be greater than zero")
            }
            Self::InvalidPressurePercentage => {
                write!(
                    formatter,
                    "memory pressure percentages must be within 0..=100"
                )
            }
            Self::InvalidPressureOrdering => write!(
                formatter,
                "critical memory percentage must be below warning and recovery must be above warning"
            ),
            Self::ZeroEmergencyMemoryFloor => {
                write!(
                    formatter,
                    "emergency memory floor must be greater than zero"
                )
            }
            Self::InvalidPsiThreshold => {
                write!(formatter, "PSI threshold must be finite and within 0..=100")
            }
            Self::ZeroCriticalSamples => {
                write!(formatter, "critical samples must be greater than zero")
            }
            Self::ZeroPressurePollInterval => {
                write!(
                    formatter,
                    "memory pressure poll intervals must be greater than zero"
                )
            }
            Self::ZeroEmergencyGracePeriod => {
                write!(
                    formatter,
                    "emergency SIGTERM grace period must be greater than zero"
                )
            }
            Self::ZeroEmergencyCooldown => {
                write!(
                    formatter,
                    "emergency action cooldown must be greater than zero"
                )
            }
            Self::ZeroEmergencyActionMemory => {
                write!(
                    formatter,
                    "emergency action available memory must be greater than zero"
                )
            }
            Self::InvalidEmergencyPsiThreshold => {
                write!(
                    formatter,
                    "emergency action PSI threshold must be finite and within 0..=100"
                )
            }
            Self::InvalidStaleWorkloadPolicy => write!(
                formatter,
                "stale workload thresholds, samples, durations, and CPU limit must be valid"
            ),
            Self::ZeroGracePeriod => write!(formatter, "grace period must be greater than zero"),
            Self::ZeroNotificationTimeout => {
                write!(formatter, "notification timeout must be greater than zero")
            }
            Self::RelativeExecutable(path) => write!(
                formatter,
                "protected and ignored executable paths must be absolute: {}",
                path.display()
            ),
        }
    }
}

impl Error for ConfigValidationError {}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, time::Duration};

    use super::{ConfigValidationError, Settings};

    #[test]
    fn default_settings_are_valid() {
        assert_eq!(Settings::default().validate(), Ok(()));
    }

    #[test]
    fn rejects_non_finite_cpu_threshold() {
        let mut settings = Settings::default();
        settings.monitor.max_cpu_percent = f32::NAN;

        assert_eq!(
            settings.validate(),
            Err(ConfigValidationError::InvalidCpuThreshold)
        );
    }

    #[test]
    fn rejects_zero_consecutive_samples() {
        let mut settings = Settings::default();
        settings.monitor.consecutive_samples = 0;

        assert_eq!(
            settings.validate(),
            Err(ConfigValidationError::ZeroConsecutiveSamples)
        );
    }

    #[test]
    fn rejects_relative_executable_paths() {
        let mut settings = Settings::default();
        settings
            .processes
            .ignored_executables
            .push(PathBuf::from("bin/compiler"));

        assert_eq!(
            settings.validate(),
            Err(ConfigValidationError::RelativeExecutable(PathBuf::from(
                "bin/compiler"
            )))
        );
    }

    #[test]
    fn exposes_domain_policies() {
        let settings = Settings::default();

        assert_eq!(settings.thresholds().max_cpu_percent, Some(90.0));
        assert_eq!(
            settings.violation_policy().minimum_duration,
            Duration::from_secs(10)
        );
        assert_eq!(
            settings
                .emergency_activation_policy()
                .action_available_bytes,
            1_024 * 1_024 * 1_024
        );
        assert!(
            (settings.emergency_activation_policy().action_psi_full_avg10 - 5.0).abs()
                < f32::EPSILON
        );
    }

    #[test]
    fn rejects_invalid_emergency_action_thresholds() {
        let mut settings = Settings::default();
        settings.emergency.action_available_bytes = 0;
        assert_eq!(
            settings.validate(),
            Err(ConfigValidationError::ZeroEmergencyActionMemory)
        );

        settings.emergency.action_available_bytes = 1;
        settings.emergency.action_psi_full_avg10 = f32::NAN;
        assert_eq!(
            settings.validate(),
            Err(ConfigValidationError::InvalidEmergencyPsiThreshold)
        );

        settings.emergency.action_psi_full_avg10 = 101.0;
        assert_eq!(
            settings.validate(),
            Err(ConfigValidationError::InvalidEmergencyPsiThreshold)
        );
    }

    #[test]
    fn rejects_zero_notification_timeout() {
        let mut settings = Settings::default();
        settings.notifications.timeout = Duration::ZERO;

        assert_eq!(
            settings.validate(),
            Err(ConfigValidationError::ZeroNotificationTimeout)
        );
    }
}
