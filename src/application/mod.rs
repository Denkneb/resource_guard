mod background_workloads;
mod config;
mod emergency;
mod memory_pressure;
mod monitor;
mod notifications;
mod ports;
mod process_control;
mod stale_workloads;

pub use background_workloads::{
    BackgroundWorkloadService, background_workload_from_root, build_background_workloads,
};
pub use config::{
    BackgroundWorkloadSettings, ConfigValidationError, EmergencySettings, MemoryPressureSettings,
    MonitorSettings, NotificationSettings, ProcessSettings, Settings, StaleWorkloadSettings,
    TerminationSettings,
};
pub use emergency::EmergencyService;
pub use memory_pressure::MemoryPressureMonitor;
pub use monitor::{MonitorEvent, MonitorReport, MonitorService, MonitoredProcess};
pub use notifications::{
    NotificationAction, NotificationActionSet, NotificationBinding, NotificationBindings,
    NotificationCloseReason, NotificationDispatch, NotificationEvent, NotificationRequest,
    NotificationSink, NotificationView, plan_notification,
};
pub use ports::{
    ForceTerminationPort, MemoryPressureSource, MonotonicClock, ObservedProcess, PortError,
    ProcessSource, ResourceSnapshot, Sleeper, TerminationPort,
};
pub use process_control::{
    ForceStopProcess, StopAndWait, StopError, StopOutcome, StopProcess, StopWorkload,
    StopWorkloadGroup, WaitForExit,
};
pub use stale_workloads::{
    StaleWorkloadDetection, StaleWorkloadEvaluation, StaleWorkloadService, detect_workloads,
    workload_from_root,
};
