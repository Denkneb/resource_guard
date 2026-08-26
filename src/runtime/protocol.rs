use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub(crate) enum ControlRequest {
    Status,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub(crate) enum ControlResponse {
    Status { status: StatusResponse },
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

#[cfg(test)]
mod tests {
    use super::{ControlRequest, ControlResponse, StatusResponse};

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
}
