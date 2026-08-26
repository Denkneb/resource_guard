use std::time::Instant;

use crate::application::MonitorReport;

use super::{
    StatusResponse,
    protocol::{TopProcess, TopResponse},
};

#[derive(Debug)]
pub(crate) struct DaemonState {
    started_at: Instant,
    last_poll_at: Option<Instant>,
    total_memory_bytes: u64,
    available_memory_bytes: u64,
    total_swap_bytes: u64,
    used_swap_bytes: u64,
    observed_processes: usize,
    monitored_processes: usize,
    active_events: usize,
    processes: Vec<TopProcess>,
    last_error: Option<String>,
    notification_error: Option<String>,
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
            observed_processes: 0,
            monitored_processes: 0,
            active_events: 0,
            processes: Vec::new(),
            last_error: None,
            notification_error: None,
        }
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
