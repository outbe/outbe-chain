//! Outbe price-feeder daemon.
//!
//! Fetches prices from external providers, aggregates them via VWAP,
//! and submits oracle votes to the on-chain Oracle precompile.

mod abi;
mod aggregator;
mod config;
mod fixed;
mod health;
mod journal;
mod oracle_client;
mod provider;
#[cfg(test)]
mod recovery_tests;
mod vote_builder;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use alloy_primitives::keccak256;
use clap::Parser;
use eyre::Result;
use tracing::{info, warn};

use crate::config::FeederConfig;
use crate::health::FeederHealth;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ShutdownReason {
    Sigint,
    Sigterm,
}

impl ShutdownReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::Sigint => "SIGINT",
            Self::Sigterm => "SIGTERM",
        }
    }
}

#[cfg(unix)]
async fn shutdown_signal() -> ShutdownReason {
    let sigterm_kind = tokio::signal::unix::SignalKind::terminate();
    let mut sigterm = match tokio::signal::unix::signal(sigterm_kind) {
        Ok(signal) => Some(signal),
        Err(e) => {
            warn!(error = %e, "failed to install SIGTERM handler; falling back to SIGINT only");
            None
        }
    };

    if let Some(sigterm) = sigterm.as_mut() {
        tokio::select! {
            result = tokio::signal::ctrl_c() => {
                if let Err(e) = result {
                    warn!(error = %e, "SIGINT handler failed");
                }
                ShutdownReason::Sigint
            }
            _ = sigterm.recv() => ShutdownReason::Sigterm,
        }
    } else {
        if let Err(e) = tokio::signal::ctrl_c().await {
            warn!(error = %e, "SIGINT handler failed");
        }
        ShutdownReason::Sigint
    }
}

#[cfg(not(unix))]
async fn shutdown_signal() -> ShutdownReason {
    if let Err(e) = tokio::signal::ctrl_c().await {
        warn!(error = %e, "SIGINT handler failed");
    }
    ShutdownReason::Sigint
}

#[derive(Parser)]
#[command(name = "outbe-feeder", about = "Outbe price oracle feeder daemon")]
struct Cli {
    /// Path to the TOML configuration file.
    #[arg(short, long, default_value = "feeder.toml")]
    config: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "outbe_feeder=info".into()),
        )
        .init();

    let cli = Cli::parse();
    let config = FeederConfig::load(&cli.config)?;
    config.validate()?;

    // ORC-AUD-039: Warn about plaintext key usage.
    // Production deployments should use an encrypted keystore or OS keyring.
    warn!("private key loaded from plaintext config - use an encrypted keystore in production");

    info!(
        rpc = %config.chain.rpc_endpoint,
        pairs = config.currency_pairs.len(),
        "starting outbe-feeder"
    );

    run_feeder(config, std::path::Path::new(&cli.config)).await
}

/// Persistence errors stop the daemon: continuing could allocate a second nonce
/// after a partially durable journal update.
#[derive(Debug)]
struct PersistenceError(String);
impl std::fmt::Display for PersistenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for PersistenceError {}
fn persistence(error: eyre::Report) -> eyre::Report {
    eyre::Report::new(PersistenceError(error.to_string()))
}

async fn run_feeder(config: FeederConfig, config_path: &std::path::Path) -> Result<()> {
    let providers = provider::create_providers(&config)?;
    let (wallet, signer) = oracle_client::create_wallet(&config.account)?;
    let validator = config.account.validator_address.parse()?;
    let client = oracle_client::OracleClient::new(&config.chain.rpc_endpoint)?;
    let identity = client
        .identity(config.chain.chain_id, signer, validator)
        .await?;
    let state_path = pending_vote_journal_path(config_path, &identity)?;
    let mut journal = journal::Journal::open(&state_path, &identity)?;
    let health = Arc::new(FeederHealth::new(config.oracle.vote_period));
    let pair_names: Vec<_> = config
        .currency_pairs
        .iter()
        .map(|p| format!("{}/{}", p.base, p.quote))
        .collect();
    health.expect_pairs(&pair_names);
    let health_bind = config
        .health
        .as_ref()
        .map(|h| h.bind_address.as_str())
        .unwrap_or("0.0.0.0:9002");
    if config.health.as_ref().map(|h| h.enabled).unwrap_or(true) {
        health::start_health_server(health_bind, health.clone()).await?;
    }
    let mut last_attempt = None;
    // A zero configured interval must not cause a busy loop.
    let interval = std::time::Duration::from_secs(config.oracle.poll_interval_secs.max(1));
    let mut shutdown = std::pin::pin!(shutdown_signal());
    loop {
        let result = tokio::select! {
            result=feeder_tick(&config,&providers,&client,&wallet,signer,validator,&mut journal,&health,&mut last_attempt)=>result,
            reason=&mut shutdown=>{info!(signal=reason.as_str(),"shutdown; pending transaction preserved");return Ok(());}
        };
        if let Err(error) = result {
            if error.downcast_ref::<PersistenceError>().is_some() {
                return Err(error);
            }
            health.record_failure();
            health.set_reason(&format!("retryable: {error}"));
            warn!(error=%error,"feeder attempt failed; retrying without consuming period");
        }
        tokio::select! {
            _=tokio::time::sleep(interval)=>{},
            reason=&mut shutdown=>{info!(signal=reason.as_str(),"shutdown; pending transaction preserved");return Ok(());}
        }
    }
}

/// Keep pending votes across machine restarts. A new genesis or signer gets a
/// different journal, so a testnet wipe cannot replay an old signed vote.
fn pending_vote_journal_path(config_path: &Path, identity: &str) -> Result<PathBuf> {
    let directory = match std::env::var_os("STATE_DIRECTORY") {
        Some(path) => PathBuf::from(path).canonicalize()?,
        None => config_path
            .canonicalize()?
            .parent()
            .expect("canonical config has a parent")
            .to_path_buf(),
    };
    eyre::ensure!(directory.is_dir(), "feeder state path is not a directory");
    Ok(directory.join(format!("pending-{}.json", keccak256(identity.as_bytes()))))
}

#[allow(clippy::too_many_arguments)]
async fn feeder_tick(
    config: &FeederConfig,
    providers: &[Box<dyn provider::Provider>],
    client: &oracle_client::OracleClient,
    wallet: &alloy_network::EthereumWallet,
    signer: alloy_primitives::Address,
    validator: alloy_primitives::Address,
    journal: &mut journal::Journal,
    health: &FeederHealth,
    last_attempt: &mut Option<u64>,
) -> Result<()> {
    use oracle_client::PreflightResult;
    let head = client.head().await?;
    let period = head.period(config.oracle.vote_period);
    health.record_head(head.height);
    health.set_period(period);
    health.set_pending(
        journal.pending().map(|p| p.hash.as_str()),
        journal.pending().map(|p| p.created_at).unwrap_or(0),
    );
    // All reads refer to the same canonical block, including price freshness.
    // A failed price-monitor read must not prevent voting; freshness expires.
    let monitor = async {
        for pair in &config.currency_pairs {
            let (base, quote) = pair.oracle_pair()?;
            match client.price_observation(&head, base, quote).await {
                Ok((block, time)) => {
                    health.record_oracle(&format!("{}/{}", pair.base, pair.quote), block, time)
                }
                Err(error) => warn!(error=%error,"cannot read oracle freshness"),
            }
        }
        Ok::<_, eyre::Report>(())
    };
    match tokio::time::timeout(std::time::Duration::from_secs(2), monitor).await {
        Ok(Ok(())) => {}
        result => warn!(
            ?result,
            "oracle freshness observation incomplete; voting continues"
        ),
    }
    let mut replace_pending = false;
    if let Some(pending) = journal.pending().cloned() {
        if let Some(receipt) = client.receipt(&pending).await? {
            if receipt.success {
                info!(tx_hash=%receipt.hash,block=receipt.height,period=receipt.height/config.oracle.vote_period,"oracle vote submitted");
                health.record_success(receipt.height);
                journal.clear().map_err(persistence)?;
                health.set_pending(None, 0);
                *last_attempt = None;
                return Ok(());
            } else {
                health.record_failure();
                warn!(tx_hash=%receipt.hash,block=receipt.height,"oracle vote reverted; rechecking canonical nonce and period");
            }
            // Outbe can emit a synthetic failure receipt without consuming the
            // nonce. Keep it until a fresh, fee-bumped same-nonce vote is durable.
            replace_pending = true;
        }
        if client.nonce(&head, signer).await? > pending.nonce {
            warn!(tx_hash=%pending.hash,nonce=pending.nonce,"canonical nonce consumed without this receipt; reconciling from chain state");
            journal.clear().map_err(persistence)?;
            health.set_pending(None, 0);
            *last_attempt = None;
            return Ok(());
        }
        replace_pending |= head.height.saturating_sub(pending.observed_height)
            > config.oracle.vote_period.saturating_mul(2).max(8);
    }
    match client
        .preflight(&head, config.oracle.vote_period, validator, signer)
        .await?
    {
        PreflightResult::AlreadyVoted => {
            health.set_reason("already voted in observed period");
            return Ok(());
        }
        PreflightResult::Blocked(reason) => {
            health.set_reason(&reason);
            warn!(%reason,"oracle voting blocked; will recheck");
            return Ok(());
        }
        PreflightResult::Eligible => {}
    }
    // At most one aggregation/broadcast per observed head; errors never consume
    // a whole period. Receipt polling continues even on an unchanged head.
    if *last_attempt == Some(head.height) {
        return Ok(());
    }
    *last_attempt = Some(head.height);
    if journal.pending().is_none() || replace_pending {
        let aggregated = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            aggregator::fetch_and_aggregate(providers, config),
        )
        .await??;
        eyre::ensure!(!aggregated.is_empty(), "no price data available");
        let fresh = client.head().await?;
        if fresh.period(config.oracle.vote_period) != period {
            health.set_reason("period changed during aggregation; fetching fresh prices");
            return Ok(());
        }
        if client
            .preflight(&fresh, config.oracle.vote_period, validator, signer)
            .await?
            != PreflightResult::Eligible
        {
            return Ok(());
        }
        let calldata = vote_builder::encode_vote(&aggregated);
        if let Ok(decoded) = vote_builder::decode_vote_log(&calldata) {
            info!(vote=%decoded,"vote calldata");
        }
        let pending = client
            .sign_vote(
                &fresh,
                wallet,
                signer,
                config.chain.chain_id,
                &calldata,
                config.chain.gasless_oracle_votes,
                journal.pending(),
            )
            .await?;
        if replace_pending {
            journal.replace(pending).map_err(persistence)?;
        } else {
            journal.store(pending).map_err(persistence)?;
        }
    }
    let pending = journal.pending().expect("persisted before broadcast");
    health.set_pending(Some(&pending.hash), pending.created_at);
    health.set_reason("awaiting canonical receipt");
    // Retain bytes before any send. Retries use the same bytes; a stalled or
    // failed vote can be fee-bumped only at the SAME nonce, never queued behind it.
    client.broadcast(pending).await?;
    info!(tx_hash=%pending.hash,nonce=pending.nonce,period,"oracle vote broadcast; awaiting inclusion");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shutdown_reason_labels_are_stable() {
        assert_eq!(ShutdownReason::Sigint.as_str(), "SIGINT");
        assert_eq!(ShutdownReason::Sigterm.as_str(), "SIGTERM");
    }
}
