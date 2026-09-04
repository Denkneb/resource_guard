use std::{collections::HashSet, time::Duration};

use super::{ProcessDescriptor, ProcessIdentity, ProcessResources};

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
pub struct WorkloadMember {
    pub process: ProcessDescriptor,
    pub resources: ProcessResources,
    pub depth: usize,
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
        let mut members = self.members.iter().collect::<Vec<_>>();
        members.sort_by(|left, right| {
            right.depth.cmp(&left.depth).then_with(|| {
                right
                    .process
                    .identity()
                    .pid()
                    .cmp(&left.process.identity().pid())
            })
        });
        members
            .into_iter()
            .map(|member| member.process.identity())
            .collect()
    }
}
