use std::{
    io,
    path::PathBuf,
    time::{Duration, SystemTime},
};

use sysinfo::{
    Pid, Process, ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, System, UpdateKind,
};

use crate::{
    application::{ObservedProcess, PortError, ProcessSource, ResourceSnapshot},
    domain::{ProcessDescriptor, ProcessIdentity, ProcessResources, ProcessState, SystemResources},
};

use super::procfs::read_process_identity;

/// Linux process and resource adapter backed by `sysinfo` and stable `/proc` identity fields.
#[derive(Debug)]
pub struct SysinfoProcessSource {
    system: System,
}

impl SysinfoProcessSource {
    #[must_use]
    pub fn new() -> Self {
        Self {
            system: System::new(),
        }
    }

    fn process_refresh_kind() -> ProcessRefreshKind {
        ProcessRefreshKind::nothing()
            .with_cpu()
            .with_memory()
            .with_exe(UpdateKind::OnlyIfNotSet)
            .without_tasks()
    }

    fn observed_process(process: &Process) -> Option<ObservedProcess> {
        if process_has_exited(process) {
            return None;
        }
        let pid = process.pid().as_u32();
        let (uid, started_at) = read_process_identity(pid).ok()?;
        let observed_at = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default();

        Some(ObservedProcess {
            descriptor: ProcessDescriptor::new(
                ProcessIdentity::new(pid, uid, started_at),
                process.name().to_string_lossy(),
                process.exe().map(PathBuf::from),
            )
            .with_runtime(
                process.parent().map(Pid::as_u32),
                process_state(process.status()),
            ),
            resources: ProcessResources {
                cpu_percent: process.cpu_usage(),
                resident_memory_bytes: process.memory(),
                virtual_memory_bytes: process.virtual_memory(),
                running_for: Duration::from_secs(process.run_time()),
                observed_at,
            },
        })
    }

    fn descriptor(process: &Process) -> io::Result<ProcessDescriptor> {
        let pid = process.pid().as_u32();
        let (uid, started_at) = read_process_identity(pid)?;

        Ok(ProcessDescriptor::new(
            ProcessIdentity::new(pid, uid, started_at),
            process.name().to_string_lossy(),
            process.exe().map(PathBuf::from),
        ))
    }
}

const fn process_state(status: ProcessStatus) -> ProcessState {
    match status {
        ProcessStatus::Run => ProcessState::Running,
        ProcessStatus::Sleep | ProcessStatus::Idle => ProcessState::Sleeping,
        ProcessStatus::UninterruptibleDiskSleep => ProcessState::Uninterruptible,
        ProcessStatus::Zombie | ProcessStatus::Dead => ProcessState::Zombie,
        _ => ProcessState::Other,
    }
}

impl Default for SysinfoProcessSource {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessSource for SysinfoProcessSource {
    fn snapshot(&mut self) -> Result<ResourceSnapshot, PortError> {
        self.system.refresh_memory();
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            Self::process_refresh_kind(),
        );

        let processes = self
            .system
            .processes()
            .values()
            .filter_map(Self::observed_process)
            .collect();

        Ok(ResourceSnapshot {
            system: SystemResources {
                total_memory_bytes: self.system.total_memory(),
                available_memory_bytes: self.system.available_memory(),
                total_swap_bytes: self.system.total_swap(),
                used_swap_bytes: self.system.used_swap(),
            },
            processes,
        })
    }

    fn find(&mut self, pid: u32) -> Result<Option<ProcessDescriptor>, PortError> {
        let sysinfo_pid = Pid::from_u32(pid);
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[sysinfo_pid]),
            true,
            Self::process_refresh_kind(),
        );

        let Some(process) = self.system.process(sysinfo_pid) else {
            return Ok(None);
        };
        if process_has_exited(process) {
            return Ok(None);
        }

        match Self::descriptor(process) {
            Ok(descriptor) => Ok(Some(descriptor)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(PortError::new("read process identity", error.to_string())),
        }
    }
}

fn process_has_exited(process: &Process) -> bool {
    matches!(
        process.status(),
        ProcessStatus::Dead | ProcessStatus::Zombie
    )
}
