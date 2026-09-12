use std::{collections::HashSet, path::PathBuf, time::Duration};

use super::workload::{WorkloadMember, termination_order};
use super::{ProcessDescriptor, ProcessIdentity};

#[derive(Clone, Debug, PartialEq)]
pub struct StaleWorkloadPolicy {
    pub enabled: bool,
    /// When set, desktop notifications require memory pressure; the stale
    /// inventory itself is always built.
    pub notify_only_under_memory_pressure: bool,
    pub candidate_names: HashSet<String>,
    pub launcher_names: HashSet<String>,
    pub ignored_root_names: HashSet<String>,
    pub minimum_age: Duration,
    pub minimum_tree_memory_bytes: u64,
    pub minimum_group_memory_bytes: u64,
    pub minimum_group_trees: usize,
    pub maximum_cpu_percent: f32,
    pub consecutive_samples: u32,
    pub notification_cooldown: Duration,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StaleWorkload {
    pub root: ProcessDescriptor,
    pub members: Vec<WorkloadMember>,
    pub total_memory_bytes: u64,
    pub total_cpu_percent: f32,
    pub age: Duration,
}

impl StaleWorkload {
    #[must_use]
    pub const fn identity(&self) -> ProcessIdentity {
        self.root.identity()
    }

    #[must_use]
    pub fn process_count(&self) -> usize {
        self.members.len()
    }

    #[must_use]
    pub fn termination_order(&self) -> Vec<ProcessIdentity> {
        termination_order(self.identity(), &self.members)
    }
}

/// A set of independent stale workload trees that share the same exact root
/// working directory.
///
/// A group is a reporting-only aggregate: it is never a termination boundary
/// and intentionally exposes no `termination_order`.
#[derive(Clone, Debug, PartialEq)]
pub struct StaleWorkloadGroup {
    pub working_directory: PathBuf,
    pub workloads: Vec<StaleWorkload>,
    pub total_memory_bytes: u64,
    pub total_cpu_percent: f32,
    pub age: Duration,
}

impl StaleWorkloadGroup {
    #[must_use]
    pub fn tree_count(&self) -> usize {
        self.workloads.len()
    }

    #[must_use]
    pub fn process_count(&self) -> usize {
        self.workloads
            .iter()
            .map(StaleWorkload::process_count)
            .sum()
    }

    /// Root identities in deterministic memory-descending order.
    #[must_use]
    pub fn root_identities(&self) -> Vec<ProcessIdentity> {
        self.workloads.iter().map(StaleWorkload::identity).collect()
    }
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, time::Duration};

    use super::{StaleWorkload, StaleWorkloadGroup};
    use crate::domain::{ProcessDescriptor, ProcessIdentity, ProcessResources, WorkloadMember};

    fn workload(pid: u32, memory: u64) -> StaleWorkload {
        let process = ProcessDescriptor::new(
            ProcessIdentity::new(pid, 1_000, u64::from(pid)),
            "pytest",
            None,
        );
        StaleWorkload {
            root: process.clone(),
            members: vec![WorkloadMember {
                process,
                resources: ProcessResources {
                    cpu_percent: 0.0,
                    resident_memory_bytes: memory,
                    virtual_memory_bytes: memory,
                    running_for: Duration::from_hours(2),
                    observed_at: Duration::ZERO,
                },
                depth: 0,
            }],
            total_memory_bytes: memory,
            total_cpu_percent: 0.0,
            age: Duration::from_hours(2),
        }
    }

    #[test]
    fn group_reports_tree_process_and_root_counts() {
        let group = StaleWorkloadGroup {
            working_directory: PathBuf::from("/work/project"),
            workloads: vec![workload(10, 100), workload(11, 200)],
            total_memory_bytes: 300,
            total_cpu_percent: 0.0,
            age: Duration::from_hours(2),
        };

        assert_eq!(group.tree_count(), 2);
        assert_eq!(group.process_count(), 2);
        assert_eq!(
            group
                .root_identities()
                .into_iter()
                .map(ProcessIdentity::pid)
                .collect::<Vec<_>>(),
            vec![10, 11]
        );
    }
}
