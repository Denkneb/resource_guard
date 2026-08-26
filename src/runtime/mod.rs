mod daemon;
mod paths;
mod protocol;
mod state;

use std::{error::Error, fmt, io};

pub use daemon::{query_status, run_daemon};
pub use protocol::StatusResponse;

#[derive(Debug)]
pub enum RuntimeError {
    Config(crate::adapters::ConfigError),
    Io {
        operation: &'static str,
        source: io::Error,
    },
    Protocol(String),
    AlreadyRunning,
    RuntimeDirectoryUnavailable,
}

impl RuntimeError {
    pub(crate) fn io(operation: &'static str, source: io::Error) -> Self {
        Self::Io { operation, source }
    }
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(error) => error.fmt(formatter),
            Self::Io { operation, source } => write!(formatter, "{operation}: {source}"),
            Self::Protocol(message) => write!(formatter, "control protocol error: {message}"),
            Self::AlreadyRunning => write!(formatter, "daemon is already running"),
            Self::RuntimeDirectoryUnavailable => write!(formatter, "XDG_RUNTIME_DIR is not set"),
        }
    }
}

impl Error for RuntimeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Config(error) => Some(error),
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl From<crate::adapters::ConfigError> for RuntimeError {
    fn from(error: crate::adapters::ConfigError) -> Self {
        Self::Config(error)
    }
}
