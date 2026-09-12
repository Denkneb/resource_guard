use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub(crate) enum ControlRequest {
    Status,
    Top,
    Stale,
    Background,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub(crate) enum ControlResponse {
    Status { status: StatusResponse },
    Top { top: TopResponse },
    Stale { stale: StaleResponse },
    Background { background: BackgroundResponse },
    Error { message: String },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct StaleResponse {
    pub workloads: Vec<StaleWorkloadSummary>,
    #[serde(default)]
    pub groups: Vec<StaleWorkloadGroupSummary>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct StaleWorkloadSummary {
    pub root_pid: u32,
    /// Root UID and Linux start time used to revalidate identity before a stop.
    /// Optional so a response from an older daemon still renders for viewing,
    /// while `stop-tree` refuses to signal a workload whose identity cannot be
    /// verified.
    #[serde(default)]
    pub root_uid: Option<u32>,
    #[serde(default)]
    pub root_started_at: Option<u64>,
    pub name: String,
    pub process_count: usize,
    pub total_memory_bytes: u64,
    pub total_cpu_percent: f32,
    pub age_seconds: u64,
}

/// Reporting-only aggregate of independent stale trees sharing an exact working
/// directory. It intentionally carries no termination or signalling handle.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct StaleWorkloadGroupSummary {
    pub working_directory: PathBuf,
    pub tree_count: usize,
    pub process_count: usize,
    pub total_memory_bytes: u64,
    pub total_cpu_percent: f32,
    pub age_seconds: u64,
    pub root_pids: Vec<u32>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct BackgroundResponse {
    pub workloads: Vec<BackgroundWorkloadSummary>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct BackgroundWorkloadSummary {
    pub group_id: String,
    pub root_pid: u32,
    pub root_uid: u32,
    pub root_started_at: u64,
    pub name: String,
    pub executable: Option<PathBuf>,
    pub process_count: usize,
    pub process_count_growth: usize,
    pub total_memory_bytes: u64,
    pub memory_growth_bytes: u64,
    pub total_cpu_percent: f32,
    pub age_seconds: u64,
    pub observed_for_seconds: u64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct StatusResponse {
    pub uptime_seconds: u64,
    pub last_poll_age_seconds: u64,
    pub total_memory_bytes: u64,
    pub available_memory_bytes: u64,
    pub total_swap_bytes: u64,
    pub used_swap_bytes: u64,
    pub memory_pressure_level: String,
    pub memory_pressure_reason: String,
    pub automatic_emergency_action_permitted: bool,
    pub emergency_action_available_bytes: u64,
    pub emergency_action_psi_full_avg10: f32,
    pub memory_psi_some_avg10: f32,
    pub memory_psi_full_avg10: f32,
    pub last_emergency_action: Option<String>,
    pub observed_processes: usize,
    pub monitored_processes: usize,
    pub active_events: usize,
    pub last_error: Option<String>,
    pub notification_error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct TopResponse {
    pub sample_age_seconds: u64,
    pub processes: Vec<TopProcess>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct TopProcess {
    pub pid: u32,
    pub name: String,
    pub cpu_percent: f32,
    pub resident_memory_bytes: u64,
    pub running_for_seconds: u64,
    pub exceeds_limit: bool,
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{
        BackgroundResponse, BackgroundWorkloadSummary, ControlRequest, ControlResponse,
        StaleResponse, StaleWorkloadGroupSummary, StaleWorkloadSummary, StatusResponse, TopProcess,
        TopResponse,
    };

    #[test]
    fn status_protocol_round_trips() {
        let request = serde_json::to_string(&ControlRequest::Status).unwrap();
        assert_eq!(request, r#"{"command":"status"}"#);

        let response = ControlResponse::Status {
            status: StatusResponse {
                uptime_seconds: 1,
                last_poll_age_seconds: 0,
                total_memory_bytes: 2,
                available_memory_bytes: 1,
                total_swap_bytes: 0,
                used_swap_bytes: 0,
                memory_pressure_level: "normal".to_owned(),
                memory_pressure_reason: "none".to_owned(),
                automatic_emergency_action_permitted: false,
                emergency_action_available_bytes: 1_024 * 1_024 * 1_024,
                emergency_action_psi_full_avg10: 5.0,
                memory_psi_some_avg10: 0.0,
                memory_psi_full_avg10: 0.0,
                last_emergency_action: None,
                observed_processes: 3,
                monitored_processes: 2,
                active_events: 0,
                last_error: None,
                notification_error: None,
            },
        };
        let encoded = serde_json::to_vec(&response).unwrap();
        assert!(matches!(
            serde_json::from_slice(&encoded).unwrap(),
            ControlResponse::Status { .. }
        ));
    }

    #[test]
    fn top_protocol_round_trips() {
        let request = serde_json::to_string(&ControlRequest::Top).unwrap();
        assert_eq!(request, r#"{"command":"top"}"#);

        let response = ControlResponse::Top {
            top: TopResponse {
                sample_age_seconds: 1,
                processes: vec![TopProcess {
                    pid: 42,
                    name: "worker".to_owned(),
                    cpu_percent: 75.0,
                    resident_memory_bytes: 4096,
                    running_for_seconds: 60,
                    exceeds_limit: true,
                }],
            },
        };
        let encoded = serde_json::to_vec(&response).unwrap();
        assert!(matches!(
            serde_json::from_slice(&encoded).unwrap(),
            ControlResponse::Top { .. }
        ));
    }

    #[test]
    fn stale_protocol_round_trips() {
        let request = serde_json::to_string(&ControlRequest::Stale).unwrap();
        assert_eq!(request, r#"{"command":"stale"}"#);
        let response = ControlResponse::Stale {
            stale: StaleResponse {
                workloads: vec![StaleWorkloadSummary {
                    root_pid: 42,
                    root_uid: Some(1_000),
                    root_started_at: Some(99),
                    name: "pytest".to_owned(),
                    process_count: 3,
                    total_memory_bytes: 4096,
                    total_cpu_percent: 0.2,
                    age_seconds: 3600,
                }],
                groups: vec![StaleWorkloadGroupSummary {
                    working_directory: PathBuf::from("/work/project"),
                    tree_count: 2,
                    process_count: 6,
                    total_memory_bytes: 8192,
                    total_cpu_percent: 0.4,
                    age_seconds: 7200,
                    root_pids: vec![42, 43],
                }],
            },
        };
        let encoded = serde_json::to_vec(&response).unwrap();
        assert!(matches!(
            serde_json::from_slice(&encoded).unwrap(),
            ControlResponse::Stale { .. }
        ));
    }

    #[test]
    fn stale_response_accepts_an_old_daemon_without_groups() {
        let encoded = br#"{"result":"stale","stale":{"workloads":[
            {"root_pid":42,"name":"pytest","process_count":3,
             "total_memory_bytes":4096,"total_cpu_percent":0.2,"age_seconds":3600}
        ]}}"#;

        let response: ControlResponse = serde_json::from_slice(encoded).unwrap();

        let ControlResponse::Stale { stale } = response else {
            panic!("expected a stale response");
        };
        assert_eq!(stale.workloads.len(), 1);
        assert!(stale.groups.is_empty());
        assert_eq!(stale.workloads[0].root_uid, None);
        assert_eq!(stale.workloads[0].root_started_at, None);
    }

    #[test]
    fn stale_group_summary_has_no_termination_identifier() {
        let group = StaleWorkloadGroupSummary {
            working_directory: PathBuf::from("/work/project"),
            tree_count: 2,
            process_count: 6,
            total_memory_bytes: 8192,
            total_cpu_percent: 0.4,
            age_seconds: 7200,
            root_pids: vec![42, 43],
        };

        let first = serde_json::to_vec(&group).unwrap();
        let second = serde_json::to_vec(&group).unwrap();
        assert_eq!(first, second);
        let encoded = String::from_utf8(first).unwrap();
        assert!(!encoded.contains("termination"));
        assert!(!encoded.contains("signal"));
    }

    #[test]
    fn background_protocol_round_trips() {
        let request = serde_json::to_string(&ControlRequest::Background).unwrap();
        assert_eq!(request, r#"{"command":"background"}"#);
        let response = ControlResponse::Background {
            background: BackgroundResponse {
                workloads: vec![BackgroundWorkloadSummary {
                    group_id: "systemd-unit:app-1.scope".to_owned(),
                    root_pid: 42,
                    root_uid: 1_000,
                    root_started_at: 99,
                    name: "worker".to_owned(),
                    executable: Some("/usr/bin/worker".into()),
                    process_count: 3,
                    process_count_growth: 2,
                    total_memory_bytes: 4_096,
                    memory_growth_bytes: 1_024,
                    total_cpu_percent: 0.4,
                    age_seconds: 3_600,
                    observed_for_seconds: 1_800,
                }],
            },
        };
        let encoded = serde_json::to_vec(&response).unwrap();
        assert!(matches!(
            serde_json::from_slice(&encoded).unwrap(),
            ControlResponse::Background { .. }
        ));
    }
}
