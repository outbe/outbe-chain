//! Outbe Hyperlane attester daemon.
//!
//! Proves that this validator's Hyperlane agent keeps signing: reads the
//! latest signed checkpoint per domain from the validator's own bucket and
//! submits it to the HyperlaneController precompile (see `attester.rs`).

mod abi;
mod attester;
mod client;
mod config;
mod health;

use std::sync::Arc;

use clap::Parser;
use eyre::Result;
use tracing::{info, warn};

use crate::config::AttesterConfig;
use crate::health::AttesterHealth;

#[derive(Parser)]
#[command(
    name = "outbe-hyperlane-attester",
    about = "Outbe Hyperlane checkpoint attester daemon"
)]
struct Cli {
    /// Path to the TOML configuration file.
    #[arg(short, long, default_value = "hyperlane-attester.toml")]
    config: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "outbe_hyperlane_attester=info".into()),
        )
        .init();

    let cli = Cli::parse();
    let config = AttesterConfig::load(&cli.config)?;
    config.validate()?;
    warn!("private key loaded from plaintext config - use an encrypted keystore in production");

    let wallet = client::create_wallet(&config.account)?;
    let health = Arc::new(AttesterHealth::new());
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
        "starting outbe-hyperlane-attester"
    );
    let attester = attester::Attester::new(
        config.hyperlane,
        config.chain.rpc_endpoint,
        config.chain.chain_id,
        wallet,
        config.account.validator_address.parse()?,
        health,
    );
    tokio::select! {
        _ = attester.run() => {}
        _ = tokio::signal::ctrl_c() => info!("shutdown signal received, exiting"),
    }
    Ok(())
}
