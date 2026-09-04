use std::time::Instant;

use crate::application::MonitorReport;
use crate::domain::{MemoryPressureEvaluation, MemoryPressureLevel, StaleWorkload};

use super::{
    StatusResponse,
    protocol::{StaleResponse, StaleWorkloadSummary, TopProcess, TopResponse},
};

#[derive(Debug)]
pub(crate) struct DaemonState {
    started_at: Instant,
    last_poll_at: Option<Instant>,
    total_memory_bytes: u64,
    available_memory_bytes: u64,
    total_swap_bytes: u64,
    used_swap_bytes: u64,
    memory_pressure_level: MemoryPressureLevel,
    memory_pressure_reason: String,
    automatic_emergency_action_permitted: bool,
    emergency_action_available_bytes: u64,
    emergency_action_psi_full_avg10: f32,
    memory_psi_some_avg10: f32,
    memory_psi_full_avg10: f32,
    last_emergency_action: Option<String>,
    observed_processes: usize,
    monitored_processes: usize,
    active_events: usize,
    processes: Vec<TopProcess>,
    last_error: Option<String>,
    notification_error: Option<String>,
    stale_workloads: Vec<StaleWorkloadSummary>,
}

impl DaemonState {
    pub(crate) fn new() -> Self {
        Self {
            started_at: Instant::now(),
            last_poll_at: None,
            total_memory_bytes: 0,
            available_memory_bytes: 0,
            total_swap_bytes: 0,
            used_swap_bytes: 0,
            memory_pressure_level: MemoryPressureLevel::Normal,
            memory_pressure_reason: "none".to_owned(),
            automatic_emergency_action_permitted: false,
            emergency_action_available_bytes: 0,
            emergency_action_psi_full_avg10: 0.0,
            memory_psi_some_avg10: 0.0,
            memory_psi_full_avg10: 0.0,
            last_emergency_action: None,
            observed_processes: 0,
            monitored_processes: 0,
            active_events: 0,
            processes: Vec::new(),
            last_error: None,
            notification_error: None,
            stale_workloads: Vec::new(),
        }
    }

    pub(crate) fn record_pressure(
        &mut self,
        evaluation: MemoryPressureEvaluation,
        automatic_action_permitted: bool,
        action_available_bytes: u64,
        action_psi_full_avg10: f32,
    ) -> bool {
        let permission_changed =
            self.automatic_emergency_action_permitted != automatic_action_permitted;
        self.last_poll_at = Some(Instant::now());
        self.total_memory_bytes = evaluation.sample.system.total_memory_bytes;
        self.available_memory_bytes = evaluation.sample.system.available_memory_bytes;
        self.total_swap_bytes = evaluation.sample.system.total_swap_bytes;
        self.used_swap_bytes = evaluation.sample.system.used_swap_bytes;
        self.memory_pressure_level = evaluation.current;
        evaluation
            .reason()
            .clone_into(&mut self.memory_pressure_reason);
        self.automatic_emergency_action_permitted = automatic_action_permitted;
        self.emergency_action_available_bytes = action_available_bytes;
        self.emergency_action_psi_full_avg10 = action_psi_full_avg10;
        self.memory_psi_some_avg10 = evaluation.sample.psi.some_avg10;
        self.memory_psi_full_avg10 = evaluation.sample.psi.full_avg10;
        self.last_error = None;
        permission_changed
    }

    pub(crate) fn record_emergency_action(&mut self, action: impl Into<String>) {
        self.last_emergency_action = Some(action.into());
    }

    pub(crate) fn record_report(&mut self, report: &MonitorReport) {
        self.last_poll_at = Some(Instant::now());
        self.total_memory_bytes = report.system.total_memory_bytes;
        self.available_memory_bytes = report.system.available_memory_bytes;
        self.total_swap_bytes = report.system.total_swap_bytes;
        self.used_swap_bytes = report.system.used_swap_bytes;
        self.observed_processes = report.observed_processes;
        self.monitored_processes = report.monitored_processes;
        self.active_events = report.events.len();
        self.processes = report
            .processes
            .iter()
            .map(|process| TopProcess {
                pid: process.observed.descriptor.identity().pid(),
                name: process.observed.descriptor.name().to_owned(),
                cpu_percent: if process.observed.resources.cpu_percent.is_finite() {
                    process.observed.resources.cpu_percent
                } else {
                    0.0
                },
                resident_memory_bytes: process.observed.resources.resident_memory_bytes,
                running_for_seconds: process.observed.resources.running_for.as_secs(),
                exceeds_limit: process.breach.any(),
            })
            .collect();
        self.last_error = None;
    }

    pub(crate) fn record_error(&mut self, error: impl Into<String>) {
        self.last_poll_at = Some(Instant::now());
        self.last_error = Some(error.into());
    }

    pub(crate) fn status(&self) -> StatusResponse {
        StatusResponse {
            uptime_seconds: self.started_at.elapsed().as_secs(),
            last_poll_age_seconds: self.last_poll_at.map_or(0, |at| at.elapsed().as_secs()),
            total_memory_bytes: self.total_memory_bytes,
            available_memory_bytes: self.available_memory_bytes,
            total_swap_bytes: self.total_swap_bytes,
            used_swap_bytes: self.used_swap_bytes,
            memory_pressure_level: pressure_level_name(self.memory_pressure_level).to_owned(),
            memory_pressure_reason: self.memory_pressure_reason.clone(),
            automatic_emergency_action_permitted: self.automatic_emergency_action_permitted,
            emergency_action_available_bytes: self.emergency_action_available_bytes,
            emergency_action_psi_full_avg10: self.emergency_action_psi_full_avg10,
            memory_psi_some_avg10: self.memory_psi_some_avg10,
            memory_psi_full_avg10: self.memory_psi_full_avg10,
            last_emergency_action: self.last_emergency_action.clone(),
            observed_processes: self.observed_processes,
            monitored_processes: self.monitored_processes,
            active_events: self.active_events,
            last_error: self.last_error.clone(),
            notification_error: self.notification_error.clone(),
        }
    }

    pub(crate) fn record_notification_error(&mut self, error: impl Into<String>) {
        self.notification_error = Some(error.into());
    }

    pub(crate) const fn pressure_level(&self) -> MemoryPressureLevel {
        self.memory_pressure_level
    }

    pub(crate) fn record_stale_workloads(&mut self, workloads: &[StaleWorkload]) {
        self.stale_workloads = workloads
            .iter()
            .map(|workload| StaleWorkloadSummary {
                root_pid: workload.identity().pid(),
                name: workload.root.name().to_owned(),
                process_count: workload.process_count(),
                total_memory_bytes: workload.total_memory_bytes,
                total_cpu_percent: workload.total_cpu_percent,
                age_seconds: workload.age.as_secs(),
            })
            .collect();
    }

    pub(crate) fn stale(&self) -> StaleResponse {
        StaleResponse {
            workloads: self.stale_workloads.clone(),
        }
    }

    pub(crate) fn clear_notification_error(&mut self) {
        self.notification_error = None;
    }

    pub(crate) fn top(&self) -> TopResponse {
        let mut processes = self.processes.clone();
        processes.sort_by(|left, right| {
            right
                .cpu_percent
                .total_cmp(&left.cpu_percent)
                .then_with(|| right.resident_memory_bytes.cmp(&left.resident_memory_bytes))
                .then_with(|| left.pid.cmp(&right.pid))
        });
        TopResponse {
            sample_age_seconds: self.last_poll_at.map_or(0, |at| at.elapsed().as_secs()),
            processes,
        }
    }
}

const fn pressure_level_name(level: MemoryPressureLevel) -> &'static str {
    match level {
        MemoryPressureLevel::Normal => "normal",
        MemoryPressureLevel::Warning => "warning",
        MemoryPressureLevel::Critical => "critical",
        MemoryPressureLevel::Recovery => "recovery",
    }
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, time::Duration};

    use super::DaemonState;
    use crate::{
        application::{MonitorReport, MonitoredProcess, ObservedProcess},
        domain::{
            ProcessDescriptor, ProcessIdentity, ProcessResources, ResourceBreach, SystemResources,
        },
    };

    fn process(pid: u32, cpu_percent: f32, memory: u64, breached: bool) -> MonitoredProcess {
        MonitoredProcess {
            observed: ObservedProcess {
                descriptor: ProcessDescriptor::new(
                    ProcessIdentity::new(pid, 1_000, u64::from(pid)),
                    format!("worker-{pid}"),
                    Some(PathBuf::from(format!("/usr/bin/worker-{pid}"))),
                ),
                resources: ProcessResources {
                    cpu_percent,
                    resident_memory_bytes: memory,
                    virtual_memory_bytes: memory * 2,
                    running_for: Duration::from_secs(90),
                    observed_at: Duration::ZERO,
                },
            },
            breach: ResourceBreach {
                cpu: breached,
                memory: false,
            },
        }
    }

    #[test]
    fn top_is_sorted_by_cpu_then_memory() {
        let processes = vec![
            process(10, 25.0, 1_024, false),
            process(11, 50.0, 2_048, true),
            process(12, 50.0, 4_096, false),
        ];
        let mut state = DaemonState::new();
        state.record_report(&MonitorReport {
            system: SystemResources {
                total_memory_bytes: 8_192,
                available_memory_bytes: 4_096,
                total_swap_bytes: 0,
                used_swap_bytes: 0,
            },
            observed_processes: 3,
            monitored_processes: 3,
            processes,
            events: Vec::new(),
        });

        let top = state.top();
        assert_eq!(
            top.processes
                .iter()
                .map(|process| process.pid)
                .collect::<Vec<_>>(),
            vec![12, 11, 10]
        );
        assert!(top.processes[1].exceeds_limit);
        assert_eq!(top.processes[1].running_for_seconds, 90);
    }
}
