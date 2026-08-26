use std::{error::Error, fmt, process::ExitCode};

use clap::{Parser, Subcommand};

use crate::{
    adapters::{
        ConfigOrigin, PidfdTerminationPort, SysinfoProcessSource, SystemClock, ThreadSleeper,
        TomlConfigRepository, current_user_id,
    },
    application::{PortError, ProcessSource, StopAndWait, StopError, StopOutcome},
    runtime::{self, RuntimeError},
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
        /// Request separately confirmed SIGKILL (not implemented yet).
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
    KillNotImplemented,
    Runtime(RuntimeError),
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
            Self::KillNotImplemented => write!(
                formatter,
                "SIGKILL is not implemented yet; no signal was sent"
            ),
            Self::Runtime(error) => error.fmt(formatter),
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
/// Returns configuration errors or a marker for commands not implemented yet.
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
            println!("active events: {}", status.active_events);
            if let Some(error) = status.last_error {
                println!("last error: {error}");
            }
            Ok(())
        }
        Command::Top { watch } => {
            let _ = watch;
            Err(CliError::NotImplemented("top"))
        }
        Command::Stop { pid, kill, yes } => execute_stop(pid, kill, yes),
    }
}

fn execute_stop(pid: u32, kill: bool, _yes: bool) -> Result<(), CliError> {
    if kill {
        return Err(CliError::KillNotImplemented);
    }

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
        StopOutcome::StillRunning => Err(CliError::StillRunning {
            pid,
            grace_period_seconds: grace_period.as_secs(),
        }),
    }
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
    use clap::Parser;

    use super::{Cli, Command, ConfigCommand};

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
}
