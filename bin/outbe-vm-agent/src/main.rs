//! Minimal guest-only runtime agent baked into Outbe confidential VM images (R-4 / S8).
//!
//! Replaces `outbe-up internal ...` inside VM images so the measured guest OS never ships
//! VMM client, SSH, image-build, or network-orchestration code.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use eyre::Result;
use outbe_operator_node_role as node_role;

#[derive(Debug, Parser)]
#[command(
    name = "outbe-vm-agent",
    about = "Minimal in-guest runtime agent for Outbe VM systemd units"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Verify the unmeasured VM data disk, configure static IPv4, and start the role's systemd units.
    VmBoot(BinDirArgs),
    /// Render feeder configuration and exec `outbe-feeder`.
    Feeder(BinDirArgs),
    /// Render Hyperlane configuration and exec `outbe-hyperlane-validator`.
    Hyperlane(BinDirArgs),
    /// Wait for the local JSON-RPC endpoint to respond.
    WaitRpc(WaitArgs),
    /// Wait for validator sidecar dependencies (Radicle control socket).
    WaitValidatorDependencies(WaitArgs),
}

#[derive(Debug, Args)]
struct BinDirArgs {
    /// Directory containing child daemon binaries. Defaults to `/opt/outbe-chain`.
    #[arg(long)]
    bin_dir: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct WaitArgs {
    #[arg(long, default_value_t = 120)]
    timeout: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::VmBoot(args) => node_role::vm_boot::run(args.bin_dir.as_deref()),
        Command::Feeder(args) => node_role::service::feeder(args.bin_dir.as_deref()),
        Command::Hyperlane(args) => {
            node_role::hyperlane::run_service(args.bin_dir.as_deref()).await
        }
        Command::WaitRpc(args) => node_role::service::wait_rpc(args.timeout).await,
        Command::WaitValidatorDependencies(args) => {
            node_role::service::wait_validator_dependencies(args.timeout).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn clap_definition_passes_debug_assert_and_parses_all_guest_commands() {
        Cli::command().debug_assert();
        for argv in [
            vec!["outbe-vm-agent", "vm-boot", "--bin-dir", "/usr/local/bin"],
            vec!["outbe-vm-agent", "feeder", "--bin-dir", "/usr/local/bin"],
            vec!["outbe-vm-agent", "hyperlane", "--bin-dir", "/usr/local/bin"],
            vec!["outbe-vm-agent", "wait-rpc"],
            vec!["outbe-vm-agent", "wait-rpc", "--timeout", "60"],
            vec!["outbe-vm-agent", "wait-validator-dependencies"],
        ] {
            assert!(
                Cli::try_parse_from(argv.clone()).is_ok(),
                "failed to parse {argv:?}"
            );
        }
    }
}
