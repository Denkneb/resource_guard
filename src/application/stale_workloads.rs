use std::{
    cmp::Reverse,
    collections::{BTreeMap, HashMap, HashSet},
    path::PathBuf,
    time::Duration,
};

use crate::domain::{
    MemoryPressureLevel, ProcessIdentity, StaleWorkload, StaleWorkloadGroup, StaleWorkloadPolicy,
    WorkloadMember,
};

use super::ObservedProcess;

/// Pure detection result for one snapshot.
///
/// `workloads` is the union of direct candidates and promoted group members
/// without duplicates. `direct_root_identities` lets the notification layer
/// suppress a group's members individually.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StaleWorkloadDetection {
    pub workloads: Vec<StaleWorkload>,
    pub groups: Vec<StaleWorkloadGroup>,
    pub direct_root_identities: HashSet<ProcessIdentity>,
}

/// One evaluation cycle: the full inventory plus the notification subsets.
#[derive(Debug)]
pub struct StaleWorkloadEvaluation {
    pub detection: StaleWorkloadDetection,
    pub direct_notifications: Vec<StaleWorkload>,
    pub group_notifications: Vec<StaleWorkloadGroup>,
}

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
    tracked_groups: HashMap<PathBuf, TrackingState>,
    ignored_until: HashMap<ProcessIdentity, Duration>,
}

impl StaleWorkloadService {
    #[must_use]
    pub fn new(current_uid: u32, policy: StaleWorkloadPolicy) -> Self {
        Self {
            current_uid,
            policy,
            tracked: HashMap::new(),
            tracked_groups: HashMap::new(),
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

    /// Evaluates one monitoring cycle.
    ///
    /// The inventory is always built while detection is enabled. Memory pressure
    /// only gates desktop notifications. Group notifications are suppressed for
    /// their member roots, and inactive roots/groups are removed immediately.
    #[must_use]
    pub fn evaluate(
        &mut self,
        processes: &[ObservedProcess],
        pressure: MemoryPressureLevel,
        now: Duration,
    ) -> StaleWorkloadEvaluation {
        if !self.policy.enabled {
            self.tracked.clear();
            self.tracked_groups.clear();
            self.ignored_until.clear();
            return StaleWorkloadEvaluation {
                detection: StaleWorkloadDetection::default(),
                direct_notifications: Vec::new(),
                group_notifications: Vec::new(),
            };
        }
        self.ignored_until.retain(|_, deadline| *deadline > now);

        let mut eligible = discover_eligible_trees(processes, self.current_uid, &self.policy);
        eligible.retain(|workload| !self.ignored_until.contains_key(&workload.identity()));
        let detection = assemble_detection(&eligible, &self.policy);

        let grouped_root_identities = detection
            .groups
            .iter()
            .flat_map(StaleWorkloadGroup::root_identities)
            .collect::<HashSet<_>>();
        let direct_candidates = detection
            .workloads
            .iter()
            .filter(|workload| {
                detection
                    .direct_root_identities
                    .contains(&workload.identity())
            })
            .filter(|workload| !grouped_root_identities.contains(&workload.identity()))
            .cloned()
            .collect::<Vec<_>>();
        let active_direct = direct_candidates
            .iter()
            .map(StaleWorkload::identity)
            .collect::<HashSet<_>>();
        let active_groups = detection
            .groups
            .iter()
            .map(|group| group.working_directory.clone())
            .collect::<HashSet<_>>();

        self.tracked
            .retain(|identity, _| active_direct.contains(identity));
        self.tracked_groups
            .retain(|working_directory, _| active_groups.contains(working_directory));

        let notifications_permitted = !self.policy.notify_only_under_memory_pressure
            || pressure != MemoryPressureLevel::Normal;

        let mut direct_notifications = Vec::new();
        let mut group_notifications = Vec::new();
        for candidate in &direct_candidates {
            let identity = candidate.identity();
            let state = self.tracked.entry(identity).or_default();
            if notifications_permitted {
                if track_and_notify(state, &self.policy, now) {
                    direct_notifications.push(candidate.clone());
                }
            } else {
                state.consecutive_samples = 0;
            }
        }
        for group in &detection.groups {
            let state = self
                .tracked_groups
                .entry(group.working_directory.clone())
                .or_default();
            if notifications_permitted {
                if track_and_notify(state, &self.policy, now) {
                    group_notifications.push(group.clone());
                }
            } else {
                state.consecutive_samples = 0;
            }
        }

        StaleWorkloadEvaluation {
            detection,
            direct_notifications,
            group_notifications,
        }
    }
}

fn track_and_notify(
    state: &mut TrackingState,
    policy: &StaleWorkloadPolicy,
    now: Duration,
) -> bool {
    state.consecutive_samples = state.consecutive_samples.saturating_add(1);
    let cooldown_elapsed = state
        .last_notification_at
        .is_none_or(|last| now.saturating_sub(last) >= policy.notification_cooldown);
    if state.consecutive_samples >= policy.consecutive_samples && cooldown_elapsed {
        state.last_notification_at = Some(now);
        true
    } else {
        false
    }
}

/// Builds the full inventory for a snapshot.
#[must_use]
pub fn detect_workloads(
    processes: &[ObservedProcess],
    current_uid: u32,
    policy: &StaleWorkloadPolicy,
) -> StaleWorkloadDetection {
    if !policy.enabled {
        return StaleWorkloadDetection::default();
    }
    let eligible = discover_eligible_trees(processes, current_uid, policy);
    assemble_detection(&eligible, policy)
}

/// Discovers trees that satisfy every per-tree eligibility rule except the
/// individual memory threshold.
fn discover_eligible_trees(
    processes: &[ObservedProcess],
    current_uid: u32,
    policy: &StaleWorkloadPolicy,
) -> Vec<StaleWorkload> {
    let by_pid = by_pid(processes);
    let mut roots = HashSet::new();
    for process in processes {
        let identity = process.descriptor.identity();
        if identity.uid() != current_uid
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

    roots
        .into_iter()
        .filter_map(|root_identity| build_workload(processes, &by_pid, root_identity, current_uid))
        .filter(|workload| {
            workload.total_cpu_percent <= policy.maximum_cpu_percent
                && !policy.ignored_root_names.contains(workload.root.name())
        })
        .collect()
}

fn select_direct_workloads(
    eligible: &[StaleWorkload],
    policy: &StaleWorkloadPolicy,
) -> Vec<StaleWorkload> {
    let mut direct = eligible
        .iter()
        .filter(|workload| workload.total_memory_bytes >= policy.minimum_tree_memory_bytes)
        .cloned()
        .collect::<Vec<_>>();
    sort_workloads(&mut direct);
    direct
}

fn group_eligible_workloads(
    eligible: &[StaleWorkload],
    policy: &StaleWorkloadPolicy,
) -> Vec<StaleWorkloadGroup> {
    let mut by_directory: BTreeMap<PathBuf, Vec<StaleWorkload>> = BTreeMap::new();
    for workload in eligible {
        let Some(working_directory) = workload.root.working_directory() else {
            continue;
        };
        if !working_directory.is_absolute() {
            continue;
        }
        by_directory
            .entry(working_directory.to_path_buf())
            .or_default()
            .push(workload.clone());
    }

    let mut groups = by_directory
        .into_iter()
        .filter_map(|(working_directory, mut workloads)| {
            if workloads.len() < policy.minimum_group_trees {
                return None;
            }
            sort_workloads(&mut workloads);
            let total_memory_bytes = workloads.iter().fold(0_u64, |total, workload| {
                total.saturating_add(workload.total_memory_bytes)
            });
            if total_memory_bytes < policy.minimum_group_memory_bytes {
                return None;
            }
            let total_cpu_percent = workloads
                .iter()
                .map(|workload| workload.total_cpu_percent)
                .sum();
            let age = workloads
                .iter()
                .map(|workload| workload.age)
                .max()
                .unwrap_or_default();
            Some(StaleWorkloadGroup {
                working_directory,
                workloads,
                total_memory_bytes,
                total_cpu_percent,
                age,
            })
        })
        .collect::<Vec<_>>();
    groups.sort_by(|left, right| {
        right
            .total_memory_bytes
            .cmp(&left.total_memory_bytes)
            .then_with(|| left.working_directory.cmp(&right.working_directory))
    });
    groups
}

fn assemble_detection(
    eligible: &[StaleWorkload],
    policy: &StaleWorkloadPolicy,
) -> StaleWorkloadDetection {
    let direct = select_direct_workloads(eligible, policy);
    let groups = group_eligible_workloads(eligible, policy);
    let direct_root_identities = direct
        .iter()
        .map(StaleWorkload::identity)
        .collect::<HashSet<_>>();

    let mut seen = HashSet::new();
    let mut workloads = Vec::new();
    for workload in direct.into_iter().chain(
        groups
            .iter()
            .flat_map(|group| group.workloads.iter().cloned()),
    ) {
        if seen.insert(workload.identity()) {
            workloads.push(workload);
        }
    }
    sort_workloads(&mut workloads);

    StaleWorkloadDetection {
        workloads,
        groups,
        direct_root_identities,
    }
}

#[must_use]
pub fn workload_from_root(
    processes: &[ObservedProcess],
    root_identity: ProcessIdentity,
    current_uid: u32,
) -> Option<StaleWorkload> {
    let by_pid = by_pid(processes);
    build_workload(processes, &by_pid, root_identity, current_uid)
}

fn build_workload(
    processes: &[ObservedProcess],
    by_pid: &HashMap<u32, &ObservedProcess>,
    root_identity: ProcessIdentity,
    current_uid: u32,
) -> Option<StaleWorkload> {
    let root = by_pid.get(&root_identity.pid()).copied()?;
    if root.descriptor.identity() != root_identity || root_identity.uid() != current_uid {
        return None;
    }
    let members = processes
        .iter()
        .filter(|process| process.descriptor.identity().uid() == current_uid)
        .filter_map(|process| {
            descendant_depth(process, root_identity.pid(), by_pid).map(|depth| WorkloadMember {
                process: process.descriptor.clone(),
                resources: process.resources,
                depth,
            })
        })
        .collect::<Vec<_>>();
    if members
        .iter()
        .any(|member| !member.resources.cpu_percent.is_finite())
    {
        return None;
    }
    let total_memory_bytes = members.iter().fold(0_u64, |total, member| {
        total.saturating_add(member.resources.resident_memory_bytes)
    });
    let total_cpu_percent: f32 = members
        .iter()
        .map(|member| member.resources.cpu_percent)
        .sum();
    if !total_cpu_percent.is_finite() {
        return None;
    }
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

fn by_pid(processes: &[ObservedProcess]) -> HashMap<u32, &ObservedProcess> {
    processes
        .iter()
        .map(|process| (process.descriptor.identity().pid(), process))
        .collect()
}

fn sort_workloads(workloads: &mut [StaleWorkload]) {
    workloads.sort_by(|left, right| {
        Reverse(left.total_memory_bytes)
            .cmp(&Reverse(right.total_memory_bytes))
            .then_with(|| left.identity().pid().cmp(&right.identity().pid()))
    });
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
    use std::{collections::HashSet, path::PathBuf, time::Duration};

    use super::{StaleWorkloadService, detect_workloads};
    use crate::{
        application::ObservedProcess,
        domain::{
            MemoryPressureLevel, ProcessDescriptor, ProcessIdentity, ProcessResources,
            ProcessState, StaleWorkloadPolicy,
        },
    };

    const UID: u32 = 1_000;
    const MIB: u64 = 1_048_576;

    fn process(pid: u32, parent: Option<u32>, name: &str, memory: u64) -> ObservedProcess {
        process_with_cwd(pid, parent, name, memory, None, Duration::from_hours(2))
    }

    fn process_with_cwd(
        pid: u32,
        parent: Option<u32>,
        name: &str,
        memory: u64,
        working_directory: Option<&str>,
        running_for: Duration,
    ) -> ObservedProcess {
        ObservedProcess {
            descriptor: ProcessDescriptor::new(
                ProcessIdentity::new(pid, UID, u64::from(pid)),
                name,
                None,
            )
            .with_runtime(parent, ProcessState::Sleeping)
            .with_working_directory(working_directory.map(PathBuf::from)),
            resources: ProcessResources {
                cpu_percent: 0.1,
                resident_memory_bytes: memory,
                virtual_memory_bytes: memory,
                running_for,
                observed_at: Duration::ZERO,
            },
        }
    }

    fn policy() -> StaleWorkloadPolicy {
        StaleWorkloadPolicy {
            enabled: true,
            notify_only_under_memory_pressure: true,
            candidate_names: HashSet::from(["pytest".to_owned()]),
            launcher_names: HashSet::from(["uv".to_owned(), "pytest".to_owned()]),
            ignored_root_names: HashSet::new(),
            minimum_age: Duration::from_hours(1),
            minimum_tree_memory_bytes: 256 * MIB,
            minimum_group_memory_bytes: 512 * MIB,
            minimum_group_trees: 2,
            maximum_cpu_percent: 5.0,
            consecutive_samples: 2,
            notification_cooldown: Duration::from_secs(60),
        }
    }

    fn tree() -> Vec<ObservedProcess> {
        vec![
            process(9, None, "bash", 10),
            process(8, Some(9), "xargs", 10),
            process(10, Some(8), "uv", 40 * MIB),
            process(11, Some(10), "pytest", 140 * MIB),
            process(12, Some(11), "pytest", 100 * MIB),
        ]
    }

    /// A `uv -> pytest` tree rooted at `uv` with the given working directory.
    fn tree_with_cwd(
        uv_pid: u32,
        cwd: Option<&str>,
        uv_memory: u64,
        pytest_memory: u64,
    ) -> [ObservedProcess; 2] {
        [
            process_with_cwd(uv_pid, None, "uv", uv_memory, cwd, Duration::from_hours(2)),
            process_with_cwd(
                uv_pid + 1,
                Some(uv_pid),
                "pytest",
                pytest_memory,
                None,
                Duration::from_hours(2),
            ),
        ]
    }

    #[test]
    fn detects_one_tree_and_orders_children_before_root() {
        let detection = detect_workloads(&tree(), UID, &policy());

        assert_eq!(detection.workloads.len(), 1);
        assert_eq!(detection.groups.len(), 0);
        assert_eq!(detection.workloads[0].identity().pid(), 10);
        assert_eq!(detection.workloads[0].total_memory_bytes, 280 * MIB);
        assert_eq!(detection.workloads[0].process_count(), 3);
        assert_eq!(
            detection.workloads[0]
                .termination_order()
                .into_iter()
                .map(ProcessIdentity::pid)
                .collect::<Vec<_>>(),
            vec![12, 11, 10]
        );
    }

    #[test]
    fn normal_pressure_still_builds_inventory_but_no_group_is_too_small() {
        let detection = detect_workloads(&tree(), UID, &policy());

        assert_eq!(detection.workloads.len(), 1);
        assert!(detection.groups.is_empty());
    }

    #[test]
    fn ignored_roots_are_not_candidates() {
        let mut ignored = policy();
        ignored.ignored_root_names.insert("uv".to_owned());

        assert!(
            detect_workloads(&tree(), UID, &ignored)
                .workloads
                .is_empty()
        );
    }

    #[test]
    fn requires_repeated_samples_and_honours_temporary_ignore() {
        let mut service = StaleWorkloadService::new(UID, policy());
        let first = service.evaluate(&tree(), MemoryPressureLevel::Warning, Duration::ZERO);
        let second = service.evaluate(
            &tree(),
            MemoryPressureLevel::Warning,
            Duration::from_secs(5),
        );
        assert!(first.direct_notifications.is_empty());
        assert_eq!(second.direct_notifications.len(), 1);

        service.ignore_for(
            second.direct_notifications[0].identity(),
            Duration::from_secs(3_605),
        );
        let ignored = service.evaluate(
            &tree(),
            MemoryPressureLevel::Warning,
            Duration::from_secs(10),
        );
        assert!(ignored.detection.workloads.is_empty());
        assert!(ignored.direct_notifications.is_empty());
    }

    #[test]
    fn disabled_detection_returns_an_empty_inventory() {
        let mut disabled = policy();
        disabled.enabled = false;
        let mut service = StaleWorkloadService::new(UID, disabled);

        let evaluation = service.evaluate(&tree(), MemoryPressureLevel::Warning, Duration::ZERO);

        assert!(evaluation.detection.workloads.is_empty());
        assert!(evaluation.detection.groups.is_empty());
    }

    #[test]
    fn normal_pressure_has_inventory_without_notifications() {
        let mut service = StaleWorkloadService::new(UID, policy());
        let evaluation = service.evaluate(&tree(), MemoryPressureLevel::Normal, Duration::ZERO);

        assert_eq!(evaluation.detection.workloads.len(), 1);
        assert!(evaluation.direct_notifications.is_empty());
        assert!(evaluation.group_notifications.is_empty());
    }

    #[test]
    fn main_regression_aggregates_seventy_three_small_pytest_trees() {
        let mut processes = Vec::new();
        let projects = [
            "/work/alpha",
            "/work/beta",
            "/work/gamma",
            "/work/delta",
            "/work/epsilon",
        ];
        for index in 0..73_u32 {
            let uv_pid = 10_000 + index * 2;
            let cwd = projects[(index as usize) % projects.len()];
            processes.extend(tree_with_cwd(uv_pid, Some(cwd), 4 * MIB, 36 * MIB));
        }

        let detection = detect_workloads(&processes, UID, &policy());

        assert!(
            detection.workloads.len() == 73,
            "all promoted roots are present, got {}",
            detection.workloads.len()
        );
        assert!(detection.direct_root_identities.is_empty());
        assert_eq!(detection.groups.len(), 5);
        for group in &detection.groups {
            assert!(group.tree_count() >= 2);
            assert!(group.total_memory_bytes >= 512 * MIB);
            assert!(
                projects
                    .iter()
                    .any(|project| group.working_directory == PathBuf::from(project)),
                "unexpected group directory {}",
                group.working_directory.display()
            );
        }

        let mut service = StaleWorkloadService::new(UID, policy());
        let evaluation = service.evaluate(&processes, MemoryPressureLevel::Normal, Duration::ZERO);
        assert_eq!(evaluation.detection.groups.len(), 5);
        assert!(evaluation.direct_notifications.is_empty());
        assert!(evaluation.group_notifications.is_empty());
    }

    #[test]
    fn main_regression_notifies_one_group_under_warning() {
        let projects = ["/work/alpha", "/work/beta"];
        let mut processes = Vec::new();
        for index in 0..4_u32 {
            let uv_pid = 20_000 + index * 2;
            processes.extend(tree_with_cwd(
                uv_pid,
                Some(projects[(index as usize) % projects.len()]),
                4 * MIB,
                300 * MIB,
            ));
        }

        let mut service = StaleWorkloadService::new(UID, policy());
        let _ = service.evaluate(&processes, MemoryPressureLevel::Warning, Duration::ZERO);
        let evaluation = service.evaluate(
            &processes,
            MemoryPressureLevel::Warning,
            Duration::from_secs(5),
        );

        assert_eq!(evaluation.group_notifications.len(), 2);
        assert!(evaluation.direct_notifications.is_empty());
    }

    #[test]
    fn individual_threshold_boundary_is_inclusive() {
        let at_threshold = tree_with_cwd(100, Some("/work/alpha"), 0, 256 * MIB).to_vec();
        let detection = detect_workloads(&at_threshold, UID, &policy());
        assert_eq!(detection.workloads.len(), 1);
        assert!(
            detection
                .direct_root_identities
                .contains(&detection.workloads[0].identity())
        );

        let just_below = tree_with_cwd(100, Some("/work/alpha"), 0, 256 * MIB - 1).to_vec();
        let detection = detect_workloads(&just_below, UID, &policy());
        assert!(detection.workloads.is_empty());
    }

    #[test]
    fn group_threshold_boundary_is_inclusive() {
        let mut at_threshold = tree_with_cwd(100, Some("/work/alpha"), 0, 256 * MIB).to_vec();
        at_threshold.extend(tree_with_cwd(110, Some("/work/alpha"), 0, 256 * MIB));
        let detection = detect_workloads(&at_threshold, UID, &policy());
        assert_eq!(detection.groups.len(), 1);
        assert_eq!(detection.groups[0].total_memory_bytes, 512 * MIB);

        let mut just_below = tree_with_cwd(100, Some("/work/alpha"), 0, 256 * MIB).to_vec();
        just_below.extend(tree_with_cwd(110, Some("/work/alpha"), 0, 256 * MIB - 1));
        let detection = detect_workloads(&just_below, UID, &policy());
        assert!(detection.groups.is_empty());
    }

    #[test]
    fn a_single_large_tree_is_direct_but_not_a_group() {
        let processes = tree_with_cwd(100, Some("/work/alpha"), 0, 600 * MIB).to_vec();
        let detection = detect_workloads(&processes, UID, &policy());

        assert_eq!(detection.workloads.len(), 1);
        assert!(detection.groups.is_empty());
        assert!(
            detection
                .direct_root_identities
                .contains(&detection.workloads[0].identity())
        );
    }

    #[test]
    fn different_exact_working_directories_do_not_merge() {
        let mut processes = tree_with_cwd(100, Some("/work/SKUart"), 0, 300 * MIB).to_vec();
        processes.extend(tree_with_cwd(
            110,
            Some("/work/SKUart/services/ai"),
            0,
            300 * MIB,
        ));

        let detection = detect_workloads(&processes, UID, &policy());

        assert!(detection.groups.is_empty());
        assert_eq!(detection.workloads.len(), 2);
    }

    #[test]
    fn unknown_working_directory_is_never_grouped() {
        let mut processes = tree_with_cwd(100, None, 0, 300 * MIB).to_vec();
        processes.extend(tree_with_cwd(110, None, 0, 300 * MIB));

        let detection = detect_workloads(&processes, UID, &policy());

        assert!(detection.groups.is_empty());
        assert_eq!(detection.workloads.len(), 2);
    }

    #[test]
    fn lexical_paths_are_not_canonicalized() {
        let mut processes = tree_with_cwd(100, Some("/work/alpha/../beta"), 0, 300 * MIB).to_vec();
        processes.extend(tree_with_cwd(110, Some("/work/beta"), 0, 300 * MIB));

        let detection = detect_workloads(&processes, UID, &policy());

        assert!(detection.groups.is_empty());
    }

    #[test]
    fn foreign_young_high_cpu_and_ignored_roots_are_excluded() {
        let foreign = ObservedProcess {
            descriptor: ProcessDescriptor::new(ProcessIdentity::new(200, UID + 1, 200), "uv", None)
                .with_runtime(None, ProcessState::Sleeping)
                .with_working_directory(Some(PathBuf::from("/work/alpha"))),
            resources: ProcessResources {
                cpu_percent: 0.0,
                resident_memory_bytes: 900 * MIB,
                virtual_memory_bytes: 900 * MIB,
                running_for: Duration::from_hours(2),
                observed_at: Duration::ZERO,
            },
        };
        let young = {
            let mut processes = tree_with_cwd(210, Some("/work/alpha"), 0, 600 * MIB).to_vec();
            processes[1].resources.running_for = Duration::from_mins(5);
            processes
        };
        let high_cpu = {
            let mut processes = tree_with_cwd(220, Some("/work/alpha"), 0, 600 * MIB).to_vec();
            processes[1].resources.cpu_percent = 50.0;
            processes
        };
        let mut ignored = policy();
        ignored.ignored_root_names.insert("uv".to_owned());

        let mut processes = vec![foreign];
        processes.extend(young);
        processes.extend(high_cpu);
        let detection = detect_workloads(&processes, UID, &policy());
        assert!(detection.workloads.is_empty());
        assert!(detection.groups.is_empty());

        let ignored_tree = tree_with_cwd(300, Some("/work/alpha"), 0, 600 * MIB).to_vec();
        assert!(
            detect_workloads(&ignored_tree, UID, &ignored)
                .workloads
                .is_empty()
        );
    }

    #[test]
    fn traversal_does_not_cross_generic_launchers() {
        let processes = vec![
            process_with_cwd(
                1,
                None,
                "bash",
                10,
                Some("/work/alpha"),
                Duration::from_hours(2),
            ),
            process_with_cwd(
                2,
                Some(1),
                "xargs",
                10,
                Some("/work/alpha"),
                Duration::from_hours(2),
            ),
            process_with_cwd(
                3,
                Some(2),
                "python3",
                10,
                Some("/work/alpha"),
                Duration::from_hours(2),
            ),
            process_with_cwd(
                4,
                Some(3),
                "uv",
                10,
                Some("/work/alpha"),
                Duration::from_hours(2),
            ),
            process_with_cwd(
                5,
                Some(4),
                "pytest",
                600 * MIB,
                None,
                Duration::from_hours(2),
            ),
        ];

        let detection = detect_workloads(&processes, UID, &policy());

        assert_eq!(detection.workloads.len(), 1);
        assert_eq!(detection.workloads[0].identity().pid(), 4);
    }

    #[test]
    fn a_process_belongs_to_exactly_one_tree() {
        let processes = vec![
            process_with_cwd(
                10,
                None,
                "uv",
                4 * MIB,
                Some("/work/alpha"),
                Duration::from_hours(2),
            ),
            process_with_cwd(
                11,
                Some(10),
                "pytest",
                4 * MIB,
                None,
                Duration::from_hours(2),
            ),
            process_with_cwd(
                20,
                None,
                "uv",
                4 * MIB,
                Some("/work/beta"),
                Duration::from_hours(2),
            ),
            process_with_cwd(
                21,
                Some(20),
                "pytest",
                4 * MIB,
                None,
                Duration::from_hours(2),
            ),
        ];
        let detection = detect_workloads(&processes, UID, &policy());

        let mut seen = HashSet::new();
        for workload in &detection.workloads {
            for member in &workload.members {
                assert!(
                    seen.insert(member.process.identity().pid()),
                    "process {} appeared in two trees",
                    member.process.identity().pid()
                );
            }
        }
    }

    #[test]
    fn missing_working_directory_keeps_individual_detection_working() {
        let processes = tree_with_cwd(100, None, 0, 600 * MIB).to_vec();
        let detection = detect_workloads(&processes, UID, &policy());

        assert_eq!(detection.workloads.len(), 1);
        assert!(detection.groups.is_empty());
    }

    #[test]
    fn reconstruction_of_one_root_excludes_a_sibling_tree_with_the_same_cwd() {
        let mut processes = tree_with_cwd(100, Some("/work/alpha"), 4 * MIB, 300 * MIB).to_vec();
        processes.extend(tree_with_cwd(110, Some("/work/alpha"), 4 * MIB, 300 * MIB));
        let detection = detect_workloads(&processes, UID, &policy());
        assert_eq!(detection.groups.len(), 1);
        assert_eq!(detection.groups[0].tree_count(), 2);

        let selected = detection.groups[0].workloads[0].identity();
        let reconstructed =
            super::workload_from_root(&processes, selected, UID).expect("root is reconstructable");

        let pids = reconstructed
            .termination_order()
            .into_iter()
            .map(ProcessIdentity::pid)
            .collect::<Vec<_>>();
        assert!(pids.contains(&100) || pids.contains(&110));
        assert!(
            !(pids.contains(&100) && pids.contains(&110)),
            "only the selected tree is reconstructed: {pids:?}"
        );
    }

    #[test]
    fn group_notifications_do_not_repeat_and_clear_when_gone() {
        let mut processes = Vec::new();
        for index in 0..2_u32 {
            processes.extend(tree_with_cwd(
                30_000 + index * 2,
                Some("/work/alpha"),
                4 * MIB,
                300 * MIB,
            ));
        }

        let mut service = StaleWorkloadService::new(UID, policy());
        let _ = service.evaluate(&processes, MemoryPressureLevel::Warning, Duration::ZERO);
        let second = service.evaluate(
            &processes,
            MemoryPressureLevel::Warning,
            Duration::from_secs(5),
        );
        assert_eq!(second.group_notifications.len(), 1);

        let third = service.evaluate(
            &processes,
            MemoryPressureLevel::Warning,
            Duration::from_secs(10),
        );
        assert!(third.group_notifications.is_empty());

        let gone = service.evaluate(&[], MemoryPressureLevel::Warning, Duration::from_secs(15));
        assert!(gone.detection.groups.is_empty());
        assert!(gone.group_notifications.is_empty());
    }

    #[test]
    fn non_finite_cpu_excludes_the_tree_from_candidate_output() {
        for cpu in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let mut processes = tree_with_cwd(100, Some("/work/alpha"), 0, 600 * MIB).to_vec();
            processes[1].resources.cpu_percent = cpu;

            let detection = detect_workloads(&processes, UID, &policy());
            assert!(detection.workloads.is_empty(), "cpu {cpu}");
            assert!(detection.groups.is_empty(), "cpu {cpu}");

            let mut service = StaleWorkloadService::new(UID, policy());
            let evaluation =
                service.evaluate(&processes, MemoryPressureLevel::Warning, Duration::ZERO);
            assert!(evaluation.detection.workloads.is_empty(), "cpu {cpu}");
            assert!(evaluation.direct_notifications.is_empty(), "cpu {cpu}");
            assert!(evaluation.group_notifications.is_empty(), "cpu {cpu}");
        }
    }

    #[test]
    fn normal_pressure_resets_the_direct_consecutive_sample_streak() {
        let mut service = StaleWorkloadService::new(UID, policy());
        let _ = service.evaluate(&tree(), MemoryPressureLevel::Warning, Duration::ZERO);
        let notified = service.evaluate(
            &tree(),
            MemoryPressureLevel::Warning,
            Duration::from_secs(5),
        );
        assert_eq!(notified.direct_notifications.len(), 1);

        let normal = service.evaluate(
            &tree(),
            MemoryPressureLevel::Normal,
            Duration::from_secs(100),
        );
        assert!(normal.direct_notifications.is_empty());

        let first_warning = service.evaluate(
            &tree(),
            MemoryPressureLevel::Warning,
            Duration::from_secs(105),
        );
        assert!(
            first_warning.direct_notifications.is_empty(),
            "the streak must restart after Normal"
        );

        let second_warning = service.evaluate(
            &tree(),
            MemoryPressureLevel::Warning,
            Duration::from_secs(110),
        );
        assert_eq!(second_warning.direct_notifications.len(), 1);
    }

    #[test]
    fn normal_pressure_resets_the_group_consecutive_sample_streak() {
        let mut processes = Vec::new();
        for index in 0..2_u32 {
            processes.extend(tree_with_cwd(
                40_000 + index * 2,
                Some("/work/alpha"),
                4 * MIB,
                300 * MIB,
            ));
        }

        let mut service = StaleWorkloadService::new(UID, policy());
        let _ = service.evaluate(&processes, MemoryPressureLevel::Warning, Duration::ZERO);
        let notified = service.evaluate(
            &processes,
            MemoryPressureLevel::Warning,
            Duration::from_secs(5),
        );
        assert_eq!(notified.group_notifications.len(), 1);

        let normal = service.evaluate(
            &processes,
            MemoryPressureLevel::Normal,
            Duration::from_secs(100),
        );
        assert!(normal.group_notifications.is_empty());

        let first_warning = service.evaluate(
            &processes,
            MemoryPressureLevel::Warning,
            Duration::from_secs(105),
        );
        assert!(
            first_warning.group_notifications.is_empty(),
            "the group streak must restart after Normal"
        );

        let second_warning = service.evaluate(
            &processes,
            MemoryPressureLevel::Warning,
            Duration::from_secs(110),
        );
        assert_eq!(second_warning.group_notifications.len(), 1);
    }
}
