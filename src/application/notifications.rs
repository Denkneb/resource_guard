use std::{
    collections::{HashMap, VecDeque},
    fmt::Write as _,
    future::Future,
};

use crate::domain::{MemoryPressureEvaluation, StaleWorkload};

use super::{MonitorEvent, PortError};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NotificationAction {
    Stop,
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
            "ignore_hour" => Some(Self::IgnoreForHour),
            "always_ignore" => Some(Self::AlwaysIgnore),
            "details" | "default" => Some(Self::Details),
            "back" => Some(Self::Back),
            _ => None,
        }
    }
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
    actions: bool,
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
            actions: true,
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
            actions: false,
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
            actions: true,
            view,
        }
    }

    #[must_use]
    pub const fn has_actions(&self) -> bool {
        self.actions
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
    pub const fn event(&self) -> Option<&MonitorEvent> {
        match &self.subject {
            NotificationSubject::Process(event) => Some(event),
            NotificationSubject::Workload(_) => None,
        }
    }

    #[must_use]
    pub const fn workload(&self) -> Option<&StaleWorkload> {
        match &self.subject {
            NotificationSubject::Workload(workload) => Some(workload),
            NotificationSubject::Process(_) => None,
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

fn escape_markup(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
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
        NotificationAction, NotificationBinding, NotificationBindings, NotificationCloseReason,
        NotificationRequest, NotificationSink, NotificationView,
    };
    use crate::{
        application::{MonitorEvent, PortError},
        domain::{
            MemoryPressureEvaluation, MemoryPressureLevel, MemoryPressureSample, MemoryPsi,
            ProcessDescriptor, ProcessIdentity, ProcessResources, ResourceBreach, StaleWorkload,
            SystemResources, WorkloadMember,
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
        assert!(!request.has_actions());
    }

    #[test]
    fn parses_only_known_action_keys() {
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
