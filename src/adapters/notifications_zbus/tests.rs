use std::{
    collections::HashMap,
    io::{BufRead, BufReader},
    path::PathBuf,
    process::{Child, ChildStdout, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, Ordering},
    },
    time::Duration,
};

use tokio::sync::mpsc;
use zbus::{connection::Builder, object_server::SignalEmitter, zvariant::OwnedValue};

use super::{
    DESKTOP_ENTRY, PATH, SERVICE, ZbusNotificationSink, notification_actions, notification_hints,
    timeout_milliseconds,
};
use crate::{
    application::{
        MonitorEvent, NotificationAction, NotificationActionSet, NotificationBinding,
        NotificationCloseReason, NotificationEvent, NotificationRequest, NotificationSink,
        NotificationView,
    },
    domain::{
        ProcessDescriptor, ProcessIdentity, ProcessResources, ResourceBreach, StaleWorkload,
        StaleWorkloadGroup, WorkloadMember,
    },
};

#[derive(Debug)]
struct RecordedNotification {
    app_name: String,
    replaces_id: u32,
    app_icon: String,
    summary: String,
    body: String,
    actions: Vec<String>,
    desktop_entry: String,
    transient: bool,
    resident: bool,
    urgency: u8,
    expire_timeout: i32,
}

#[derive(Debug, Default)]
struct FakeState {
    next_id: AtomicU32,
    notifications: Mutex<Vec<RecordedNotification>>,
    closed: Mutex<Vec<u32>>,
}

#[derive(Clone, Debug)]
struct FakeNotifications {
    state: Arc<FakeState>,
    supports_actions: bool,
}

#[zbus::interface(name = "org.freedesktop.Notifications")]
impl FakeNotifications {
    #[allow(clippy::unused_self)]
    fn get_capabilities(&self) -> Vec<String> {
        let mut capabilities = vec!["persistence".to_owned()];
        if self.supports_actions {
            capabilities.push("actions".to_owned());
        }
        capabilities
    }

    #[allow(clippy::unused_self)]
    fn get_server_information(&self) -> (String, String, String, String) {
        (
            "Fake Notifications".to_owned(),
            "Resource Guard tests".to_owned(),
            "1.0".to_owned(),
            "1.2".to_owned(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        app_name: String,
        replaces_id: u32,
        app_icon: String,
        summary: String,
        body: String,
        actions: Vec<String>,
        mut hints: HashMap<String, OwnedValue>,
        expire_timeout: i32,
    ) -> u32 {
        let notification = RecordedNotification {
            app_name,
            replaces_id,
            app_icon,
            summary,
            body,
            actions,
            desktop_entry: String::try_from(hints.remove("desktop-entry").unwrap()).unwrap(),
            transient: bool::try_from(hints.remove("transient").unwrap()).unwrap(),
            resident: bool::try_from(hints.remove("resident").unwrap()).unwrap(),
            urgency: u8::try_from(hints.remove("urgency").unwrap()).unwrap(),
            expire_timeout,
        };
        self.state.notifications.lock().unwrap().push(notification);
        if replaces_id == 0 {
            self.state.next_id.fetch_add(1, Ordering::Relaxed) + 1
        } else {
            replaces_id
        }
    }

    fn close_notification(&self, notification_id: u32) {
        self.state.closed.lock().unwrap().push(notification_id);
    }

    #[zbus(signal)]
    async fn action_invoked(
        signal_emitter: &SignalEmitter<'_>,
        notification_id: u32,
        action_key: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn notification_closed(
        signal_emitter: &SignalEmitter<'_>,
        notification_id: u32,
        reason: u32,
    ) -> zbus::Result<()>;
}

struct PrivateBus {
    child: Child,
    _stdout: BufReader<ChildStdout>,
    address: String,
}

impl PrivateBus {
    fn spawn() -> Self {
        let mut child = Command::new("dbus-daemon")
            .args([
                "--session",
                "--nofork",
                "--print-address=1",
                "--print-pid=1",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let mut address = String::new();
        let mut pid = String::new();
        stdout.read_line(&mut address).unwrap();
        stdout.read_line(&mut pid).unwrap();
        assert!(!pid.trim().is_empty());
        Self {
            child,
            _stdout: stdout,
            address: address.trim().to_owned(),
        }
    }
}

impl Drop for PrivateBus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn monitor_event() -> MonitorEvent {
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

fn stale_workload() -> StaleWorkload {
    let event = monitor_event();
    StaleWorkload {
        root: event.process.clone(),
        members: vec![WorkloadMember {
            process: event.process,
            resources: event.resources,
            depth: 0,
        }],
        total_memory_bytes: 700 * 1_048_576,
        total_cpu_percent: 0.3,
        age: Duration::from_hours(72),
    }
}

fn stale_group() -> StaleWorkloadGroup {
    StaleWorkloadGroup {
        working_directory: PathBuf::from("/home/user/work/alpha"),
        workloads: vec![stale_workload()],
        total_memory_bytes: 700 * 1_048_576,
        total_cpu_percent: 0.3,
        age: Duration::from_hours(72),
    }
}

async fn emitted_action(
    server: &zbus::Connection,
    events: &mut mpsc::Receiver<Result<NotificationEvent, crate::application::PortError>>,
    notification_id: u32,
    key: &str,
) -> NotificationAction {
    let interface = server
        .object_server()
        .interface::<_, FakeNotifications>(PATH)
        .await
        .unwrap();
    FakeNotifications::action_invoked(interface.signal_emitter(), notification_id, key)
        .await
        .unwrap();
    let event = tokio::time::timeout(Duration::from_secs(2), events.recv())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    match event {
        NotificationEvent::Action { action, .. } => action,
        other => panic!("expected an action event, got {other:?}"),
    }
}

#[test]
fn notification_timeout_is_safely_bounded() {
    assert_eq!(timeout_milliseconds(Duration::from_secs(15)), 15_000);
    assert_eq!(timeout_milliseconds(Duration::MAX), i32::MAX);
}

#[test]
fn action_sets_select_the_expected_buttons() {
    let standard = notification_actions(NotificationActionSet::StandardSummary, true);
    assert!(standard.contains(&"details"));
    assert!(standard.contains(&"stop"));
    assert!(standard.contains(&"ignore_hour"));
    assert!(standard.contains(&"always_ignore"));

    assert_eq!(
        notification_actions(NotificationActionSet::DetailsOnly, true),
        ["details", "Подробнее"]
    );
    assert_eq!(
        notification_actions(NotificationActionSet::BackOnly, true),
        ["back", "Назад"]
    );
    assert_eq!(
        notification_actions(NotificationActionSet::GroupDetails, true),
        ["stop_group", "Завершить все деревья", "back", "Назад"]
    );
    assert!(notification_actions(NotificationActionSet::None, true).is_empty());
    assert!(notification_actions(NotificationActionSet::StandardSummary, false).is_empty());
    assert!(notification_actions(NotificationActionSet::GroupDetails, false).is_empty());
}

#[test]
fn notification_hints_identify_a_persistent_desktop_application() {
    let hints = notification_hints(true);
    let desktop_entry = <&str>::try_from(hints.get("desktop-entry").unwrap()).unwrap();
    let transient = bool::try_from(hints.get("transient").unwrap()).unwrap();
    let urgency = u8::try_from(hints.get("urgency").unwrap()).unwrap();
    let resident = bool::try_from(hints.get("resident").unwrap()).unwrap();

    assert_eq!(desktop_entry, DESKTOP_ENTRY);
    assert!(!transient);
    assert_eq!(urgency, 1);
    assert!(resident);
    assert!(!notification_hints(false).contains_key("resident"));
}

#[tokio::test]
async fn adapter_navigates_and_reports_closure_over_a_private_dbus() {
    let bus = PrivateBus::spawn();
    let state = Arc::new(FakeState::default());
    let server = Builder::address(bus.address.as_str())
        .unwrap()
        .name(SERVICE)
        .unwrap()
        .serve_at(
            PATH,
            FakeNotifications {
                state: Arc::clone(&state),
                supports_actions: true,
            },
        )
        .unwrap()
        .build()
        .await
        .unwrap();
    let client = Builder::address(bus.address.as_str())
        .unwrap()
        .build()
        .await
        .unwrap();
    let (sender, mut events) = mpsc::channel(8);
    let mut sink =
        ZbusNotificationSink::connect_with_connection(client, sender, Duration::from_secs(15))
            .await
            .unwrap();
    assert!(sink.supports_actions());
    assert!(sink.supports_persistence());

    let summary = NotificationBinding::new(monitor_event(), NotificationView::Summary);
    let identity = summary.event().unwrap().process.identity();
    let id = sink.notify(summary.request(), None).await.unwrap();
    let details = summary
        .transition(emitted_action(&server, &mut events, id, "details").await)
        .unwrap();
    assert_eq!(details.event().unwrap().process.identity(), identity);
    assert_eq!(sink.notify(details.request(), Some(id)).await.unwrap(), id);
    let restored = details
        .transition(emitted_action(&server, &mut events, id, "back").await)
        .unwrap();
    assert_eq!(restored.event().unwrap().process.identity(), identity);
    assert_eq!(sink.notify(restored.request(), Some(id)).await.unwrap(), id);
    sink.close(id).await.unwrap();

    let interface = server
        .object_server()
        .interface::<_, FakeNotifications>(PATH)
        .await
        .unwrap();
    FakeNotifications::notification_closed(interface.signal_emitter(), id, 3)
        .await
        .unwrap();
    let closed = tokio::time::timeout(Duration::from_secs(2), events.recv())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        closed,
        NotificationEvent::Closed {
            notification_id: id,
            reason: NotificationCloseReason::ClosedBySender,
        }
    );

    let notifications = state.notifications.lock().unwrap();
    assert_eq!(notifications.len(), 3);
    assert_eq!(notifications[0].replaces_id, 0);
    assert_eq!(notifications[0].app_name, "Resource Guard");
    assert_eq!(notifications[0].app_icon, "dialog-warning");
    assert!(notifications[0].summary.contains("worker (42)"));
    assert!(notifications[0].body.contains("CPU: 95.5%"));
    assert!(notifications[0].actions.contains(&"details".to_owned()));
    assert_eq!(notifications[0].desktop_entry, DESKTOP_ENTRY);
    assert!(!notifications[0].transient);
    assert!(notifications[0].resident);
    assert_eq!(notifications[0].urgency, 1);
    assert_eq!(notifications[0].expire_timeout, 15_000);
    assert_eq!(notifications[1].replaces_id, id);
    assert_eq!(notifications[1].actions, ["back", "Назад"]);
    assert_eq!(notifications[2].replaces_id, id);
    assert!(notifications[2].actions.contains(&"stop".to_owned()));
    assert_eq!(*state.closed.lock().unwrap(), [id]);
}

#[tokio::test]
async fn group_notifications_expose_two_step_actions_over_a_private_dbus() {
    let bus = PrivateBus::spawn();
    let state = Arc::new(FakeState::default());
    let server = Builder::address(bus.address.as_str())
        .unwrap()
        .name(SERVICE)
        .unwrap()
        .serve_at(
            PATH,
            FakeNotifications {
                state: Arc::clone(&state),
                supports_actions: true,
            },
        )
        .unwrap()
        .build()
        .await
        .unwrap();
    let client = Builder::address(bus.address.as_str())
        .unwrap()
        .build()
        .await
        .unwrap();
    let (sender, mut events) = mpsc::channel(8);
    let mut sink =
        ZbusNotificationSink::connect_with_connection(client, sender, Duration::from_secs(15))
            .await
            .unwrap();

    let group = stale_group();
    let summary =
        NotificationBinding::for_stale_workload_group(group.clone(), NotificationView::Summary);
    let id = sink.notify(summary.request(), None).await.unwrap();
    let details = summary
        .transition(emitted_action(&server, &mut events, id, "details").await)
        .unwrap();
    assert_eq!(details.stale_workload_group().unwrap(), &group);
    assert_eq!(sink.notify(details.request(), Some(id)).await.unwrap(), id);
    let restored = details
        .transition(emitted_action(&server, &mut events, id, "back").await)
        .unwrap();
    assert_eq!(restored.stale_workload_group().unwrap(), &group);
    assert_eq!(sink.notify(restored.request(), Some(id)).await.unwrap(), id);

    assert_eq!(
        emitted_action(&server, &mut events, id, "stop_group").await,
        NotificationAction::StopGroup
    );

    let notifications = state.notifications.lock().unwrap();
    assert_eq!(notifications.len(), 3);
    assert_eq!(notifications[0].actions, ["details", "Подробнее"]);
    assert!(
        notifications[0]
            .summary
            .contains("Stale workload group in alpha")
    );
    assert!(notifications[0].body.contains("Trees: 1"));
    assert_eq!(notifications[1].replaces_id, id);
    assert_eq!(
        notifications[1].actions,
        ["stop_group", "Завершить все деревья", "back", "Назад"]
    );
    assert!(notifications[1].body.contains("Root PIDs: 42"));
    assert_eq!(notifications[2].replaces_id, id);
    assert_eq!(notifications[2].actions, ["details", "Подробнее"]);
}

#[tokio::test]
async fn no_action_server_receives_an_empty_action_list() {
    let bus = PrivateBus::spawn();
    let state = Arc::new(FakeState::default());
    let _server = Builder::address(bus.address.as_str())
        .unwrap()
        .name(SERVICE)
        .unwrap()
        .serve_at(
            PATH,
            FakeNotifications {
                state: Arc::clone(&state),
                supports_actions: false,
            },
        )
        .unwrap()
        .build()
        .await
        .unwrap();
    let client = Builder::address(bus.address.as_str())
        .unwrap()
        .build()
        .await
        .unwrap();
    let (sender, _events) = mpsc::channel(8);
    let mut sink =
        ZbusNotificationSink::connect_with_connection(client, sender, Duration::from_secs(15))
            .await
            .unwrap();
    assert!(!sink.supports_actions());

    sink.notify(NotificationRequest::from_event(&monitor_event()), None)
        .await
        .unwrap();

    let notifications = state.notifications.lock().unwrap();
    assert_eq!(notifications.len(), 1);
    assert!(notifications[0].actions.is_empty());
}
