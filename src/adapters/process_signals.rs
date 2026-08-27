use rustix::process::{Pid, PidfdFlags, Signal, getuid, pidfd_open, pidfd_send_signal};

use crate::{
    application::{ForceTerminationPort, PortError, TerminationPort},
    domain::ProcessIdentity,
};

use super::procfs::read_process_identity;

#[must_use]
pub fn current_user_id() -> u32 {
    getuid().as_raw()
}

#[derive(Clone, Copy, Debug, Default)]
pub struct PidfdTerminationPort;

impl TerminationPort for PidfdTerminationPort {
    fn terminate(&mut self, identity: ProcessIdentity) -> Result<(), PortError> {
        send_signal(identity, Signal::TERM, "send SIGTERM through pidfd")
    }
}

impl ForceTerminationPort for PidfdTerminationPort {
    fn force_terminate(&mut self, identity: ProcessIdentity) -> Result<(), PortError> {
        send_signal(identity, Signal::KILL, "send SIGKILL through pidfd")
    }
}

fn send_signal(
    identity: ProcessIdentity,
    signal: Signal,
    operation: &'static str,
) -> Result<(), PortError> {
    let raw_pid = i32::try_from(identity.pid())
        .ok()
        .and_then(Pid::from_raw)
        .ok_or_else(|| PortError::new("open pidfd", "PID is outside the supported range"))?;
    let pidfd = pidfd_open(raw_pid, PidfdFlags::empty())
        .map_err(|error| PortError::new("open pidfd", error.to_string()))?;

    let (uid, started_at) = read_process_identity(identity.pid())
        .map_err(|error| PortError::new("revalidate pidfd identity", error.to_string()))?;
    if uid != identity.uid() || started_at != identity.started_at() {
        return Err(PortError::new(
            "revalidate pidfd identity",
            "PID now refers to a different process",
        ));
    }

    pidfd_send_signal(pidfd, signal).map_err(|error| PortError::new(operation, error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::current_user_id;
    use crate::adapters::procfs::read_process_identity;

    #[test]
    fn current_uid_matches_procfs() {
        let (proc_uid, _) = read_process_identity(std::process::id()).unwrap();

        assert_eq!(current_user_id(), proc_uid);
    }
}
