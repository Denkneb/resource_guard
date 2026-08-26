use std::{error::Error, fmt, process::ExitCode};

use clap::{Parser, Subcommand};

use crate::adapters::{ConfigOrigin, TomlConfigRepository};

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
        /// Request SIGKILL after a separate confirmation.
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
    NotImplemented(&'static str),
}

impl fmt::Display for CliError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(error) => error.fmt(formatter),
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
        Command::Daemon => Err(CliError::NotImplemented("daemon")),
        Command::Status => Err(CliError::NotImplemented("status")),
        Command::Top { watch } => {
            let _ = watch;
            Err(CliError::NotImplemented("top"))
        }
        Command::Stop { pid, kill, yes } => {
            let _ = (pid, kill, yes);
            Err(CliError::NotImplemented("stop"))
        }
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
}
