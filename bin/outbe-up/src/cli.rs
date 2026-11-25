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
    #[command(alias = "init")]
    ConfigExample {
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
    /// Stop every founder and disable automatic start after VM reboot.
    Stop {
        #[arg(long)]
        artifacts: PathBuf,
        #[arg(long)]
        ssh_key: Option<PathBuf>,
        #[arg(long)]
        skip_dkg_guard: bool,
    },
    /// Restart founders one at a time, waiting for health after each host.
    Restart {
        #[arg(long)]
        artifacts: PathBuf,
        #[arg(long)]
        ssh_key: Option<PathBuf>,
        #[arg(long, default_value_t = 600)]
        timeout: u64,
        #[arg(long)]
        skip_dkg_guard: bool,
        #[arg(long)]
        skip_quorum_guard: bool,
    },
    /// Upgrade founders one at a time from a verified software bundle.
    Upgrade {
        #[arg(long)]
        artifacts: PathBuf,
        #[arg(long)]
        software_bundle: PathBuf,
        #[arg(long)]
        ssh_key: Option<PathBuf>,
        #[arg(long, default_value_t = 600)]
        timeout: u64,
        #[arg(long)]
        skip_dkg_guard: bool,
        #[arg(long)]
        skip_quorum_guard: bool,
    },
}

#[derive(Debug, Args)]
struct ValidatorArgs {
    #[command(subcommand)]
    command: ValidatorCommand,
}

#[derive(Debug, Subcommand)]
enum ValidatorCommand {
    /// Print a documented operator configuration template, or write it to --output.
    ConfigExample {
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Verify and install an operator bundle without starting services.
    Prepare {
        #[arg(long)]
        bundle: PathBuf,
        #[arg(long)]
        config: PathBuf,
    },
    /// Restore an optional snapshot, start the validator node, and reach tip.
    Sync {
        #[arg(long)]
        snapshot: Option<PathBuf>,
    },
    /// Stake and confirm readiness without restarting the synchronized node.
    Promote,
    /// Show the current lifecycle state.
    Status {
        /// Keep updating until interrupted.
        #[arg(long)]
        watch: bool,
    },
    /// Start an already prepared validator and wait until it is synchronized.
    Start,
    /// Stop the validator and disable automatic start after VM reboot.
    Stop {
        #[arg(long)]
        skip_dkg_guard: bool,
    },
    /// Restart the validator without changing its persistent auto-start policy.
    Restart {
        #[arg(long)]
        skip_dkg_guard: bool,
    },
    /// Upgrade this validator from a verified software-only bundle.
    Upgrade {
        #[arg(long)]
        software_bundle: PathBuf,
        #[arg(long)]
        skip_dkg_guard: bool,
    },
}

#[derive(Debug, Args)]
struct InternalArgs {
    #[command(subcommand)]
    command: InternalCommand,
}

#[derive(Debug, Subcommand)]
enum InternalCommand {
    Feeder,
    Hyperlane,
    HyperlaneSetup,
    WaitRpc(WaitArgs),
    WaitValidatorDependencies(WaitArgs),
    ApplySoftwareUpgrade {
        #[arg(long)]
        software_bundle: PathBuf,
        #[arg(long)]
        founder_index: Option<usize>,
    },
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
                NetworkCommand::ConfigExample { output } => network::config::init(&output),
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
                NetworkCommand::Stop {
                    artifacts,
                    ssh_key,
                    skip_dkg_guard,
                } => network::lifecycle::stop(&artifacts, ssh_key.as_deref(), skip_dkg_guard).await,
                NetworkCommand::Restart {
                    artifacts,
                    ssh_key,
                    timeout,
                    skip_dkg_guard,
                    skip_quorum_guard,
                } => {
                    network::lifecycle::restart(
                        &artifacts,
                        ssh_key.as_deref(),
                        std::time::Duration::from_secs(timeout),
                        skip_dkg_guard,
                        skip_quorum_guard,
                    )
                    .await
                }
                NetworkCommand::Upgrade {
                    artifacts,
                    software_bundle,
                    ssh_key,
                    timeout,
                    skip_dkg_guard,
                    skip_quorum_guard,
                } => {
                    network::upgrade::run(
                        &artifacts,
                        &software_bundle,
                        ssh_key.as_deref(),
                        std::time::Duration::from_secs(timeout),
                        skip_dkg_guard,
                        skip_quorum_guard,
                    )
                    .await
                }
            },
            Command::Validator(args) => match args.command {
                ValidatorCommand::ConfigExample { output } => match output {
                    Some(path) => validator::assets::write_config_example(&path),
                    None => {
                        print!("{}", validator::assets::OPERATOR_CONFIG_EXAMPLE);
                        Ok(())
                    }
                },
                ValidatorCommand::Prepare { bundle, config } => {
                    validator::prepare::run(&bundle, &config).await
                }
                ValidatorCommand::Sync { snapshot } => {
                    validator::sync::run(snapshot.as_deref()).await
                }
                ValidatorCommand::Promote => validator::promote::run().await,
                ValidatorCommand::Status { watch } => validator::status::show(watch).await,
                ValidatorCommand::Start => validator::maintenance::start().await,
                ValidatorCommand::Stop { skip_dkg_guard } => {
                    validator::maintenance::stop(skip_dkg_guard).await
                }
                ValidatorCommand::Restart { skip_dkg_guard } => {
                    validator::maintenance::restart(skip_dkg_guard).await
                }
                ValidatorCommand::Upgrade {
                    software_bundle,
                    skip_dkg_guard,
                } => validator::upgrade::run(&software_bundle, skip_dkg_guard).await,
            },
            Command::Internal(args) => match args.command {
                InternalCommand::Feeder => validator::service::feeder(),
                InternalCommand::Hyperlane => validator::hyperlane::run_service().await,
                InternalCommand::HyperlaneSetup => validator::hyperlane::setup_founder().await,
                InternalCommand::WaitRpc(args) => validator::service::wait_rpc(args.timeout).await,
                InternalCommand::WaitValidatorDependencies(args) => {
                    validator::service::wait_validator_dependencies(args.timeout).await
                }
                InternalCommand::ApplySoftwareUpgrade {
                    software_bundle,
                    founder_index,
                } => validator::upgrade::apply(&software_bundle, founder_index),
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
        for command in ["config-example", "init"] {
            let parsed = Cli::try_parse_from(["outbe-up", "network", command]).unwrap();
            assert!(matches!(
                parsed.command,
                Command::Network(NetworkArgs {
                    command: NetworkCommand::ConfigExample { output }
                }) if output.as_os_str() == "network.yaml"
            ));
        }
        assert!(Cli::try_parse_from([
            "outbe-up",
            "network",
            "build",
            "--software-bundle",
            "software.tar.zst",
            "--config",
            "network.yaml",
            "--output",
            "network-artifacts",
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
    fn validator_config_example_output_is_optional() {
        for args in [
            vec!["outbe-up", "validator", "config-example"],
            vec![
                "outbe-up",
                "validator",
                "config-example",
                "--output",
                "operator-config.yaml",
            ],
        ] {
            let expected = args.get(4).map(PathBuf::from);
            let parsed = Cli::try_parse_from(args).unwrap();
            assert!(matches!(
                parsed.command,
                Command::Validator(ValidatorArgs {
                    command: ValidatorCommand::ConfigExample { output }
                }) if output == expected
            ));
        }
    }

    #[test]
    fn internal_commands_are_hidden_from_help() {
        let help = Cli::command().render_help().to_string();
        assert!(!help.contains("internal"));
    }
}
