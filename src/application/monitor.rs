use std::{collections::HashSet, time::Duration};

use crate::domain::{
    Evaluation, IgnoreRegistry, ProcessDescriptor, ProcessDisposition, ProcessIdentity,
    ProcessResources, ProtectionPolicy, ResourceBreach, SystemResources, Thresholds,
    ViolationPolicy, ViolationTracker,
};

use super::{MonotonicClock, PortError, ProcessSource};

#[derive(Clone, Debug, PartialEq)]
pub struct MonitorEvent {
    pub process: ProcessDescriptor,
    pub resources: ProcessResources,
    pub breach: ResourceBreach,
    pub exceeded_for: Duration,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MonitorReport {
    pub system: SystemResources,
    pub observed_processes: usize,
    pub monitored_processes: usize,
    pub processes: Vec<super::ObservedProcess>,
    pub events: Vec<MonitorEvent>,
}

/// Application use case which evaluates resource snapshots against domain policy.
pub struct MonitorService<S, C> {
    source: S,
    clock: C,
    current_uid: u32,
    thresholds: Thresholds,
    protection: ProtectionPolicy,
    temporary_ignores: IgnoreRegistry,
    violations: ViolationTracker,
    tracked_identities: HashSet<ProcessIdentity>,
}

impl<S, C> MonitorService<S, C>
where
    S: ProcessSource,
    C: MonotonicClock,
{
    #[must_use]
    pub fn new(
        source: S,
        clock: C,
        current_uid: u32,
        thresholds: Thresholds,
        protection: ProtectionPolicy,
        violation_policy: ViolationPolicy,
    ) -> Self {
        Self {
            source,
            clock,
            current_uid,
            thresholds,
            protection,
            temporary_ignores: IgnoreRegistry::default(),
            violations: ViolationTracker::new(violation_policy),
            tracked_identities: HashSet::new(),
        }
    }

    /// Ignores one exact process identity for the requested duration.
    pub fn ignore_for(&mut self, identity: ProcessIdentity, duration: Duration) {
        let deadline = self.clock.now().saturating_add(duration);
        self.temporary_ignores.ignore_until(identity, deadline);
        self.violations.forget(identity);
        self.tracked_identities.remove(&identity);
    }

    /// Performs one monitoring cycle.
    ///
    /// # Errors
    ///
    /// Returns the process-source error when a snapshot cannot be collected.
    pub fn poll(&mut self) -> Result<MonitorReport, PortError> {
        let now = self.clock.now();
        self.temporary_ignores.remove_expired(now);
        let snapshot = self.source.snapshot()?;
        let observed_processes = snapshot.processes.len();
        let mut active_identities = HashSet::new();
        let mut monitored_processes = 0;
        let mut processes = Vec::new();
        let mut events = Vec::new();

        for observed in snapshot.processes {
            let identity = observed.descriptor.identity();
            if identity.uid() != self.current_uid
                || self.protection.disposition(&observed.descriptor) != ProcessDisposition::Monitor
                || self.temporary_ignores.is_ignored(identity, now)
            {
                self.violations.forget(identity);
                continue;
            }

            active_identities.insert(identity);
            monitored_processes += 1;
            let breach = self.thresholds.evaluate(observed.resources);
            if let Evaluation::Notify { elapsed, .. } =
                self.violations.evaluate(identity, breach, now)
            {
                events.push(MonitorEvent {
                    process: observed.descriptor.clone(),
                    resources: observed.resources,
                    breach,
                    exceeded_for: elapsed,
                });
            }
            processes.push(observed);
        }

        for stale in self.tracked_identities.difference(&active_identities) {
            self.violations.forget(*stale);
        }
        self.tracked_identities = active_identities;

        Ok(MonitorReport {
            system: snapshot.system,
            observed_processes,
            monitored_processes,
            processes,
            events,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, path::PathBuf, rc::Rc, time::Duration};

    use super::MonitorService;
    use crate::{
        application::{
            MonotonicClock, ObservedProcess, PortError, ProcessSource, ResourceSnapshot,
        },
        domain::{
            ProcessDescriptor, ProcessIdentity, ProcessResources, ProtectionPolicy,
            SystemResources, Thresholds, ViolationPolicy,
        },
    };

    const CURRENT_UID: u32 = 1_000;

    #[derive(Clone)]
    struct TestClock(Rc<Cell<Duration>>);

    impl TestClock {
        fn new() -> (Self, Rc<Cell<Duration>>) {
            let time = Rc::new(Cell::new(Duration::ZERO));
            (Self(Rc::clone(&time)), time)
        }
    }

    impl MonotonicClock for TestClock {
        fn now(&self) -> Duration {
            self.0.get()
        }
    }

    struct FakeSource {
        snapshot: ResourceSnapshot,
    }

    impl ProcessSource for FakeSource {
        fn snapshot(&mut self) -> Result<ResourceSnapshot, PortError> {
            Ok(self.snapshot.clone())
        }

        fn find(&mut self, _pid: u32) -> Result<Option<ProcessDescriptor>, PortError> {
            Ok(None)
        }
    }

    fn observed(pid: u32, uid: u32, name: &str) -> ObservedProcess {
        ObservedProcess {
            descriptor: ProcessDescriptor::new(
                ProcessIdentity::new(pid, uid, 100),
                name,
                Some(PathBuf::from(format!("/usr/bin/{name}"))),
            ),
            resources: ProcessResources {
                cpu_percent: 90.0,
                resident_memory_bytes: 2_048,
                virtual_memory_bytes: 4_096,
                observed_at: Duration::ZERO,
            },
        }
    }

    fn snapshot(processes: Vec<ObservedProcess>) -> ResourceSnapshot {
        ResourceSnapshot {
            system: SystemResources {
                total_memory_bytes: 16_384,
                available_memory_bytes: 8_192,
                total_swap_bytes: 4_096,
                used_swap_bytes: 0,
            },
            processes,
        }
    }

    fn service(
        processes: Vec<ObservedProcess>,
        protection: ProtectionPolicy,
    ) -> (MonitorService<FakeSource, TestClock>, Rc<Cell<Duration>>) {
        let (clock, time) = TestClock::new();
        let service = MonitorService::new(
            FakeSource {
                snapshot: snapshot(processes),
            },
            clock,
            CURRENT_UID,
            Thresholds {
                max_cpu_percent: Some(80.0),
                max_resident_memory_bytes: Some(1_024),
            },
            protection,
            ViolationPolicy::new(3, Duration::from_secs(10), Duration::from_secs(60)),
        );
        (service, time)
    }

    #[test]
    fn emits_event_only_after_a_sustained_violation() {
        let (mut service, time) = service(
            vec![observed(42, CURRENT_UID, "worker")],
            ProtectionPolicy::default(),
        );

        assert!(service.poll().unwrap().events.is_empty());
        time.set(Duration::from_secs(5));
        assert!(service.poll().unwrap().events.is_empty());
        time.set(Duration::from_secs(10));

        let report = service.poll().unwrap();
        assert_eq!(report.events.len(), 1);
        assert_eq!(report.events[0].process.identity().pid(), 42);
        assert_eq!(report.events[0].exceeded_for, Duration::from_secs(10));
    }

    #[test]
    fn skips_processes_owned_by_another_user() {
        let (mut service, _) = service(
            vec![observed(42, CURRENT_UID + 1, "worker")],
            ProtectionPolicy::default(),
        );

        let report = service.poll().unwrap();
        assert_eq!(report.observed_processes, 1);
        assert_eq!(report.monitored_processes, 0);
        assert!(report.processes.is_empty());
    }

    #[test]
    fn skips_protected_and_permanently_ignored_processes() {
        let protection =
            ProtectionPolicy::new(["desktop".to_owned()], [], ["compiler".to_owned()], []);
        let (mut service, _) = service(
            vec![
                observed(42, CURRENT_UID, "desktop"),
                observed(43, CURRENT_UID, "compiler"),
            ],
            protection,
        );

        let report = service.poll().unwrap();
        assert_eq!(report.monitored_processes, 0);
        assert!(report.events.is_empty());
    }

    #[test]
    fn skips_a_temporarily_ignored_identity() {
        let process = observed(42, CURRENT_UID, "worker");
        let identity = process.descriptor.identity();
        let (mut service, time) = service(vec![process], ProtectionPolicy::default());
        service.ignore_for(identity, Duration::from_secs(60));
        time.set(Duration::from_secs(10));

        assert_eq!(service.poll().unwrap().monitored_processes, 0);
    }
}
