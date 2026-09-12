use std::{collections::HashSet, path::PathBuf, time::Duration};

use super::workload::termination_order;
use super::{ProcessDescriptor, ProcessIdentity, WorkloadMember};

#[derive(Clone, Debug, PartialEq)]
pub struct BackgroundWorkloadPolicy {
    pub enabled: bool,
    pub minimum_age: Duration,
    pub minimum_memory_bytes: u64,
    pub large_memory_bytes: u64,
    pub growth_window: Duration,
    pub minimum_memory_growth_bytes: u64,
    pub minimum_process_count_growth: usize,
    pub maximum_cpu_percent: f32,
    pub consecutive_samples: u32,
    pub sample_interval: Duration,
    pub notification_cooldown: Duration,
    pub ignored_root_names: HashSet<String>,
    pub ignored_root_executables: HashSet<PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BackgroundWorkloadSample {
    pub observed_at: Duration,
    pub memory_bytes: u64,
    pub process_count: usize,
    pub cpu_percent: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BackgroundWorkload {
    pub group_id: String,
    pub root: ProcessDescriptor,
    pub members: Vec<WorkloadMember>,
    pub total_memory_bytes: u64,
    pub total_cpu_percent: f32,
    pub age: Duration,
    pub observed_for: Duration,
    pub memory_growth_bytes: u64,
    pub process_count_growth: usize,
}

impl BackgroundWorkload {
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
