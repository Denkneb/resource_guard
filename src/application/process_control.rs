use std::{error::Error, fmt, time::Duration};

use crate::domain::{ProcessDisposition, ProcessIdentity, ProtectionPolicy, StaleWorkload};

use super::{
    ForceTerminationPort, MonotonicClock, PortError, ProcessSource, Sleeper, TerminationPort,
};

const WAIT_POLL_INTERVAL: Duration = Duration::from_millis(100);

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
    ForceTermination(PortError),
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
            Self::ForceTermination(error) => {
                write!(formatter, "cannot forcefully terminate process: {error}")
            }
        }
    }
}

impl Error for StopError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StopOutcome {
    Exited,
    StillRunning,
}

pub struct StopProcess<'a, S, T> {
    source: &'a mut S,
    terminator: &'a mut T,
    current_uid: u32,
    protection: &'a ProtectionPolicy,
}

pub struct ForceStopProcess<'a, S, T> {
    source: &'a mut S,
    terminator: &'a mut T,
    current_uid: u32,
    protection: &'a ProtectionPolicy,
}

pub struct StopWorkload<'a, S, T> {
    source: &'a mut S,
    terminator: &'a mut T,
    current_uid: u32,
    protection: &'a ProtectionPolicy,
}

impl<'a, S, T> StopWorkload<'a, S, T>
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

    /// Revalidates every identity and sends SIGTERM leaf-first, ending with the root.
    ///
    /// # Errors
    /// Returns the first ownership, identity, protection, inspection, or signalling error.
    pub fn execute(&mut self, workload: &StaleWorkload) -> Result<usize, StopError> {
        let mut signalled = 0;
        for identity in workload.termination_order() {
            match StopProcess::new(
                self.source,
                self.terminator,
                self.current_uid,
                self.protection,
            )
            .execute(identity)
            {
                Ok(()) => signalled += 1,
                Err(StopError::NotFound { .. }) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(signalled)
    }
}

pub struct WaitForExit<'a, S, C, D> {
    source: &'a mut S,
    clock: &'a C,
    sleeper: &'a D,
}

pub struct StopAndWait<'a, S, T, C, D> {
    source: &'a mut S,
    terminator: &'a mut T,
    clock: &'a C,
    sleeper: &'a D,
    current_uid: u32,
    protection: &'a ProtectionPolicy,
}

impl<'a, S, T, C, D> StopAndWait<'a, S, T, C, D>
where
    S: ProcessSource,
    T: TerminationPort,
    C: MonotonicClock,
    D: Sleeper,
{
    pub fn new(
        source: &'a mut S,
        terminator: &'a mut T,
        clock: &'a C,
        sleeper: &'a D,
        current_uid: u32,
        protection: &'a ProtectionPolicy,
    ) -> Self {
        Self {
            source,
            terminator,
            clock,
            sleeper,
            current_uid,
            protection,
        }
    }

    /// Sends `SIGTERM` after revalidation and waits for the exact process to exit.
    ///
    /// # Errors
    ///
    /// Returns process inspection, protection, ownership, identity, or signalling errors.
    pub fn execute(
        &mut self,
        expected: ProcessIdentity,
        grace_period: Duration,
    ) -> Result<StopOutcome, StopError> {
        StopProcess::new(
            self.source,
            self.terminator,
            self.current_uid,
            self.protection,
        )
        .execute(expected)?;

        WaitForExit::new(self.source, self.clock, self.sleeper).execute(expected, grace_period)
    }
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
        let actual_identity =
            validate_process(self.source, expected, self.current_uid, self.protection)?;
        self.terminator
            .terminate(actual_identity)
            .map_err(StopError::Termination)
    }
}

impl<'a, S, T> ForceStopProcess<'a, S, T>
where
    S: ProcessSource,
    T: ForceTerminationPort,
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

    /// Revalidates and forcefully terminates one exact process identity.
    ///
    /// # Errors
    ///
    /// Returns an error when the process disappeared, changed identity, belongs to
    /// another user, is protected, or cannot receive the forceful termination request.
    pub fn execute(&mut self, expected: ProcessIdentity) -> Result<(), StopError> {
        let actual_identity =
            validate_process(self.source, expected, self.current_uid, self.protection)?;
        self.terminator
            .force_terminate(actual_identity)
            .map_err(StopError::ForceTermination)
    }
}

impl<'a, S, C, D> WaitForExit<'a, S, C, D>
where
    S: ProcessSource,
    C: MonotonicClock,
    D: Sleeper,
{
    pub fn new(source: &'a mut S, clock: &'a C, sleeper: &'a D) -> Self {
        Self {
            source,
            clock,
            sleeper,
        }
    }

    /// Waits until the exact process identity exits or the timeout expires.
    ///
    /// # Errors
    ///
    /// Returns an error when the process inventory cannot be inspected.
    pub fn execute(
        &mut self,
        expected: ProcessIdentity,
        timeout: Duration,
    ) -> Result<StopOutcome, StopError> {
        let started_waiting_at = self.clock.now();
        loop {
            let process = self
                .source
                .find(expected.pid())
                .map_err(StopError::Inspection)?;
            if process.is_none_or(|process| process.identity() != expected) {
                return Ok(StopOutcome::Exited);
            }

            let elapsed = self.clock.now().saturating_sub(started_waiting_at);
            if elapsed >= timeout {
                return Ok(StopOutcome::StillRunning);
            }

            self.sleeper
                .sleep(WAIT_POLL_INTERVAL.min(timeout.saturating_sub(elapsed)));
        }
    }
}

fn validate_process<S: ProcessSource>(
    source: &mut S,
    expected: ProcessIdentity,
    current_uid: u32,
    protection: &ProtectionPolicy,
) -> Result<ProcessIdentity, StopError> {
    let Some(actual) = source.find(expected.pid()).map_err(StopError::Inspection)? else {
        return Err(StopError::NotFound {
            pid: expected.pid(),
        });
    };
    let actual_identity = actual.identity();

    if actual_identity.uid() != current_uid {
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
    if protection.disposition(&actual) == ProcessDisposition::Protect {
        return Err(StopError::Protected {
            pid: actual_identity.pid(),
        });
    }

    Ok(actual_identity)
}

#[cfg(test)]
mod tests {
    use std::{
        cell::Cell,
        collections::{HashMap, VecDeque},
        path::PathBuf,
        rc::Rc,
        time::Duration,
    };

    use super::{ForceStopProcess, StopAndWait, StopError, StopOutcome, StopProcess, StopWorkload};
    use crate::{
        application::{
            ForceTerminationPort, MonotonicClock, PortError, ProcessSource, ResourceSnapshot,
            Sleeper, TerminationPort,
        },
        domain::{
            ProcessDescriptor, ProcessIdentity, ProcessResources, ProtectionPolicy, StaleWorkload,
            WorkloadMember,
        },
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

    #[derive(Default)]
    struct FakeForceTerminator {
        terminated: Vec<ProcessIdentity>,
    }

    impl ForceTerminationPort for FakeForceTerminator {
        fn force_terminate(&mut self, identity: ProcessIdentity) -> Result<(), PortError> {
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

    fn execute_force(
        expected: ProcessIdentity,
        actual: Option<ProcessDescriptor>,
        protection: &ProtectionPolicy,
    ) -> (Result<(), StopError>, FakeForceTerminator) {
        let mut source = FakeSource {
            process: actual,
            error: None,
        };
        let mut terminator = FakeForceTerminator::default();
        let result = ForceStopProcess::new(&mut source, &mut terminator, CURRENT_UID, protection)
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
    fn stops_a_workload_leaf_first_after_revalidating_every_identity() {
        struct TreeSource(HashMap<u32, ProcessDescriptor>);
        impl ProcessSource for TreeSource {
            fn snapshot(&mut self) -> Result<ResourceSnapshot, PortError> {
                unreachable!("snapshot is not used by the stop use case")
            }

            fn find(&mut self, pid: u32) -> Result<Option<ProcessDescriptor>, PortError> {
                Ok(self.0.get(&pid).cloned())
            }
        }

        let root = descriptor(ProcessIdentity::new(10, CURRENT_UID, 10), "uv");
        let child = descriptor(ProcessIdentity::new(11, CURRENT_UID, 11), "pytest");
        let resources = ProcessResources {
            cpu_percent: 0.0,
            resident_memory_bytes: 100,
            virtual_memory_bytes: 100,
            running_for: Duration::from_hours(1),
            observed_at: Duration::ZERO,
        };
        let workload = StaleWorkload {
            root: root.clone(),
            members: vec![
                WorkloadMember {
                    process: root.clone(),
                    resources,
                    depth: 0,
                },
                WorkloadMember {
                    process: child.clone(),
                    resources,
                    depth: 1,
                },
            ],
            total_memory_bytes: 200,
            total_cpu_percent: 0.0,
            age: Duration::from_hours(1),
        };
        let mut source = TreeSource(HashMap::from([
            (root.identity().pid(), root),
            (child.identity().pid(), child),
        ]));
        let mut terminator = FakeTerminator::default();

        let count = StopWorkload::new(
            &mut source,
            &mut terminator,
            CURRENT_UID,
            &ProtectionPolicy::default(),
        )
        .execute(&workload)
        .unwrap();

        assert_eq!(count, 2);
        assert_eq!(
            terminator
                .terminated
                .into_iter()
                .map(ProcessIdentity::pid)
                .collect::<Vec<_>>(),
            vec![11, 10]
        );
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

    #[test]
    fn forcefully_terminates_a_revalidated_process() {
        let identity = ProcessIdentity::new(42, CURRENT_UID, 100);

        let (result, terminator) = execute_force(
            identity,
            Some(descriptor(identity, "worker")),
            &ProtectionPolicy::default(),
        );

        assert_eq!(result, Ok(()));
        assert_eq!(terminator.terminated, vec![identity]);
    }

    #[test]
    fn force_stop_rejects_a_reused_pid() {
        let expected = ProcessIdentity::new(42, CURRENT_UID, 100);
        let actual = ProcessIdentity::new(42, CURRENT_UID, 101);

        let (result, terminator) = execute_force(
            expected,
            Some(descriptor(actual, "worker")),
            &ProtectionPolicy::default(),
        );

        assert_eq!(result, Err(StopError::IdentityChanged { expected, actual }));
        assert!(terminator.terminated.is_empty());
    }

    #[test]
    fn force_stop_rejects_a_process_owned_by_another_user() {
        let expected = ProcessIdentity::new(42, CURRENT_UID, 100);
        let actual = ProcessIdentity::new(42, CURRENT_UID + 1, 100);

        let (result, terminator) = execute_force(
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
    fn force_stop_rejects_a_protected_process() {
        let identity = ProcessIdentity::new(42, CURRENT_UID, 100);
        let protection = ProtectionPolicy::new(["desktop".to_owned()], [], [], []);

        let (result, terminator) =
            execute_force(identity, Some(descriptor(identity, "desktop")), &protection);

        assert_eq!(result, Err(StopError::Protected { pid: 42 }));
        assert!(terminator.terminated.is_empty());
    }

    struct SequenceSource {
        responses: VecDeque<Option<ProcessDescriptor>>,
        fallback: Option<ProcessDescriptor>,
    }

    impl ProcessSource for SequenceSource {
        fn snapshot(&mut self) -> Result<ResourceSnapshot, PortError> {
            unreachable!("snapshot is not used by the stop use case")
        }

        fn find(&mut self, _pid: u32) -> Result<Option<ProcessDescriptor>, PortError> {
            Ok(self
                .responses
                .pop_front()
                .unwrap_or_else(|| self.fallback.clone()))
        }
    }

    #[derive(Clone)]
    struct FakeTime(Rc<Cell<Duration>>);

    impl MonotonicClock for FakeTime {
        fn now(&self) -> Duration {
            self.0.get()
        }
    }

    impl Sleeper for FakeTime {
        fn sleep(&self, duration: Duration) {
            self.0.set(self.0.get().saturating_add(duration));
        }
    }

    #[test]
    fn stop_and_wait_reports_an_exited_process() {
        let identity = ProcessIdentity::new(42, CURRENT_UID, 100);
        let process = descriptor(identity, "worker");
        let mut source = SequenceSource {
            responses: VecDeque::from([Some(process), None]),
            fallback: None,
        };
        let mut terminator = FakeTerminator::default();
        let time = FakeTime(Rc::new(Cell::new(Duration::ZERO)));

        let outcome = StopAndWait::new(
            &mut source,
            &mut terminator,
            &time,
            &time,
            CURRENT_UID,
            &ProtectionPolicy::default(),
        )
        .execute(identity, Duration::from_secs(1));

        assert_eq!(outcome, Ok(StopOutcome::Exited));
        assert_eq!(terminator.terminated, vec![identity]);
    }

    #[test]
    fn stop_and_wait_reports_a_process_surviving_the_grace_period() {
        let identity = ProcessIdentity::new(42, CURRENT_UID, 100);
        let process = descriptor(identity, "worker");
        let mut source = SequenceSource {
            responses: VecDeque::new(),
            fallback: Some(process),
        };
        let mut terminator = FakeTerminator::default();
        let time = FakeTime(Rc::new(Cell::new(Duration::ZERO)));

        let outcome = StopAndWait::new(
            &mut source,
            &mut terminator,
            &time,
            &time,
            CURRENT_UID,
            &ProtectionPolicy::default(),
        )
        .execute(identity, Duration::from_millis(250));

        assert_eq!(outcome, Ok(StopOutcome::StillRunning));
        assert_eq!(time.now(), Duration::from_millis(250));
        assert_eq!(terminator.terminated, vec![identity]);
    }
}
