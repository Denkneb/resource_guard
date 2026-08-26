use std::{env, error::Error, ffi::OsString, fmt, path::PathBuf};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConfigPathError {
    MissingHome,
}

impl fmt::Display for ConfigPathError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingHome => write!(
                formatter,
                "cannot determine config path: HOME and XDG_CONFIG_HOME are unset"
            ),
        }
    }
}

impl Error for ConfigPathError {}

/// Resolves the Resource Guard configuration path from process environment.
///
/// # Errors
///
/// Returns an error when no override, XDG directory, or home directory is available.
pub fn resolve_config_path() -> Result<PathBuf, ConfigPathError> {
    resolve_from(
        env::var_os("RESOURCE_GUARD_CONFIG"),
        env::var_os("XDG_CONFIG_HOME"),
        env::var_os("HOME"),
    )
}

fn resolve_from(
    override_path: Option<OsString>,
    xdg_config_home: Option<OsString>,
    home: Option<OsString>,
) -> Result<PathBuf, ConfigPathError> {
    if let Some(path) = non_empty(override_path) {
        return Ok(PathBuf::from(path));
    }
    if let Some(path) = non_empty(xdg_config_home) {
        return Ok(PathBuf::from(path).join("resource-guard/config.toml"));
    }
    non_empty(home)
        .map(PathBuf::from)
        .map(|path| path.join(".config/resource-guard/config.toml"))
        .ok_or(ConfigPathError::MissingHome)
}

fn non_empty(value: Option<OsString>) -> Option<OsString> {
    value.filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use std::{ffi::OsString, path::PathBuf};

    use super::{ConfigPathError, resolve_from};

    #[test]
    fn override_has_highest_priority() {
        assert_eq!(
            resolve_from(
                Some(OsString::from("/custom/config.toml")),
                Some(OsString::from("/xdg")),
                Some(OsString::from("/home/user")),
            ),
            Ok(PathBuf::from("/custom/config.toml"))
        );
    }

    #[test]
    fn uses_xdg_before_home() {
        assert_eq!(
            resolve_from(
                None,
                Some(OsString::from("/xdg")),
                Some(OsString::from("/home/user")),
            ),
            Ok(PathBuf::from("/xdg/resource-guard/config.toml"))
        );
    }

    #[test]
    fn falls_back_to_home() {
        assert_eq!(
            resolve_from(None, None, Some(OsString::from("/home/user"))),
            Ok(PathBuf::from(
                "/home/user/.config/resource-guard/config.toml"
            ))
        );
    }

    #[test]
    fn fails_without_any_base_directory() {
        assert_eq!(
            resolve_from(None, None, None),
            Err(ConfigPathError::MissingHome)
        );
    }
}
