//! Outbe Hyperlane validator daemon.
//!
//! Syncs the MerkleTreeHook of each origin chain, signs checkpoints into a
//! local store, serves them as an S3-compatible bucket, announces the bucket
//! on each origin chain, and proves liveness to the HyperlaneController.

mod abi;
mod checkpoint;
mod client;
mod config;
mod daemon;
mod errors;
mod health;
mod liveness;
mod merkle;
mod server;
mod signer;
mod store;
mod sync;

use std::sync::Arc;
use std::time::Duration;

use alloy_network::EthereumWallet;
use alloy_signer_local::PrivateKeySigner;
use clap::{Parser, Subcommand};
use eyre::{Context, Result};
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

    let key: PrivateKeySigner = config
        .account
        .private_key
        .parse()
        .with_context(|| "invalid private key")?;
    let validator = config.account.validator_address.parse()?;
    let server = config.server.as_ref().ok_or(ValidatorError::NoServer)?;
    let db_path = &config
        .storage
        .as_ref()
        .ok_or(ValidatorError::NoDbPath)?
        .db_path;
    let bucket = format!("{:#x}", key.address());
    let outbe = sync::connect(&config.chain.rpc_endpoint)?;

    let store = Arc::new(Store::open(db_path)?);
    let health = Arc::new(ValidatorHealth::new(store.clone()));
    if config.health.as_ref().is_none_or(|h| h.enabled) {
        let bind = config
            .health
            .as_ref()
            .map(|h| h.bind_address.clone())
            .unwrap_or_else(|| "0.0.0.0:9003".to_string());
        health::start_health_server(&bind, health.clone()).await?;
    }
    let location = format!("{}/{bucket}", server.location_base()?);
    server::start(
        &server.bind_address,
        server::Bucket {
            name: bucket,
            store: store.clone(),
        },
    )
    .await?;

    info!(
        rpc = %config.chain.rpc_endpoint,
        validator = %validator,
        signer = %key.address(),
        location,
        "starting outbe-hyperlane-validator"
    );
    let poll_interval = Duration::from_secs(config.hyperlane.poll_interval_secs);
    let liveness = liveness::Liveness::new(
        config.hyperlane,
        &config.chain,
        EthereumWallet::from(key.clone()),
        validator,
        health.clone(),
    );
    let daemon = daemon::Daemon {
        origins: config.origin,
        poll_interval,
        key,
        validator,
        store,
        health,
        location,
    };
    tokio::select! {
        _ = daemon.run(outbe) => {}
        _ = liveness.run() => {}
        signal = shutdown_signal() => info!(signal, "shutdown signal received"),
    }
    // Leaving `main` drops the domain tasks. The runtime waits for an open
    // store write to end, then the last store handle closes MDBX.
    info!("stopped");
    Ok(())
}

async fn shutdown_signal() -> &'static str {
    let mut term = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
        Ok(term) => term,
        Err(error) => {
            warn!(%error, "SIGTERM handler failed; only SIGINT stops the daemon");
            let _ = tokio::signal::ctrl_c().await;
            return "SIGINT";
        }
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => "SIGINT",
        _ = term.recv() => "SIGTERM",
    }
}
