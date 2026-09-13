use std::{
    collections::{HashMap, VecDeque},
    fmt::Write as _,
    future::Future,
};

use crate::domain::{
    BackgroundWorkload, MemoryPressureEvaluation, StaleWorkload, StaleWorkloadGroup,
};

use super::{MonitorEvent, PortError};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NotificationAction {
    Stop,
    StopGroup,
    IgnoreForHour,
    AlwaysIgnore,
    Details,
    Back,
}

impl NotificationAction {
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "stop" => Some(Self::Stop),
            "stop_group" => Some(Self::StopGroup),
            "ignore_hour" => Some(Self::IgnoreForHour),
            "always_ignore" => Some(Self::AlwaysIgnore),
            "details" | "default" => Some(Self::Details),
            "back" => Some(Self::Back),
            _ => None,
        }
    }
}

/// Explicit set of notification actions rendered for one view.
///
/// The request owns the profile so the adapter never has to infer destructive
/// availability from the view alone.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NotificationActionSet {
    None,
    StandardSummary,
    DetailsOnly,
    BackOnly,
    GroupDetails,
}

/// Decision for one incoming notification action, before any side effect.
#[derive(Clone, Debug, PartialEq)]
pub enum NotificationDispatch {
    /// The notification ID is closed, evicted, or otherwise unknown.
    Unknown,
    /// Replace the current notification with the next view.
    Navigate(Box<NotificationBinding>),
    /// A permitted non-navigation action.
    Execute(NotificationAction),
    /// The action is not valid for the current subject or view.
    Rejected,
}

/// Resolves an action against a binding, if any, without performing side effects.
#[must_use]
pub fn plan_notification(
    binding: Option<&NotificationBinding>,
    action: NotificationAction,
) -> NotificationDispatch {
    binding.map_or(NotificationDispatch::Unknown, |binding| {
        binding.dispatch(action)
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NotificationView {
    Summary,
    Details,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NotificationEvent {
    Action {
        notification_id: u32,
        action: NotificationAction,
    },
    Closed {
        notification_id: u32,
        reason: NotificationCloseReason,
    },
    UnknownAction {
        notification_id: u32,
        key: String,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NotificationCloseReason {
    Expired,
    DismissedByUser,
    ClosedBySender,
    Undefined(u32),
}

impl NotificationCloseReason {
    #[must_use]
    pub const fn from_code(code: u32) -> Self {
        match code {
            1 => Self::Expired,
            2 => Self::DismissedByUser,
            3 => Self::ClosedBySender,
            other => Self::Undefined(other),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct NotificationRequest {
    summary: String,
    body: String,
    action_set: NotificationActionSet,
    pub view: NotificationView,
}

impl NotificationRequest {
    #[must_use]
    pub fn from_event(event: &MonitorEvent) -> Self {
        Self::for_view(event, NotificationView::Summary)
    }

    #[must_use]
    pub fn details(event: &MonitorEvent) -> Self {
        Self::for_view(event, NotificationView::Details)
    }

    #[must_use]
    pub fn for_view(event: &MonitorEvent, view: NotificationView) -> Self {
        let reason = match (event.breach.cpu, event.breach.memory) {
            (true, true) => "CPU and RAM",
            (true, false) => "CPU",
            (false, true) => "RAM",
            (false, false) => "configured resource",
        };
        let mut body = format!(
            "CPU: {:.1}%\nRAM: {} MiB\nExceeded for: {}s\nReason: {reason}",
            event.resources.cpu_percent,
            event.resources.resident_memory_bytes / 1_048_576,
            event.exceeded_for.as_secs(),
        );
        if view == NotificationView::Details {
            let executable = event
                .process
                .executable()
                .map_or_else(|| "unknown".to_owned(), |path| path.display().to_string());
            let executable = escape_markup(&executable);
            let _ = write!(
                body,
                "\nExecutable: {executable}\nVirtual memory: {} MiB\nRuntime: {}s",
                event.resources.virtual_memory_bytes / 1_048_576,
                event.resources.running_for.as_secs(),
            );
        }
        Self {
            summary: format!(
                "Resource limit exceeded: {} ({})",
                event.process.name(),
                event.process.identity().pid()
            ),
            body,
            action_set: summary_details_action_set(view),
            view,
        }
    }

    #[must_use]
    pub fn for_pressure(
        evaluation: MemoryPressureEvaluation,
        outcome: Option<&str>,
        automatic_action_permitted: bool,
        action_available_bytes: u64,
        action_psi_full_avg10: f32,
    ) -> Self {
        let sample = evaluation.sample;
        let mut body = format!(
            "Available RAM: {} MiB ({:.1}%)\nSwap used: {:.1}%\nPSI some/full avg10: {:.2}% / {:.2}%\nReason: {}\nAutomatic action: {}\nAction threshold: {} MiB or critical RAM with PSI full avg10 >= {:.2}%",
            sample.system.available_memory_bytes / 1_048_576,
            sample.available_percent(),
            sample.swap_used_percent(),
            sample.psi.some_avg10,
            sample.psi.full_avg10,
            evaluation.reason(),
            if automatic_action_permitted {
                "permitted"
            } else {
                "blocked"
            },
            action_available_bytes / 1_048_576,
            action_psi_full_avg10,
        );
        if let Some(outcome) = outcome {
            let _ = write!(body, "\nAction: {}", escape_markup(outcome));
        }
        Self {
            summary: format!("System memory pressure: {:?}", evaluation.current),
            body,
            action_set: NotificationActionSet::None,
            view: NotificationView::Summary,
        }
    }

    #[must_use]
    pub fn for_stale_workload(workload: &StaleWorkload, view: NotificationView) -> Self {
        let mut body = format!(
            "Processes: {}\nTree RAM: {} MiB\nTree CPU: {:.1}%\nAge: {}s\nReason: long-lived low-CPU workload under memory pressure",
            workload.process_count(),
            workload.total_memory_bytes / 1_048_576,
            workload.total_cpu_percent,
            workload.age.as_secs(),
        );
        if view == NotificationView::Details {
            let executable = workload.root.executable().map_or_else(
                || "unknown".to_owned(),
                |path| escape_markup(&path.display().to_string()),
            );
            let _ = write!(
                body,
                "\nRoot PID: {}\nExecutable: {executable}\nStop affects only this workload tree; parent sessions are preserved",
                workload.identity().pid()
            );
        }
        Self {
            summary: format!(
                "Suspected stale workload: {} ({})",
                workload.root.name(),
                workload.identity().pid()
            ),
            body,
            action_set: summary_details_action_set(view),
            view,
        }
    }

    /// Builds the two-step aggregate stale workload group notification.
    ///
    /// The summary offers only `Подробнее`. The details clarify that the action
    /// targets every listed independent workload tree from the immutable group
    /// snapshot, sends `SIGTERM` only, and does not select parent terminal or
    /// session processes. Only the directory basename is shown; the full path
    /// stays in the explicit local CLI output.
    #[must_use]
    pub fn for_stale_workload_group(group: &StaleWorkloadGroup, view: NotificationView) -> Self {
        let directory = group.working_directory.file_name().map_or_else(
            || "unknown".to_owned(),
            |name| name.to_string_lossy().into_owned(),
        );
        let directory = escape_markup(&directory);
        let mut body = format!(
            "Project: {directory}\nTrees: {}\nProcesses: {}\nGroup RAM: {} MiB\nGroup CPU: {:.1}%\nAge: {}\nReason: multiple long-lived low-CPU workload trees from one project",
            group.tree_count(),
            group.process_count(),
            group.total_memory_bytes / 1_048_576,
            group.total_cpu_percent,
            format_duration(group.age),
        );
        let action_set = match view {
            NotificationView::Summary => NotificationActionSet::DetailsOnly,
            NotificationView::Details => {
                let root_pids = bounded_root_pid_list(group);
                let _ = write!(
                    body,
                    "\nThis action sends SIGTERM to all listed independent workload trees\nParent terminal/session processes are not selected automatically\nRoot PIDs: {root_pids}",
                );
                NotificationActionSet::GroupDetails
            }
        };
        Self {
            summary: format!(
                "Stale workload group in {directory}: {} trees, {} processes",
                group.tree_count(),
                group.process_count()
            ),
            body,
            action_set,
            view,
        }
    }

    #[must_use]
    pub fn for_background_workload(workload: &BackgroundWorkload, view: NotificationView) -> Self {
        let mut body = format!(
            "Processes: {} (+{})\nRAM: {} MiB (+{} MiB)\nCPU: {:.1}%\nRunning for: {}\nReason: long-lived low-CPU application with growing or high memory use",
            workload.process_count(),
            workload.process_count_growth,
            workload.total_memory_bytes / 1_048_576,
            workload.memory_growth_bytes / 1_048_576,
            workload.total_cpu_percent,
            format_duration(workload.age),
        );
        if view == NotificationView::Details {
            let executable = workload.root.executable().map_or_else(
                || "unknown".to_owned(),
                |path| escape_markup(&path.display().to_string()),
            );
            let _ = write!(
                body,
                "\nRoot PID: {}\nExecutable: {executable}\nObserved for: {}\nStop sends SIGTERM only to the reported application group",
                workload.identity().pid(),
                format_duration(workload.observed_for),
            );
        }
        Self {
            summary: format!(
                "Background application is retaining memory: {} ({})",
                workload.root.name(),
                workload.identity().pid()
            ),
            body,
            action_set: summary_details_action_set(view),
            view,
        }
    }

    #[must_use]
    pub const fn action_set(&self) -> NotificationActionSet {
        self.action_set
    }

    #[must_use]
    pub fn summary(&self) -> String {
        self.summary.clone()
    }

    #[must_use]
    pub fn body(&self) -> String {
        self.body.clone()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct NotificationBinding {
    subject: NotificationSubject,
    view: NotificationView,
}

#[derive(Clone, Debug, PartialEq)]
enum NotificationSubject {
    Process(MonitorEvent),
    Workload(StaleWorkload),
    StaleWorkloadGroup(StaleWorkloadGroup),
    BackgroundWorkload(BackgroundWorkload),
}

impl NotificationBinding {
    #[must_use]
    pub const fn new(event: MonitorEvent, view: NotificationView) -> Self {
        Self {
            subject: NotificationSubject::Process(event),
            view,
        }
    }

    #[must_use]
    pub const fn for_workload(workload: StaleWorkload, view: NotificationView) -> Self {
        Self {
            subject: NotificationSubject::Workload(workload),
            view,
        }
    }

    #[must_use]
    pub const fn for_stale_workload_group(
        group: StaleWorkloadGroup,
        view: NotificationView,
    ) -> Self {
        Self {
            subject: NotificationSubject::StaleWorkloadGroup(group),
            view,
        }
    }

    #[must_use]
    pub const fn for_background_workload(
        workload: BackgroundWorkload,
        view: NotificationView,
    ) -> Self {
        Self {
            subject: NotificationSubject::BackgroundWorkload(workload),
            view,
        }
    }

    #[must_use]
    pub const fn event(&self) -> Option<&MonitorEvent> {
        match &self.subject {
            NotificationSubject::Process(event) => Some(event),
            NotificationSubject::Workload(_)
            | NotificationSubject::StaleWorkloadGroup(_)
            | NotificationSubject::BackgroundWorkload(_) => None,
        }
    }

    #[must_use]
    pub const fn workload(&self) -> Option<&StaleWorkload> {
        match &self.subject {
            NotificationSubject::Workload(workload) => Some(workload),
            NotificationSubject::Process(_)
            | NotificationSubject::StaleWorkloadGroup(_)
            | NotificationSubject::BackgroundWorkload(_) => None,
        }
    }

    #[must_use]
    pub const fn stale_workload_group(&self) -> Option<&StaleWorkloadGroup> {
        match &self.subject {
            NotificationSubject::StaleWorkloadGroup(group) => Some(group),
            NotificationSubject::Process(_)
            | NotificationSubject::Workload(_)
            | NotificationSubject::BackgroundWorkload(_) => None,
        }
    }

    #[must_use]
    pub const fn background_workload(&self) -> Option<&BackgroundWorkload> {
        match &self.subject {
            NotificationSubject::BackgroundWorkload(workload) => Some(workload),
            NotificationSubject::Process(_)
            | NotificationSubject::Workload(_)
            | NotificationSubject::StaleWorkloadGroup(_) => None,
        }
    }

    #[must_use]
    pub const fn view(&self) -> NotificationView {
        self.view
    }

    #[must_use]
    pub fn request(&self) -> NotificationRequest {
        match &self.subject {
            NotificationSubject::Process(event) => NotificationRequest::for_view(event, self.view),
            NotificationSubject::Workload(workload) => {
                NotificationRequest::for_stale_workload(workload, self.view)
            }
            NotificationSubject::StaleWorkloadGroup(group) => {
                NotificationRequest::for_stale_workload_group(group, self.view)
            }
            NotificationSubject::BackgroundWorkload(workload) => {
                NotificationRequest::for_background_workload(workload, self.view)
            }
        }
    }

    #[must_use]
    pub fn transition(&self, action: NotificationAction) -> Option<Self> {
        let view = match (self.view, action) {
            (NotificationView::Summary, NotificationAction::Details) => NotificationView::Details,
            (NotificationView::Details, NotificationAction::Back) => NotificationView::Summary,
            _ => return None,
        };
        Some(Self {
            subject: self.subject.clone(),
            view,
        })
    }

    /// Classifies an action for this binding without performing any side effect.
    ///
    /// The destructive group stop is accepted only for a stale workload group
    /// binding currently in the details view, so a forged action on the summary
    /// or after navigating back is rejected. Single-tree stop and ignore actions
    /// are accepted only for non-group subjects.
    #[must_use]
    pub fn dispatch(&self, action: NotificationAction) -> NotificationDispatch {
        if let Some(next) = self.transition(action) {
            return NotificationDispatch::Navigate(Box::new(next));
        }
        let is_group = self.stale_workload_group().is_some();
        let permitted = match action {
            NotificationAction::Details | NotificationAction::Back => false,
            NotificationAction::StopGroup => is_group && self.view == NotificationView::Details,
            NotificationAction::Stop
            | NotificationAction::IgnoreForHour
            | NotificationAction::AlwaysIgnore => !is_group,
        };
        if permitted {
            NotificationDispatch::Execute(action)
        } else {
            NotificationDispatch::Rejected
        }
    }
}

#[derive(Debug)]
pub struct NotificationBindings {
    capacity: usize,
    bindings: HashMap<u32, NotificationBinding>,
    insertion_order: VecDeque<u32>,
}

impl NotificationBindings {
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            bindings: HashMap::new(),
            insertion_order: VecDeque::new(),
        }
    }

    pub fn remember(&mut self, notification_id: u32, binding: NotificationBinding) {
        self.remove(notification_id);
        while self.bindings.len() >= self.capacity {
            let Some(oldest_id) = self.insertion_order.pop_front() else {
                break;
            };
            self.bindings.remove(&oldest_id);
        }
        if self.capacity > 0 {
            self.bindings.insert(notification_id, binding);
            self.insertion_order.push_back(notification_id);
        }
    }

    #[must_use]
    pub fn get(&self, notification_id: u32) -> Option<&NotificationBinding> {
        self.bindings.get(&notification_id)
    }

    #[must_use]
    pub fn contains(&self, notification_id: u32) -> bool {
        self.bindings.contains_key(&notification_id)
    }

    pub fn remove(&mut self, notification_id: u32) -> Option<NotificationBinding> {
        if self.bindings.contains_key(&notification_id) {
            self.insertion_order
                .retain(|stored_id| *stored_id != notification_id);
        }
        self.bindings.remove(&notification_id)
    }

    pub fn clear(&mut self) {
        self.bindings.clear();
        self.insertion_order.clear();
    }
}

const MAX_NOTIFICATION_ROOT_PIDS: usize = 20;

const fn summary_details_action_set(view: NotificationView) -> NotificationActionSet {
    match view {
        NotificationView::Summary => NotificationActionSet::StandardSummary,
        NotificationView::Details => NotificationActionSet::BackOnly,
    }
}

fn bounded_root_pid_list(group: &StaleWorkloadGroup) -> String {
    let identities = group.root_identities();
    let shown = identities
        .iter()
        .take(MAX_NOTIFICATION_ROOT_PIDS)
        .map(|identity| identity.pid().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let remaining = identities.len().saturating_sub(MAX_NOTIFICATION_ROOT_PIDS);
    if remaining > 0 {
        format!("{shown} and {remaining} more")
    } else {
        shown
    }
}

fn escape_markup(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn format_duration(duration: std::time::Duration) -> String {
    let total_seconds = duration.as_secs();
    let days = total_seconds / 86_400;
    let hours = (total_seconds % 86_400) / 3_600;
    let minutes = (total_seconds % 3_600) / 60;
    if days > 0 {
        format!("{days}d {hours}h {minutes}m")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else if minutes > 0 {
        format!("{minutes}m")
    } else {
        format!("{total_seconds}s")
    }
}

pub trait NotificationSink {
    fn notify(
        &mut self,
        request: NotificationRequest,
        replaces_id: Option<u32>,
    ) -> impl Future<Output = Result<u32, PortError>> + Send;

    fn close(&mut self, notification_id: u32)
    -> impl Future<Output = Result<(), PortError>> + Send;
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, time::Duration};

    use super::{
        NotificationAction, NotificationActionSet, NotificationBinding, NotificationBindings,
        NotificationCloseReason, NotificationDispatch, NotificationRequest, NotificationSink,
        NotificationView, plan_notification,
    };
    use crate::{
        application::{MonitorEvent, PortError},
        domain::{
            BackgroundWorkload, MemoryPressureEvaluation, MemoryPressureLevel,
            MemoryPressureSample, MemoryPsi, ProcessDescriptor, ProcessIdentity, ProcessResources,
            ResourceBreach, StaleWorkload, StaleWorkloadGroup, SystemResources, WorkloadMember,
        },
    };

    fn event() -> MonitorEvent {
        MonitorEvent {
            process: ProcessDescriptor::new(
                ProcessIdentity::new(42, 1_000, 100),
                "worker",
                Some(PathBuf::from("/usr/bin/worker")),
            ),
            resources: ProcessResources {
                cpu_percent: 95.5,
                resident_memory_bytes: 256 * 1_048_576,
                virtual_memory_bytes: 512 * 1_048_576,
                running_for: Duration::from_secs(90),
                observed_at: Duration::ZERO,
            },
            breach: ResourceBreach {
                cpu: true,
                memory: false,
            },
            exceeded_for: Duration::from_secs(15),
        }
    }

    fn workload() -> StaleWorkload {
        let event = event();
        StaleWorkload {
            root: event.process.clone(),
            members: vec![WorkloadMember {
                process: event.process,
                resources: event.resources,
                depth: 0,
            }],
            total_memory_bytes: 256 * 1_048_576,
            total_cpu_percent: 0.2,
            age: Duration::from_hours(2),
        }
    }

    fn background_workload() -> BackgroundWorkload {
        let event = event();
        BackgroundWorkload {
            group_id: "systemd-unit:app-1.scope".to_owned(),
            root: event.process.clone(),
            members: vec![WorkloadMember {
                process: event.process,
                resources: event.resources,
                depth: 0,
            }],
            total_memory_bytes: 256 * 1_048_576,
            total_cpu_percent: 1.2,
            age: Duration::from_mins(2 * 24 * 60 + 3 * 60 + 14),
            observed_for: Duration::from_mins(30),
            memory_growth_bytes: 128 * 1_048_576,
            process_count_growth: 2,
        }
    }

    #[test]
    fn builds_a_safe_human_readable_message() {
        let request = NotificationRequest::from_event(&event());

        assert!(request.summary().contains("worker (42)"));
        assert!(request.body().contains("CPU: 95.5%"));
        assert!(request.body().contains("RAM: 256 MiB"));
        assert!(request.body().contains("Reason: CPU"));
    }

    #[test]
    fn details_include_executable_and_runtime() {
        let request = NotificationRequest::details(&event());

        assert!(request.body().contains("Executable: /usr/bin/worker"));
        assert!(request.body().contains("Runtime: 90s"));
    }

    #[test]
    fn stale_workload_notification_keeps_tree_identity_across_navigation() {
        let summary = NotificationBinding::for_workload(workload(), NotificationView::Summary);
        assert!(
            summary
                .request()
                .summary()
                .contains("Suspected stale workload")
        );
        let details = summary.transition(NotificationAction::Details).unwrap();
        assert_eq!(details.workload().unwrap().identity().pid(), 42);
        assert!(
            details
                .request()
                .body()
                .contains("parent sessions are preserved")
        );
        assert!(details.transition(NotificationAction::Back).is_some());
    }

    #[test]
    fn escapes_markup_in_detailed_executable_paths() {
        let mut event = event();
        event.process = ProcessDescriptor::new(
            event.process.identity(),
            "worker",
            Some(PathBuf::from("/tmp/<worker&helper>")),
        );

        let body = NotificationRequest::details(&event).body();

        assert!(body.contains("/tmp/&lt;worker&amp;helper&gt;"));
    }

    #[test]
    fn builds_a_system_pressure_message_without_process_actions() {
        let request = NotificationRequest::for_pressure(
            MemoryPressureEvaluation {
                previous: MemoryPressureLevel::Warning,
                current: MemoryPressureLevel::Critical,
                sample: MemoryPressureSample {
                    system: SystemResources {
                        total_memory_bytes: 16 * 1_024 * 1_024,
                        available_memory_bytes: 1_024 * 1_024,
                        total_swap_bytes: 4 * 1_024 * 1_024,
                        used_swap_bytes: 3 * 1_024 * 1_024,
                    },
                    psi: MemoryPsi {
                        some_avg10: 12.0,
                        full_avg10: 5.0,
                    },
                },
                signals: crate::domain::MemoryPressureSignals {
                    available_warning: true,
                    available_critical: true,
                    available_recovered: false,
                    swap_critical: false,
                    psi_critical: true,
                    emergency_floor: true,
                },
            },
            Some("SIGTERM sent to worker (42)"),
            true,
            1_024 * 1_024,
            5.0,
        );

        assert!(request.summary().contains("Critical"));
        assert!(request.body().contains("Action: SIGTERM sent"));
        assert!(request.body().contains("Automatic action: permitted"));
        assert_eq!(request.action_set(), NotificationActionSet::None);
    }

    #[test]
    fn background_workload_notification_contains_name_pid_memory_and_growth() {
        let request = NotificationRequest::for_background_workload(
            &background_workload(),
            NotificationView::Summary,
        );

        assert!(
            request
                .summary()
                .contains("Background application is retaining memory: worker (42)")
        );
        assert!(request.body().contains("Processes: 1 (+2)"));
        assert!(request.body().contains("RAM: 256 MiB (+128 MiB)"));
        assert!(request.body().contains("CPU: 1.2%"));
        assert!(request.body().contains("Running for: 2d 3h 14m"));
        assert_eq!(request.action_set(), NotificationActionSet::StandardSummary);
    }

    #[test]
    fn background_details_include_escaped_executable_and_durations() {
        let request = NotificationRequest::for_background_workload(
            &background_workload(),
            NotificationView::Details,
        );

        assert!(request.body().contains("Root PID: 42"));
        assert!(request.body().contains("Executable: /usr/bin/worker"));
        assert!(request.body().contains("Observed for: 30m"));
        assert!(
            request
                .body()
                .contains("Stop sends SIGTERM only to the reported application group")
        );
    }

    #[test]
    fn background_executable_is_markup_escaped() {
        let mut workload = background_workload();
        workload.root = ProcessDescriptor::new(
            workload.root.identity(),
            "worker",
            Some(PathBuf::from("/tmp/<worker&helper>")),
        );

        let request =
            NotificationRequest::for_background_workload(&workload, NotificationView::Details);

        assert!(request.body().contains("/tmp/&lt;worker&amp;helper&gt;"));
    }

    #[test]
    fn background_binding_preserves_identity_across_navigation() {
        let summary = NotificationBinding::for_background_workload(
            background_workload(),
            NotificationView::Summary,
        );
        let identity = summary.background_workload().unwrap().identity();

        let details = summary.transition(NotificationAction::Details).unwrap();
        assert_eq!(details.view(), NotificationView::Details);
        assert_eq!(details.background_workload().unwrap().identity(), identity);

        let restored = details.transition(NotificationAction::Back).unwrap();
        assert_eq!(restored.view(), NotificationView::Summary);
        assert_eq!(restored.background_workload().unwrap().identity(), identity);
        assert!(restored.background_workload().is_some());
        assert!(restored.workload().is_none());
        assert!(restored.event().is_none());
    }

    fn group(tree_count: usize) -> StaleWorkloadGroup {
        let workloads = (0..tree_count)
            .map(|index| {
                let pid = 100 + u32::try_from(index).unwrap();
                let mut workload = workload();
                workload.root = ProcessDescriptor::new(
                    ProcessIdentity::new(pid, 1_000, u64::from(pid)),
                    "pytest",
                    None,
                );
                workload.members = vec![WorkloadMember {
                    process: workload.root.clone(),
                    resources: ProcessResources {
                        cpu_percent: 0.0,
                        resident_memory_bytes: 256 * 1_048_576,
                        virtual_memory_bytes: 256 * 1_048_576,
                        running_for: Duration::from_hours(2),
                        observed_at: Duration::ZERO,
                    },
                    depth: 0,
                }];
                workload
            })
            .collect();
        StaleWorkloadGroup {
            working_directory: PathBuf::from("/work/project"),
            total_memory_bytes: 256 * 1_048_576 * u64::try_from(tree_count).unwrap(),
            workloads,
            total_cpu_percent: 0.0,
            age: Duration::from_hours(72),
        }
    }

    #[test]
    fn stale_group_summary_is_details_only_and_hides_the_full_path() {
        let group = group(3);

        let request =
            NotificationRequest::for_stale_workload_group(&group, NotificationView::Summary);

        assert_eq!(request.action_set(), NotificationActionSet::DetailsOnly);
        assert!(request.summary().contains("project"));
        assert!(request.body().contains("Trees: 3"));
        assert!(request.body().contains("Processes: 3"));
        assert!(request.body().contains("Group RAM: 768 MiB"));
        assert!(!request.summary().contains("/work/project"));
        assert!(!request.body().contains("/work/project"));
    }

    #[test]
    fn stale_group_details_offer_group_actions_and_bound_the_root_pid_list() {
        let group = group(25);

        let request =
            NotificationRequest::for_stale_workload_group(&group, NotificationView::Details);

        assert_eq!(request.action_set(), NotificationActionSet::GroupDetails);
        assert!(
            request
                .body()
                .contains("all listed independent workload trees")
        );
        assert!(request.body().contains("SIGTERM"));
        assert!(request.body().contains("Parent terminal/session"));
        assert!(request.body().contains("Root PIDs:"));
        assert!(request.body().contains("119"));
        assert!(request.body().contains("and 5 more"));
        assert!(!request.body().contains("120,"));
    }

    #[test]
    fn stale_group_binding_keeps_the_immutable_group_across_navigation() {
        let group = group(4);
        let summary =
            NotificationBinding::for_stale_workload_group(group.clone(), NotificationView::Summary);

        let details = summary.transition(NotificationAction::Details).unwrap();
        assert_eq!(details.view(), NotificationView::Details);
        assert_eq!(
            details.stale_workload_group().unwrap(),
            &group,
            "details must keep the exact immutable group"
        );
        assert!(details.event().is_none());
        assert!(details.workload().is_none());
        assert!(details.background_workload().is_none());

        let restored = details.transition(NotificationAction::Back).unwrap();
        assert_eq!(restored.view(), NotificationView::Summary);
        assert_eq!(restored.stale_workload_group().unwrap(), &group);
    }

    #[test]
    fn stale_group_notification_escapes_the_directory_name() {
        let mut group = group(1);
        group.working_directory = PathBuf::from("/tmp/<evil&dir>");

        let request =
            NotificationRequest::for_stale_workload_group(&group, NotificationView::Summary);

        assert!(request.summary().contains("&lt;evil&amp;dir&gt;"));
    }

    #[test]
    fn group_stop_is_dispatched_only_from_the_details_view() {
        let group = group(2);
        let summary =
            NotificationBinding::for_stale_workload_group(group.clone(), NotificationView::Summary);
        let details =
            NotificationBinding::for_stale_workload_group(group.clone(), NotificationView::Details);

        assert_eq!(
            summary.dispatch(NotificationAction::Details),
            NotificationDispatch::Navigate(Box::new(
                NotificationBinding::for_stale_workload_group(group, NotificationView::Details),
            ))
        );
        assert_eq!(
            details.dispatch(NotificationAction::StopGroup),
            NotificationDispatch::Execute(NotificationAction::StopGroup)
        );
        assert_eq!(
            summary.dispatch(NotificationAction::StopGroup),
            NotificationDispatch::Rejected
        );

        let back = details.transition(NotificationAction::Back).unwrap();
        assert_eq!(back.view(), NotificationView::Summary);
        assert_eq!(
            back.dispatch(NotificationAction::StopGroup),
            NotificationDispatch::Rejected
        );
    }

    #[test]
    fn group_binding_rejects_single_stop_and_ignore_actions() {
        let details =
            NotificationBinding::for_stale_workload_group(group(2), NotificationView::Details);

        for action in [
            NotificationAction::Stop,
            NotificationAction::IgnoreForHour,
            NotificationAction::AlwaysIgnore,
        ] {
            assert_eq!(details.dispatch(action), NotificationDispatch::Rejected);
        }
    }

    #[test]
    fn group_stop_is_rejected_for_non_group_subjects() {
        let details = NotificationBinding::new(event(), NotificationView::Details);

        assert_eq!(
            details.dispatch(NotificationAction::StopGroup),
            NotificationDispatch::Rejected
        );
        assert_eq!(
            details.dispatch(NotificationAction::Stop),
            NotificationDispatch::Execute(NotificationAction::Stop)
        );
    }

    #[test]
    fn unknown_notification_ids_are_an_unknown_dispatch() {
        assert_eq!(
            plan_notification(None, NotificationAction::StopGroup),
            NotificationDispatch::Unknown
        );
        assert_eq!(
            plan_notification(None, NotificationAction::Stop),
            NotificationDispatch::Unknown
        );
    }

    #[test]
    fn ordinary_bindings_keep_their_action_sets() {
        assert_eq!(
            NotificationRequest::from_event(&event()).action_set(),
            NotificationActionSet::StandardSummary
        );
        assert_eq!(
            NotificationRequest::details(&event()).action_set(),
            NotificationActionSet::BackOnly
        );
        assert_eq!(
            NotificationRequest::for_stale_workload(&workload(), NotificationView::Summary)
                .action_set(),
            NotificationActionSet::StandardSummary
        );
        assert_eq!(
            NotificationRequest::for_background_workload(
                &background_workload(),
                NotificationView::Summary,
            )
            .action_set(),
            NotificationActionSet::StandardSummary
        );
    }

    #[test]
    fn parses_stop_group_only_from_the_exact_key() {
        assert_eq!(
            NotificationAction::from_key("stop_group"),
            Some(NotificationAction::StopGroup)
        );
        assert_eq!(
            NotificationAction::from_key("stop"),
            Some(NotificationAction::Stop)
        );
        assert_eq!(
            NotificationAction::from_key("default"),
            Some(NotificationAction::Details)
        );
        assert_eq!(
            NotificationAction::from_key("back"),
            Some(NotificationAction::Back)
        );
        assert_eq!(NotificationAction::from_key("stop-group"), None);
        assert_eq!(NotificationAction::from_key("group_stop"), None);
        assert_eq!(NotificationAction::from_key("STOP_GROUP"), None);
        assert_eq!(NotificationAction::from_key("stop_grouping"), None);
        assert_eq!(NotificationAction::from_key("unknown"), None);
    }

    #[test]
    fn maps_freedesktop_notification_close_reasons() {
        assert_eq!(
            NotificationCloseReason::from_code(1),
            NotificationCloseReason::Expired
        );
        assert_eq!(
            NotificationCloseReason::from_code(2),
            NotificationCloseReason::DismissedByUser
        );
        assert_eq!(
            NotificationCloseReason::from_code(3),
            NotificationCloseReason::ClosedBySender
        );
        assert_eq!(
            NotificationCloseReason::from_code(99),
            NotificationCloseReason::Undefined(99)
        );
    }

    #[test]
    fn navigates_from_summary_to_details_and_back_for_the_same_process() {
        let summary = NotificationBinding::new(event(), NotificationView::Summary);
        let identity = summary.event().unwrap().process.identity();

        let details = summary.transition(NotificationAction::Details).unwrap();
        assert_eq!(details.view(), NotificationView::Details);
        assert_eq!(details.event().unwrap().process.identity(), identity);
        assert!(details.request().body().contains("Executable:"));

        let restored = details.transition(NotificationAction::Back).unwrap();
        assert_eq!(restored.view(), NotificationView::Summary);
        assert_eq!(restored.event().unwrap().process.identity(), identity);
        assert!(!restored.request().body().contains("Executable:"));
    }

    #[test]
    fn rejects_navigation_actions_from_the_wrong_view() {
        let summary = NotificationBinding::new(event(), NotificationView::Summary);
        let details = NotificationBinding::new(event(), NotificationView::Details);

        assert!(summary.transition(NotificationAction::Back).is_none());
        assert!(details.transition(NotificationAction::Details).is_none());
    }

    #[test]
    fn bindings_remove_closed_notifications_and_evict_the_oldest_entry() {
        let mut bindings = NotificationBindings::new(2);
        bindings.remember(
            20,
            NotificationBinding::new(event(), NotificationView::Summary),
        );
        bindings.remember(
            10,
            NotificationBinding::new(event(), NotificationView::Summary),
        );
        bindings.remember(
            30,
            NotificationBinding::new(event(), NotificationView::Details),
        );

        assert!(!bindings.contains(20));
        assert!(bindings.contains(10));
        assert!(bindings.contains(30));

        let closed = bindings.remove(10).unwrap();
        assert_eq!(closed.view(), NotificationView::Summary);
        assert!(!bindings.contains(10));
    }

    #[derive(Default)]
    struct FakeNotificationSink {
        requests: Vec<NotificationRequest>,
    }

    impl NotificationSink for FakeNotificationSink {
        fn notify(
            &mut self,
            request: NotificationRequest,
            _replaces_id: Option<u32>,
        ) -> impl Future<Output = Result<u32, PortError>> + Send {
            self.requests.push(request);
            std::future::ready(Ok(7))
        }

        fn close(
            &mut self,
            _notification_id: u32,
        ) -> impl Future<Output = Result<(), PortError>> + Send {
            std::future::ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn notification_port_accepts_a_fake_adapter() {
        let mut sink = FakeNotificationSink::default();

        let id = sink
            .notify(NotificationRequest::from_event(&event()), None)
            .await
            .unwrap();

        assert_eq!(id, 7);
        assert_eq!(sink.requests.len(), 1);
    }
}
