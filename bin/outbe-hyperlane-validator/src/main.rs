//! Outbe Hyperlane validator daemon.
//!
//! Proves that this validator's Hyperlane agent keeps signing. The daemon reads
//! the latest signed checkpoint per domain from the validator's own bucket. It
//! submits that checkpoint to the HyperlaneController precompile (see
//! `liveness.rs`).

mod abi;
// The validator port uses these modules. The submit loop wires them in.
#[allow(dead_code)]
mod checkpoint;
mod client;
mod config;
mod errors;
mod health;
mod liveness;
#[allow(dead_code)]
mod merkle;
#[allow(dead_code)]
mod signer;
#[allow(dead_code)]
mod store;
#[allow(dead_code)]
mod sync;

use std::sync::Arc;

use clap::{Parser, Subcommand};
use eyre::Result;
use tracing::{info, warn};

use crate::config::ValidatorConfig;
use crate::errors::ValidatorError;
use crate::health::ValidatorHealth;
use crate::store::{Meta, Store};

#[derive(Parser)]
#[command(
    name = "outbe-hyperlane-validator",
    about = "Outbe Hyperlane validator daemon"
)]
struct Cli {
    /// Path to the TOML configuration file.
    #[arg(short, long, global = true, default_value = "hyperlane-validator.toml")]
    config: String,
    /// Without a command, run the daemon.
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Read or repair the local store (`[storage] db_path`). Stop the daemon
    /// first: the store opens in exclusive mode.
    #[command(subcommand)]
    Db(DbCommand),
}

#[derive(Subcommand)]
enum DbCommand {
    /// Per domain: leaf count, latest signed index, cursor, reorg flag.
    Status,
    /// Print one stored leaf and its signed root.
    Checkpoint {
        #[arg(long)]
        domain: u32,
        #[arg(long)]
        index: u32,
    },
    /// Remove the reorg flag of a domain. Do this only after the cause is fixed.
    ClearReorg {
        #[arg(long)]
        domain: u32,
    },
}

fn run_db(config: &ValidatorConfig, command: DbCommand) -> Result<()> {
    let path = &config
        .storage
        .as_ref()
        .ok_or(ValidatorError::NoDbPath)?
        .db_path;
    let store = Store::open(path)?;
    match command {
        DbCommand::Status => {
            for domain in store.domains()? {
                println!(
                    "domain {domain}: leaves {}, latest signed {:?}, cursor {:?}, reorg {}",
                    store.leaf_count(domain)?,
                    store.latest_checkpoint_index(domain)?,
                    store.cursor(domain)?,
                    store.meta(domain, Meta::ReorgFlag)?.is_some(),
                );
            }
        }
        DbCommand::Checkpoint { domain, index } => {
            let leaf = store.leaf(domain, index)?;
            let signed = store.checkpoint(domain, index)?;
            println!("leaf: {leaf:?}");
            match signed {
                Some(s) => println!(
                    "root: {}\nsignature: 0x{}",
                    s.root,
                    alloy_primitives::hex::encode(s.signature)
                ),
                None => println!("not signed"),
            }
        }
        DbCommand::ClearReorg { domain } => {
            store.delete_meta(domain, Meta::ReorgFlag)?;
            println!("reorg flag of domain {domain} removed");
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "outbe_hyperlane_validator=info".into()),
        )
        .init();

    let cli = Cli::parse();
    let config = ValidatorConfig::load(&cli.config)?;
    if let Some(Command::Db(command)) = cli.command {
        return run_db(&config, command);
    }
    config.validate()?;
    warn!("private key loaded from plaintext config - use an encrypted keystore in production");

    let wallet = client::create_wallet(&config.account)?;
    let health = Arc::new(ValidatorHealth::new());
    if config.health.as_ref().is_none_or(|h| h.enabled) {
        let bind = config
            .health
            .as_ref()
            .map(|h| h.bind_address.clone())
            .unwrap_or_else(|| "0.0.0.0:9003".to_string());
        health::start_health_server(&bind, health.clone()).await?;
    }

    info!(
        rpc = %config.chain.rpc_endpoint,
        validator = %config.account.validator_address,
        "starting outbe-hyperlane-validator"
    );
    let liveness = liveness::Liveness::new(
        config.hyperlane,
        config.chain,
        wallet,
        config.account.validator_address.parse()?,
        health,
    );
    tokio::select! {
        _ = liveness.run() => {}
        _ = tokio::signal::ctrl_c() => info!("shutdown signal received, exiting"),
    }
    Ok(())
}
