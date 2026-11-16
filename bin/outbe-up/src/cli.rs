use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use eyre::Result;

use crate::{network, validator};

#[derive(Debug, Parser)]
#[command(name = "outbe-up", about = "Outbe node operator CLI")]
pub struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Build and operate a new founding-validator network.
    Network(NetworkArgs),
    /// Prepare, synchronize, promote, and inspect a validator host.
    Validator(ValidatorArgs),
    #[command(hide = true)]
    Internal(InternalArgs),
}

#[derive(Debug, Args)]
struct NetworkArgs {
    #[command(subcommand)]
    command: NetworkCommand,
}

#[derive(Debug, Subcommand)]
enum NetworkCommand {
    /// Write a documented network configuration template.
    Init {
        #[arg(long, default_value = "network.yaml")]
        output: PathBuf,
    },
    /// Build genesis and host-specific artifacts from one immutable software bundle.
    Build {
        #[arg(long)]
        software_bundle: PathBuf,
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        output: PathBuf,
        /// Reuse an existing validator-N key tree instead of generating new identities.
        #[arg(long)]
        keys_dir: Option<PathBuf>,
    },
    /// Verify and install every founding host without starting services.
    Prepare {
        #[arg(long)]
        artifacts: PathBuf,
        #[arg(long)]
        ssh_key: Option<PathBuf>,
    },
    /// Start every founder and wait for finalized consensus.
    Start {
        #[arg(long)]
        artifacts: PathBuf,
        #[arg(long)]
        ssh_key: Option<PathBuf>,
        #[arg(long, default_value_t = 600)]
        timeout: u64,
    },
    /// Show the coordinated state of every founding validator.
    Status {
        #[arg(long)]
        artifacts: PathBuf,
        #[arg(long)]
        ssh_key: Option<PathBuf>,
        #[arg(long)]
        watch: bool,
    },
}

#[derive(Debug, Args)]
struct ValidatorArgs {
    #[command(subcommand)]
    command: ValidatorCommand,
}

#[derive(Debug, Subcommand)]
enum ValidatorCommand {
    /// Write a documented operator configuration template.
    ConfigExample {
        #[arg(long, default_value = "operator-config.yaml")]
        output: PathBuf,
    },
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
            Command::Network(args) => match args.command {
                NetworkCommand::Init { output } => network::config::init(&output),
                NetworkCommand::Build {
                    software_bundle,
                    config,
                    output,
                    keys_dir,
                } => network::build::run(&software_bundle, &config, &output, keys_dir.as_deref()),
                NetworkCommand::Prepare { artifacts, ssh_key } => {
                    network::prepare::run(&artifacts, ssh_key.as_deref()).await
                }
                NetworkCommand::Start {
                    artifacts,
                    ssh_key,
                    timeout,
                } => {
                    network::start::run(
                        &artifacts,
                        ssh_key.as_deref(),
                        std::time::Duration::from_secs(timeout),
                    )
                    .await
                }
                NetworkCommand::Status {
                    artifacts,
                    ssh_key,
                    watch,
                } => network::status::show(&artifacts, ssh_key.as_deref(), watch).await,
            },
            Command::Validator(args) => match args.command {
                ValidatorCommand::ConfigExample { output } => {
                    validator::assets::write_config_example(&output)
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
        assert!(
            Cli::try_parse_from(["outbe-up", "network", "init", "--output", "network.yaml",])
                .is_ok()
        );
        assert!(Cli::try_parse_from([
            "outbe-up",
            "network",
            "build",
            "--software-bundle",
            "software.tar.zst",
            "--config",
            "network.yaml",
            "--output",
            "network-build",
        ])
        .is_ok());
        assert!(Cli::try_parse_from([
            "outbe-up",
            "network",
            "start",
            "--artifacts",
            "network-final",
            "--timeout",
            "900",
        ])
        .is_ok());
        assert!(Cli::try_parse_from([
            "outbe-up",
            "network",
            "status",
            "--artifacts",
            "network-final",
            "--watch",
        ])
        .is_ok());
        assert!(Cli::try_parse_from([
            "outbe-up",
            "network",
            "prepare",
            "--artifacts",
            "network-final",
            "--ssh-key",
            "operator-key",
        ])
        .is_ok());
        assert!(Cli::try_parse_from([
            "outbe-up",
            "validator",
            "config-example",
            "--output",
            "operator-config.yaml",
        ])
        .is_ok());
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
