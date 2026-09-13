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
/// A group is not a termination boundary on its own. It becomes one only as an
/// explicit, immutable identity snapshot collected through the two-step
/// notification flow: the user first opens the details view and then confirms
/// the group stop. The working directory never selects processes.
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

    /// Builds the deterministic, duplicate-free termination order of the whole
    /// immutable group snapshot.
    ///
    /// Descendants of every tree precede their root, trees keep the stored
    /// deterministic order, and each identity appears at most once. The working
    /// directory is never used to select processes, and this method sends no
    /// signals and does not depend on any Linux adapter.
    #[must_use]
    pub fn termination_order(&self) -> Vec<ProcessIdentity> {
        let mut seen = HashSet::new();
        let mut order = Vec::new();
        for workload in &self.workloads {
            for identity in workload.termination_order() {
                if seen.insert(identity) {
                    order.push(identity);
                }
            }
        }
        order
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

    fn process(pid: u32, memory: u64, depth: usize) -> WorkloadMember {
        WorkloadMember {
            process: ProcessDescriptor::new(
                ProcessIdentity::new(pid, 1_000, u64::from(pid)),
                "pytest",
                None,
            ),
            resources: ProcessResources {
                cpu_percent: 0.0,
                resident_memory_bytes: memory,
                virtual_memory_bytes: memory,
                running_for: Duration::from_hours(2),
                observed_at: Duration::ZERO,
            },
            depth,
        }
    }

    fn tree(root_pid: u32, root_memory: u64, children: &[(u32, u64)]) -> StaleWorkload {
        let mut members = vec![process(root_pid, root_memory, 0)];
        members.extend(
            children
                .iter()
                .map(|(pid, memory)| process(*pid, *memory, 1)),
        );
        let total_memory_bytes = members.iter().fold(0_u64, |total, member| {
            total + member.resources.resident_memory_bytes
        });
        StaleWorkload {
            root: members[0].process.clone(),
            members,
            total_memory_bytes,
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

    #[test]
    fn group_termination_order_is_leaf_first_across_independent_trees() {
        let group = StaleWorkloadGroup {
            working_directory: PathBuf::from("/work/project"),
            workloads: vec![
                tree(10, 100, &[(11, 10), (12, 10)]),
                tree(20, 100, &[(21, 10)]),
            ],
            total_memory_bytes: 330,
            total_cpu_percent: 0.0,
            age: Duration::from_hours(2),
        };

        let order = group
            .termination_order()
            .into_iter()
            .map(ProcessIdentity::pid)
            .collect::<Vec<_>>();

        assert_eq!(order, vec![12, 11, 10, 21, 20]);
    }

    #[test]
    fn group_termination_order_deduplicates_shared_identities_in_order() {
        let group = StaleWorkloadGroup {
            working_directory: PathBuf::from("/work/project"),
            workloads: vec![tree(10, 100, &[(11, 10)]), tree(10, 100, &[(11, 10)])],
            total_memory_bytes: 220,
            total_cpu_percent: 0.0,
            age: Duration::from_hours(2),
        };

        let order = group
            .termination_order()
            .into_iter()
            .map(ProcessIdentity::pid)
            .collect::<Vec<_>>();

        assert_eq!(order, vec![11, 10]);
    }

    #[test]
    fn group_termination_order_does_not_change_roots_or_totals() {
        let group = StaleWorkloadGroup {
            working_directory: PathBuf::from("/work/project"),
            workloads: vec![tree(10, 100, &[(11, 10)])],
            total_memory_bytes: 110,
            total_cpu_percent: 0.0,
            age: Duration::from_hours(2),
        };
        let roots_before = group.root_identities();
        let trees_before = group.tree_count();
        let processes_before = group.process_count();
        let memory_before = group.total_memory_bytes;

        let _ = group.termination_order();

        assert_eq!(group.root_identities(), roots_before);
        assert_eq!(group.tree_count(), trees_before);
        assert_eq!(group.process_count(), processes_before);
        assert_eq!(group.total_memory_bytes, memory_before);
    }
}
