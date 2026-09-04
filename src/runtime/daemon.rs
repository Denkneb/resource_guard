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
        PidfdTerminationPort, ProcMemoryPressureSource, SysinfoProcessSource, SystemClock,
        TomlConfigRepository, ZbusNotificationSink, current_user_id,
    },
    application::{
        EmergencyService, ForceStopProcess, MemoryPressureMonitor, MonitorService,
        NotificationAction, NotificationBinding, NotificationBindings, NotificationEvent,
        NotificationRequest, NotificationSink, NotificationView, ProcessSource,
        StaleWorkloadService, StopProcess, StopWorkload,
    },
    domain::{
        EmergencyAction, EmergencyCandidate, IgnoreRule, MemoryPressureLevel,
        force_termination_permitted,
    },
};

use super::{
    RuntimeError,
    paths::{control_socket_path, prepare_runtime_directory},
    protocol::{ControlRequest, ControlResponse, StaleResponse, StatusResponse, TopResponse},
    state::DaemonState,
};

const MAX_REQUEST_BYTES: u64 = 8 * 1_024;
const MAX_RESPONSE_BYTES: u64 = 1_024 * 1_024;
const CONTROL_TIMEOUT: Duration = Duration::from_secs(2);
const NOTIFICATION_RETRY_INTERVAL: Duration = Duration::from_secs(60);
const MAX_NOTIFICATION_BINDINGS: usize = 256;

#[derive(Debug)]
struct PendingEmergency {
    candidate: EmergencyCandidate,
    force_at: tokio::time::Instant,
}

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

/// Fetches workload trees currently classified as stale by the daemon.
///
/// # Errors
///
/// Returns an error when the runtime path is unavailable, the daemon cannot be
/// reached, or its response is invalid.
pub fn query_stale() -> Result<StaleResponse, RuntimeError> {
    build_runtime()?.block_on(query_stale_async())
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

#[allow(clippy::too_many_lines)]
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
    let mut pressure_monitor = MemoryPressureMonitor::new(
        ProcMemoryPressureSource::new(),
        settings.memory_pressure_policy(),
    );
    let mut emergency = EmergencyService::new(
        current_user_id(),
        settings.protection_policy(),
        settings.emergency_policy(),
        settings.emergency.action_cooldown,
    );
    let mut stale_workloads =
        StaleWorkloadService::new(current_user_id(), settings.stale_workload_policy());
    let emergency_epoch = std::time::Instant::now();
    let mut pending_emergency = None;
    let mut last_emergency_scan = None;
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
    let pressure_sleep = tokio::time::sleep(Duration::ZERO);
    tokio::pin!(pressure_sleep);
    let shutdown = shutdown_signal();
    tokio::pin!(shutdown);

    info!(socket = %socket_path.display(), "resource guard daemon started");
    loop {
        tokio::select! {
            () = &mut pressure_sleep => {
                let level = handle_memory_pressure(
                    &mut pressure_monitor,
                    &mut emergency,
                    &mut pending_emergency,
                    &mut last_emergency_scan,
                    emergency_epoch.elapsed(),
                    &settings,
                    &mut notifier,
                    &state,
                ).await;
                pressure_sleep.as_mut().reset(
                    tokio::time::Instant::now() + pressure_poll_interval(&settings, level),
                );
            }
            _ = interval.tick() => {
                match monitor.poll() {
                    Ok(report) => record_monitor_report(
                        report,
                        &mut stale_workloads,
                        emergency_epoch.elapsed(),
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
                            &mut stale_workloads,
                            emergency_epoch.elapsed(),
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

const fn pressure_poll_interval(
    settings: &crate::application::Settings,
    level: MemoryPressureLevel,
) -> Duration {
    match level {
        MemoryPressureLevel::Normal | MemoryPressureLevel::Recovery => {
            settings.monitor.poll_interval
        }
        MemoryPressureLevel::Warning => settings.memory_pressure.warning_poll_interval,
        MemoryPressureLevel::Critical => settings.memory_pressure.critical_poll_interval,
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_memory_pressure(
    pressure_monitor: &mut MemoryPressureMonitor<ProcMemoryPressureSource>,
    emergency: &mut EmergencyService,
    pending: &mut Option<PendingEmergency>,
    last_emergency_scan: &mut Option<tokio::time::Instant>,
    now: Duration,
    settings: &crate::application::Settings,
    notifier: &mut Option<ZbusNotificationSink>,
    state: &Arc<RwLock<DaemonState>>,
) -> MemoryPressureLevel {
    let evaluation = match pressure_monitor.poll() {
        Ok(evaluation) => evaluation,
        Err(error) => {
            record_poll_error(state, error).await;
            return MemoryPressureLevel::Normal;
        }
    };
    let activation = settings.emergency_activation_policy();
    let automatic_action_permitted = activation.permits(evaluation);
    let permission_changed = state.write().await.record_pressure(
        evaluation,
        automatic_action_permitted,
        activation.action_available_bytes,
        activation.action_psi_full_avg10,
    );

    if evaluation.changed() || permission_changed {
        warn!(
            previous = ?evaluation.previous,
            current = ?evaluation.current,
            available_bytes = evaluation.sample.system.available_memory_bytes,
            swap_used_bytes = evaluation.sample.system.used_swap_bytes,
            psi_some_avg10 = evaluation.sample.psi.some_avg10,
            psi_full_avg10 = evaluation.sample.psi.full_avg10,
            reason = evaluation.reason(),
            automatic_action_permitted,
            "system memory pressure changed"
        );
    }

    let mut outcome = None;
    if !automatic_action_permitted {
        if pending.take().is_some() {
            outcome =
                Some("automatic emergency signal cleared before forceful termination".to_owned());
        }
    } else if pending
        .as_ref()
        .is_some_and(|pending| tokio::time::Instant::now() >= pending.force_at)
    {
        let completed = pending.take().expect("pending emergency was checked above");
        outcome = Some(
            finish_pending_emergency(completed, automatic_action_permitted, settings, state).await,
        );
    }

    let scan_due = last_emergency_scan
        .is_none_or(|last| last.elapsed() >= settings.memory_pressure.warning_poll_interval);
    if automatic_action_permitted
        && pending.is_none()
        && settings.emergency.action != EmergencyAction::NotifyOnly
        && scan_due
    {
        *last_emergency_scan = Some(tokio::time::Instant::now());
        let mut inventory = SysinfoProcessSource::new();
        match inventory.snapshot() {
            Ok(snapshot) => {
                if let Some(candidate) =
                    emergency.consider(automatic_action_permitted, &snapshot.processes, now, false)
                {
                    outcome = Some(
                        start_emergency_termination(candidate, settings, pending, state).await,
                    );
                }
            }
            Err(error) => record_poll_error(state, error).await,
        }
    }

    if ((evaluation.changed() || permission_changed)
        && evaluation.current != MemoryPressureLevel::Normal)
        || outcome.is_some()
    {
        send_pressure_notification(
            evaluation,
            outcome.as_deref(),
            automatic_action_permitted,
            activation.action_available_bytes,
            activation.action_psi_full_avg10,
            notifier,
            state,
        )
        .await;
    }

    evaluation.current
}

async fn start_emergency_termination(
    candidate: EmergencyCandidate,
    settings: &crate::application::Settings,
    pending: &mut Option<PendingEmergency>,
    state: &Arc<RwLock<DaemonState>>,
) -> String {
    let identity = candidate.process.identity();
    let mut source = SysinfoProcessSource::new();
    let mut terminator = PidfdTerminationPort;
    match StopProcess::new(
        &mut source,
        &mut terminator,
        current_user_id(),
        &settings.protection_policy(),
    )
    .execute(identity)
    {
        Ok(()) => {
            let outcome = format!(
                "SIGTERM sent to {} ({})",
                candidate.process.name(),
                identity.pid()
            );
            *pending = Some(PendingEmergency {
                candidate,
                force_at: tokio::time::Instant::now() + settings.emergency.term_grace_period,
            });
            state.write().await.record_emergency_action(outcome.clone());
            warn!(pid = identity.pid(), "emergency SIGTERM sent");
            outcome
        }
        Err(error) => {
            let outcome = format!("emergency SIGTERM rejected for {}: {error}", identity.pid());
            warn!(pid = identity.pid(), %error, "emergency SIGTERM rejected");
            outcome
        }
    }
}

async fn finish_pending_emergency(
    pending: PendingEmergency,
    automatic_action_permitted: bool,
    settings: &crate::application::Settings,
    state: &Arc<RwLock<DaemonState>>,
) -> String {
    let identity = pending.candidate.process.identity();
    let outcome = if force_termination_permitted(
        automatic_action_permitted,
        settings.emergency.allow_sigkill,
    ) {
        let mut source = SysinfoProcessSource::new();
        let mut terminator = PidfdTerminationPort;
        match ForceStopProcess::new(
            &mut source,
            &mut terminator,
            current_user_id(),
            &settings.protection_policy(),
        )
        .execute(identity)
        {
            Ok(()) => format!(
                "SIGKILL sent to {} ({}) after persistent critical pressure",
                pending.candidate.process.name(),
                identity.pid()
            ),
            Err(crate::application::StopError::NotFound { .. }) => format!(
                "{} ({}) exited after SIGTERM",
                pending.candidate.process.name(),
                identity.pid()
            ),
            Err(error) => format!("emergency SIGKILL rejected for {}: {error}", identity.pid()),
        }
    } else {
        format!(
            "{} ({}) survived SIGTERM; automatic SIGKILL is disabled",
            pending.candidate.process.name(),
            identity.pid()
        )
    };
    state.write().await.record_emergency_action(outcome.clone());
    warn!(pid = identity.pid(), %outcome, "emergency action completed");
    outcome
}

async fn send_pressure_notification(
    evaluation: crate::domain::MemoryPressureEvaluation,
    outcome: Option<&str>,
    automatic_action_permitted: bool,
    action_available_bytes: u64,
    action_psi_full_avg10: f32,
    notifier: &mut Option<ZbusNotificationSink>,
    state: &Arc<RwLock<DaemonState>>,
) {
    let Some(sink) = notifier.as_mut() else {
        return;
    };
    if let Err(error) = sink
        .notify(
            NotificationRequest::for_pressure(
                evaluation,
                outcome,
                automatic_action_permitted,
                action_available_bytes,
                action_psi_full_avg10,
            ),
            None,
        )
        .await
    {
        warn!(%error, "memory pressure notification failed");
        state
            .write()
            .await
            .record_notification_error(error.to_string());
        *notifier = None;
    }
}

async fn record_poll_error(state: &Arc<RwLock<DaemonState>>, error: crate::application::PortError) {
    warn!(%error, "resource polling failed");
    state.write().await.record_error(error.to_string());
}

async fn record_monitor_report(
    report: crate::application::MonitorReport,
    stale_service: &mut StaleWorkloadService,
    now: Duration,
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
    let pressure = state.read().await.pressure_level();
    let observed = report
        .processes
        .iter()
        .map(|process| process.observed.clone())
        .collect::<Vec<_>>();
    let (stale_candidates, notifications) = stale_service.evaluate(&observed, pressure, now);
    state
        .write()
        .await
        .record_stale_workloads(&stale_candidates);
    for workload in notifications {
        warn!(
            pid = workload.identity().pid(),
            process = workload.root.name(),
            process_count = workload.process_count(),
            memory_bytes = workload.total_memory_bytes,
            cpu_percent = workload.total_cpu_percent,
            "suspected stale workload detected"
        );
        if let Some(sink) = notifier.as_mut() {
            match sink
                .notify(
                    NotificationRequest::for_stale_workload(&workload, NotificationView::Summary),
                    None,
                )
                .await
            {
                Ok(notification_id) => bindings.remember(
                    notification_id,
                    NotificationBinding::for_workload(workload, NotificationView::Summary),
                ),
                Err(error) => {
                    warn!(%error, "stale workload notification failed");
                    state
                        .write()
                        .await
                        .record_notification_error(error.to_string());
                    bindings.clear();
                    *notifier = None;
                    break;
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

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
async fn handle_notification_event(
    event: NotificationEvent,
    bindings: &mut NotificationBindings,
    monitor: &mut MonitorService<SysinfoProcessSource, SystemClock>,
    stale_service: &mut StaleWorkloadService,
    now: Duration,
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
    close_handled_notification(notifier, notification_id).await;

    match action {
        NotificationAction::Stop => {
            let mut source = SysinfoProcessSource::new();
            let mut terminator = PidfdTerminationPort;
            let policy = settings.protection_policy();
            if let Some(workload) = binding.workload() {
                let identity = workload.identity();
                match StopWorkload::new(&mut source, &mut terminator, current_user_id(), &policy)
                    .execute(workload)
                {
                    Ok(count) => info!(
                        pid = identity.pid(),
                        count, "SIGTERM sent to stale workload"
                    ),
                    Err(error) => warn!(pid = identity.pid(), %error, "workload stop rejected"),
                }
            } else if let Some(monitored_event) = binding.event() {
                let identity = monitored_event.process.identity();
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
        }
        NotificationAction::IgnoreForHour => {
            if let Some(workload) = binding.workload() {
                stale_service.ignore_for(workload.identity(), now + Duration::from_hours(1));
                info!(
                    pid = workload.identity().pid(),
                    "workload ignored for one hour"
                );
            } else if let Some(monitored_event) = binding.event() {
                monitor.ignore_for(monitored_event.process.identity(), Duration::from_hours(1));
                info!(
                    pid = monitored_event.process.identity().pid(),
                    "process ignored for one hour"
                );
            }
        }
        NotificationAction::AlwaysIgnore => {
            let mut updated = settings.clone();
            if let Some(workload) = binding.workload() {
                updated.add_stale_workload_ignore(workload.root.name().to_owned());
            } else if let Some(monitored_event) = binding.event() {
                updated.add_ignore_rule(IgnoreRule::for_process(&monitored_event.process));
            }
            match repository.save(&updated) {
                Ok(()) => {
                    *settings = updated;
                    if let Some(workload) = binding.workload() {
                        stale_service.ignore_name(workload.root.name().to_owned());
                        info!(
                            pid = workload.identity().pid(),
                            "workload permanently ignored"
                        );
                    } else if let Some(monitored_event) = binding.event() {
                        monitor.ignore_permanently(&monitored_event.process);
                        info!(
                            pid = monitored_event.process.identity().pid(),
                            "process permanently ignored"
                        );
                    }
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
        Ok(ControlRequest::Stale) => ControlResponse::Stale {
            stale: state.read().await.stale(),
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
        ControlResponse::Stale { .. } => Err(RuntimeError::Protocol(
            "daemon returned stale data for a status request".to_owned(),
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
        ControlResponse::Stale { .. } => Err(RuntimeError::Protocol(
            "daemon returned stale data for a top request".to_owned(),
        )),
    }
}

async fn query_stale_async() -> Result<StaleResponse, RuntimeError> {
    match query(ControlRequest::Stale).await? {
        ControlResponse::Stale { stale } => Ok(stale),
        ControlResponse::Error { message } => Err(RuntimeError::Protocol(message)),
        ControlResponse::Status { .. } => Err(RuntimeError::Protocol(
            "daemon returned status data for a stale request".to_owned(),
        )),
        ControlResponse::Top { .. } => Err(RuntimeError::Protocol(
            "daemon returned top data for a stale request".to_owned(),
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
