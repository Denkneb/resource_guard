use std::{error::Error, fmt};

use crate::domain::{ProcessDisposition, ProcessIdentity, ProtectionPolicy};

use super::{PortError, ProcessSource, TerminationPort};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StopError {
    Inspection(PortError),
    NotFound {
        pid: u32,
    },
    WrongOwner {
        pid: u32,
        owner_uid: u32,
    },
    IdentityChanged {
        expected: ProcessIdentity,
        actual: ProcessIdentity,
    },
    Protected {
        pid: u32,
    },
    Termination(PortError),
}

impl fmt::Display for StopError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Inspection(error) => write!(formatter, "cannot inspect process: {error}"),
            Self::NotFound { pid } => write!(formatter, "process {pid} no longer exists"),
            Self::WrongOwner { pid, owner_uid } => {
                write!(formatter, "process {pid} belongs to UID {owner_uid}")
            }
            Self::IdentityChanged { expected, actual } => write!(
                formatter,
                "PID {} was reused (expected start {}, actual start {})",
                expected.pid(),
                expected.started_at(),
                actual.started_at()
            ),
            Self::Protected { pid } => write!(formatter, "process {pid} is protected"),
            Self::Termination(error) => write!(formatter, "cannot terminate process: {error}"),
        }
    }
}

impl Error for StopError {}

pub struct StopProcess<'a, S, T> {
    source: &'a mut S,
    terminator: &'a mut T,
    current_uid: u32,
    protection: &'a ProtectionPolicy,
}

impl<'a, S, T> StopProcess<'a, S, T>
where
    S: ProcessSource,
    T: TerminationPort,
{
    pub fn new(
        source: &'a mut S,
        terminator: &'a mut T,
        current_uid: u32,
        protection: &'a ProtectionPolicy,
    ) -> Self {
        Self {
            source,
            terminator,
            current_uid,
            protection,
        }
    }

    /// Revalidates and gracefully terminates one exact process identity.
    ///
    /// # Errors
    ///
    /// Returns an error when the process disappeared, changed identity, belongs to
    /// another user, is protected, or cannot receive the termination request.
    pub fn execute(&mut self, expected: ProcessIdentity) -> Result<(), StopError> {
        let Some(actual) = self
            .source
            .find(expected.pid())
            .map_err(StopError::Inspection)?
        else {
            return Err(StopError::NotFound {
                pid: expected.pid(),
            });
        };
        let actual_identity = actual.identity();

        if actual_identity.uid() != self.current_uid {
            return Err(StopError::WrongOwner {
                pid: actual_identity.pid(),
                owner_uid: actual_identity.uid(),
            });
        }
        if actual_identity != expected {
            return Err(StopError::IdentityChanged {
                expected,
                actual: actual_identity,
            });
        }
        if self.protection.disposition(&actual) == ProcessDisposition::Protect {
            return Err(StopError::Protected {
                pid: actual_identity.pid(),
            });
        }

        self.terminator
            .terminate(actual_identity)
            .map_err(StopError::Termination)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{StopError, StopProcess};
    use crate::{
        application::{PortError, ProcessSource, ResourceSnapshot, TerminationPort},
        domain::{ProcessDescriptor, ProcessIdentity, ProtectionPolicy},
    };

    const CURRENT_UID: u32 = 1_000;

    struct FakeSource {
        process: Option<ProcessDescriptor>,
        error: Option<PortError>,
    }

    impl ProcessSource for FakeSource {
        fn snapshot(&mut self) -> Result<ResourceSnapshot, PortError> {
            unreachable!("snapshot is not used by the stop use case")
        }

        fn find(&mut self, _pid: u32) -> Result<Option<ProcessDescriptor>, PortError> {
            if let Some(error) = self.error.take() {
                Err(error)
            } else {
                Ok(self.process.clone())
            }
        }
    }

    #[derive(Default)]
    struct FakeTerminator {
        terminated: Vec<ProcessIdentity>,
    }

    impl TerminationPort for FakeTerminator {
        fn terminate(&mut self, identity: ProcessIdentity) -> Result<(), PortError> {
            self.terminated.push(identity);
            Ok(())
        }
    }

    fn descriptor(identity: ProcessIdentity, name: &str) -> ProcessDescriptor {
        ProcessDescriptor::new(
            identity,
            name,
            Some(PathBuf::from(format!("/usr/bin/{name}"))),
        )
    }

    fn execute(
        expected: ProcessIdentity,
        actual: Option<ProcessDescriptor>,
        protection: &ProtectionPolicy,
    ) -> (Result<(), StopError>, FakeTerminator) {
        let mut source = FakeSource {
            process: actual,
            error: None,
        };
        let mut terminator = FakeTerminator::default();
        let result = StopProcess::new(&mut source, &mut terminator, CURRENT_UID, protection)
            .execute(expected);
        (result, terminator)
    }

    #[test]
    fn terminates_a_revalidated_process() {
        let identity = ProcessIdentity::new(42, CURRENT_UID, 100);

        let (result, terminator) = execute(
            identity,
            Some(descriptor(identity, "worker")),
            &ProtectionPolicy::default(),
        );

        assert_eq!(result, Ok(()));
        assert_eq!(terminator.terminated, vec![identity]);
    }

    #[test]
    fn rejects_a_disappeared_process() {
        let identity = ProcessIdentity::new(42, CURRENT_UID, 100);

        let (result, terminator) = execute(identity, None, &ProtectionPolicy::default());

        assert_eq!(result, Err(StopError::NotFound { pid: 42 }));
        assert!(terminator.terminated.is_empty());
    }

    #[test]
    fn rejects_a_reused_pid() {
        let expected = ProcessIdentity::new(42, CURRENT_UID, 100);
        let actual = ProcessIdentity::new(42, CURRENT_UID, 101);

        let (result, terminator) = execute(
            expected,
            Some(descriptor(actual, "worker")),
            &ProtectionPolicy::default(),
        );

        assert_eq!(result, Err(StopError::IdentityChanged { expected, actual }));
        assert!(terminator.terminated.is_empty());
    }

    #[test]
    fn rejects_a_process_owned_by_another_user() {
        let expected = ProcessIdentity::new(42, CURRENT_UID, 100);
        let actual = ProcessIdentity::new(42, CURRENT_UID + 1, 100);

        let (result, terminator) = execute(
            expected,
            Some(descriptor(actual, "worker")),
            &ProtectionPolicy::default(),
        );

        assert_eq!(
            result,
            Err(StopError::WrongOwner {
                pid: 42,
                owner_uid: CURRENT_UID + 1,
            })
        );
        assert!(terminator.terminated.is_empty());
    }

    #[test]
    fn rejects_a_protected_process() {
        let identity = ProcessIdentity::new(42, CURRENT_UID, 100);
        let protection = ProtectionPolicy::new(["desktop".to_owned()], [], [], []);

        let (result, terminator) =
            execute(identity, Some(descriptor(identity, "desktop")), &protection);

        assert_eq!(result, Err(StopError::Protected { pid: 42 }));
        assert!(terminator.terminated.is_empty());
    }
}
