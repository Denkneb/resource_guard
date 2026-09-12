use std::{
    error::Error,
    fmt::{self, Write as _},
    io::{self, BufRead, IsTerminal, Write as _},
    process::ExitCode,
    thread,
    time::Duration,
};

use clap::{Parser, Subcommand};

use crate::{
    adapters::{
        ConfigOrigin, PidfdTerminationPort, SysinfoProcessSource, SystemClock, ThreadSleeper,
        TomlConfigRepository, current_user_id,
    },
    application::{
        ForceStopProcess, ObservedProcess, PortError, ProcessSource, StopAndWait, StopError,
        StopOutcome, StopWorkload, WaitForExit, background_workload_from_root, workload_from_root,
    },
    domain::{BackgroundWorkload, BackgroundWorkloadPolicy, ProcessIdentity, ProtectionPolicy},
    runtime::{
        self, BackgroundResponse, BackgroundWorkloadSummary, RuntimeError, StaleResponse,
        StaleWorkloadSummary, TopResponse,
    },
};

#[derive(Debug, Parser)]
#[command(name = "resource-guard", version, about)]
pub struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the resource monitoring daemon.
    Daemon,
    /// Show daemon and system status.
    Status,
    /// Show the most resource-intensive processes.
    Top {
        /// Refresh the table continuously.
        #[arg(long)]
        watch: bool,
    },
    /// Show workload trees suspected to be stale.
    Stale,
    /// Show background applications that are growing or retaining memory.
    Background,
    /// Gracefully stop a currently reported background application group.
    StopBackground { root_pid: u32 },
    /// Inspect and manage configuration.
    Config {
        #[command(subcommand)]
        command: Option<ConfigCommand>,
    },
    /// Gracefully stop a process after identity verification.
    Stop {
        pid: u32,
        /// Send SIGKILL after SIGTERM fails and a separate confirmation is given.
        #[arg(long)]
        kill: bool,
        /// Confirm a non-interactive SIGKILL request.
        #[arg(long, requires = "kill")]
        yes: bool,
    },
    /// Gracefully stop a currently reported stale workload tree.
    StopTree { root_pid: u32 },
}

#[derive(Debug, Subcommand)]
enum ConfigCommand {
    /// Print the resolved configuration path.
    Path,
    /// Validate the effective configuration.
    Check,
    /// Create a default configuration file.
    Init {
        /// Replace an existing configuration file.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Debug)]
pub enum CliError {
    Config(crate::adapters::ConfigError),
    Inspection(PortError),
    Stop(StopError),
    ProcessNotFound(u32),
    StillRunning { pid: u32, grace_period_seconds: u64 },
    StillRunningAfterKill { pid: u32, wait_seconds: u64 },
    ConfirmationRequired { pid: u32 },
    ConfirmationDeclined { pid: u32 },
    TreeConfirmationRequired { pid: u32 },
    TreeConfirmationDeclined { pid: u32 },
    WorkloadNotReported(u32),
    WorkloadIdentityUnavailable(u32),
    BackgroundWorkloadNotReported(u32),
    BackgroundWorkloadChanged(u32),
    BackgroundConfirmationRequired { pid: u32 },
    BackgroundConfirmationDeclined { pid: u32 },
    ConfirmationIo(io::Error),
    Runtime(RuntimeError),
    Output(io::Error),
    NotImplemented(&'static str),
}

impl fmt::Display for CliError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(error) => error.fmt(formatter),
            Self::Inspection(error) => write!(formatter, "cannot inspect process: {error}"),
            Self::Stop(error) => error.fmt(formatter),
            Self::ProcessNotFound(pid) => write!(formatter, "process {pid} does not exist"),
            Self::StillRunning {
                pid,
                grace_period_seconds,
            } => write!(
                formatter,
                "process {pid} is still running after {grace_period_seconds} seconds"
            ),
            Self::StillRunningAfterKill { pid, wait_seconds } => write!(
                formatter,
                "process {pid} is still running {wait_seconds} seconds after SIGKILL"
            ),
            Self::ConfirmationRequired { pid } => write!(
                formatter,
                "SIGKILL for process {pid} requires an interactive terminal; rerun with --kill --yes to confirm non-interactively"
            ),
            Self::ConfirmationDeclined { pid } => {
                write!(formatter, "SIGKILL for process {pid} was not confirmed")
            }
            Self::TreeConfirmationRequired { pid } => write!(
                formatter,
                "stopping workload tree {pid} requires an interactive terminal"
            ),
            Self::TreeConfirmationDeclined { pid } => {
                write!(formatter, "stopping workload tree {pid} was not confirmed")
            }
            Self::WorkloadNotReported(pid) => write!(
                formatter,
                "PID {pid} is not a stale workload currently reported by the daemon"
            ),
            Self::WorkloadIdentityUnavailable(pid) => write!(
                formatter,
                "daemon did not report a verifiable identity for stale workload {pid}; refusing to stop it"
            ),
            Self::BackgroundWorkloadNotReported(pid) => write!(
                formatter,
                "PID {pid} is not a background application currently reported by the daemon"
            ),
            Self::BackgroundWorkloadChanged(pid) => write!(
                formatter,
                "background application rooted at PID {pid} changed identity or group since it was reported"
            ),
            Self::BackgroundConfirmationRequired { pid } => write!(
                formatter,
                "stopping background application {pid} requires an interactive terminal"
            ),
            Self::BackgroundConfirmationDeclined { pid } => {
                write!(
                    formatter,
                    "stopping background application {pid} was not confirmed"
                )
            }
            Self::ConfirmationIo(error) => {
                write!(formatter, "cannot read SIGKILL confirmation: {error}")
            }
            Self::Runtime(error) => error.fmt(formatter),
            Self::Output(error) => write!(formatter, "cannot write command output: {error}"),
            Self::NotImplemented(command) => {
                write!(formatter, "command '{command}' is not implemented yet")
            }
        }
    }
}

impl Error for CliError {}

impl From<crate::adapters::ConfigError> for CliError {
    fn from(error: crate::adapters::ConfigError) -> Self {
        Self::Config(error)
    }
}

impl From<StopError> for CliError {
    fn from(error: StopError) -> Self {
        Self::Stop(error)
    }
}

impl From<RuntimeError> for CliError {
    fn from(error: RuntimeError) -> Self {
        Self::Runtime(error)
    }
}

/// Parses process arguments and runs the selected CLI command.
#[must_use]
pub fn run_from_environment() -> ExitCode {
    match execute(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("resource-guard: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Runs an already parsed command.
///
/// # Errors
///
/// Returns configuration, runtime, process-control, or output errors.
pub fn execute(cli: Cli) -> Result<(), CliError> {
    match cli.command {
        Command::Config { command } => execute_config(command.as_ref()),
        Command::Daemon => runtime::run_daemon().map_err(Into::into),
        Command::Status => {
            let status = runtime::query_status()?;
            println!("daemon: running");
            println!("uptime: {}s", status.uptime_seconds);
            println!("last poll: {}s ago", status.last_poll_age_seconds);
            println!(
                "processes: {} observed, {} monitored",
                status.observed_processes, status.monitored_processes
            );
            println!(
                "memory: {} / {} bytes available",
                status.available_memory_bytes, status.total_memory_bytes
            );
            println!(
                "swap: {} / {} bytes used",
                status.used_swap_bytes, status.total_swap_bytes
            );
            println!("memory pressure: {}", status.memory_pressure_level);
            println!("memory pressure reason: {}", status.memory_pressure_reason);
            println!(
                "automatic emergency action: {}",
                if status.automatic_emergency_action_permitted {
                    "permitted"
                } else {
                    "blocked"
                }
            );
            println!(
                "emergency thresholds: {} MiB available or critical RAM with PSI full avg10 >= {:.2}%",
                status.emergency_action_available_bytes / 1_048_576,
                status.emergency_action_psi_full_avg10
            );
            println!(
                "memory PSI avg10: some {:.2}%, full {:.2}%",
                status.memory_psi_some_avg10, status.memory_psi_full_avg10
            );
            if let Some(action) = status.last_emergency_action {
                println!("last emergency action: {action}");
            }
            println!("active events: {}", status.active_events);
            if let Some(error) = status.last_error {
                println!("last error: {error}");
            }
            if let Some(error) = status.notification_error {
                println!("notification error: {error}");
            }
            Ok(())
        }
        Command::Top { watch } => execute_top(watch),
        Command::Stale => execute_stale(),
        Command::Background => execute_background(),
        Command::Stop { pid, kill, yes } => execute_stop(pid, kill, yes),
        Command::StopTree { root_pid } => execute_stop_tree(root_pid),
        Command::StopBackground { root_pid } => execute_stop_background(root_pid),
    }
}

fn execute_stale() -> Result<(), CliError> {
    let stale = runtime::query_stale()?;
    print!("{}", render_stale(&stale));
    Ok(())
}

fn render_stale(stale: &StaleResponse) -> String {
    if stale.workloads.is_empty() && stale.groups.is_empty() {
        return "no stale workloads detected\n".to_owned();
    }

    let mut output = String::new();
    if !stale.groups.is_empty() {
        output.push_str("STALE WORKLOAD GROUPS\n");
        output.push_str("CWD                             TREES PROCS      RAM    CPU   AGE\n");
        for group in &stale.groups {
            let _ = writeln!(
                output,
                "{:<31} {:>5} {:>5} {:>8} {:>5.1}% {:>5}",
                group.working_directory.display(),
                group.tree_count,
                group.process_count,
                format_bytes(group.total_memory_bytes),
                group.total_cpu_percent,
                format_duration(group.age_seconds),
            );
            for root_pid in &group.root_pids {
                if let Some(member) = stale
                    .workloads
                    .iter()
                    .find(|workload| workload.root_pid == *root_pid)
                {
                    let _ = writeln!(
                        output,
                        "  ROOT {:<7} {:>3} procs {:>8} {}",
                        member.root_pid,
                        member.process_count,
                        format_bytes(member.total_memory_bytes),
                        member.name,
                    );
                }
            }
        }
    }

    let grouped_pids = stale
        .groups
        .iter()
        .flat_map(|group| group.root_pids.iter().copied())
        .collect::<std::collections::HashSet<_>>();
    let direct = stale
        .workloads
        .iter()
        .filter(|workload| !grouped_pids.contains(&workload.root_pid))
        .collect::<Vec<&StaleWorkloadSummary>>();
    if !direct.is_empty() {
        output.push_str("DIRECT STALE WORKLOADS\n");
        output.push_str("ROOT PID   CPU      RAM       AGE PROCS NAME\n");
        for workload in direct {
            let _ = writeln!(
                output,
                "{:<10} {:>6.1}% {:>8} {:>9} {:>5} {}",
                workload.root_pid,
                workload.total_cpu_percent,
                format_bytes(workload.total_memory_bytes),
                format_duration(workload.age_seconds),
                workload.process_count,
                workload.name,
            );
        }
    }
    output
}

fn execute_background() -> Result<(), CliError> {
    let background = runtime::query_background()?;
    print!("{}", render_background(&background));
    Ok(())
}

fn render_background(background: &BackgroundResponse) -> String {
    if background.workloads.is_empty() {
        return "no growing background applications detected\n".to_owned();
    }
    let mut output = String::from("ROOT PID   CPU      RAM      +GROWTH  AGE       PROCS NAME\n");
    for workload in &background.workloads {
        let executable = workload
            .executable
            .as_ref()
            .map_or_else(|| "-".to_owned(), |path| path.display().to_string());
        let _ = writeln!(
            output,
            "{:<10} {:>6.1}% {:>8} {:>8} {:>9} {:>4}(+{}) {} [{}]",
            workload.root_pid,
            workload.total_cpu_percent,
            format_bytes(workload.total_memory_bytes),
            format_bytes(workload.memory_growth_bytes),
            format_duration(workload.age_seconds),
            workload.process_count,
            workload.process_count_growth,
            workload.name,
            executable,
        );
    }
    output
}

fn execute_stop_background(root_pid: u32) -> Result<(), CliError> {
    let response = runtime::query_background()?;
    let summary = response
        .workloads
        .iter()
        .find(|workload| workload.root_pid == root_pid)
        .ok_or(CliError::BackgroundWorkloadNotReported(root_pid))?;

    let repository = TomlConfigRepository::from_environment()?;
    let settings = repository.load()?.settings;
    let protection = settings.protection_policy();
    let policy = settings.background_workload_policy();
    let mut source = SysinfoProcessSource::new();
    let snapshot = source.snapshot().map_err(CliError::Inspection)?;
    let workload = background_workload_for_stop(
        &snapshot.processes,
        summary,
        current_user_id(),
        &protection,
        &policy,
    )
    .ok_or(CliError::BackgroundWorkloadChanged(root_pid))?;

    confirm_background_stop(&workload)?;

    let mut terminator = PidfdTerminationPort;
    let count = StopWorkload::new(&mut source, &mut terminator, current_user_id(), &protection)
        .execute(workload.termination_order())?;
    println!(
        "sent SIGTERM to {count} processes in background application rooted at PID {root_pid}"
    );
    Ok(())
}

/// Rebuilds the reported background group from a fresh snapshot using the full
/// daemon identity (PID, UID, start time) and the current protection/policy.
fn background_workload_for_stop(
    processes: &[ObservedProcess],
    summary: &BackgroundWorkloadSummary,
    current_uid: u32,
    protection: &ProtectionPolicy,
    policy: &BackgroundWorkloadPolicy,
) -> Option<BackgroundWorkload> {
    let expected =
        ProcessIdentity::new(summary.root_pid, summary.root_uid, summary.root_started_at);
    background_workload_from_root(
        processes,
        expected,
        &summary.group_id,
        current_uid,
        protection,
        policy,
    )
}

fn confirm_background_stop(workload: &BackgroundWorkload) -> Result<(), CliError> {
    let pid = workload.identity().pid();
    if !io::stdin().is_terminal() {
        return Err(CliError::BackgroundConfirmationRequired { pid });
    }
    let mut stderr = io::stderr().lock();
    write!(
        stderr,
        "stop {} processes in {} background application ({pid}, {} MiB) with SIGTERM; type {pid} to confirm: ",
        workload.process_count(),
        workload.root.name(),
        workload.total_memory_bytes / 1_048_576,
    )
    .map_err(CliError::ConfirmationIo)?;
    stderr.flush().map_err(CliError::ConfirmationIo)?;
    let mut confirmation = String::new();
    io::stdin()
        .read_line(&mut confirmation)
        .map_err(CliError::ConfirmationIo)?;
    if confirmation.trim() == pid.to_string() {
        Ok(())
    } else {
        Err(CliError::BackgroundConfirmationDeclined { pid })
    }
}

fn execute_stop_tree(root_pid: u32) -> Result<(), CliError> {
    let summary = runtime::query_stale()?
        .workloads
        .into_iter()
        .find(|workload| workload.root_pid == root_pid)
        .ok_or(CliError::WorkloadNotReported(root_pid))?;
    let repository = TomlConfigRepository::from_environment()?;
    let settings = repository.load()?.settings;
    let protection = settings.protection_policy();
    let mut source = SysinfoProcessSource::new();
    let snapshot = source.snapshot().map_err(CliError::Inspection)?;
    let workload = stale_workload_for_stop(&snapshot.processes, &summary, current_user_id())?;
    confirm_tree_stop(&workload)?;

    let mut terminator = PidfdTerminationPort;
    let count = StopWorkload::new(&mut source, &mut terminator, current_user_id(), &protection)
        .execute(workload.termination_order())?;
    println!("sent SIGTERM to {count} processes in workload tree rooted at PID {root_pid}");
    Ok(())
}

/// Rebuilds the reported stale tree from a fresh snapshot using the full daemon
/// identity (PID, UID, start time), so a reused PID cannot redirect the signal.
fn stale_workload_for_stop(
    processes: &[ObservedProcess],
    summary: &StaleWorkloadSummary,
    current_uid: u32,
) -> Result<crate::domain::StaleWorkload, CliError> {
    let expected = ProcessIdentity::new(
        summary.root_pid,
        summary
            .root_uid
            .ok_or(CliError::WorkloadIdentityUnavailable(summary.root_pid))?,
        summary
            .root_started_at
            .ok_or(CliError::WorkloadIdentityUnavailable(summary.root_pid))?,
    );
    workload_from_root(processes, expected, current_uid)
        .ok_or(CliError::WorkloadNotReported(summary.root_pid))
}

fn confirm_tree_stop(workload: &crate::domain::StaleWorkload) -> Result<(), CliError> {
    let pid = workload.identity().pid();
    if !io::stdin().is_terminal() {
        return Err(CliError::TreeConfirmationRequired { pid });
    }
    let mut stderr = io::stderr().lock();
    write!(
        stderr,
        "stop {} processes in {} workload ({pid}) with SIGTERM; type {pid} to confirm: ",
        workload.process_count(),
        workload.root.name()
    )
    .map_err(CliError::ConfirmationIo)?;
    stderr.flush().map_err(CliError::ConfirmationIo)?;
    let mut confirmation = String::new();
    io::stdin()
        .read_line(&mut confirmation)
        .map_err(CliError::ConfirmationIo)?;
    if confirmation.trim() == pid.to_string() {
        Ok(())
    } else {
        Err(CliError::TreeConfirmationDeclined { pid })
    }
}

fn execute_top(watch: bool) -> Result<(), CliError> {
    loop {
        let top = runtime::query_top()?;
        let mut stdout = io::stdout().lock();
        if watch {
            write!(stdout, "\x1b[2J\x1b[H").map_err(CliError::Output)?;
        }
        write!(stdout, "{}", render_top(&top)).map_err(CliError::Output)?;
        stdout.flush().map_err(CliError::Output)?;
        drop(stdout);

        if !watch {
            return Ok(());
        }
        thread::sleep(Duration::from_secs(2));
    }
}

fn render_top(top: &TopResponse) -> String {
    let mut output = format!(
        "sample: {}s ago; {} monitored processes\n",
        top.sample_age_seconds,
        top.processes.len()
    );
    output.push_str("PID        CPU      RAM       AGE LIMIT NAME\n");
    for process in &top.processes {
        let limit = if process.exceeds_limit { "yes" } else { "-" };
        let _ = writeln!(
            output,
            "{:<7} {:>6.1}% {:>8} {:>9} {:>5} {}",
            process.pid,
            process.cpu_percent,
            format_bytes(process.resident_memory_bytes),
            format_duration(process.running_for_seconds),
            limit,
            process.name,
        );
    }
    output
}

fn format_bytes(bytes: u64) -> String {
    const KIB: u64 = 1_024;
    const MIB: u64 = KIB * 1_024;
    const GIB: u64 = MIB * 1_024;
    if bytes >= GIB {
        format_unit(bytes, GIB, "GiB")
    } else if bytes >= MIB {
        format_unit(bytes, MIB, "MiB")
    } else if bytes >= KIB {
        format_unit(bytes, KIB, "KiB")
    } else {
        format!("{bytes}B")
    }
}

fn format_unit(bytes: u64, unit: u64, suffix: &str) -> String {
    let tenths = (u128::from(bytes) * 10 + u128::from(unit / 2)) / u128::from(unit);
    format!("{}.{:01}{suffix}", tenths / 10, tenths % 10)
}

fn format_duration(seconds: u64) -> String {
    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3_600;
    let minutes = (seconds % 3_600) / 60;
    let seconds = seconds % 60;
    if days > 0 {
        format!("{days}d{hours:02}h")
    } else if hours > 0 {
        format!("{hours}h{minutes:02}m")
    } else if minutes > 0 {
        format!("{minutes}m{seconds:02}s")
    } else {
        format!("{seconds}s")
    }
}

fn execute_stop(pid: u32, kill: bool, yes: bool) -> Result<(), CliError> {
    let repository = TomlConfigRepository::from_environment()?;
    let loaded = repository.load()?;
    let protection = loaded.settings.protection_policy();
    let grace_period = loaded.settings.termination.grace_period;
    let mut source = SysinfoProcessSource::new();
    let process = source
        .find(pid)
        .map_err(CliError::Inspection)?
        .ok_or(CliError::ProcessNotFound(pid))?;
    let identity = process.identity();
    let process_name = process.name().to_owned();
    let mut terminator = PidfdTerminationPort;
    let clock = SystemClock::new();
    let sleeper = ThreadSleeper;

    let outcome = StopAndWait::new(
        &mut source,
        &mut terminator,
        &clock,
        &sleeper,
        current_user_id(),
        &protection,
    )
    .execute(identity, grace_period)?;

    match outcome {
        StopOutcome::Exited => {
            println!("sent SIGTERM to {process_name} ({pid}); process exited");
            Ok(())
        }
        StopOutcome::StillRunning if !kill => Err(CliError::StillRunning {
            pid,
            grace_period_seconds: grace_period.as_secs(),
        }),
        StopOutcome::StillRunning => {
            if !yes {
                confirm_force_kill(pid, &process_name)?;
            }

            let force_result =
                ForceStopProcess::new(&mut source, &mut terminator, current_user_id(), &protection)
                    .execute(identity);
            match force_result {
                Ok(()) => {}
                Err(StopError::NotFound { .. } | StopError::IdentityChanged { .. }) => {
                    println!(
                        "sent SIGTERM to {process_name} ({pid}); original process exited before SIGKILL"
                    );
                    return Ok(());
                }
                Err(error) => return Err(error.into()),
            }

            match WaitForExit::new(&mut source, &clock, &sleeper).execute(identity, grace_period)? {
                StopOutcome::Exited => {
                    println!(
                        "sent SIGTERM and confirmed SIGKILL to {process_name} ({pid}); process exited"
                    );
                    Ok(())
                }
                StopOutcome::StillRunning => Err(CliError::StillRunningAfterKill {
                    pid,
                    wait_seconds: grace_period.as_secs(),
                }),
            }
        }
    }
}

fn confirm_force_kill(pid: u32, process_name: &str) -> Result<(), CliError> {
    if !io::stdin().is_terminal() {
        return Err(CliError::ConfirmationRequired { pid });
    }

    let mut stdin = io::stdin().lock();
    let mut stderr = io::stderr().lock();
    if read_force_kill_confirmation(&mut stdin, &mut stderr, pid, process_name)
        .map_err(CliError::ConfirmationIo)?
    {
        Ok(())
    } else {
        Err(CliError::ConfirmationDeclined { pid })
    }
}

fn read_force_kill_confirmation<R: BufRead, W: io::Write>(
    input: &mut R,
    output: &mut W,
    pid: u32,
    process_name: &str,
) -> io::Result<bool> {
    write!(
        output,
        "process {process_name} ({pid}) ignored SIGTERM; type {pid} to confirm SIGKILL: "
    )?;
    output.flush()?;

    let mut confirmation = String::new();
    input.read_line(&mut confirmation)?;
    Ok(confirmation.trim() == pid.to_string())
}

fn execute_config(command: Option<&ConfigCommand>) -> Result<(), CliError> {
    let repository = TomlConfigRepository::from_environment()?;
    match command {
        None => {
            let loaded = repository.load()?;
            print!("{}", TomlConfigRepository::render(&loaded.settings)?);
        }
        Some(ConfigCommand::Path) => println!("{}", repository.path().display()),
        Some(ConfigCommand::Check) => {
            let loaded = repository.load()?;
            match loaded.origin {
                ConfigOrigin::Defaults => println!(
                    "configuration is valid (using defaults; {} does not exist)",
                    repository.path().display()
                ),
                ConfigOrigin::File => {
                    println!("configuration is valid ({})", repository.path().display());
                }
            }
        }
        Some(ConfigCommand::Init { force }) => {
            repository.initialize(*force)?;
            println!("created {}", repository.path().display());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{collections::HashSet, io::Cursor, path::PathBuf, time::Duration};

    use clap::Parser;

    use super::{
        Cli, Command, ConfigCommand, background_workload_for_stop, format_bytes, format_duration,
        read_force_kill_confirmation, render_background, render_stale, render_top,
        stale_workload_for_stop,
    };
    use crate::{
        application::ObservedProcess,
        domain::{
            BackgroundWorkloadPolicy, ProcessDescriptor, ProcessExecutionContext, ProcessIdentity,
            ProcessOrigin, ProcessResources, ProcessState, ProtectionPolicy,
        },
        runtime::{
            BackgroundResponse, BackgroundWorkloadSummary, StaleResponse,
            StaleWorkloadGroupSummary, StaleWorkloadSummary, TopProcess, TopResponse,
        },
    };

    #[test]
    fn parses_config_without_a_nested_command() {
        let cli = Cli::try_parse_from(["resource-guard", "config"]).unwrap();

        assert!(matches!(cli.command, Command::Config { command: None }));
    }

    #[test]
    fn parses_force_only_for_config_init() {
        let cli = Cli::try_parse_from(["resource-guard", "config", "init", "--force"]).unwrap();

        assert!(matches!(
            cli.command,
            Command::Config {
                command: Some(ConfigCommand::Init { force: true })
            }
        ));
    }

    #[test]
    fn yes_requires_kill_for_stop() {
        assert!(Cli::try_parse_from(["resource-guard", "stop", "42", "--yes"]).is_err());
    }

    #[test]
    fn parses_an_unforced_stop() {
        let cli = Cli::try_parse_from(["resource-guard", "stop", "42"]).unwrap();

        assert!(matches!(
            cli.command,
            Command::Stop {
                pid: 42,
                kill: false,
                yes: false
            }
        ));
    }

    #[test]
    fn parses_a_confirmed_force_stop() {
        let cli = Cli::try_parse_from(["resource-guard", "stop", "42", "--kill", "--yes"]).unwrap();

        assert!(matches!(
            cli.command,
            Command::Stop {
                pid: 42,
                kill: true,
                yes: true
            }
        ));
    }

    #[test]
    fn accepts_only_the_exact_pid_as_force_kill_confirmation() {
        let mut output = Vec::new();

        assert!(
            read_force_kill_confirmation(&mut Cursor::new(b"42\n"), &mut output, 42, "worker")
                .unwrap()
        );
        assert!(String::from_utf8(output).unwrap().contains("type 42"));
    }

    #[test]
    fn rejects_an_inexact_force_kill_confirmation() {
        assert!(
            !read_force_kill_confirmation(
                &mut Cursor::new(b"yes\n"),
                &mut Vec::new(),
                42,
                "worker"
            )
            .unwrap()
        );
    }

    #[test]
    fn parses_top_watch_mode() {
        let cli = Cli::try_parse_from(["resource-guard", "top", "--watch"]).unwrap();

        assert!(matches!(cli.command, Command::Top { watch: true }));
    }

    #[test]
    fn parses_stale_and_stop_tree_commands() {
        let stale = Cli::try_parse_from(["resource-guard", "stale"]).unwrap();
        assert!(matches!(stale.command, Command::Stale));

        let stop_tree = Cli::try_parse_from(["resource-guard", "stop-tree", "42"]).unwrap();
        assert!(matches!(
            stop_tree.command,
            Command::StopTree { root_pid: 42 }
        ));
    }

    #[test]
    fn parses_background_and_stop_background_commands() {
        let background = Cli::try_parse_from(["resource-guard", "background"]).unwrap();
        assert!(matches!(background.command, Command::Background));

        let stop_background =
            Cli::try_parse_from(["resource-guard", "stop-background", "42"]).unwrap();
        assert!(matches!(
            stop_background.command,
            Command::StopBackground { root_pid: 42 }
        ));
    }

    #[test]
    fn renders_background_workloads_with_growth_and_executable() {
        let response = BackgroundResponse {
            workloads: vec![BackgroundWorkloadSummary {
                group_id: "systemd-unit:app-1.scope".to_owned(),
                root_pid: 42,
                root_uid: 1_000,
                root_started_at: 99,
                name: "worker".to_owned(),
                executable: Some(PathBuf::from("/usr/bin/worker")),
                process_count: 3,
                process_count_growth: 2,
                total_memory_bytes: 1_572_864,
                memory_growth_bytes: 1_048_576,
                total_cpu_percent: 1.2,
                age_seconds: 3_661,
                observed_for_seconds: 1_800,
            }],
        };

        let output = render_background(&response);

        assert!(output.contains("worker"));
        assert!(output.contains("42"));
        assert!(output.contains("3(+2)"));
        assert!(output.contains("/usr/bin/worker"));
        assert!(output.contains("1h01m"));
    }

    #[test]
    fn renders_an_empty_background_response() {
        let response = BackgroundResponse {
            workloads: Vec::new(),
        };

        assert_eq!(
            render_background(&response).trim(),
            "no growing background applications detected"
        );
    }

    #[test]
    fn renders_stale_groups_with_members_and_direct_workloads() {
        let response = StaleResponse {
            workloads: vec![
                StaleWorkloadSummary {
                    root_pid: 10,
                    root_uid: Some(1_000),
                    root_started_at: Some(10),
                    name: "uv".to_owned(),
                    process_count: 2,
                    total_memory_bytes: 300 * 1_048_576,
                    total_cpu_percent: 0.1,
                    age_seconds: 7_200,
                },
                StaleWorkloadSummary {
                    root_pid: 20,
                    root_uid: Some(1_000),
                    root_started_at: Some(20),
                    name: "uv".to_owned(),
                    process_count: 2,
                    total_memory_bytes: 300 * 1_048_576,
                    total_cpu_percent: 0.1,
                    age_seconds: 7_200,
                },
                StaleWorkloadSummary {
                    root_pid: 30,
                    root_uid: Some(1_000),
                    root_started_at: Some(30),
                    name: "uv".to_owned(),
                    process_count: 2,
                    total_memory_bytes: 400 * 1_048_576,
                    total_cpu_percent: 0.1,
                    age_seconds: 7_200,
                },
            ],
            groups: vec![StaleWorkloadGroupSummary {
                working_directory: PathBuf::from("/work/alpha"),
                tree_count: 2,
                process_count: 4,
                total_memory_bytes: 600 * 1_048_576,
                total_cpu_percent: 0.2,
                age_seconds: 7_200,
                root_pids: vec![10, 20],
            }],
        };

        let output = render_stale(&response);

        assert!(output.contains("STALE WORKLOAD GROUPS"));
        assert!(output.contains("/work/alpha"));
        assert!(output.contains("ROOT 10"));
        assert!(output.contains("ROOT 20"));
        assert!(output.contains("DIRECT STALE WORKLOADS"));
        assert!(output.contains("30"));
        assert!(!output.contains("ROOT 30"));
    }

    #[test]
    fn renders_an_empty_stale_response() {
        let response = StaleResponse {
            workloads: Vec::new(),
            groups: Vec::new(),
        };

        assert_eq!(
            render_stale(&response).trim(),
            "no stale workloads detected"
        );
    }

    #[test]
    fn formats_resource_values_for_top() {
        assert_eq!(format_bytes(1_572_864), "1.5MiB");
        assert_eq!(format_duration(3_661), "1h01m");
    }

    #[test]
    fn renders_top_rows_and_limit_state() {
        let output = render_top(&TopResponse {
            sample_age_seconds: 2,
            processes: vec![TopProcess {
                pid: 42,
                name: "worker".to_owned(),
                cpu_percent: 75.5,
                resident_memory_bytes: 1_572_864,
                running_for_seconds: 61,
                exceeds_limit: true,
            }],
        });

        assert!(output.contains("sample: 2s ago"));
        assert!(output.contains("42"));
        assert!(output.contains("75.5%"));
        assert!(output.contains("1.5MiB"));
        assert!(output.contains("1m01s"));
        assert!(output.contains("yes worker"));
    }

    fn background_policy() -> BackgroundWorkloadPolicy {
        BackgroundWorkloadPolicy {
            enabled: true,
            minimum_age: Duration::from_hours(1),
            minimum_memory_bytes: 256 * 1_048_576,
            large_memory_bytes: 512 * 1_048_576,
            growth_window: Duration::from_secs(60),
            minimum_memory_growth_bytes: 128 * 1_048_576,
            minimum_process_count_growth: 2,
            maximum_cpu_percent: 5.0,
            consecutive_samples: 3,
            sample_interval: Duration::from_secs(60),
            notification_cooldown: Duration::from_secs(60),
            ignored_root_names: HashSet::new(),
            ignored_root_executables: HashSet::new(),
        }
    }

    fn background_process() -> ObservedProcess {
        ObservedProcess {
            descriptor: ProcessDescriptor::new(
                ProcessIdentity::new(10, 1_000, 10),
                "app10",
                Some(PathBuf::from("/usr/bin/app10")),
            )
            .with_runtime(None, ProcessState::Sleeping)
            .with_execution_context(ProcessExecutionContext::new(
                Some("systemd-unit:app-1.scope".to_owned()),
                Some("app-1.scope".to_owned()),
                ProcessOrigin::UserApplication,
                false,
            )),
            resources: ProcessResources {
                cpu_percent: 1.0,
                resident_memory_bytes: 600 * 1_048_576,
                virtual_memory_bytes: 600 * 1_048_576,
                running_for: Duration::from_hours(2),
                observed_at: Duration::ZERO,
            },
        }
    }

    fn background_summary(root_uid: u32, root_started_at: u64) -> BackgroundWorkloadSummary {
        BackgroundWorkloadSummary {
            group_id: "systemd-unit:app-1.scope".to_owned(),
            root_pid: 10,
            root_uid,
            root_started_at,
            name: "app10".to_owned(),
            executable: Some(PathBuf::from("/usr/bin/app10")),
            process_count: 1,
            process_count_growth: 0,
            total_memory_bytes: 600 * 1_048_576,
            memory_growth_bytes: 0,
            total_cpu_percent: 1.0,
            age_seconds: 7_200,
            observed_for_seconds: 3_600,
        }
    }

    #[test]
    fn stop_background_uses_pid_uid_and_start_time_and_rejects_mismatch() {
        let processes = vec![background_process()];
        let protection = ProtectionPolicy::default();
        let policy = background_policy();

        let matching = background_summary(1_000, 10);
        assert!(
            background_workload_for_stop(&processes, &matching, 1_000, &protection, &policy)
                .is_some()
        );

        let reused_start = background_summary(1_000, 11);
        assert!(
            background_workload_for_stop(&processes, &reused_start, 1_000, &protection, &policy)
                .is_none()
        );

        let wrong_owner = background_summary(1_001, 10);
        assert!(
            background_workload_for_stop(&processes, &wrong_owner, 1_000, &protection, &policy)
                .is_none()
        );
    }

    fn stale_process(pid: u32, started_at: u64) -> ObservedProcess {
        ObservedProcess {
            descriptor: ProcessDescriptor::new(
                ProcessIdentity::new(pid, 1_000, started_at),
                "pytest",
                None,
            )
            .with_runtime(None, ProcessState::Sleeping),
            resources: ProcessResources {
                cpu_percent: 0.1,
                resident_memory_bytes: 600 * 1_048_576,
                virtual_memory_bytes: 600 * 1_048_576,
                running_for: Duration::from_hours(2),
                observed_at: Duration::ZERO,
            },
        }
    }

    fn stale_summary(root_uid: Option<u32>, root_started_at: Option<u64>) -> StaleWorkloadSummary {
        StaleWorkloadSummary {
            root_pid: 10,
            root_uid,
            root_started_at,
            name: "pytest".to_owned(),
            process_count: 1,
            total_memory_bytes: 600 * 1_048_576,
            total_cpu_percent: 0.1,
            age_seconds: 7_200,
        }
    }

    #[test]
    fn stop_tree_uses_pid_uid_and_start_time_and_rejects_mismatch() {
        let processes = vec![stale_process(10, 10)];

        assert!(
            stale_workload_for_stop(&processes, &stale_summary(Some(1_000), Some(10)), 1_000)
                .is_ok()
        );
        assert!(
            stale_workload_for_stop(&processes, &stale_summary(Some(1_000), Some(11)), 1_000)
                .is_err()
        );
        assert!(
            stale_workload_for_stop(&processes, &stale_summary(Some(1_001), Some(10)), 1_000)
                .is_err()
        );
        assert!(stale_workload_for_stop(&processes, &stale_summary(None, None), 1_000).is_err());
    }
}
