//! Outbe chain management CLI.
//!
//! Client-side tool for interacting with a running outbe-chain node via JSON-RPC.
//! Supports validator management, staking, slashing evidence, monitoring, TEE status,
//! governance votes, and local Radicle setup.

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use eyre::{bail, Context, Result};
use zeroize::Zeroizing;

mod abi;
mod commands;
pub(crate) mod errors;
mod rpc;
mod tx;

#[derive(Parser)]
#[command(name = "outbe-cli", about = "Outbe chain management CLI")]
struct Cli {
    /// JSON-RPC endpoint URL
    #[arg(long, default_value = "http://localhost:8545", global = true)]
    rpc_url: String,

    /// Private key for signing transactions (hex, with or without 0x prefix)
    #[arg(long, global = true, conflicts_with = "private_key_file")]
    private_key: Option<String>,

    /// Read the transaction signing key from a private file.
    #[arg(long, global = true, conflicts_with = "private_key")]
    private_key_file: Option<PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Validator management
    Validator {
        #[command(subcommand)]
        cmd: commands::validator::ValidatorCmd,
    },
    /// Staking operations
    Staking {
        #[command(subcommand)]
        cmd: commands::staking::StakingCmd,
    },
    /// Slash information and evidence submission
    Slash {
        #[command(subcommand)]
        cmd: commands::slash::SlashCmd,
    },
    /// Chain information
    Chain {
        #[command(subcommand)]
        cmd: commands::chain::ChainCmd,
    },
    /// Monitor node status, health checks, and readiness probes.
    Monitor {
        #[command(subcommand)]
        cmd: commands::monitor::MonitorCmd,
    },
    /// TEE: read registry, policy, upgrade status and node-reported enclave health.
    Tee {
        #[command(subcommand)]
        cmd: commands::tee::TeeCmd,
    },
    /// On-chain generic vote proposals.
    Vote {
        #[command(subcommand)]
        cmd: commands::vote::VoteCmd,
    },
    /// Local Radicle client and public repository registry.
    Rad {
        #[command(subcommand)]
        cmd: commands::rad::RadCmd,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let client = rpc::RpcClient::new(&cli.rpc_url)?;
    let private_key = resolve_private_key(cli.private_key, cli.private_key_file.as_deref())?;
    let private_key = private_key.as_deref().map(String::as_str);

    match cli.command {
        Commands::Validator { cmd } => cmd.run(&client, private_key).await,
        Commands::Staking { cmd } => cmd.run(&client, private_key).await,
        Commands::Slash { cmd } => cmd.run(&client, private_key).await,
        Commands::Chain { cmd } => cmd.run(&client).await,
        Commands::Monitor { cmd } => cmd.run(&client).await,
        Commands::Tee { cmd } => cmd.run(&client).await,
        Commands::Vote { cmd } => cmd.run(&client, private_key).await,
        Commands::Rad { cmd } => cmd.run(&client, private_key).await,
    }
}

fn resolve_private_key(
    value: Option<String>,
    path: Option<&Path>,
) -> Result<Option<Zeroizing<String>>> {
    if let Some(value) = value {
        return Ok(Some(Zeroizing::new(value)));
    }
    let Some(path) = path else {
        return Ok(None);
    };
    let metadata = path
        .symlink_metadata()
        .wrap_err_with(|| format!("failed to inspect private key file: {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("private key path must be a regular file, not a symlink");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        if metadata.permissions().mode() & 0o077 != 0 {
            bail!("private key file must not be accessible by group or others");
        }
    }
    let value = Zeroizing::new(
        std::fs::read_to_string(path)
            .wrap_err_with(|| format!("failed to read private key file: {}", path.display()))?,
    );
    let trimmed = value.trim();
    if trimmed.is_empty() {
        bail!("private key file is empty");
    }
    Ok(Some(Zeroizing::new(trimmed.to_owned())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn test_cli_parse_chain_status() {
        assert!(Cli::try_parse_from(["outbe-cli", "chain", "status"]).is_ok());
        assert!(Cli::try_parse_from(["outbe-cli", "chain", "consensus"]).is_ok());
    }

    #[test]
    fn test_cli_parse_validator_list() {
        let cli = Cli::try_parse_from(["outbe-cli", "validator", "list"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_cli_parse_staking_info() {
        let cli = Cli::try_parse_from([
            "outbe-cli",
            "staking",
            "info",
            "0x1111111111111111111111111111111111111111",
        ]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_cli_parse_staking_unstake_zero_amount() {
        let cli = Cli::try_parse_from(["outbe-cli", "staking", "unstake", "--amount", "0"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn tee_status_parses_without_socket_or_signer() {
        assert!(Cli::try_parse_from(["outbe-cli", "tee", "status"]).is_ok());
        assert!(Cli::try_parse_from([
            "outbe-cli",
            "tee",
            "status",
            "--node-data-dir",
            "/tmp/node",
            "--warning-blocks",
            "600",
            "--critical-blocks",
            "120",
        ])
        .is_ok());
        for flag in ["--warning-blocks", "--critical-blocks"] {
            let error = Cli::try_parse_from(["outbe-cli", "tee", "status", flag, "1"])
                .err()
                .unwrap();
            assert_eq!(
                error.kind(),
                clap::error::ErrorKind::MissingRequiredArgument
            );
        }
    }

    #[test]
    fn tee_removed_commands_are_rejected() {
        for command in [
            "join",
            "renew",
            "renew-now",
            "pubkey",
            "upgrade-status",
            "upgrade-prepare",
            "upgrade-provision",
            "upgrade-copy-root",
            "upgrade-submit",
            "upgrade-finalize",
        ] {
            let error = Cli::try_parse_from(["outbe-cli", "tee", command])
                .err()
                .unwrap();
            assert_eq!(
                error.kind(),
                clap::error::ErrorKind::InvalidSubcommand,
                "{command}"
            );
        }
    }

    #[test]
    fn tee_status_rejects_removed_socket_flags() {
        for flag in ["--enclave-socket", "--candidate-enclave-socket"] {
            let error = Cli::try_parse_from([
                "outbe-cli",
                "tee",
                "status",
                "--node-data-dir",
                "/tmp/node",
                flag,
                "/tmp/enclave.sock",
            ])
            .err()
            .unwrap();
            assert_eq!(
                error.kind(),
                clap::error::ErrorKind::UnknownArgument,
                "{flag}"
            );
        }
    }

    #[test]
    fn private_key_file_parses_and_conflicts_with_inline_key() {
        assert!(Cli::try_parse_from([
            "outbe-cli",
            "--private-key-file",
            "/tmp/evm-key.hex",
            "staking",
            "claim",
        ])
        .is_ok());
        assert!(Cli::try_parse_from([
            "outbe-cli",
            "--private-key",
            "11",
            "--private-key-file",
            "/tmp/evm-key.hex",
            "staking",
            "claim",
        ])
        .is_err());
    }

    #[test]
    fn test_cli_parse_monitor_health() {
        let cli = Cli::try_parse_from(["outbe-cli", "monitor", "health"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_cli_parse_slash_config() {
        let cli = Cli::try_parse_from(["outbe-cli", "slash", "config"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_cli_rejects_removed_subcommands() {
        for command in [
            "oracle",
            "stablecoin",
            "radicle",
            "rewards",
            "epoch",
            "zero-fee",
        ] {
            let error = Cli::try_parse_from(["outbe-cli", command]).err().unwrap();
            assert_eq!(
                error.kind(),
                clap::error::ErrorKind::InvalidSubcommand,
                "{command}"
            );
        }

        for nested in [
            ["outbe-cli", "slash", "history"],
            ["outbe-cli", "validator", "fund-operational-key"],
            ["outbe-cli", "chain", "info"],
            ["outbe-cli", "chain", "balance"],
            ["outbe-cli", "chain", "block"],
            ["outbe-cli", "chain", "vrf-seed"],
            ["outbe-cli", "chain", "network"],
        ] {
            let error = Cli::try_parse_from(nested).err().unwrap();
            assert_eq!(
                error.kind(),
                clap::error::ErrorKind::InvalidSubcommand,
                "{nested:?}"
            );
        }
    }

    #[test]
    fn test_cli_parse_custom_rpc_url() {
        let cli = Cli::try_parse_from([
            "outbe-cli",
            "--rpc-url",
            "http://localhost:9545",
            "chain",
            "status",
        ])
        .unwrap();
        assert_eq!(cli.rpc_url, "http://localhost:9545");
    }

    #[test]
    fn test_cli_parse_invalid_command() {
        let cli = Cli::try_parse_from(["outbe-cli", "bogus"]);
        assert!(cli.is_err());
    }

    #[test]
    fn test_cli_default_rpc_url() {
        let cli = Cli::try_parse_from(["outbe-cli", "chain", "status"]).unwrap();
        assert_eq!(cli.rpc_url, "http://localhost:8545");
    }
}
