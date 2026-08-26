use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub(crate) enum ControlRequest {
    Status,
    Top,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub(crate) enum ControlResponse {
    Status { status: StatusResponse },
    Top { top: TopResponse },
    Error { message: String },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct StatusResponse {
    pub uptime_seconds: u64,
    pub last_poll_age_seconds: u64,
    pub total_memory_bytes: u64,
    pub available_memory_bytes: u64,
    pub total_swap_bytes: u64,
    pub used_swap_bytes: u64,
    pub observed_processes: usize,
    pub monitored_processes: usize,
    pub active_events: usize,
    pub last_error: Option<String>,
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
    use super::{ControlRequest, ControlResponse, StatusResponse, TopProcess, TopResponse};

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
                observed_processes: 3,
                monitored_processes: 2,
                active_events: 0,
                last_error: None,
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
}
