use std::{error::Error, fmt, path::PathBuf, time::Duration};

use crate::domain::{ProtectionPolicy, Thresholds, ViolationPolicy};

const BYTES_PER_MIB: u64 = 1_048_576;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Settings {
    pub monitor: MonitorSettings,
    pub termination: TerminationSettings,
    pub processes: ProcessSettings,
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
        if self.termination.grace_period.is_zero() {
            return Err(ConfigValidationError::ZeroGracePeriod);
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
    ZeroGracePeriod,
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
            Self::ZeroGracePeriod => write!(formatter, "grace period must be greater than zero"),
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
    }
}
