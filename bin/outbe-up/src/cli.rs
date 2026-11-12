use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use eyre::Result;

use crate::validator;

#[derive(Debug, Parser)]
#[command(name = "outbe-up", about = "Outbe node operator CLI")]
pub struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Prepare, synchronize, promote, and inspect a validator host.
    Validator(ValidatorArgs),
    #[command(hide = true)]
    Internal(InternalArgs),
}

#[derive(Debug, Args)]
struct ValidatorArgs {
    #[command(subcommand)]
    command: ValidatorCommand,
}

#[derive(Debug, Subcommand)]
enum ValidatorCommand {
    /// Print an operator configuration template.
    ConfigExample,
    /// Verify and install an operator bundle without starting services.
    Prepare {
        #[arg(long)]
        bundle: PathBuf,
        #[arg(long)]
        config: PathBuf,
    },
    /// Restore an optional snapshot, start a keyless full node, and reach tip.
    Sync {
        #[arg(long)]
        snapshot: Option<PathBuf>,
    },
    /// Join the TEE registry and promote the synchronized node to a validator.
    Promote,
    /// Show the current lifecycle state.
    Status {
        /// Keep updating until interrupted.
        #[arg(long)]
        watch: bool,
    },
}

#[derive(Debug, Args)]
struct InternalArgs {
    #[command(subcommand)]
    command: InternalCommand,
}

#[derive(Debug, Subcommand)]
enum InternalCommand {
    Enclave,
    Feeder,
    WaitRpc(WaitArgs),
    WaitEnclave(WaitArgs),
    WaitValidatorDependencies(WaitArgs),
}

#[derive(Debug, Args)]
struct WaitArgs {
    #[arg(long, default_value_t = 120)]
    timeout: u64,
}

impl Cli {
    pub async fn run(self) -> Result<()> {
        match self.command {
            Command::Validator(args) => match args.command {
                ValidatorCommand::ConfigExample => {
                    print!("{}", validator::assets::OPERATOR_CONFIG_EXAMPLE);
                    Ok(())
                }
                ValidatorCommand::Prepare { bundle, config } => {
                    validator::prepare::run(&bundle, &config).await
                }
                ValidatorCommand::Sync { snapshot } => {
                    validator::sync::run(snapshot.as_deref()).await
                }
                ValidatorCommand::Promote => validator::promote::run().await,
                ValidatorCommand::Status { watch } => validator::status::show(watch).await,
            },
            Command::Internal(args) => match args.command {
                InternalCommand::Enclave => validator::service::enclave(),
                InternalCommand::Feeder => validator::service::feeder(),
                InternalCommand::WaitRpc(args) => validator::service::wait_rpc(args.timeout).await,
                InternalCommand::WaitEnclave(args) => {
                    validator::service::wait_enclave(args.timeout).await
                }
                InternalCommand::WaitValidatorDependencies(args) => {
                    validator::service::wait_validator_dependencies(args.timeout).await
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn lifecycle_commands_parse() {
        assert!(Cli::try_parse_from([
            "outbe-up",
            "validator",
            "prepare",
            "--bundle",
            "bundle.tar.zst",
            "--config",
            "validator.yaml",
        ])
        .is_ok());
        assert!(Cli::try_parse_from([
            "outbe-up",
            "validator",
            "sync",
            "--snapshot",
            "snapshot.tar",
        ])
        .is_ok());
        assert!(Cli::try_parse_from(["outbe-up", "validator", "promote"]).is_ok());
        assert!(Cli::try_parse_from(["outbe-up", "validator", "status", "--watch"]).is_ok());
    }

    #[test]
    fn internal_commands_are_hidden_from_help() {
        let help = Cli::command().render_help().to_string();
        assert!(!help.contains("internal"));
    }
}
