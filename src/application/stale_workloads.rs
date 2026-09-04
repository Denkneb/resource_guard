use std::{
    cmp::Reverse,
    collections::{HashMap, HashSet},
    time::Duration,
};

use crate::domain::{
    MemoryPressureLevel, ProcessIdentity, StaleWorkload, StaleWorkloadPolicy, WorkloadMember,
};

use super::ObservedProcess;

#[derive(Debug, Default)]
struct TrackingState {
    consecutive_samples: u32,
    last_notification_at: Option<Duration>,
}

#[derive(Debug)]
pub struct StaleWorkloadService {
    current_uid: u32,
    policy: StaleWorkloadPolicy,
    tracked: HashMap<ProcessIdentity, TrackingState>,
    ignored_until: HashMap<ProcessIdentity, Duration>,
}

impl StaleWorkloadService {
    #[must_use]
    pub fn new(current_uid: u32, policy: StaleWorkloadPolicy) -> Self {
        Self {
            current_uid,
            policy,
            tracked: HashMap::new(),
            ignored_until: HashMap::new(),
        }
    }

    pub fn ignore_for(&mut self, identity: ProcessIdentity, until: Duration) {
        self.ignored_until.insert(identity, until);
        self.tracked.remove(&identity);
    }

    pub fn ignore_name(&mut self, name: String) {
        self.policy.ignored_root_names.insert(name);
    }

    #[must_use]
    pub fn evaluate(
        &mut self,
        processes: &[ObservedProcess],
        pressure: MemoryPressureLevel,
        now: Duration,
    ) -> (Vec<StaleWorkload>, Vec<StaleWorkload>) {
        self.ignored_until.retain(|_, deadline| *deadline > now);
        let candidates = detect_workloads(processes, self.current_uid, &self.policy, pressure)
            .into_iter()
            .filter(|workload| !self.ignored_until.contains_key(&workload.identity()))
            .collect::<Vec<_>>();
        let active = candidates
            .iter()
            .map(StaleWorkload::identity)
            .collect::<HashSet<_>>();
        self.tracked.retain(|identity, _| active.contains(identity));
        let mut notifications = Vec::new();
        for candidate in &candidates {
            let state = self.tracked.entry(candidate.identity()).or_default();
            state.consecutive_samples = state.consecutive_samples.saturating_add(1);
            let cooldown_elapsed = state
                .last_notification_at
                .is_none_or(|last| now.saturating_sub(last) >= self.policy.notification_cooldown);
            if state.consecutive_samples >= self.policy.consecutive_samples && cooldown_elapsed {
                state.last_notification_at = Some(now);
                notifications.push(candidate.clone());
            }
        }
        (candidates, notifications)
    }
}

#[must_use]
pub fn detect_workloads(
    processes: &[ObservedProcess],
    current_uid: u32,
    policy: &StaleWorkloadPolicy,
    pressure: MemoryPressureLevel,
) -> Vec<StaleWorkload> {
    if !policy.enabled
        || (policy.only_under_memory_pressure && pressure == MemoryPressureLevel::Normal)
    {
        return Vec::new();
    }
    let by_pid = processes
        .iter()
        .map(|process| (process.descriptor.identity().pid(), process))
        .collect::<HashMap<_, _>>();
    let mut roots = HashSet::new();
    for process in processes {
        if process.descriptor.identity().uid() != current_uid
            || !policy.candidate_names.contains(process.descriptor.name())
            || process.resources.running_for < policy.minimum_age
        {
            continue;
        }
        let mut root = process;
        while let Some(parent) = root
            .descriptor
            .parent_pid()
            .and_then(|pid| by_pid.get(&pid).copied())
        {
            if parent.descriptor.identity().uid() != current_uid
                || !policy.launcher_names.contains(parent.descriptor.name())
            {
                break;
            }
            root = parent;
        }
        roots.insert(root.descriptor.identity());
    }

    let mut workloads = roots
        .into_iter()
        .filter_map(|root_identity| {
            let workload = workload_from_root(processes, root_identity, current_uid)?;
            (workload.total_memory_bytes >= policy.minimum_tree_memory_bytes
                && workload.total_cpu_percent <= policy.maximum_cpu_percent
                && !policy.ignored_root_names.contains(workload.root.name()))
            .then_some(workload)
        })
        .collect::<Vec<_>>();
    workloads.sort_by_key(|workload| Reverse(workload.total_memory_bytes));
    workloads
}

#[must_use]
pub fn workload_from_root(
    processes: &[ObservedProcess],
    root_identity: ProcessIdentity,
    current_uid: u32,
) -> Option<StaleWorkload> {
    let by_pid = processes
        .iter()
        .map(|process| (process.descriptor.identity().pid(), process))
        .collect::<HashMap<_, _>>();
    let root = by_pid.get(&root_identity.pid()).copied()?;
    if root.descriptor.identity() != root_identity || root_identity.uid() != current_uid {
        return None;
    }
    let members = processes
        .iter()
        .filter(|process| process.descriptor.identity().uid() == current_uid)
        .filter_map(|process| {
            descendant_depth(process, root_identity.pid(), &by_pid).map(|depth| WorkloadMember {
                process: process.descriptor.clone(),
                resources: process.resources,
                depth,
            })
        })
        .collect::<Vec<_>>();
    let total_memory_bytes = members
        .iter()
        .map(|member| member.resources.resident_memory_bytes)
        .sum();
    let total_cpu_percent = members
        .iter()
        .map(|member| member.resources.cpu_percent)
        .sum();
    let age = members
        .iter()
        .map(|member| member.resources.running_for)
        .max()
        .unwrap_or_default();
    Some(StaleWorkload {
        root: root.descriptor.clone(),
        members,
        total_memory_bytes,
        total_cpu_percent,
        age,
    })
}

fn descendant_depth(
    process: &ObservedProcess,
    root_pid: u32,
    by_pid: &HashMap<u32, &ObservedProcess>,
) -> Option<usize> {
    let mut current = process;
    let mut depth = 0;
    loop {
        if current.descriptor.identity().pid() == root_pid {
            return Some(depth);
        }
        let parent = current.descriptor.parent_pid()?;
        current = by_pid.get(&parent).copied()?;
        depth += 1;
        if depth > by_pid.len() {
            return None;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashSet, time::Duration};

    use super::{StaleWorkloadService, detect_workloads};
    use crate::{
        application::ObservedProcess,
        domain::{
            MemoryPressureLevel, ProcessDescriptor, ProcessIdentity, ProcessResources,
            ProcessState, StaleWorkloadPolicy,
        },
    };

    const UID: u32 = 1_000;

    fn process(pid: u32, parent: Option<u32>, name: &str, memory: u64) -> ObservedProcess {
        ObservedProcess {
            descriptor: ProcessDescriptor::new(
                ProcessIdentity::new(pid, UID, u64::from(pid)),
                name,
                None,
            )
            .with_runtime(parent, ProcessState::Sleeping),
            resources: ProcessResources {
                cpu_percent: 0.1,
                resident_memory_bytes: memory,
                virtual_memory_bytes: memory,
                running_for: Duration::from_hours(2),
                observed_at: Duration::ZERO,
            },
        }
    }

    fn policy() -> StaleWorkloadPolicy {
        StaleWorkloadPolicy {
            enabled: true,
            only_under_memory_pressure: true,
            candidate_names: HashSet::from(["pytest".to_owned()]),
            launcher_names: HashSet::from(["uv".to_owned(), "pytest".to_owned()]),
            ignored_root_names: HashSet::new(),
            minimum_age: Duration::from_hours(1),
            minimum_tree_memory_bytes: 100,
            maximum_cpu_percent: 1.0,
            consecutive_samples: 2,
            notification_cooldown: Duration::from_secs(60),
        }
    }

    fn tree() -> Vec<ObservedProcess> {
        vec![
            process(9, None, "bash", 10),
            process(8, Some(9), "xargs", 10),
            process(10, Some(8), "uv", 40),
            process(11, Some(10), "pytest", 80),
            process(12, Some(11), "pytest", 60),
        ]
    }

    #[test]
    fn detects_one_tree_and_orders_children_before_root() {
        let workloads = detect_workloads(&tree(), UID, &policy(), MemoryPressureLevel::Warning);

        assert_eq!(workloads.len(), 1);
        assert_eq!(workloads[0].identity().pid(), 10);
        assert_eq!(workloads[0].total_memory_bytes, 180);
        assert_eq!(workloads[0].process_count(), 3);
        assert_eq!(
            workloads[0]
                .termination_order()
                .into_iter()
                .map(ProcessIdentity::pid)
                .collect::<Vec<_>>(),
            vec![12, 11, 10]
        );
    }

    #[test]
    fn normal_pressure_and_ignored_roots_are_not_candidates() {
        assert!(detect_workloads(&tree(), UID, &policy(), MemoryPressureLevel::Normal).is_empty());
        let mut ignored = policy();
        ignored.ignored_root_names.insert("uv".to_owned());
        assert!(detect_workloads(&tree(), UID, &ignored, MemoryPressureLevel::Warning).is_empty());
    }

    #[test]
    fn requires_repeated_samples_and_honours_temporary_ignore() {
        let mut service = StaleWorkloadService::new(UID, policy());
        let (_, first) = service.evaluate(&tree(), MemoryPressureLevel::Warning, Duration::ZERO);
        let (_, second) = service.evaluate(
            &tree(),
            MemoryPressureLevel::Warning,
            Duration::from_secs(5),
        );
        assert!(first.is_empty());
        assert_eq!(second.len(), 1);

        service.ignore_for(second[0].identity(), Duration::from_secs(3_605));
        let (ignored, notifications) = service.evaluate(
            &tree(),
            MemoryPressureLevel::Warning,
            Duration::from_secs(10),
        );
        assert!(ignored.is_empty());
        assert!(notifications.is_empty());
    }
}
