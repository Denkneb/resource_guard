use std::{error::Error, fmt, time::Duration};

use crate::domain::{ProcessDescriptor, ProcessIdentity, ProcessResources, SystemResources};

#[derive(Clone, Debug, PartialEq)]
pub struct ObservedProcess {
    pub descriptor: ProcessDescriptor,
    pub resources: ProcessResources,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResourceSnapshot {
    pub system: SystemResources,
    pub processes: Vec<ObservedProcess>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PortError {
    operation: &'static str,
    message: String,
}

impl PortError {
    #[must_use]
    pub fn new(operation: &'static str, message: impl Into<String>) -> Self {
        Self {
            operation,
            message: message.into(),
        }
    }

    #[must_use]
    pub const fn operation(&self) -> &'static str {
        self.operation
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for PortError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.operation, self.message)
    }
}

impl Error for PortError {}

/// Outbound port for reading the current process inventory.
pub trait ProcessSource {
    /// Returns a complete resource snapshot for one monitoring cycle.
    ///
    /// # Errors
    ///
    /// Returns an error when the underlying process source cannot be read.
    fn snapshot(&mut self) -> Result<ResourceSnapshot, PortError>;

    /// Finds the current process occupying `pid`.
    ///
    /// # Errors
    ///
    /// Returns an error when the underlying process source cannot be read.
    fn find(&mut self, pid: u32) -> Result<Option<ProcessDescriptor>, PortError>;
}

/// Outbound port which represents a graceful process termination request.
pub trait TerminationPort {
    /// Sends a graceful termination request to the exact process identity.
    ///
    /// # Errors
    ///
    /// Returns an error when the termination request cannot be delivered.
    fn terminate(&mut self, identity: ProcessIdentity) -> Result<(), PortError>;
}

/// Outbound port for monotonic application time.
pub trait MonotonicClock {
    fn now(&self) -> Duration;
}

/// Outbound port for delaying application workflows without binding them to a runtime.
pub trait Sleeper {
    fn sleep(&self, duration: Duration);
}
