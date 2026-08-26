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
    adapters::{SysinfoProcessSource, SystemClock, TomlConfigRepository, current_user_id},
    application::MonitorService,
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
    let settings = repository.load()?.settings;
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
    let mut interval = tokio::time::interval(settings.monitor.poll_interval);
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let shutdown = shutdown_signal();
    tokio::pin!(shutdown);

    info!(socket = %socket_path.display(), "resource guard daemon started");
    loop {
        tokio::select! {
            _ = interval.tick() => {
                match monitor.poll() {
                    Ok(report) => {
                        for event in &report.events {
                            warn!(
                                pid = event.process.identity().pid(),
                                process = event.process.name(),
                                cpu_percent = event.resources.cpu_percent,
                                memory_bytes = event.resources.resident_memory_bytes,
                                exceeded_seconds = event.exceeded_for.as_secs(),
                                "process exceeded configured resource limits"
                            );
                        }
                        state.write().await.record_report(&report);
                    }
                    Err(error) => {
                        warn!(%error, "resource polling failed");
                        state.write().await.record_error(error.to_string());
                    }
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
            result = &mut shutdown => {
                result?;
                break;
            }
        }
    }
    info!("resource guard daemon stopped");
    Ok(())
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
