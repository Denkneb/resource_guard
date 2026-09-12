use std::{fs, io, os::unix::ffi::OsStrExt as _, path::PathBuf};

use crate::domain::{ProcessExecutionContext, ProcessOrigin};

pub(super) fn read_process_identity(pid: u32) -> io::Result<(u32, u64)> {
    let (started_at, _tty_nr) = read_process_start_time(pid)?;
    let uid = read_process_uid(pid)?;
    Ok((uid, started_at))
}

/// Reads the validated absolute working directory of a process.
///
/// Returns `None` for a relative, empty, or deleted target. The caller keeps the
/// process in the snapshot either way; only group aggregation is skipped.
pub(super) fn read_working_directory(pid: u32) -> Option<PathBuf> {
    let path = fs::read_link(format!("/proc/{pid}/cwd")).ok()?;
    validated_working_directory(path)
}

fn validated_working_directory(path: PathBuf) -> Option<PathBuf> {
    let bytes = path.as_os_str().as_bytes();
    if bytes.is_empty() || !path.is_absolute() || bytes.ends_with(b" (deleted)") {
        return None;
    }
    Some(path)
}

/// Reads the stable identity and neutral execution context of a process.
pub(super) fn read_process_identity_and_context(
    pid: u32,
) -> io::Result<(u32, u64, ProcessExecutionContext)> {
    let (started_at, tty_nr) = read_process_start_time(pid)?;
    let uid = read_process_uid(pid)?;
    let context = read_execution_context(pid, true, tty_nr);
    Ok((uid, started_at, context))
}

/// Reads `starttime` and `tty_nr` from a single `/proc/<pid>/stat` read.
///
/// The raw `starttime` is the value already used for PID-reuse protection, so a
/// caller may safely reuse cached per-process facts while it stays unchanged.
pub(super) fn read_process_start_time(pid: u32) -> io::Result<(u64, Option<u32>)> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let started_at = parse_start_time(&stat)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing process start time"))?;
    Ok((started_at, parse_tty_nr(&stat)))
}

pub(super) fn read_process_uid(pid: u32) -> io::Result<u32> {
    let status = fs::read_to_string(format!("/proc/{pid}/status"))?;
    parse_real_uid(&status)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing process UID"))
}

/// Reads the neutral execution context of a process.
///
/// When `is_current_user` is false the `/proc/<pid>/cgroup` read is skipped,
/// because execution context is only consumed for the current user's background
/// classification and another user's group is never a candidate.
///
/// Returns `ProcessExecutionContext::default()` (fully `Unknown`) whenever the
/// cgroup metadata is missing, malformed, or unreadable, or whenever the
/// controlling terminal cannot be determined. An indeterminate TTY must never
/// be treated as "no controlling terminal", because that would let a process
/// with an unknown session become a stop candidate.
pub(super) fn read_execution_context(
    pid: u32,
    is_current_user: bool,
    tty_nr: Option<u32>,
) -> ProcessExecutionContext {
    let classification = if is_current_user {
        fs::read_to_string(format!("/proc/{pid}/cgroup"))
            .ok()
            .as_deref()
            .map_or_else(CgroupClassification::unknown, classify_cgroup)
    } else {
        CgroupClassification::unknown()
    };
    execution_context_from(classification, tty_nr)
}

fn execution_context_from(
    classification: CgroupClassification,
    tty_nr: Option<u32>,
) -> ProcessExecutionContext {
    let Some(tty_nr) = tty_nr else {
        return ProcessExecutionContext::default();
    };
    ProcessExecutionContext::new(
        classification.group_id,
        classification.unit_name,
        classification.origin,
        tty_nr != 0,
    )
}

fn parse_start_time(stat: &str) -> Option<u64> {
    let command_end = stat.rfind(')')?;
    let mut fields_after_command = stat.get(command_end + 1..)?.split_whitespace();
    fields_after_command.nth(19)?.parse().ok()
}

fn parse_tty_nr(stat: &str) -> Option<u32> {
    let command_end = stat.rfind(')')?;
    let mut fields_after_command = stat.get(command_end + 1..)?.split_whitespace();
    fields_after_command.nth(4)?.parse().ok()
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

#[derive(Clone, Debug, Eq, PartialEq)]
struct CgroupClassification {
    origin: ProcessOrigin,
    unit_name: Option<String>,
    group_id: Option<String>,
}

impl CgroupClassification {
    const fn unknown() -> Self {
        Self {
            origin: ProcessOrigin::Unknown,
            unit_name: None,
            group_id: None,
        }
    }
}

#[allow(clippy::case_sensitive_file_extension_comparisons)]
fn classify_cgroup(contents: &str) -> CgroupClassification {
    let Some(path) = cgroup_v2_path(contents) else {
        return CgroupClassification::unknown();
    };
    let components = path
        .split('/')
        .filter(|component| !component.is_empty())
        .collect::<Vec<_>>();
    let Some(unit) = components
        .iter()
        .rev()
        .find(|component| component.ends_with(".scope") || component.ends_with(".service"))
        .copied()
    else {
        return CgroupClassification::unknown();
    };

    let has_app_slice = components.contains(&"app.slice");
    let has_session_or_background =
        components.contains(&"session.slice") || components.contains(&"background.slice");

    let origin = if has_app_slice && unit.ends_with(".scope") {
        ProcessOrigin::UserApplication
    } else if unit.ends_with(".service") || has_session_or_background {
        ProcessOrigin::UserService
    } else {
        ProcessOrigin::Unknown
    };

    let group_id = matches!(
        origin,
        ProcessOrigin::UserApplication | ProcessOrigin::UserService
    )
    .then(|| format!("systemd-unit:{unit}"));

    CgroupClassification {
        origin,
        unit_name: Some(unit.to_owned()),
        group_id,
    }
}

fn cgroup_v2_path(contents: &str) -> Option<&str> {
    contents.lines().find_map(|line| {
        let path = line.strip_prefix("0::")?;
        (!path.is_empty()).then_some(path)
    })
}

#[cfg(test)]
mod tests {
    use crate::domain::{ProcessExecutionContext, ProcessOrigin};

    use super::{
        classify_cgroup, execution_context_from, parse_real_uid, parse_start_time, parse_tty_nr,
        read_execution_context, read_process_identity_and_context, validated_working_directory,
    };

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

    #[test]
    fn parses_tty_nr_when_it_is_zero() {
        let stat = "42 (worker) S 1 2 3 0 6 7 8 9 10 11 12 13 14 15 16 17 18 12345 20";

        assert_eq!(parse_tty_nr(stat), Some(0));
    }

    #[test]
    fn parses_tty_nr_when_it_is_nonzero() {
        let stat = "42 (worker) S 1 2 3 34816 6 7 8 9 10 11 12 13 14 15 16 17 18 12345 20";

        assert_eq!(parse_tty_nr(stat), Some(34_816));
    }

    #[test]
    fn parses_tty_nr_after_a_command_with_nested_parentheses() {
        let stat = "42 (worker (busy)) S 1 2 3 1024 6 7 8 9 10 11 12 13 14 15 16 17 18 12345 20";

        assert_eq!(parse_tty_nr(stat), Some(1_024));
    }

    #[test]
    fn classifies_a_cgroup_v2_app_scope_as_a_user_application() {
        let cgroup =
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-org.example.App.scope";

        let classification = classify_cgroup(cgroup);

        assert_eq!(classification.origin, ProcessOrigin::UserApplication);
        assert_eq!(
            classification.unit_name.as_deref(),
            Some("app-org.example.App.scope")
        );
        assert_eq!(
            classification.group_id.as_deref(),
            Some("systemd-unit:app-org.example.App.scope")
        );
    }

    #[test]
    fn classifies_a_service_unit_as_a_user_service() {
        let cgroup = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-org.example.App.service";

        let classification = classify_cgroup(cgroup);

        assert_eq!(classification.origin, ProcessOrigin::UserService);
        assert_eq!(
            classification.group_id.as_deref(),
            Some("systemd-unit:app-org.example.App.service")
        );
    }

    #[test]
    fn classifies_a_session_slice_as_a_user_service() {
        let cgroup = "0::/user.slice/user-1000.slice/session.slice/app-gnome-terminal-1234.scope";

        let classification = classify_cgroup(cgroup);

        assert_eq!(classification.origin, ProcessOrigin::UserService);
        assert_eq!(
            classification.group_id.as_deref(),
            Some("systemd-unit:app-gnome-terminal-1234.scope")
        );
    }

    #[test]
    fn classifies_a_scope_outside_app_slice_as_unknown() {
        let cgroup = "0::/user.slice/user-1000.slice/user@1000.service/session-c2.scope";

        let classification = classify_cgroup(cgroup);

        assert_eq!(classification.origin, ProcessOrigin::Unknown);
        assert_eq!(classification.group_id, None);
        assert_eq!(
            classification.unit_name.as_deref(),
            Some("session-c2.scope")
        );
    }

    #[test]
    fn classifies_an_unknown_hierarchy_as_unknown() {
        let cgroup = "2:cpu:/user.slice/user-1000.slice/foo.scope";

        let classification = classify_cgroup(cgroup);

        assert_eq!(classification.origin, ProcessOrigin::Unknown);
        assert_eq!(classification.group_id, None);
        assert_eq!(classification.unit_name, None);
    }

    #[test]
    fn classifies_empty_and_malformed_input_as_unknown() {
        for input in ["", "0::", "garbage", "0::/"] {
            let classification = classify_cgroup(input);
            assert_eq!(classification.origin, ProcessOrigin::Unknown);
            assert_eq!(classification.group_id, None);
        }
    }

    #[test]
    fn an_undetermined_tty_forces_a_fully_unknown_context() {
        let classification = classify_cgroup(
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-org.example.App.scope",
        );

        let context = execution_context_from(classification, None);

        assert_eq!(context, ProcessExecutionContext::default());
        assert_eq!(context.origin(), ProcessOrigin::Unknown);
        assert_eq!(context.group_id(), None);
        assert!(!context.has_controlling_terminal());
    }

    #[test]
    fn a_determined_tty_keeps_the_classification_and_sets_the_flag() {
        let classification = classify_cgroup(
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-org.example.App.scope",
        );

        let without_tty = execution_context_from(classification.clone(), Some(0));
        assert_eq!(without_tty.origin(), ProcessOrigin::UserApplication);
        assert_eq!(
            without_tty.group_id(),
            Some("systemd-unit:app-org.example.App.scope")
        );
        assert!(!without_tty.has_controlling_terminal());

        let with_tty = execution_context_from(classification, Some(1_024));
        assert_eq!(with_tty.origin(), ProcessOrigin::UserApplication);
        assert!(with_tty.has_controlling_terminal());
    }

    #[test]
    fn reads_the_current_process_identity_and_context() {
        let pid = std::process::id();
        let (uid, started_at, _context) = read_process_identity_and_context(pid).unwrap();

        assert_eq!(uid, rustix::process::getuid().as_raw());
        assert!(started_at > 0);
    }
    #[test]
    fn skips_cgroup_for_another_user() {
        let pid = std::process::id();
        let context = read_execution_context(pid, false, Some(0));

        assert_eq!(context.origin(), ProcessOrigin::Unknown);
        assert_eq!(context.group_id(), None);
    }

    #[test]
    fn accepts_an_absolute_working_directory() {
        let path = std::path::PathBuf::from("/work/project");

        assert_eq!(validated_working_directory(path.clone()), Some(path));
    }

    #[test]
    fn rejects_relative_empty_and_deleted_working_directories() {
        for path in [
            std::path::PathBuf::from("relative/project"),
            std::path::PathBuf::new(),
            std::path::PathBuf::from("/work/project (deleted)"),
        ] {
            assert_eq!(validated_working_directory(path), None);
        }
    }
}
