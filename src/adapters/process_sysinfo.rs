use std::{fs, io, path::PathBuf, time::SystemTime};

use sysinfo::{Pid, Process, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

use crate::{
    application::{ObservedProcess, PortError, ProcessSource, ResourceSnapshot},
    domain::{ProcessDescriptor, ProcessIdentity, ProcessResources, SystemResources},
};

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
            ),
            resources: ProcessResources {
                cpu_percent: process.cpu_usage(),
                resident_memory_bytes: process.memory(),
                virtual_memory_bytes: process.virtual_memory(),
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

        match Self::descriptor(process) {
            Ok(descriptor) => Ok(Some(descriptor)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(PortError::new("read process identity", error.to_string())),
        }
    }
}

fn read_process_identity(pid: u32) -> io::Result<(u32, u64)> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let status = fs::read_to_string(format!("/proc/{pid}/status"))?;
    let started_at = parse_start_time(&stat)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing process start time"))?;
    let uid = parse_real_uid(&status)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing process UID"))?;
    Ok((uid, started_at))
}

fn parse_start_time(stat: &str) -> Option<u64> {
    let command_end = stat.rfind(')')?;
    let mut fields_after_command = stat.get(command_end + 1..)?.split_whitespace();
    fields_after_command.nth(19)?.parse().ok()
}

fn parse_real_uid(status: &str) -> Option<u32> {
    status.lines().find_map(|line| {
        line.strip_prefix("Uid:")?
            .split_whitespace()
            .next()?
            .parse()
            .ok()
    })
}

#[cfg(test)]
mod tests {
    use super::{parse_real_uid, parse_start_time};

    #[test]
    fn parses_start_time_after_a_command_with_spaces() {
        let stat = "42 (resource guard) S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 98765 20";

        assert_eq!(parse_start_time(stat), Some(98_765));
    }

    #[test]
    fn parses_start_time_after_a_command_containing_parentheses() {
        let stat = "42 (worker (busy)) S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 12345 20";

        assert_eq!(parse_start_time(stat), Some(12_345));
    }

    #[test]
    fn rejects_an_incomplete_stat_record() {
        assert_eq!(parse_start_time("42 (worker) S 1 2 3"), None);
    }

    #[test]
    fn parses_real_uid_from_status() {
        let status = "Name:\tworker\nUid:\t1000\t1001\t1002\t1003\nGid:\t1000\t1000\t1000\t1000\n";

        assert_eq!(parse_real_uid(status), Some(1_000));
    }
}
