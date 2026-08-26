use std::{fmt::Write as _, future::Future, time::Duration};

use crate::domain::{ProcessDescriptor, ProcessResources, ResourceBreach};

use super::{MonitorEvent, PortError};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NotificationAction {
    Stop,
    IgnoreForHour,
    AlwaysIgnore,
    Details,
}

impl NotificationAction {
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "stop" => Some(Self::Stop),
            "ignore_hour" => Some(Self::IgnoreForHour),
            "always_ignore" => Some(Self::AlwaysIgnore),
            "details" | "default" => Some(Self::Details),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NotificationEvent {
    Action {
        notification_id: u32,
        action: NotificationAction,
    },
    Closed {
        notification_id: u32,
    },
    UnknownAction {
        notification_id: u32,
        key: String,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct NotificationRequest {
    pub process: ProcessDescriptor,
    pub resources: ProcessResources,
    pub breach: ResourceBreach,
    pub exceeded_for: Duration,
    pub detailed: bool,
}

impl NotificationRequest {
    #[must_use]
    pub fn from_event(event: &MonitorEvent) -> Self {
        Self {
            process: event.process.clone(),
            resources: event.resources,
            breach: event.breach,
            exceeded_for: event.exceeded_for,
            detailed: false,
        }
    }

    #[must_use]
    pub fn details(event: &MonitorEvent) -> Self {
        Self {
            detailed: true,
            ..Self::from_event(event)
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
        if self.detailed {
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
    ) -> impl Future<Output = Result<u32, PortError>> + Send;
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, time::Duration};

    use super::{NotificationAction, NotificationRequest, NotificationSink};
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
        assert_eq!(NotificationAction::from_key("unknown"), None);
    }

    #[derive(Default)]
    struct FakeNotificationSink {
        requests: Vec<NotificationRequest>,
    }

    impl NotificationSink for FakeNotificationSink {
        fn notify(
            &mut self,
            request: NotificationRequest,
        ) -> impl Future<Output = Result<u32, PortError>> + Send {
            self.requests.push(request);
            std::future::ready(Ok(7))
        }
    }

    #[tokio::test]
    async fn notification_port_accepts_a_fake_adapter() {
        let mut sink = FakeNotificationSink::default();

        let id = sink
            .notify(NotificationRequest::from_event(&event()))
            .await
            .unwrap();

        assert_eq!(id, 7);
        assert_eq!(sink.requests.len(), 1);
    }
}
