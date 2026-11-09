//! Host-local Outbe validator lifecycle orchestration.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use eyre::Result;

mod bundle;
mod config;
mod journal;
mod process;
mod progress;
mod rpc;
mod snapshot;
mod validator;

#[derive(Debug, Parser)]
#[command(name = "outbe", about = "Outbe node operator CLI")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Prepare, synchronize, promote, and inspect a validator host.
    Validator(ValidatorArgs),
}

#[derive(Debug, Args)]
struct ValidatorArgs {
    #[command(subcommand)]
    command: ValidatorCommand,
}

#[derive(Debug, Subcommand)]
enum ValidatorCommand {
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

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Validator(args) => match args.command {
            ValidatorCommand::Prepare { bundle, config } => {
                validator::prepare(&bundle, &config).await
            }
            ValidatorCommand::Sync { snapshot } => validator::sync(snapshot.as_deref()).await,
            ValidatorCommand::Promote => validator::promote().await,
            ValidatorCommand::Status { watch } => validator::status(watch).await,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_commands_parse() {
        assert!(Cli::try_parse_from([
            "outbe",
            "validator",
            "prepare",
            "--bundle",
            "bundle.tar.zst",
            "--config",
            "validator.yaml",
        ])
        .is_ok());
        assert!(
            Cli::try_parse_from(["outbe", "validator", "sync", "--snapshot", "snapshot.tar",])
                .is_ok()
        );
        assert!(Cli::try_parse_from(["outbe", "validator", "promote"]).is_ok());
        assert!(Cli::try_parse_from(["outbe", "validator", "status", "--watch"]).is_ok());
    }
}
