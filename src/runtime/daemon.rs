use std::{
    fs, io,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixListener, UnixStream},
    runtime::Builder,
    sync::RwLock,
    time::{MissedTickBehavior, timeout},
};
use tracing::{info, warn};
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

use crate::{
    adapters::{
        PidfdTerminationPort, SysinfoProcessSource, SystemClock, TomlConfigRepository,
        ZbusNotificationSink, current_user_id,
    },
    application::{
        MonitorService, NotificationAction, NotificationBinding, NotificationBindings,
        NotificationEvent, NotificationRequest, NotificationSink, NotificationView, StopProcess,
    },
    domain::IgnoreRule,
};

use super::{
    RuntimeError,
    paths::{control_socket_path, prepare_runtime_directory},
    protocol::{ControlRequest, ControlResponse, StatusResponse, TopResponse},
    state::DaemonState,
};

const MAX_REQUEST_BYTES: u64 = 8 * 1_024;
const MAX_RESPONSE_BYTES: u64 = 1_024 * 1_024;
const CONTROL_TIMEOUT: Duration = Duration::from_secs(2);
const NOTIFICATION_RETRY_INTERVAL: Duration = Duration::from_secs(60);
const MAX_NOTIFICATION_BINDINGS: usize = 256;

/// Runs the foreground monitoring daemon until SIGINT or SIGTERM is received.
///
/// # Errors
///
/// Returns an error when configuration, runtime setup, polling infrastructure,
/// or signal handling cannot be initialized.
pub fn run_daemon() -> Result<(), RuntimeError> {
    initialize_tracing();
    build_runtime()?.block_on(run_daemon_async())
}

/// Fetches the current daemon status through the authenticated local socket.
///
/// # Errors
///
/// Returns an error when the runtime path is unavailable, the daemon cannot be
/// reached, or its response is invalid.
pub fn query_status() -> Result<StatusResponse, RuntimeError> {
    build_runtime()?.block_on(query_status_async())
}

/// Fetches the latest monitored process snapshot from the daemon.
///
/// # Errors
///
/// Returns an error when the runtime path is unavailable, the daemon cannot be
/// reached, or its response is invalid.
pub fn query_top() -> Result<TopResponse, RuntimeError> {
    build_runtime()?.block_on(query_top_async())
}

fn build_runtime() -> Result<tokio::runtime::Runtime, RuntimeError> {
    Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| RuntimeError::io("create async runtime", error))
}

fn initialize_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer())
        .try_init();
}

async fn run_daemon_async() -> Result<(), RuntimeError> {
    let repository = TomlConfigRepository::from_environment()?;
    let mut settings = repository.load()?.settings;
    let socket_path = control_socket_path()?;
    let listener = bind_control_socket(&socket_path).await?;
    let _socket_guard = SocketGuard::new(socket_path.clone());
    let state = Arc::new(RwLock::new(DaemonState::new()));
    let mut monitor = MonitorService::new(
        SysinfoProcessSource::new(),
        SystemClock::new(),
        current_user_id(),
        settings.thresholds(),
        settings.protection_policy(),
        settings.violation_policy(),
    );
    let (notification_sender, mut notification_events) = tokio::sync::mpsc::channel(64);
    let mut notifier = if settings.notifications.enabled {
        connect_notifications(
            notification_sender.clone(),
            settings.notifications.timeout,
            &state,
        )
        .await
    } else {
        None
    };
    let mut notification_bindings = NotificationBindings::new(MAX_NOTIFICATION_BINDINGS);
    let mut notification_retry = tokio::time::interval(NOTIFICATION_RETRY_INTERVAL);
    notification_retry.set_missed_tick_behavior(MissedTickBehavior::Skip);
    notification_retry.tick().await;
    let mut interval = tokio::time::interval(settings.monitor.poll_interval);
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let shutdown = shutdown_signal();
    tokio::pin!(shutdown);

    info!(socket = %socket_path.display(), "resource guard daemon started");
    loop {
        tokio::select! {
            _ = interval.tick() => {
                match monitor.poll() {
                    Ok(report) => record_monitor_report(
                        report,
                        &mut notifier,
                        &mut notification_bindings,
                        &state,
                    ).await,
                    Err(error) => record_poll_error(&state, error).await,
                }
            }
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, _)) => {
                        let client_state = Arc::clone(&state);
                        tokio::spawn(async move {
                            if let Err(error) = handle_client(stream, client_state).await {
                                warn!(%error, "control request failed");
                            }
                        });
                    }
                    Err(error) => warn!(%error, "cannot accept control connection"),
                }
            }
            Some(event) = notification_events.recv(), if settings.notifications.enabled => {
                match event {
                    Ok(event) => {
                        handle_notification_event(
                            event,
                            &mut notification_bindings,
                            &mut monitor,
                            &mut notifier,
                            &repository,
                            &mut settings,
                            &state,
                        ).await;
                    }
                    Err(error) => {
                        warn!(%error, "desktop notification event stream failed");
                        state.write().await.record_notification_error(error.to_string());
                        notification_bindings.clear();
                        notifier = None;
                    }
                }
            }
            _ = notification_retry.tick(), if settings.notifications.enabled && notifier.is_none() => {
                notifier = connect_notifications(
                    notification_sender.clone(),
                    settings.notifications.timeout,
                    &state,
                ).await;
            }
            result = &mut shutdown => {
                result?;
                break;
            }
        }
    }
    info!("resource guard daemon stopped");
    Ok(())
}

async fn record_poll_error(state: &Arc<RwLock<DaemonState>>, error: crate::application::PortError) {
    warn!(%error, "resource polling failed");
    state.write().await.record_error(error.to_string());
}

async fn record_monitor_report(
    report: crate::application::MonitorReport,
    notifier: &mut Option<ZbusNotificationSink>,
    bindings: &mut NotificationBindings,
    state: &Arc<RwLock<DaemonState>>,
) {
    for event in &report.events {
        warn!(
            pid = event.process.identity().pid(),
            process = event.process.name(),
            cpu_percent = event.resources.cpu_percent,
            memory_bytes = event.resources.resident_memory_bytes,
            exceeded_seconds = event.exceeded_for.as_secs(),
            "process exceeded configured resource limits"
        );
        if let Some(sink) = notifier.as_mut() {
            match sink
                .notify(NotificationRequest::from_event(event), None)
                .await
            {
                Ok(notification_id) => {
                    bindings.remember(
                        notification_id,
                        NotificationBinding::new(event.clone(), NotificationView::Summary),
                    );
                    state.write().await.clear_notification_error();
                }
                Err(error) => {
                    warn!(%error, "desktop notification failed");
                    state
                        .write()
                        .await
                        .record_notification_error(error.to_string());
                    bindings.clear();
                    *notifier = None;
                }
            }
        }
    }
    state.write().await.record_report(&report);
}

async fn connect_notifications(
    sender: tokio::sync::mpsc::Sender<Result<NotificationEvent, crate::application::PortError>>,
    timeout: Duration,
    state: &Arc<RwLock<DaemonState>>,
) -> Option<ZbusNotificationSink> {
    match ZbusNotificationSink::connect(sender, timeout).await {
        Ok(sink) => {
            info!(
                server = sink.server_name(),
                vendor = sink.server_vendor(),
                version = sink.server_version(),
                specification = sink.specification_version(),
                actions = sink.supports_actions(),
                persistence = sink.supports_persistence(),
                "desktop notifications connected"
            );
            state.write().await.clear_notification_error();
            Some(sink)
        }
        Err(error) => {
            warn!(%error, "desktop notifications unavailable; monitoring continues");
            state
                .write()
                .await
                .record_notification_error(error.to_string());
            None
        }
    }
}

async fn handle_notification_event(
    event: NotificationEvent,
    bindings: &mut NotificationBindings,
    monitor: &mut MonitorService<SysinfoProcessSource, SystemClock>,
    notifier: &mut Option<ZbusNotificationSink>,
    repository: &TomlConfigRepository,
    settings: &mut crate::application::Settings,
    state: &Arc<RwLock<DaemonState>>,
) {
    let (notification_id, action) = match event {
        NotificationEvent::Action {
            notification_id,
            action,
        } => (notification_id, action),
        NotificationEvent::Closed {
            notification_id,
            reason,
        } => {
            record_notification_closed(bindings, notification_id, reason);
            return;
        }
        NotificationEvent::UnknownAction {
            notification_id,
            key,
        } => {
            record_unknown_notification_action(bindings, notification_id, &key);
            return;
        }
    };
    let Some(binding) = bindings.get(notification_id).cloned() else {
        warn!(
            notification_id,
            "ignoring action for an unknown notification"
        );
        return;
    };

    if let Some(next_binding) = binding.transition(action) {
        navigate_notification(notification_id, next_binding, bindings, notifier, state).await;
        return;
    }

    if matches!(
        action,
        NotificationAction::Details | NotificationAction::Back
    ) {
        warn!(
            notification_id,
            view = ?binding.view(),
            ?action,
            "ignoring invalid notification navigation"
        );
        return;
    }

    bindings.remove(notification_id);
    let monitored_event = binding.event();
    close_handled_notification(notifier, notification_id).await;

    match action {
        NotificationAction::Stop => {
            let identity = monitored_event.process.identity();
            let mut source = SysinfoProcessSource::new();
            let mut terminator = PidfdTerminationPort;
            let policy = settings.protection_policy();
            match StopProcess::new(&mut source, &mut terminator, current_user_id(), &policy)
                .execute(identity)
            {
                Ok(()) => info!(
                    pid = identity.pid(),
                    "SIGTERM sent from notification action"
                ),
                Err(error) => {
                    warn!(pid = identity.pid(), %error, "notification stop action rejected");
                }
            }
        }
        NotificationAction::IgnoreForHour => {
            monitor.ignore_for(monitored_event.process.identity(), Duration::from_hours(1));
            info!(
                pid = monitored_event.process.identity().pid(),
                "process ignored for one hour"
            );
        }
        NotificationAction::AlwaysIgnore => {
            let rule = IgnoreRule::for_process(&monitored_event.process);
            let mut updated = settings.clone();
            updated.add_ignore_rule(rule);
            match repository.save(&updated) {
                Ok(()) => {
                    *settings = updated;
                    monitor.ignore_permanently(&monitored_event.process);
                    info!(
                        pid = monitored_event.process.identity().pid(),
                        "process permanently ignored"
                    );
                }
                Err(error) => {
                    warn!(%error, "cannot persist permanent process ignore");
                    state
                        .write()
                        .await
                        .record_notification_error(error.to_string());
                }
            }
        }
        NotificationAction::Details | NotificationAction::Back => {}
    }
}

async fn close_handled_notification(
    notifier: &mut Option<ZbusNotificationSink>,
    notification_id: u32,
) {
    if let Some(sink) = notifier.as_mut()
        && let Err(error) = sink.close(notification_id).await
    {
        warn!(notification_id, %error, "cannot close handled desktop notification");
    }
}

fn record_unknown_notification_action(
    bindings: &NotificationBindings,
    notification_id: u32,
    key: &str,
) {
    if bindings.contains(notification_id) {
        warn!(notification_id, %key, "ignoring unknown notification action");
    }
}

fn record_notification_closed(
    bindings: &mut NotificationBindings,
    notification_id: u32,
    reason: crate::application::NotificationCloseReason,
) {
    bindings.remove(notification_id);
    info!(notification_id, ?reason, "desktop notification closed");
}

async fn navigate_notification(
    notification_id: u32,
    next_binding: NotificationBinding,
    bindings: &mut NotificationBindings,
    notifier: &mut Option<ZbusNotificationSink>,
    state: &Arc<RwLock<DaemonState>>,
) {
    let Some(sink) = notifier.as_mut() else {
        bindings.remove(notification_id);
        return;
    };
    match sink
        .notify(next_binding.request(), Some(notification_id))
        .await
    {
        Ok(replacement_id) => {
            bindings.remove(notification_id);
            bindings.remember(replacement_id, next_binding);
            state.write().await.clear_notification_error();
        }
        Err(error) => {
            warn!(%error, "cannot navigate desktop notification");
            state
                .write()
                .await
                .record_notification_error(error.to_string());
            bindings.clear();
            *notifier = None;
        }
    }
}

async fn shutdown_signal() -> Result<(), RuntimeError> {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|error| RuntimeError::io("register SIGTERM handler", error))?;
    tokio::select! {
        result = tokio::signal::ctrl_c() => {
            result.map_err(|error| RuntimeError::io("wait for interrupt signal", error))?;
        }
        _ = terminate.recv() => {}
    }
    Ok(())
}

async fn bind_control_socket(path: &Path) -> Result<UnixListener, RuntimeError> {
    prepare_runtime_directory()?;
    if fs::symlink_metadata(path).is_ok() {
        match UnixStream::connect(path).await {
            Ok(_) => return Err(RuntimeError::AlreadyRunning),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
                ) =>
            {
                fs::remove_file(path)
                    .map_err(|error| RuntimeError::io("remove stale control socket", error))?;
            }
            Err(error) => return Err(RuntimeError::io("inspect control socket", error)),
        }
    }

    let listener = UnixListener::bind(path).map_err(|error| {
        if error.kind() == io::ErrorKind::AddrInUse {
            RuntimeError::AlreadyRunning
        } else {
            RuntimeError::io("bind control socket", error)
        }
    })?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|error| RuntimeError::io("secure control socket", error))?;
    Ok(listener)
}

async fn handle_client(
    mut stream: UnixStream,
    state: Arc<RwLock<DaemonState>>,
) -> Result<(), RuntimeError> {
    let credentials = rustix::net::sockopt::socket_peercred(&stream)
        .map_err(|error| RuntimeError::io("read control peer credentials", error.into()))?;
    if credentials.uid.as_raw() != current_user_id() {
        return Err(RuntimeError::Protocol(
            "peer UID is not authorized".to_owned(),
        ));
    }

    let mut bytes = Vec::new();
    timeout(
        CONTROL_TIMEOUT,
        (&mut stream)
            .take(MAX_REQUEST_BYTES + 1)
            .read_to_end(&mut bytes),
    )
    .await
    .map_err(|_| RuntimeError::Protocol("request timed out".to_owned()))?
    .map_err(|error| RuntimeError::io("read control request", error))?;
    if bytes.len() as u64 > MAX_REQUEST_BYTES {
        return write_response(
            &mut stream,
            &ControlResponse::Error {
                message: "request is too large".to_owned(),
            },
        )
        .await;
    }

    let response = match serde_json::from_slice::<ControlRequest>(&bytes) {
        Ok(ControlRequest::Status) => ControlResponse::Status {
            status: state.read().await.status(),
        },
        Ok(ControlRequest::Top) => ControlResponse::Top {
            top: state.read().await.top(),
        },
        Err(error) => ControlResponse::Error {
            message: format!("invalid request: {error}"),
        },
    };
    write_response(&mut stream, &response).await
}

async fn write_response(
    stream: &mut UnixStream,
    response: &ControlResponse,
) -> Result<(), RuntimeError> {
    let bytes = serde_json::to_vec(response)
        .map_err(|error| RuntimeError::Protocol(format!("cannot encode response: {error}")))?;
    stream
        .write_all(&bytes)
        .await
        .map_err(|error| RuntimeError::io("write control response", error))?;
    stream
        .shutdown()
        .await
        .map_err(|error| RuntimeError::io("close control response", error))
}

async fn query_status_async() -> Result<StatusResponse, RuntimeError> {
    match query(ControlRequest::Status).await? {
        ControlResponse::Status { status } => Ok(status),
        ControlResponse::Error { message } => Err(RuntimeError::Protocol(message)),
        ControlResponse::Top { .. } => Err(RuntimeError::Protocol(
            "daemon returned top data for a status request".to_owned(),
        )),
    }
}

async fn query_top_async() -> Result<TopResponse, RuntimeError> {
    match query(ControlRequest::Top).await? {
        ControlResponse::Top { top } => Ok(top),
        ControlResponse::Error { message } => Err(RuntimeError::Protocol(message)),
        ControlResponse::Status { .. } => Err(RuntimeError::Protocol(
            "daemon returned status data for a top request".to_owned(),
        )),
    }
}

async fn query(request: ControlRequest) -> Result<ControlResponse, RuntimeError> {
    let path = control_socket_path()?;
    let mut stream = timeout(CONTROL_TIMEOUT, UnixStream::connect(&path))
        .await
        .map_err(|_| RuntimeError::Protocol("connection timed out".to_owned()))?
        .map_err(|error| RuntimeError::io("connect to daemon", error))?;
    let request = serde_json::to_vec(&request)
        .map_err(|error| RuntimeError::Protocol(format!("cannot encode request: {error}")))?;
    stream
        .write_all(&request)
        .await
        .map_err(|error| RuntimeError::io("write control request", error))?;
    stream
        .shutdown()
        .await
        .map_err(|error| RuntimeError::io("close control request", error))?;

    let mut bytes = Vec::new();
    timeout(
        CONTROL_TIMEOUT,
        (&mut stream)
            .take(MAX_RESPONSE_BYTES + 1)
            .read_to_end(&mut bytes),
    )
    .await
    .map_err(|_| RuntimeError::Protocol("response timed out".to_owned()))?
    .map_err(|error| RuntimeError::io("read control response", error))?;
    if bytes.len() as u64 > MAX_RESPONSE_BYTES {
        return Err(RuntimeError::Protocol("response is too large".to_owned()));
    }
    serde_json::from_slice(&bytes)
        .map_err(|error| RuntimeError::Protocol(format!("invalid response: {error}")))
}

struct SocketGuard {
    path: PathBuf,
}

impl SocketGuard {
    fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl Drop for SocketGuard {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_file(&self.path)
            && error.kind() != io::ErrorKind::NotFound
        {
            warn!(path = %self.path.display(), %error, "cannot remove control socket");
        }
    }
}
