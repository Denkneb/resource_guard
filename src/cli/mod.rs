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
        ForceStopProcess, PortError, ProcessSource, StopAndWait, StopError, StopOutcome,
        WaitForExit,
    },
    runtime::{self, RuntimeError, TopResponse},
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
        Command::Stop { pid, kill, yes } => execute_stop(pid, kill, yes),
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
    use std::io::Cursor;

    use clap::Parser;

    use super::{
        Cli, Command, ConfigCommand, format_bytes, format_duration, read_force_kill_confirmation,
        render_top,
    };
    use crate::runtime::{TopProcess, TopResponse};

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
}
