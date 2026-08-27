use std::{
    error::Error,
    fmt, fs,
    io::{self, Write},
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use serde::{Deserialize, Serialize};

use crate::application::{
    ConfigValidationError, EmergencySettings, MemoryPressureSettings, MonitorSettings,
    NotificationSettings, ProcessSettings, Settings, TerminationSettings,
};
use crate::domain::EmergencyAction;

use super::{ConfigPathError, resolve_config_path};

const BYTES_PER_MIB: u64 = 1_048_576;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigOrigin {
    Defaults,
    File,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LoadedSettings {
    pub settings: Settings,
    pub origin: ConfigOrigin,
}

#[derive(Clone, Debug)]
pub struct TomlConfigRepository {
    path: PathBuf,
}

impl TomlConfigRepository {
    /// Creates a repository using the standard environment-based config path.
    ///
    /// # Errors
    ///
    /// Returns an error when the configuration path cannot be resolved.
    pub fn from_environment() -> Result<Self, ConfigError> {
        resolve_config_path()
            .map(Self::new)
            .map_err(ConfigError::Path)
    }

    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Loads a file or returns validated defaults when it does not exist.
    ///
    /// # Errors
    ///
    /// Returns an error for unreadable, malformed, or invalid configuration.
    pub fn load(&self) -> Result<LoadedSettings, ConfigError> {
        match fs::read_to_string(&self.path) {
            Ok(contents) => {
                let document: ConfigDocument =
                    toml::from_str(&contents).map_err(|error| ConfigError::Parse {
                        path: self.path.clone(),
                        message: error.to_string(),
                    })?;
                let settings = Settings::from(document);
                settings.validate().map_err(ConfigError::Validation)?;
                Ok(LoadedSettings {
                    settings,
                    origin: ConfigOrigin::File,
                })
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let settings = Settings::default();
                settings.validate().map_err(ConfigError::Validation)?;
                Ok(LoadedSettings {
                    settings,
                    origin: ConfigOrigin::Defaults,
                })
            }
            Err(error) => Err(ConfigError::Read {
                path: self.path.clone(),
                message: error.to_string(),
            }),
        }
    }

    /// Serializes effective settings as a complete TOML document.
    ///
    /// # Errors
    ///
    /// Returns an error when settings cannot be serialized.
    pub fn render(settings: &Settings) -> Result<String, ConfigError> {
        toml::to_string_pretty(&ConfigDocument::from(settings)).map_err(|error| {
            ConfigError::Serialize {
                message: error.to_string(),
            }
        })
    }

    /// Atomically replaces the configuration with the supplied validated settings.
    ///
    /// # Errors
    ///
    /// Returns an error when validation, serialization, directory creation, writing,
    /// syncing, or atomic replacement fails.
    pub fn save(&self, settings: &Settings) -> Result<(), ConfigError> {
        settings.validate().map_err(ConfigError::Validation)?;
        let contents = Self::render(settings)?;
        let Some(parent) = self.path.parent() else {
            return Err(ConfigError::Write {
                path: self.path.clone(),
                message: "config path has no parent directory".to_owned(),
            });
        };
        fs::create_dir_all(parent).map_err(|error| ConfigError::CreateDirectory {
            path: parent.to_path_buf(),
            message: error.to_string(),
        })?;
        let nonce = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let temporary = self
            .path
            .with_extension(format!("tmp-{}-{nonce}", std::process::id()));
        let result = (|| {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)
                .map_err(|error| ConfigError::Write {
                    path: temporary.clone(),
                    message: error.to_string(),
                })?;
            file.write_all(contents.as_bytes())
                .and_then(|()| file.sync_all())
                .map_err(|error| ConfigError::Write {
                    path: temporary.clone(),
                    message: error.to_string(),
                })?;
            fs::rename(&temporary, &self.path).map_err(|error| ConfigError::Write {
                path: self.path.clone(),
                message: error.to_string(),
            })
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result
    }

    /// Creates a default configuration file.
    ///
    /// # Errors
    ///
    /// Returns an error if the file exists without `force`, or if a directory or
    /// file cannot be created.
    pub fn initialize(&self, force: bool) -> Result<(), ConfigError> {
        let Some(parent) = self.path.parent() else {
            return Err(ConfigError::Write {
                path: self.path.clone(),
                message: "config path has no parent directory".to_owned(),
            });
        };
        fs::create_dir_all(parent).map_err(|error| ConfigError::CreateDirectory {
            path: parent.to_path_buf(),
            message: error.to_string(),
        })?;

        let contents = Self::render(&Settings::default())?;
        let mut options = fs::OpenOptions::new();
        options.write(true);
        if force {
            options.create(true).truncate(true);
        } else {
            options.create_new(true);
        }

        let mut file = options.open(&self.path).map_err(|error| {
            if error.kind() == io::ErrorKind::AlreadyExists {
                ConfigError::AlreadyExists(self.path.clone())
            } else {
                ConfigError::Write {
                    path: self.path.clone(),
                    message: error.to_string(),
                }
            }
        })?;
        file.write_all(contents.as_bytes())
            .map_err(|error| ConfigError::Write {
                path: self.path.clone(),
                message: error.to_string(),
            })
    }
}

#[derive(Debug)]
pub enum ConfigError {
    Path(ConfigPathError),
    Read { path: PathBuf, message: String },
    Parse { path: PathBuf, message: String },
    Validation(ConfigValidationError),
    Serialize { message: String },
    CreateDirectory { path: PathBuf, message: String },
    Write { path: PathBuf, message: String },
    AlreadyExists(PathBuf),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Path(error) => error.fmt(formatter),
            Self::Read { path, message } => {
                write!(formatter, "cannot read {}: {message}", path.display())
            }
            Self::Parse { path, message } => {
                write!(formatter, "invalid TOML in {}: {message}", path.display())
            }
            Self::Validation(error) => write!(formatter, "invalid configuration: {error}"),
            Self::Serialize { message } => {
                write!(formatter, "cannot serialize configuration: {message}")
            }
            Self::CreateDirectory { path, message } => write!(
                formatter,
                "cannot create config directory {}: {message}",
                path.display()
            ),
            Self::Write { path, message } => {
                write!(formatter, "cannot write {}: {message}", path.display())
            }
            Self::AlreadyExists(path) => write!(
                formatter,
                "configuration already exists at {}; use --force to overwrite it",
                path.display()
            ),
        }
    }
}

impl Error for ConfigError {}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct ConfigDocument {
    monitor: MonitorDocument,
    memory_pressure: MemoryPressureDocument,
    emergency: EmergencyDocument,
    termination: TerminationDocument,
    processes: ProcessDocument,
    notifications: NotificationDocument,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct MemoryPressureDocument {
    enabled: bool,
    warning_available_percent: f32,
    critical_available_percent: f32,
    emergency_available_mib: u64,
    critical_swap_used_percent: f32,
    critical_psi_full_avg10: f32,
    critical_samples: u32,
    warning_poll_interval_ms: u64,
    critical_poll_interval_ms: u64,
    recovery_available_percent: f32,
}

impl Default for MemoryPressureDocument {
    fn default() -> Self {
        Self::from(&MemoryPressureSettings::default())
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum EmergencyActionDocument {
    #[default]
    NotifyOnly,
    TerminateAllowlisted,
    TerminateLargestUnprotected,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct EmergencyDocument {
    action: EmergencyActionDocument,
    allow_sigkill: bool,
    term_grace_seconds: u64,
    action_cooldown_seconds: u64,
    allowed_names: Vec<String>,
    allowed_executables: Vec<PathBuf>,
    exempt_names: Vec<String>,
    exempt_executables: Vec<PathBuf>,
}

impl Default for EmergencyDocument {
    fn default() -> Self {
        Self::from(&EmergencySettings::default())
    }
}

impl Default for ConfigDocument {
    fn default() -> Self {
        Self::from(&Settings::default())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct MonitorDocument {
    poll_interval_seconds: u64,
    consecutive_samples: u32,
    minimum_duration_seconds: u64,
    cooldown_seconds: u64,
    max_cpu_percent: f32,
    max_memory_mib: u64,
}

impl Default for MonitorDocument {
    fn default() -> Self {
        Self::from(&MonitorSettings::default())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct TerminationDocument {
    grace_period_seconds: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct NotificationDocument {
    enabled: bool,
    timeout_seconds: u64,
}

impl Default for NotificationDocument {
    fn default() -> Self {
        Self::from(&NotificationSettings::default())
    }
}

impl Default for TerminationDocument {
    fn default() -> Self {
        Self::from(&TerminationSettings::default())
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct ProcessDocument {
    protected_names: Vec<String>,
    protected_executables: Vec<PathBuf>,
    ignored_names: Vec<String>,
    ignored_executables: Vec<PathBuf>,
}

impl From<ConfigDocument> for Settings {
    fn from(document: ConfigDocument) -> Self {
        Self {
            monitor: MonitorSettings {
                poll_interval: Duration::from_secs(document.monitor.poll_interval_seconds),
                consecutive_samples: document.monitor.consecutive_samples,
                minimum_duration: Duration::from_secs(document.monitor.minimum_duration_seconds),
                cooldown: Duration::from_secs(document.monitor.cooldown_seconds),
                max_cpu_percent: document.monitor.max_cpu_percent,
                max_memory_bytes: document
                    .monitor
                    .max_memory_mib
                    .saturating_mul(BYTES_PER_MIB),
            },
            memory_pressure: MemoryPressureSettings {
                enabled: document.memory_pressure.enabled,
                warning_available_percent: document.memory_pressure.warning_available_percent,
                critical_available_percent: document.memory_pressure.critical_available_percent,
                emergency_available_bytes: document
                    .memory_pressure
                    .emergency_available_mib
                    .saturating_mul(BYTES_PER_MIB),
                critical_swap_used_percent: document.memory_pressure.critical_swap_used_percent,
                critical_psi_full_avg10: document.memory_pressure.critical_psi_full_avg10,
                critical_samples: document.memory_pressure.critical_samples,
                warning_poll_interval: Duration::from_millis(
                    document.memory_pressure.warning_poll_interval_ms,
                ),
                critical_poll_interval: Duration::from_millis(
                    document.memory_pressure.critical_poll_interval_ms,
                ),
                recovery_available_percent: document.memory_pressure.recovery_available_percent,
            },
            emergency: EmergencySettings {
                action: EmergencyAction::from(document.emergency.action),
                allow_sigkill: document.emergency.allow_sigkill,
                term_grace_period: Duration::from_secs(document.emergency.term_grace_seconds),
                action_cooldown: Duration::from_secs(document.emergency.action_cooldown_seconds),
                allowed_names: document.emergency.allowed_names,
                allowed_executables: document.emergency.allowed_executables,
                exempt_names: document.emergency.exempt_names,
                exempt_executables: document.emergency.exempt_executables,
            },
            termination: TerminationSettings {
                grace_period: Duration::from_secs(document.termination.grace_period_seconds),
            },
            processes: ProcessSettings {
                protected_names: document.processes.protected_names,
                protected_executables: document.processes.protected_executables,
                ignored_names: document.processes.ignored_names,
                ignored_executables: document.processes.ignored_executables,
            },
            notifications: NotificationSettings {
                enabled: document.notifications.enabled,
                timeout: Duration::from_secs(document.notifications.timeout_seconds),
            },
        }
    }
}

impl From<&Settings> for ConfigDocument {
    fn from(settings: &Settings) -> Self {
        Self {
            monitor: MonitorDocument::from(&settings.monitor),
            memory_pressure: MemoryPressureDocument::from(&settings.memory_pressure),
            emergency: EmergencyDocument::from(&settings.emergency),
            termination: TerminationDocument::from(&settings.termination),
            processes: ProcessDocument::from(&settings.processes),
            notifications: NotificationDocument::from(&settings.notifications),
        }
    }
}

impl From<EmergencyActionDocument> for EmergencyAction {
    fn from(action: EmergencyActionDocument) -> Self {
        match action {
            EmergencyActionDocument::NotifyOnly => Self::NotifyOnly,
            EmergencyActionDocument::TerminateAllowlisted => Self::TerminateAllowlisted,
            EmergencyActionDocument::TerminateLargestUnprotected => {
                Self::TerminateLargestUnprotected
            }
        }
    }
}

impl From<EmergencyAction> for EmergencyActionDocument {
    fn from(action: EmergencyAction) -> Self {
        match action {
            EmergencyAction::NotifyOnly => Self::NotifyOnly,
            EmergencyAction::TerminateAllowlisted => Self::TerminateAllowlisted,
            EmergencyAction::TerminateLargestUnprotected => Self::TerminateLargestUnprotected,
        }
    }
}

impl From<&MemoryPressureSettings> for MemoryPressureDocument {
    fn from(settings: &MemoryPressureSettings) -> Self {
        Self {
            enabled: settings.enabled,
            warning_available_percent: settings.warning_available_percent,
            critical_available_percent: settings.critical_available_percent,
            emergency_available_mib: settings.emergency_available_bytes / BYTES_PER_MIB,
            critical_swap_used_percent: settings.critical_swap_used_percent,
            critical_psi_full_avg10: settings.critical_psi_full_avg10,
            critical_samples: settings.critical_samples,
            warning_poll_interval_ms: duration_milliseconds(settings.warning_poll_interval),
            critical_poll_interval_ms: duration_milliseconds(settings.critical_poll_interval),
            recovery_available_percent: settings.recovery_available_percent,
        }
    }
}

impl From<&EmergencySettings> for EmergencyDocument {
    fn from(settings: &EmergencySettings) -> Self {
        Self {
            action: settings.action.into(),
            allow_sigkill: settings.allow_sigkill,
            term_grace_seconds: settings.term_grace_period.as_secs(),
            action_cooldown_seconds: settings.action_cooldown.as_secs(),
            allowed_names: settings.allowed_names.clone(),
            allowed_executables: settings.allowed_executables.clone(),
            exempt_names: settings.exempt_names.clone(),
            exempt_executables: settings.exempt_executables.clone(),
        }
    }
}

fn duration_milliseconds(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

impl From<&MonitorSettings> for MonitorDocument {
    fn from(settings: &MonitorSettings) -> Self {
        Self {
            poll_interval_seconds: settings.poll_interval.as_secs(),
            consecutive_samples: settings.consecutive_samples,
            minimum_duration_seconds: settings.minimum_duration.as_secs(),
            cooldown_seconds: settings.cooldown.as_secs(),
            max_cpu_percent: settings.max_cpu_percent,
            max_memory_mib: settings.max_memory_bytes / BYTES_PER_MIB,
        }
    }
}

impl From<&TerminationSettings> for TerminationDocument {
    fn from(settings: &TerminationSettings) -> Self {
        Self {
            grace_period_seconds: settings.grace_period.as_secs(),
        }
    }
}

impl From<&NotificationSettings> for NotificationDocument {
    fn from(settings: &NotificationSettings) -> Self {
        Self {
            enabled: settings.enabled,
            timeout_seconds: settings.timeout.as_secs(),
        }
    }
}

impl From<&ProcessSettings> for ProcessDocument {
    fn from(settings: &ProcessSettings) -> Self {
        Self {
            protected_names: settings.protected_names.clone(),
            protected_executables: settings.protected_executables.clone(),
            ignored_names: settings.ignored_names.clone(),
            ignored_executables: settings.ignored_executables.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::{ConfigError, ConfigOrigin, TomlConfigRepository};

    #[test]
    fn missing_file_uses_defaults() {
        let directory = tempdir().unwrap();
        let repository = TomlConfigRepository::new(directory.path().join("config.toml"));

        let loaded = repository.load().unwrap();

        assert_eq!(loaded.origin, ConfigOrigin::Defaults);
        assert_eq!(loaded.settings.monitor.poll_interval.as_secs(), 5);
    }

    #[test]
    fn loads_partial_document_with_section_defaults() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("config.toml");
        fs::write(&path, "[monitor]\npoll_interval_seconds = 7\n").unwrap();
        let repository = TomlConfigRepository::new(path);

        let loaded = repository.load().unwrap();

        assert_eq!(loaded.origin, ConfigOrigin::File);
        assert_eq!(loaded.settings.monitor.poll_interval.as_secs(), 7);
        assert_eq!(loaded.settings.monitor.consecutive_samples, 3);
        assert!(loaded.settings.memory_pressure.enabled);
        assert_eq!(
            loaded
                .settings
                .memory_pressure
                .critical_poll_interval
                .as_millis(),
            500
        );
        assert_eq!(
            loaded.settings.emergency.action,
            crate::domain::EmergencyAction::NotifyOnly
        );
    }

    #[test]
    fn rejects_unknown_fields() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("config.toml");
        fs::write(&path, "[monitor]\nunknown = 7\n").unwrap();
        let repository = TomlConfigRepository::new(path);

        assert!(matches!(repository.load(), Err(ConfigError::Parse { .. })));
    }

    #[test]
    fn rejects_invalid_values() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("config.toml");
        fs::write(&path, "[monitor]\npoll_interval_seconds = 0\n").unwrap();
        let repository = TomlConfigRepository::new(path);

        assert!(matches!(repository.load(), Err(ConfigError::Validation(_))));
    }

    #[test]
    fn loads_explicit_emergency_policy() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("config.toml");
        fs::write(
            &path,
            "[emergency]\naction = \"terminate_allowlisted\"\nallowed_names = [\"worker\"]\n",
        )
        .unwrap();
        let repository = TomlConfigRepository::new(path);

        let settings = repository.load().unwrap().settings;

        assert_eq!(
            settings.emergency.action,
            crate::domain::EmergencyAction::TerminateAllowlisted
        );
        assert_eq!(settings.emergency.allowed_names, vec!["worker"]);
    }

    #[test]
    fn rejects_invalid_pressure_threshold_ordering() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("config.toml");
        fs::write(
            &path,
            "[memory_pressure]\nwarning_available_percent = 5\ncritical_available_percent = 8\n",
        )
        .unwrap();
        let repository = TomlConfigRepository::new(path);

        assert!(matches!(repository.load(), Err(ConfigError::Validation(_))));
    }

    #[test]
    fn initialize_does_not_overwrite_without_force() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("nested/config.toml");
        let repository = TomlConfigRepository::new(&path);
        repository.initialize(false).unwrap();
        fs::write(&path, "custom").unwrap();

        assert!(matches!(
            repository.initialize(false),
            Err(ConfigError::AlreadyExists(_))
        ));
        assert_eq!(fs::read_to_string(path).unwrap(), "custom");
    }

    #[test]
    fn force_replaces_existing_file_with_valid_defaults() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("config.toml");
        fs::write(&path, "invalid").unwrap();
        let repository = TomlConfigRepository::new(path);

        repository.initialize(true).unwrap();

        assert_eq!(repository.load().unwrap().origin, ConfigOrigin::File);
    }

    #[test]
    fn rendered_defaults_round_trip() {
        let defaults = crate::application::Settings::default();
        let rendered = TomlConfigRepository::render(&defaults).unwrap();
        let document: super::ConfigDocument = toml::from_str(&rendered).unwrap();

        assert_eq!(crate::application::Settings::from(document), defaults);
    }

    #[test]
    fn save_atomically_persists_updated_settings() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("config.toml");
        let repository = TomlConfigRepository::new(&path);
        let mut settings = crate::application::Settings::default();
        settings.processes.ignored_names.push("compiler".to_owned());

        repository.save(&settings).unwrap();

        assert_eq!(repository.load().unwrap().settings, settings);
        assert!(fs::read_to_string(path).unwrap().contains("compiler"));
    }
}
