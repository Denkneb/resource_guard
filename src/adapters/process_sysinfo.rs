use std::{
    collections::{HashMap, HashSet},
    io,
    path::PathBuf,
    time::{Duration, SystemTime},
};

use sysinfo::{
    Pid, Process, ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, System, UpdateKind,
};

use crate::{
    application::{ObservedProcess, PortError, ProcessSource, ResourceSnapshot},
    domain::{
        ProcessDescriptor, ProcessExecutionContext, ProcessIdentity, ProcessResources,
        ProcessState, SystemResources,
    },
};

use super::procfs::{
    read_execution_context, read_process_identity_and_context, read_process_start_time,
    read_process_uid,
};

/// Per-process facts that are immutable for the lifetime of a PID and can be
/// reused across snapshots as long as the raw `starttime` is unchanged.
#[derive(Debug)]
struct CachedFacts {
    started_at: u64,
    uid: u32,
    context: ProcessExecutionContext,
}

/// Linux process and resource adapter backed by `sysinfo` and stable `/proc` identity fields.
#[derive(Debug)]
pub struct SysinfoProcessSource {
    system: System,
    facts: HashMap<u32, CachedFacts>,
}

impl SysinfoProcessSource {
    #[must_use]
    pub fn new() -> Self {
        Self {
            system: System::new(),
            facts: HashMap::new(),
        }
    }

    fn process_refresh_kind() -> ProcessRefreshKind {
        ProcessRefreshKind::nothing()
            .with_cpu()
            .with_memory()
            .with_exe(UpdateKind::OnlyIfNotSet)
            .without_tasks()
    }

    fn observed_process(
        process: &Process,
        current_uid: u32,
        facts: &mut HashMap<u32, CachedFacts>,
    ) -> Option<ObservedProcess> {
        if process_has_exited(process) {
            return None;
        }
        let pid = process.pid().as_u32();
        let (started_at, tty_nr) = read_process_start_time(pid).ok()?;
        let (uid, execution_context) = match facts.get(&pid) {
            Some(cached) if cached.started_at == started_at => (cached.uid, cached.context.clone()),
            _ => {
                let uid = read_process_uid(pid).ok()?;
                let context = read_execution_context(pid, uid == current_uid, tty_nr);
                facts.insert(
                    pid,
                    CachedFacts {
                        started_at,
                        uid,
                        context: context.clone(),
                    },
                );
                (uid, context)
            }
        };
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
            )
            .with_execution_context(execution_context),
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
        let (uid, started_at, execution_context) = read_process_identity_and_context(pid)?;

        Ok(ProcessDescriptor::new(
            ProcessIdentity::new(pid, uid, started_at),
            process.name().to_string_lossy(),
            process.exe().map(PathBuf::from),
        )
        .with_execution_context(execution_context))
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

        let current_uid = rustix::process::getuid().as_raw();
        let Self { system, facts } = self;
        let mut active = HashSet::new();
        let processes = system
            .processes()
            .values()
            .filter_map(|process| {
                let observed = Self::observed_process(process, current_uid, facts);
                if let Some(observed) = &observed {
                    active.insert(observed.descriptor.identity().pid());
                }
                observed
            })
            .collect();
        facts.retain(|pid, _| active.contains(pid));

        Ok(ResourceSnapshot {
            system: SystemResources {
                total_memory_bytes: system.total_memory(),
                available_memory_bytes: system.available_memory(),
                total_swap_bytes: system.total_swap(),
                used_swap_bytes: system.used_swap(),
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

#[cfg(test)]
mod tests {
    use super::SysinfoProcessSource;
    use crate::application::ProcessSource;

    #[test]
    fn repeated_snapshots_keep_a_stable_identity_and_context() {
        let mut source = SysinfoProcessSource::new();
        let first = source.snapshot().unwrap();
        let second = source.snapshot().unwrap();
        let pid = std::process::id();

        let first_process = first
            .processes
            .iter()
            .find(|process| process.descriptor.identity().pid() == pid)
            .expect("current process is present in the first snapshot");
        let second_process = second
            .processes
            .iter()
            .find(|process| process.descriptor.identity().pid() == pid)
            .expect("current process is present in the second snapshot");

        assert_eq!(
            first_process.descriptor.identity(),
            second_process.descriptor.identity()
        );
        assert_eq!(
            first_process.descriptor.execution_context(),
            second_process.descriptor.execution_context()
        );
        assert!(source.facts.len() <= second.processes.len());
    }
}
