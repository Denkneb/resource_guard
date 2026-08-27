use std::{
    collections::{HashMap, VecDeque},
    fmt::Write as _,
    future::Future,
    time::Duration,
};

use crate::domain::{ProcessDescriptor, ProcessResources, ResourceBreach};

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
    pub process: ProcessDescriptor,
    pub resources: ProcessResources,
    pub breach: ResourceBreach,
    pub exceeded_for: Duration,
    pub view: NotificationView,
}

impl NotificationRequest {
    #[must_use]
    pub fn from_event(event: &MonitorEvent) -> Self {
        Self {
            process: event.process.clone(),
            resources: event.resources,
            breach: event.breach,
            exceeded_for: event.exceeded_for,
            view: NotificationView::Summary,
        }
    }

    #[must_use]
    pub fn details(event: &MonitorEvent) -> Self {
        Self {
            view: NotificationView::Details,
            ..Self::from_event(event)
        }
    }

    #[must_use]
    pub fn for_view(event: &MonitorEvent, view: NotificationView) -> Self {
        match view {
            NotificationView::Summary => Self::from_event(event),
            NotificationView::Details => Self::details(event),
        }
    }

    #[must_use]
    pub fn summary(&self) -> String {
        format!(
            "Resource limit exceeded: {} ({})",
            self.process.name(),
            self.process.identity().pid()
        )
    }

    #[must_use]
    pub fn body(&self) -> String {
        let reason = match (self.breach.cpu, self.breach.memory) {
            (true, true) => "CPU and RAM",
            (true, false) => "CPU",
            (false, true) => "RAM",
            (false, false) => "configured resource",
        };
        let mut body = format!(
            "CPU: {:.1}%\nRAM: {} MiB\nExceeded for: {}s\nReason: {reason}",
            self.resources.cpu_percent,
            self.resources.resident_memory_bytes / 1_048_576,
            self.exceeded_for.as_secs(),
        );
        if self.view == NotificationView::Details {
            let executable = self
                .process
                .executable()
                .map_or_else(|| "unknown".to_owned(), |path| path.display().to_string());
            let executable = escape_markup(&executable);
            let _ = write!(
                body,
                "\nExecutable: {executable}\nVirtual memory: {} MiB\nRuntime: {}s",
                self.resources.virtual_memory_bytes / 1_048_576,
                self.resources.running_for.as_secs(),
            );
        }
        body
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct NotificationBinding {
    event: MonitorEvent,
    view: NotificationView,
}

impl NotificationBinding {
    #[must_use]
    pub const fn new(event: MonitorEvent, view: NotificationView) -> Self {
        Self { event, view }
    }

    #[must_use]
    pub const fn event(&self) -> &MonitorEvent {
        &self.event
    }

    #[must_use]
    pub const fn view(&self) -> NotificationView {
        self.view
    }

    #[must_use]
    pub fn request(&self) -> NotificationRequest {
        NotificationRequest::for_view(&self.event, self.view)
    }

    #[must_use]
    pub fn transition(&self, action: NotificationAction) -> Option<Self> {
        let view = match (self.view, action) {
            (NotificationView::Summary, NotificationAction::Details) => NotificationView::Details,
            (NotificationView::Details, NotificationAction::Back) => NotificationView::Summary,
            _ => return None,
        };
        Some(Self::new(self.event.clone(), view))
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
        domain::{ProcessDescriptor, ProcessIdentity, ProcessResources, ResourceBreach},
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
        let identity = summary.event().process.identity();

        let details = summary.transition(NotificationAction::Details).unwrap();
        assert_eq!(details.view(), NotificationView::Details);
        assert_eq!(details.event().process.identity(), identity);
        assert!(details.request().body().contains("Executable:"));

        let restored = details.transition(NotificationAction::Back).unwrap();
        assert_eq!(restored.view(), NotificationView::Summary);
        assert_eq!(restored.event().process.identity(), identity);
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
