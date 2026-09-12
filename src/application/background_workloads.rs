use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::PathBuf,
    time::Duration,
};

use crate::domain::{
    BackgroundWorkload, BackgroundWorkloadPolicy, BackgroundWorkloadSample, MemoryPressureLevel,
    ProcessDescriptor, ProcessDisposition, ProcessIdentity, ProcessOrigin, ProtectionPolicy,
    WorkloadMember,
};

use super::ObservedProcess;

const MAX_TRACKED_WORKLOADS: usize = 2048;

const GENERIC_LAUNCHER_NAMES: &[&str] = &[
    "bash", "sh", "zsh", "fish", "dash", "tmux", "screen", "xargs", "python", "python3",
];

#[derive(Debug, Default)]
struct TrackingState {
    consecutive_samples: u32,
    last_notification_at: Option<Duration>,
}

#[derive(Debug)]
pub struct BackgroundWorkloadService {
    current_uid: u32,
    protection: ProtectionPolicy,
    policy: BackgroundWorkloadPolicy,
    history: HashMap<ProcessIdentity, VecDeque<BackgroundWorkloadSample>>,
    tracking: HashMap<ProcessIdentity, TrackingState>,
    ignored_until: HashMap<ProcessIdentity, Duration>,
    last_sample_at: Option<Duration>,
    current_candidates: Vec<BackgroundWorkload>,
}

impl BackgroundWorkloadService {
    #[must_use]
    pub fn new(
        current_uid: u32,
        protection: ProtectionPolicy,
        policy: BackgroundWorkloadPolicy,
    ) -> Self {
        Self {
            current_uid,
            protection,
            policy,
            history: HashMap::new(),
            tracking: HashMap::new(),
            ignored_until: HashMap::new(),
            last_sample_at: None,
            current_candidates: Vec::new(),
        }
    }

    pub fn replace_protection_policy(&mut self, policy: ProtectionPolicy) {
        self.protection = policy;
        self.retain_monitored_candidates();
    }

    pub fn ignore_for(&mut self, identity: ProcessIdentity, until: Duration) {
        self.ignored_until.insert(identity, until);
    }

    pub fn ignore_name(&mut self, name: String) {
        self.policy.ignored_root_names.insert(name);
        self.retain_monitored_candidates();
    }

    pub fn ignore_executable(&mut self, executable: PathBuf) {
        self.policy.ignored_root_executables.insert(executable);
        self.retain_monitored_candidates();
    }

    pub fn ignore_process(&mut self, process: &ProcessDescriptor) {
        if let Some(executable) = process.executable() {
            self.policy
                .ignored_root_executables
                .insert(executable.to_path_buf());
        } else {
            self.policy
                .ignored_root_names
                .insert(process.name().to_owned());
        }
        self.retain_monitored_candidates();
    }

    /// Drops cached candidates that the current protection or background-ignore
    /// policy no longer allows, so an ignore takes effect before the next sample.
    fn retain_monitored_candidates(&mut self) {
        let protection = &self.protection;
        let ignored_names = &self.policy.ignored_root_names;
        let ignored_executables = &self.policy.ignored_root_executables;
        self.current_candidates.retain(|workload| {
            workload.members.iter().all(|member| {
                protection.disposition(&member.process) == ProcessDisposition::Monitor
            }) && !ignored_names.contains(workload.root.name())
                && !workload
                    .root
                    .executable()
                    .is_some_and(|executable| ignored_executables.contains(executable))
        });
    }

    /// Evaluates one monitoring cycle.
    ///
    /// Returns the current candidates plus the subset that should produce a new
    /// desktop notification. Intermediate polls that fall within `sample_interval`
    /// return the previous candidates without recording history or notifying.
    pub fn evaluate(
        &mut self,
        processes: &[ObservedProcess],
        pressure: MemoryPressureLevel,
        now: Duration,
    ) -> (Vec<BackgroundWorkload>, Vec<BackgroundWorkload>) {
        if !self.policy.enabled {
            self.history.clear();
            self.tracking.clear();
            self.last_sample_at = None;
            self.current_candidates.clear();
            return (Vec::new(), Vec::new());
        }

        if let Some(last) = self.last_sample_at
            && now.saturating_sub(last) < self.policy.sample_interval
        {
            return (self.current_candidates.clone(), Vec::new());
        }

        self.ignored_until.retain(|_, deadline| *deadline > now);

        let workloads = cap_workloads(build_background_workloads(
            processes,
            self.current_uid,
            &self.protection,
            &self.policy,
        ));

        let active = workloads
            .iter()
            .map(BackgroundWorkload::identity)
            .collect::<HashSet<_>>();
        self.history.retain(|identity, _| active.contains(identity));
        self.tracking
            .retain(|identity, _| active.contains(identity));

        self.last_sample_at = Some(now);

        let mut candidates = Vec::new();
        let mut notifications = Vec::new();

        for mut workload in workloads {
            let identity = workload.identity();
            let current_cpu = workload.total_cpu_percent;
            let sample = BackgroundWorkloadSample {
                observed_at: now,
                memory_bytes: workload.total_memory_bytes,
                process_count: workload.process_count(),
                cpu_percent: current_cpu,
            };
            let (memory_growth, process_growth, observed_for, average_cpu) = {
                let history = self.history.entry(identity).or_default();
                history.push_back(sample);
                prune_history(history, now, self.policy.growth_window);
                let baseline = history.front().copied().unwrap_or(sample);
                (
                    workload
                        .total_memory_bytes
                        .saturating_sub(baseline.memory_bytes),
                    workload
                        .process_count()
                        .saturating_sub(baseline.process_count),
                    now.saturating_sub(baseline.observed_at),
                    average_cpu(history),
                )
            };

            workload.total_cpu_percent = average_cpu;
            workload.memory_growth_bytes = memory_growth;
            workload.process_count_growth = process_growth;
            workload.observed_for = observed_for;

            let is_candidate = is_candidate(&workload, pressure, &self.policy);
            let state = self.tracking.entry(identity).or_default();
            if is_candidate {
                state.consecutive_samples = state.consecutive_samples.saturating_add(1);
                if state.consecutive_samples >= self.policy.consecutive_samples
                    && state.last_notification_at.is_none_or(|last| {
                        now.saturating_sub(last) >= self.policy.notification_cooldown
                    })
                    && !self.ignored_until.contains_key(&identity)
                {
                    state.last_notification_at = Some(now);
                    notifications.push(workload.clone());
                }
                candidates.push(workload);
            } else {
                state.consecutive_samples = 0;
            }
        }

        self.current_candidates.clone_from(&candidates);
        (candidates, notifications)
    }
}

fn is_candidate(
    workload: &BackgroundWorkload,
    pressure: MemoryPressureLevel,
    policy: &BackgroundWorkloadPolicy,
) -> bool {
    workload.age >= policy.minimum_age
        && workload.total_memory_bytes >= policy.minimum_memory_bytes
        && workload.total_cpu_percent <= policy.maximum_cpu_percent
        && workload.observed_for >= policy.growth_window
        && match pressure {
            MemoryPressureLevel::Normal => {
                workload.memory_growth_bytes >= policy.minimum_memory_growth_bytes
                    || workload.process_count_growth >= policy.minimum_process_count_growth
                    || workload.total_memory_bytes >= policy.large_memory_bytes
            }
            _ => true,
        }
}

#[must_use]
pub fn build_background_workloads(
    processes: &[ObservedProcess],
    current_uid: u32,
    protection: &ProtectionPolicy,
    policy: &BackgroundWorkloadPolicy,
) -> Vec<BackgroundWorkload> {
    if !policy.enabled {
        return Vec::new();
    }
    let by_pid = processes
        .iter()
        .map(|process| (process.descriptor.identity().pid(), process))
        .collect::<HashMap<_, _>>();

    eligible_groups(processes, current_uid)
        .into_iter()
        .filter_map(|(group_id, group)| {
            safe_workload(&group_id, &group, &by_pid, protection, policy)
        })
        .collect()
}

/// Rebuilds one background group from a fresh snapshot, verifying that the
/// expected root identity and execution group still match exactly and that the
/// whole group still satisfies every candidate safety condition.
#[must_use]
pub fn background_workload_from_root(
    processes: &[ObservedProcess],
    expected: ProcessIdentity,
    group_id: &str,
    current_uid: u32,
    protection: &ProtectionPolicy,
    policy: &BackgroundWorkloadPolicy,
) -> Option<BackgroundWorkload> {
    let by_pid = processes
        .iter()
        .map(|process| (process.descriptor.identity().pid(), process))
        .collect::<HashMap<_, _>>();
    let root = by_pid.get(&expected.pid()).copied()?;
    if root.descriptor.identity() != expected || expected.uid() != current_uid {
        return None;
    }
    let context = root.descriptor.execution_context();
    if context.origin() != ProcessOrigin::UserApplication || context.group_id() != Some(group_id) {
        return None;
    }
    let group = eligible_groups(processes, current_uid).remove(group_id)?;
    let workload = safe_workload(group_id, &group, &by_pid, protection, policy)?;
    (workload.identity() == expected).then_some(workload)
}

/// Applies every candidate safety rule to one group: current UID eligibility is
/// already assumed, while TTY, protection, executable, launcher, and ignore
/// checks are re-applied here.
fn safe_workload(
    group_id: &str,
    group: &[&ObservedProcess],
    by_pid: &HashMap<u32, &ObservedProcess>,
    protection: &ProtectionPolicy,
    policy: &BackgroundWorkloadPolicy,
) -> Option<BackgroundWorkload> {
    let group_pids = group
        .iter()
        .map(|process| process.descriptor.identity().pid())
        .collect::<HashSet<_>>();
    if group.iter().any(|process| {
        protection.disposition(&process.descriptor) != ProcessDisposition::Monitor
            || process
                .descriptor
                .execution_context()
                .has_controlling_terminal()
    }) {
        return None;
    }
    let root = select_root(group, &group_pids);
    root.descriptor.executable()?;
    if GENERIC_LAUNCHER_NAMES.contains(&root.descriptor.name()) {
        return None;
    }
    if policy.ignored_root_names.contains(root.descriptor.name())
        || root
            .descriptor
            .executable()
            .is_some_and(|executable| policy.ignored_root_executables.contains(executable))
    {
        return None;
    }
    Some(build_group(group_id, group, by_pid))
}

fn eligible_groups(
    processes: &[ObservedProcess],
    current_uid: u32,
) -> HashMap<String, Vec<&ObservedProcess>> {
    let mut groups = HashMap::new();
    for process in processes {
        if process.descriptor.identity().uid() != current_uid {
            continue;
        }
        let context = process.descriptor.execution_context();
        if context.origin() != ProcessOrigin::UserApplication {
            continue;
        }
        let Some(group_id) = context.group_id() else {
            continue;
        };
        groups
            .entry(group_id.to_owned())
            .or_insert_with(Vec::new)
            .push(process);
    }
    groups
}

fn build_group(
    group_id: &str,
    group: &[&ObservedProcess],
    by_pid: &HashMap<u32, &ObservedProcess>,
) -> BackgroundWorkload {
    let group_pids = group
        .iter()
        .map(|process| process.descriptor.identity().pid())
        .collect::<HashSet<_>>();
    let root = select_root(group, &group_pids);
    let root_pid = root.descriptor.identity().pid();
    let members = group
        .iter()
        .map(|process| WorkloadMember {
            process: process.descriptor.clone(),
            resources: process.resources,
            depth: member_depth(
                process.descriptor.identity().pid(),
                root_pid,
                by_pid,
                &group_pids,
            ),
        })
        .collect::<Vec<_>>();
    let total_memory_bytes = members.iter().fold(0_u64, |total, member| {
        total.saturating_add(member.resources.resident_memory_bytes)
    });
    let total_cpu_percent = members
        .iter()
        .map(|member| sanitize_cpu(member.resources.cpu_percent))
        .sum();
    let age = root.resources.running_for;

    BackgroundWorkload {
        group_id: group_id.to_owned(),
        root: root.descriptor.clone(),
        members,
        total_memory_bytes,
        total_cpu_percent,
        age,
        observed_for: Duration::ZERO,
        memory_growth_bytes: 0,
        process_count_growth: 0,
    }
}

fn select_root<'a>(
    group: &[&'a ObservedProcess],
    group_pids: &HashSet<u32>,
) -> &'a ObservedProcess {
    let roots = group
        .iter()
        .copied()
        .filter(|process| {
            process
                .descriptor
                .parent_pid()
                .is_none_or(|parent| !group_pids.contains(&parent))
        })
        .collect::<Vec<_>>();
    let pool = if roots.is_empty() {
        group.to_vec()
    } else {
        roots
    };
    pool.into_iter()
        .max_by(|left, right| {
            left.resources
                .running_for
                .cmp(&right.resources.running_for)
                .then_with(|| {
                    right
                        .descriptor
                        .identity()
                        .pid()
                        .cmp(&left.descriptor.identity().pid())
                })
        })
        .expect("a group always contains at least one process")
}

fn member_depth(
    pid: u32,
    root_pid: u32,
    by_pid: &HashMap<u32, &ObservedProcess>,
    group_pids: &HashSet<u32>,
) -> usize {
    let mut current = pid;
    let mut depth = 0;
    loop {
        if current == root_pid {
            return depth;
        }
        let Some(process) = by_pid.get(&current) else {
            return 0;
        };
        let Some(parent) = process.descriptor.parent_pid() else {
            return 0;
        };
        if !group_pids.contains(&parent) {
            return 0;
        }
        current = parent;
        depth += 1;
        if depth > group_pids.len() {
            return 0;
        }
    }
}

fn cap_workloads(mut workloads: Vec<BackgroundWorkload>) -> Vec<BackgroundWorkload> {
    if workloads.len() <= MAX_TRACKED_WORKLOADS {
        return workloads;
    }
    workloads.sort_by(|left, right| {
        right
            .total_memory_bytes
            .cmp(&left.total_memory_bytes)
            .then_with(|| left.identity().pid().cmp(&right.identity().pid()))
    });
    workloads.truncate(MAX_TRACKED_WORKLOADS);
    workloads
}

fn prune_history(
    history: &mut VecDeque<BackgroundWorkloadSample>,
    now: Duration,
    window: Duration,
) {
    while history.len() >= 2 {
        let Some(second) = history.get(1).copied() else {
            break;
        };
        if now.saturating_sub(second.observed_at) > window {
            history.pop_front();
        } else {
            break;
        }
    }
}

#[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
fn average_cpu(history: &VecDeque<BackgroundWorkloadSample>) -> f32 {
    if history.is_empty() {
        return 0.0;
    }
    let mut sum = 0.0_f64;
    let mut count = 0_u64;
    for sample in history {
        sum += f64::from(sanitize_cpu(sample.cpu_percent));
        count += 1;
    }
    let average = (sum / count as f64) as f32;
    sanitize_cpu(average)
}

fn sanitize_cpu(value: f32) -> f32 {
    if value.is_finite() { value } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashSet, path::PathBuf, time::Duration};

    use super::{
        BackgroundWorkloadService, background_workload_from_root, build_background_workloads,
    };
    use crate::{
        application::ObservedProcess,
        domain::{
            BackgroundWorkloadPolicy, MemoryPressureLevel, ProcessDescriptor,
            ProcessExecutionContext, ProcessIdentity, ProcessOrigin, ProcessResources,
            ProcessState, ProtectionPolicy,
        },
    };

    const UID: u32 = 1_000;
    const MIB: u64 = 1_048_576;

    #[allow(clippy::too_many_arguments)]
    fn observed(
        pid: u32,
        uid: u32,
        parent: Option<u32>,
        group_id: &str,
        origin: ProcessOrigin,
        tty: bool,
        running_for: Duration,
        cpu: f32,
        memory: u64,
        name: &str,
    ) -> ObservedProcess {
        ObservedProcess {
            descriptor: ProcessDescriptor::new(
                ProcessIdentity::new(pid, uid, u64::from(pid)),
                name,
                Some(PathBuf::from(format!("/usr/bin/{name}"))),
            )
            .with_runtime(parent, ProcessState::Sleeping)
            .with_execution_context(ProcessExecutionContext::new(
                Some(format!("systemd-unit:{group_id}")),
                Some(group_id.to_owned()),
                origin,
                tty,
            )),
            resources: ProcessResources {
                cpu_percent: cpu,
                resident_memory_bytes: memory,
                virtual_memory_bytes: memory,
                running_for,
                observed_at: Duration::ZERO,
            },
        }
    }

    fn app(
        pid: u32,
        parent: Option<u32>,
        age_hours: u64,
        cpu: f32,
        memory: u64,
    ) -> ObservedProcess {
        observed(
            pid,
            UID,
            parent,
            "app-1.scope",
            ProcessOrigin::UserApplication,
            false,
            Duration::from_hours(age_hours),
            cpu,
            memory,
            &format!("app{pid}"),
        )
    }

    fn policy() -> BackgroundWorkloadPolicy {
        BackgroundWorkloadPolicy {
            enabled: true,
            minimum_age: Duration::from_hours(1),
            minimum_memory_bytes: 256 * MIB,
            large_memory_bytes: 512 * MIB,
            growth_window: Duration::from_secs(120),
            minimum_memory_growth_bytes: 128 * MIB,
            minimum_process_count_growth: 2,
            maximum_cpu_percent: 5.0,
            consecutive_samples: 3,
            sample_interval: Duration::from_secs(60),
            notification_cooldown: Duration::from_secs(10_000),
            ignored_root_names: HashSet::new(),
            ignored_root_executables: HashSet::new(),
        }
    }

    fn service(policy: BackgroundWorkloadPolicy) -> BackgroundWorkloadService {
        BackgroundWorkloadService::new(UID, ProtectionPolicy::default(), policy)
    }

    fn group_with(child_count: usize) -> Vec<ObservedProcess> {
        let mut processes = vec![app(10, None, 2, 1.0, 256 * MIB)];
        for index in 1..=child_count {
            let pid = 10 + u32::try_from(index).unwrap();
            processes.push(app(pid, Some(10), 2, 1.0, 2 * MIB));
        }
        processes
    }

    #[test]
    fn old_application_scope_with_growing_rss_is_a_candidate_after_full_window() {
        let mut service = service(policy());

        let (candidates, notifications) = service.evaluate(
            &[app(10, None, 2, 1.0, 256 * MIB)],
            MemoryPressureLevel::Normal,
            Duration::from_secs(0),
        );
        assert!(candidates.is_empty());
        assert!(notifications.is_empty());

        let (candidates, _) = service.evaluate(
            &[app(10, None, 2, 1.0, 300 * MIB)],
            MemoryPressureLevel::Normal,
            Duration::from_secs(60),
        );
        assert!(candidates.is_empty());

        let (candidates, notifications) = service.evaluate(
            &[app(10, None, 2, 1.0, 384 * MIB)],
            MemoryPressureLevel::Normal,
            Duration::from_secs(120),
        );
        assert_eq!(candidates.len(), 1);
        assert!(notifications.is_empty());

        let (_, notifications) = service.evaluate(
            &[app(10, None, 2, 1.0, 448 * MIB)],
            MemoryPressureLevel::Normal,
            Duration::from_secs(180),
        );
        assert!(notifications.is_empty());

        let (candidates, notifications) = service.evaluate(
            &[app(10, None, 2, 1.0, 512 * MIB)],
            MemoryPressureLevel::Normal,
            Duration::from_secs(240),
        );
        assert_eq!(candidates.len(), 1);
        assert_eq!(notifications.len(), 1);
        assert!(notifications[0].memory_growth_bytes >= 128 * MIB);
    }

    #[test]
    fn process_count_growth_with_small_rss_growth_is_a_candidate() {
        let mut service = service(policy());

        service.evaluate(
            &group_with(0),
            MemoryPressureLevel::Normal,
            Duration::from_secs(0),
        );
        service.evaluate(
            &group_with(1),
            MemoryPressureLevel::Normal,
            Duration::from_secs(60),
        );
        let (candidates, notifications) = service.evaluate(
            &group_with(2),
            MemoryPressureLevel::Normal,
            Duration::from_secs(120),
        );
        assert_eq!(candidates.len(), 1);
        assert!(notifications.is_empty());

        service.evaluate(
            &group_with(3),
            MemoryPressureLevel::Normal,
            Duration::from_secs(180),
        );
        let (candidates, notifications) = service.evaluate(
            &group_with(4),
            MemoryPressureLevel::Normal,
            Duration::from_secs(240),
        );
        assert_eq!(candidates.len(), 1);
        assert_eq!(notifications.len(), 1);
        assert!(notifications[0].process_count_growth >= 2);
    }

    #[test]
    fn rss_above_large_memory_is_a_candidate_at_normal_pressure() {
        let mut service = service(policy());
        for now in [60_u64, 120, 180, 240] {
            service.evaluate(
                &[app(10, None, 2, 1.0, 600 * MIB)],
                MemoryPressureLevel::Normal,
                Duration::from_secs(now),
            );
        }
        let (candidates, notifications) = service.evaluate(
            &[app(10, None, 2, 1.0, 600 * MIB)],
            MemoryPressureLevel::Normal,
            Duration::from_secs(300),
        );
        assert_eq!(candidates.len(), 1);
        assert_eq!(notifications.len(), 1);
    }

    #[test]
    fn stable_group_between_minimum_and_large_is_not_a_candidate_at_normal_pressure() {
        let mut service = service(policy());
        for now in [60_u64, 120, 180, 240, 300, 360] {
            service.evaluate(
                &[app(10, None, 2, 1.0, 300 * MIB)],
                MemoryPressureLevel::Normal,
                Duration::from_secs(now),
            );
        }
        let (candidates, _) = service.evaluate(
            &[app(10, None, 2, 1.0, 300 * MIB)],
            MemoryPressureLevel::Normal,
            Duration::from_secs(420),
        );
        assert!(candidates.is_empty());
    }

    #[test]
    fn same_stable_group_is_a_candidate_under_warning_pressure() {
        let mut service = service(policy());
        for now in [60_u64, 120, 180, 240] {
            service.evaluate(
                &[app(10, None, 2, 1.0, 300 * MIB)],
                MemoryPressureLevel::Warning,
                Duration::from_secs(now),
            );
        }
        let (candidates, notifications) = service.evaluate(
            &[app(10, None, 2, 1.0, 300 * MIB)],
            MemoryPressureLevel::Warning,
            Duration::from_secs(300),
        );
        assert_eq!(candidates.len(), 1);
        assert_eq!(notifications.len(), 1);
    }

    #[test]
    fn a_young_process_is_not_a_candidate() {
        let mut service = service(policy());
        for now in [60_u64, 120, 180, 240, 300] {
            service.evaluate(
                &[app(10, None, 0, 1.0, 600 * MIB)],
                MemoryPressureLevel::Normal,
                Duration::from_secs(now),
            );
        }
        let (candidates, _) = service.evaluate(
            &[app(10, None, 0, 1.0, 600 * MIB)],
            MemoryPressureLevel::Normal,
            Duration::from_secs(360),
        );
        assert!(candidates.is_empty());
    }

    #[test]
    fn a_high_cpu_process_is_not_a_candidate() {
        let mut service = service(policy());
        for now in [60_u64, 120, 180, 240, 300] {
            service.evaluate(
                &[app(10, None, 2, 50.0, 600 * MIB)],
                MemoryPressureLevel::Normal,
                Duration::from_secs(now),
            );
        }
        let (candidates, _) = service.evaluate(
            &[app(10, None, 2, 50.0, 600 * MIB)],
            MemoryPressureLevel::Normal,
            Duration::from_secs(360),
        );
        assert!(candidates.is_empty());
    }

    #[test]
    fn a_user_service_is_not_a_candidate() {
        let mut service = service(policy());
        let process = observed(
            10,
            UID,
            None,
            "app-1.scope",
            ProcessOrigin::UserService,
            false,
            Duration::from_hours(2),
            1.0,
            600 * MIB,
            "worker",
        );
        for now in [60_u64, 120, 180, 240, 300] {
            service.evaluate(
                std::slice::from_ref(&process),
                MemoryPressureLevel::Normal,
                Duration::from_secs(now),
            );
        }
        let (candidates, _) = service.evaluate(
            &[process],
            MemoryPressureLevel::Normal,
            Duration::from_secs(360),
        );
        assert!(candidates.is_empty());
    }

    #[test]
    fn an_unknown_origin_is_not_a_candidate() {
        let mut service = service(policy());
        let process = observed(
            10,
            UID,
            None,
            "app-1.scope",
            ProcessOrigin::Unknown,
            false,
            Duration::from_hours(2),
            1.0,
            600 * MIB,
            "worker",
        );
        for now in [60_u64, 120, 180, 240, 300] {
            service.evaluate(
                std::slice::from_ref(&process),
                MemoryPressureLevel::Normal,
                Duration::from_secs(now),
            );
        }
        let (candidates, _) = service.evaluate(
            &[process],
            MemoryPressureLevel::Normal,
            Duration::from_secs(360),
        );
        assert!(candidates.is_empty());
    }

    #[test]
    fn a_group_with_a_controlling_terminal_member_is_not_a_candidate() {
        let mut service = service(policy());
        let tty = observed(
            11,
            UID,
            Some(10),
            "app-1.scope",
            ProcessOrigin::UserApplication,
            true,
            Duration::from_hours(2),
            1.0,
            2 * MIB,
            "shell",
        );
        for now in [60_u64, 120, 180, 240, 300] {
            let processes = vec![app(10, None, 2, 1.0, 600 * MIB), tty.clone()];
            service.evaluate(
                &processes,
                MemoryPressureLevel::Normal,
                Duration::from_secs(now),
            );
        }
        let processes = vec![app(10, None, 2, 1.0, 600 * MIB), tty];
        let (candidates, _) = service.evaluate(
            &processes,
            MemoryPressureLevel::Normal,
            Duration::from_secs(360),
        );
        assert!(candidates.is_empty());
    }

    #[test]
    fn a_protected_root_is_not_a_candidate() {
        let protection = ProtectionPolicy::new(["app10".to_owned()], [], [], []);
        let mut service = BackgroundWorkloadService::new(UID, protection, policy());
        for now in [60_u64, 120, 180, 240, 300] {
            service.evaluate(
                &[app(10, None, 2, 1.0, 600 * MIB)],
                MemoryPressureLevel::Normal,
                Duration::from_secs(now),
            );
        }
        let (candidates, _) = service.evaluate(
            &[app(10, None, 2, 1.0, 600 * MIB)],
            MemoryPressureLevel::Normal,
            Duration::from_secs(360),
        );
        assert!(candidates.is_empty());
    }

    #[test]
    fn ignored_executable_takes_precedence_over_name() {
        let mut service = service(policy());
        let process = app(10, None, 2, 1.0, 600 * MIB);
        service.ignore_process(&process.descriptor);

        let same_name_different_executable = ObservedProcess {
            descriptor: ProcessDescriptor::new(
                process.descriptor.identity(),
                "app10",
                Some(PathBuf::from("/opt/app10")),
            )
            .with_runtime(None, ProcessState::Sleeping)
            .with_execution_context(ProcessExecutionContext::new(
                Some("systemd-unit:app-1.scope".to_owned()),
                Some("app-1.scope".to_owned()),
                ProcessOrigin::UserApplication,
                false,
            )),
            resources: process.resources,
        };
        for now in [60_u64, 120, 180, 240, 300] {
            let (candidates, _) = service.evaluate(
                std::slice::from_ref(&same_name_different_executable),
                MemoryPressureLevel::Normal,
                Duration::from_secs(now),
            );
            if now >= 240 {
                assert_eq!(candidates.len(), 1);
            }
        }
    }

    #[test]
    fn temporary_ignore_expires() {
        let mut service = service(policy());
        let root = app(10, None, 2, 1.0, 600 * MIB);
        service.ignore_for(root.descriptor.identity(), Duration::from_secs(600));

        for now in [60_u64, 120, 180, 240, 300, 360, 420, 480, 540] {
            service.evaluate(
                std::slice::from_ref(&root),
                MemoryPressureLevel::Normal,
                Duration::from_secs(now),
            );
        }
        let (candidates, notifications) = service.evaluate(
            &[root],
            MemoryPressureLevel::Normal,
            Duration::from_secs(600),
        );
        assert_eq!(candidates.len(), 1);
        assert_eq!(notifications.len(), 1);
    }

    #[test]
    fn cooldown_suppresses_repeated_notifications_but_keeps_candidates() {
        let mut service = service(policy());
        for now in [60_u64, 120, 180, 240] {
            service.evaluate(
                &[app(10, None, 2, 1.0, 600 * MIB)],
                MemoryPressureLevel::Normal,
                Duration::from_secs(now),
            );
        }
        let (candidates, notifications) = service.evaluate(
            &[app(10, None, 2, 1.0, 600 * MIB)],
            MemoryPressureLevel::Normal,
            Duration::from_secs(300),
        );
        assert_eq!(candidates.len(), 1);
        assert_eq!(notifications.len(), 1);

        let (candidates, notifications) = service.evaluate(
            &[app(10, None, 2, 1.0, 600 * MIB)],
            MemoryPressureLevel::Normal,
            Duration::from_secs(360),
        );
        assert_eq!(candidates.len(), 1);
        assert!(notifications.is_empty());
    }

    #[test]
    fn finished_identities_are_removed_from_history() {
        let mut service = service(policy());
        for now in [60_u64, 120] {
            service.evaluate(
                &[app(10, None, 2, 1.0, 600 * MIB)],
                MemoryPressureLevel::Normal,
                Duration::from_secs(now),
            );
        }
        assert_eq!(service.history.len(), 1);

        service.evaluate(&[], MemoryPressureLevel::Normal, Duration::from_secs(180));
        assert!(service.history.is_empty());
        assert!(service.current_candidates.is_empty());
    }

    #[test]
    fn a_reused_pid_with_a_different_start_time_gets_a_new_history() {
        let mut service = service(policy());
        let first = app(10, None, 2, 1.0, 600 * MIB);
        let first_identity = first.descriptor.identity();
        service.evaluate(
            &[first],
            MemoryPressureLevel::Normal,
            Duration::from_secs(0),
        );

        let reused = ObservedProcess {
            descriptor: ProcessDescriptor::new(
                ProcessIdentity::new(10, UID, 999),
                "app10",
                Some(PathBuf::from("/usr/bin/app10")),
            )
            .with_runtime(None, ProcessState::Sleeping)
            .with_execution_context(ProcessExecutionContext::new(
                Some("systemd-unit:app-1.scope".to_owned()),
                Some("app-1.scope".to_owned()),
                ProcessOrigin::UserApplication,
                false,
            )),
            resources: crate::domain::ProcessResources {
                cpu_percent: 1.0,
                resident_memory_bytes: 600 * MIB,
                virtual_memory_bytes: 600 * MIB,
                running_for: Duration::from_hours(2),
                observed_at: Duration::ZERO,
            },
        };
        let reused_identity = reused.descriptor.identity();
        service.evaluate(
            &[reused],
            MemoryPressureLevel::Normal,
            Duration::from_secs(60),
        );

        assert!(!service.history.contains_key(&first_identity));
        assert!(service.history.contains_key(&reused_identity));
        assert_eq!(service.history.len(), 1);
    }

    #[test]
    fn two_scopes_with_the_same_process_name_are_not_merged() {
        let first = observed(
            10,
            UID,
            None,
            "app-1.scope",
            ProcessOrigin::UserApplication,
            false,
            Duration::from_hours(2),
            1.0,
            600 * MIB,
            "worker",
        );
        let second = observed(
            20,
            UID,
            None,
            "app-2.scope",
            ProcessOrigin::UserApplication,
            false,
            Duration::from_hours(2),
            1.0,
            600 * MIB,
            "worker",
        );
        let processes = vec![first, second];
        let candidates =
            build_background_workloads(&processes, UID, &ProtectionPolicy::default(), &policy());
        assert_eq!(candidates.len(), 2);
    }

    #[test]
    fn a_parent_outside_the_group_is_not_included_in_members() {
        let parent = observed(
            5,
            UID,
            None,
            "other.scope",
            ProcessOrigin::UserService,
            false,
            Duration::from_hours(2),
            1.0,
            10 * MIB,
            "parent",
        );
        let child = app(10, Some(5), 2, 1.0, 600 * MIB);
        let candidates = build_background_workloads(
            &[parent, child],
            UID,
            &ProtectionPolicy::default(),
            &policy(),
        );
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].process_count(), 1);
        assert_eq!(candidates[0].identity().pid(), 10);
    }

    #[test]
    fn termination_order_is_leaf_first_and_excludes_other_groups() {
        let root = app(10, None, 2, 1.0, 300 * MIB);
        let child = observed(
            11,
            UID,
            Some(10),
            "app-1.scope",
            ProcessOrigin::UserApplication,
            false,
            Duration::from_hours(2),
            1.0,
            2 * MIB,
            "child",
        );
        let grandchild = observed(
            12,
            UID,
            Some(11),
            "app-1.scope",
            ProcessOrigin::UserApplication,
            false,
            Duration::from_hours(2),
            1.0,
            2 * MIB,
            "grandchild",
        );
        let other = observed(
            20,
            UID,
            None,
            "app-2.scope",
            ProcessOrigin::UserApplication,
            false,
            Duration::from_hours(2),
            1.0,
            300 * MIB,
            "other",
        );
        let workloads = build_background_workloads(
            &[root, child, grandchild, other],
            UID,
            &ProtectionPolicy::default(),
            &policy(),
        )
        .into_iter()
        .find(|workload| workload.identity().pid() == 10)
        .unwrap();

        assert_eq!(
            workloads
                .termination_order()
                .into_iter()
                .map(ProcessIdentity::pid)
                .collect::<Vec<_>>(),
            vec![12, 11, 10]
        );
    }

    #[test]
    fn non_finite_cpu_does_not_enter_candidate_output() {
        let mut service = service(policy());
        for now in [60_u64, 120, 180, 240] {
            service.evaluate(
                &[app(10, None, 2, f32::NAN, 600 * MIB)],
                MemoryPressureLevel::Normal,
                Duration::from_secs(now),
            );
        }
        let (candidates, notifications) = service.evaluate(
            &[app(10, None, 2, f32::INFINITY, 600 * MIB)],
            MemoryPressureLevel::Normal,
            Duration::from_secs(300),
        );
        assert_eq!(candidates.len(), 1);
        assert_eq!(notifications.len(), 1);
        assert!(candidates[0].total_cpu_percent.abs() < f32::EPSILON);
        assert!(notifications[0].total_cpu_percent.abs() < f32::EPSILON);
    }
    #[test]
    fn a_protected_or_ignored_member_excludes_the_whole_group() {
        let protection = ProtectionPolicy::new([], [], [], [PathBuf::from("/usr/bin/app11")]);
        let processes = vec![
            app(10, None, 2, 1.0, 600 * MIB),
            app(11, Some(10), 2, 1.0, 2 * MIB),
        ];
        let candidates = build_background_workloads(&processes, UID, &protection, &policy());
        assert!(candidates.is_empty());
    }

    #[test]
    fn replacing_the_protection_policy_removes_a_previously_visible_candidate() {
        let mut service = service(policy());
        for now in [60_u64, 120, 180, 240] {
            service.evaluate(
                &[app(10, None, 2, 1.0, 600 * MIB)],
                MemoryPressureLevel::Normal,
                Duration::from_secs(now),
            );
        }
        let (candidates, _) = service.evaluate(
            &[app(10, None, 2, 1.0, 600 * MIB)],
            MemoryPressureLevel::Normal,
            Duration::from_secs(300),
        );
        assert_eq!(candidates.len(), 1);

        service.replace_protection_policy(ProtectionPolicy::new(["app10".to_owned()], [], [], []));
        let (candidates, _) = service.evaluate(
            &[app(10, None, 2, 1.0, 600 * MIB)],
            MemoryPressureLevel::Normal,
            Duration::from_secs(310),
        );
        assert!(candidates.is_empty());
    }

    #[test]
    fn permanently_ignoring_a_process_removes_the_candidate_before_the_next_sample() {
        let mut service = service(policy());
        let process = app(10, None, 2, 1.0, 600 * MIB);
        for now in [60_u64, 120, 180, 240] {
            service.evaluate(
                std::slice::from_ref(&process),
                MemoryPressureLevel::Normal,
                Duration::from_secs(now),
            );
        }
        let (candidates, _) = service.evaluate(
            std::slice::from_ref(&process),
            MemoryPressureLevel::Normal,
            Duration::from_secs(300),
        );
        assert_eq!(candidates.len(), 1);

        service.ignore_process(&process.descriptor);
        let (candidates, _) = service.evaluate(
            std::slice::from_ref(&process),
            MemoryPressureLevel::Normal,
            Duration::from_secs(310),
        );
        assert!(candidates.is_empty());
    }

    #[test]
    fn ignoring_by_name_or_executable_removes_the_candidate_before_the_next_sample() {
        for ignore in [
            Ignore::Name("app10".to_owned()),
            Ignore::Executable(PathBuf::from("/usr/bin/app10")),
        ] {
            let mut service = service(policy());
            let process = app(10, None, 2, 1.0, 600 * MIB);
            for now in [60_u64, 120, 180, 240] {
                service.evaluate(
                    std::slice::from_ref(&process),
                    MemoryPressureLevel::Normal,
                    Duration::from_secs(now),
                );
            }
            let (candidates, _) = service.evaluate(
                std::slice::from_ref(&process),
                MemoryPressureLevel::Normal,
                Duration::from_secs(300),
            );
            assert_eq!(candidates.len(), 1);

            match ignore {
                Ignore::Name(name) => service.ignore_name(name),
                Ignore::Executable(executable) => service.ignore_executable(executable),
            }
            let (candidates, _) = service.evaluate(
                std::slice::from_ref(&process),
                MemoryPressureLevel::Normal,
                Duration::from_secs(310),
            );
            assert!(candidates.is_empty());
        }
    }

    enum Ignore {
        Name(String),
        Executable(PathBuf),
    }

    #[test]
    fn intermediate_polls_do_not_record_or_notify() {
        let mut service = service(policy());
        service.evaluate(
            &[app(10, None, 2, 1.0, 600 * MIB)],
            MemoryPressureLevel::Normal,
            Duration::from_secs(0),
        );
        let (candidates, notifications) = service.evaluate(
            &[app(10, None, 2, 1.0, 600 * MIB)],
            MemoryPressureLevel::Normal,
            Duration::from_secs(10),
        );
        assert!(candidates.is_empty());
        assert!(notifications.is_empty());
        assert_eq!(service.history.len(), 1);
    }

    #[test]
    fn background_workload_from_root_requires_matching_identity_and_group() {
        let root = app(10, None, 2, 1.0, 300 * MIB);
        let child = app(11, Some(10), 2, 1.0, 2 * MIB);
        let processes = vec![root.clone(), child];
        let identity = root.descriptor.identity();
        let protection = ProtectionPolicy::default();
        let policy = policy();

        let workload = background_workload_from_root(
            &processes,
            identity,
            "systemd-unit:app-1.scope",
            UID,
            &protection,
            &policy,
        )
        .unwrap();
        assert_eq!(workload.identity(), identity);
        assert_eq!(workload.process_count(), 2);

        let wrong_start =
            ProcessIdentity::new(identity.pid(), identity.uid(), identity.started_at() + 1);
        assert!(
            background_workload_from_root(
                &processes,
                wrong_start,
                "systemd-unit:app-1.scope",
                UID,
                &protection,
                &policy,
            )
            .is_none()
        );

        assert!(
            background_workload_from_root(
                &processes,
                identity,
                "systemd-unit:other.scope",
                UID,
                &protection,
                &policy,
            )
            .is_none()
        );
    }

    #[test]
    fn background_workload_from_root_revalidates_every_group_safety_rule() {
        let root = app(10, None, 2, 1.0, 300 * MIB);
        let identity = root.descriptor.identity();
        let policy = policy();

        let tty_child = observed(
            11,
            UID,
            Some(10),
            "app-1.scope",
            ProcessOrigin::UserApplication,
            true,
            Duration::from_hours(2),
            1.0,
            2 * MIB,
            "shell",
        );
        assert!(
            background_workload_from_root(
                &[root.clone(), tty_child],
                identity,
                "systemd-unit:app-1.scope",
                UID,
                &ProtectionPolicy::default(),
                &policy,
            )
            .is_none()
        );

        let ignored_child = app(11, Some(10), 2, 1.0, 2 * MIB);
        let protection = ProtectionPolicy::new([], [], [], [PathBuf::from("/usr/bin/app11")]);
        assert!(
            background_workload_from_root(
                &[root.clone(), ignored_child],
                identity,
                "systemd-unit:app-1.scope",
                UID,
                &protection,
                &policy,
            )
            .is_none()
        );

        let launcher = observed(
            10,
            UID,
            None,
            "app-1.scope",
            ProcessOrigin::UserApplication,
            false,
            Duration::from_hours(2),
            1.0,
            300 * MIB,
            "bash",
        );
        assert!(
            background_workload_from_root(
                std::slice::from_ref(&launcher),
                launcher.descriptor.identity(),
                "systemd-unit:app-1.scope",
                UID,
                &ProtectionPolicy::default(),
                &policy,
            )
            .is_none()
        );

        let mut ignored_policy = policy.clone();
        ignored_policy
            .ignored_root_executables
            .insert(PathBuf::from("/usr/bin/app10"));
        assert!(
            background_workload_from_root(
                std::slice::from_ref(&root),
                identity,
                "systemd-unit:app-1.scope",
                UID,
                &ProtectionPolicy::default(),
                &ignored_policy,
            )
            .is_none()
        );
    }
}
