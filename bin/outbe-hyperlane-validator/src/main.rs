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

use std::sync::Arc;

use clap::Parser;
use eyre::Result;
use tracing::{info, warn};

use crate::config::ValidatorConfig;
use crate::health::ValidatorHealth;

#[derive(Parser)]
#[command(
    name = "outbe-hyperlane-validator",
    about = "Outbe Hyperlane validator daemon"
)]
struct Cli {
    /// Path to the TOML configuration file.
    #[arg(short, long, default_value = "hyperlane-validator.toml")]
    config: String,
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
