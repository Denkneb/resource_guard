use std::time::Instant;

use crate::application::MonitorReport;

use super::StatusResponse;

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
    last_error: Option<String>,
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
            last_error: None,
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
        }
    }
}
