use std::{env, ffi::OsString, fs, os::unix::fs::PermissionsExt, path::PathBuf};

use super::RuntimeError;

const RUNTIME_OVERRIDE: &str = "RESOURCE_GUARD_RUNTIME_DIR";

pub(crate) fn runtime_directory() -> Result<PathBuf, RuntimeError> {
    runtime_directory_from(
        env::var_os(RUNTIME_OVERRIDE),
        env::var_os("XDG_RUNTIME_DIR"),
    )
}

fn runtime_directory_from(
    override_path: Option<OsString>,
    xdg_runtime_directory: Option<OsString>,
) -> Result<PathBuf, RuntimeError> {
    if let Some(path) = override_path.filter(|path| !path.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    xdg_runtime_directory
        .filter(|path| !path.is_empty())
        .map(|path| PathBuf::from(path).join("resource-guard"))
        .ok_or(RuntimeError::RuntimeDirectoryUnavailable)
}

pub(crate) fn control_socket_path() -> Result<PathBuf, RuntimeError> {
    Ok(runtime_directory()?.join("control.sock"))
}

pub(crate) fn prepare_runtime_directory() -> Result<PathBuf, RuntimeError> {
    let directory = runtime_directory()?;
    fs::create_dir_all(&directory)
        .map_err(|error| RuntimeError::io("create runtime directory", error))?;
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
        .map_err(|error| RuntimeError::io("secure runtime directory", error))?;
    Ok(directory)
}

#[cfg(test)]
mod tests {
    use std::{ffi::OsString, path::PathBuf};

    use super::runtime_directory_from;
    use crate::runtime::RuntimeError;

    #[test]
    fn explicit_override_has_priority() {
        assert_eq!(
            runtime_directory_from(
                Some(OsString::from("/custom/runtime")),
                Some(OsString::from("/run/user/1000")),
            )
            .unwrap(),
            PathBuf::from("/custom/runtime")
        );
    }

    #[test]
    fn xdg_directory_gets_an_application_subdirectory() {
        assert_eq!(
            runtime_directory_from(None, Some(OsString::from("/run/user/1000"))).unwrap(),
            PathBuf::from("/run/user/1000/resource-guard")
        );
    }

    #[test]
    fn empty_paths_are_not_accepted() {
        assert!(matches!(
            runtime_directory_from(Some(OsString::new()), Some(OsString::new())),
            Err(RuntimeError::RuntimeDirectoryUnavailable)
        ));
    }
}
