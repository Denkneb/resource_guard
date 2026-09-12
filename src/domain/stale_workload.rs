use std::{collections::HashSet, time::Duration};

use super::workload::{WorkloadMember, termination_order};
use super::{ProcessDescriptor, ProcessIdentity};

#[derive(Clone, Debug, PartialEq)]
pub struct StaleWorkloadPolicy {
    pub enabled: bool,
    pub only_under_memory_pressure: bool,
    pub candidate_names: HashSet<String>,
    pub launcher_names: HashSet<String>,
    pub ignored_root_names: HashSet<String>,
    pub minimum_age: Duration,
    pub minimum_tree_memory_bytes: u64,
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
