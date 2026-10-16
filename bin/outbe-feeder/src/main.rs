//! Outbe price-feeder daemon.
//!
//! Fetches prices from external providers, aggregates them via VWAP,
//! and submits oracle votes to the on-chain Oracle precompile.

mod abi;
mod aggregator;
mod config;
mod errors;
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

use crate::config::{FeederConfig, SourcesMode};
use crate::errors::FeederError;
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
    let local = FeederConfig::read_local(&cli.config)?;
    let bootstrap = FeederConfig::build(local.clone(), None)?;
    let client = oracle_client::OracleClient::new(&bootstrap.chain.rpc_endpoint)?;
    let chain_sources = match bootstrap.oracle.sources {
        SourcesMode::Local => None,
        SourcesMode::Chain => {
            let head = client.head().await?;
            let period = head.period(bootstrap.oracle.vote_period.max(1));
            let (version, text) = client
                .sources(period * bootstrap.oracle.vote_period.max(1))
                .await?;
            Some(ChainSources {
                local,
                version,
                text,
                period,
            })
        }
    };
    let config = match &chain_sources {
        Some(chain) => FeederConfig::build(chain.local.clone(), Some(&chain.text))?,
        None => bootstrap,
    };
    config.validate()?;
    if let Some(chain) = &chain_sources {
        chain.log("feeder sources loaded", &config);
    }

    // ORC-AUD-039: Warn about plaintext key usage.
    // Production deployments should use an encrypted keystore or OS keyring.
    warn!("private key loaded from plaintext config - use an encrypted keystore in production");

    info!(
        rpc = %config.chain.rpc_endpoint,
        pairs = config.currency_pairs.len(),
        "starting outbe-feeder"
    );

    run_feeder(
        config,
        client,
        chain_sources,
        std::path::Path::new(&cli.config),
    )
    .await
}

/// The local config table and the node's sources it was last built from.
struct ChainSources {
    local: toml::Table,
    /// The active protocol version when `text` was read.
    version: u32,
    text: String,
    period: u64,
}

impl ChainSources {
    /// Logs the protocol version and the hash of the sources text. Feeders
    /// with the same hash use the same file.
    fn log(&self, message: &str, config: &FeederConfig) {
        info!(
            version = %format!("v{}.{}", self.version >> 24, self.version & 0x00ff_ffff),
            hash = %keccak256(self.text.as_bytes()),
            pairs = config.currency_pairs.len(),
            "{message}"
        );
    }

    /// Once per vote period, reads the node's sources at the period's first
    /// block, so every feeder reads the same file in one period. Returns a new
    /// config when the sources changed.
    async fn reload(
        &mut self,
        client: &oracle_client::OracleClient,
        vote_period: u64,
    ) -> Result<Option<FeederConfig>> {
        let period = client.head().await?.period(vote_period);
        if period == self.period {
            return Ok(None);
        }
        let (version, text) = client.sources(period * vote_period).await?;
        self.period = period;
        self.version = version;
        if text == self.text {
            return Ok(None);
        }
        let config = FeederConfig::build(self.local.clone(), Some(&text))?;
        config.validate()?;
        self.text = text;
        self.log("feeder sources changed", &config);
        Ok(Some(config))
    }
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

async fn run_feeder(
    mut config: FeederConfig,
    client: oracle_client::OracleClient,
    mut chain_sources: Option<ChainSources>,
    config_path: &std::path::Path,
) -> Result<()> {
    let mut providers = provider::create_providers(&config)?;
    let (wallet, signer) = oracle_client::create_wallet(&config.account)?;
    let validator = config.account.validator_address.parse()?;
    let identity = client
        .identity(config.chain.chain_id, signer, validator)
        .await?;
    let state_path = pending_vote_journal_path(config_path, &identity)?;
    let mut journal = journal::Journal::open(&state_path, &identity)?;
    let health = Arc::new(FeederHealth::new(config.oracle.vote_period));
    health.expect_pairs(&pair_names(&config));
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
        if let Some(chain) = chain_sources.as_mut() {
            let reloaded = async {
                let Some(next) = chain.reload(&client, config.oracle.vote_period).await? else {
                    return Ok(None);
                };
                let next_providers = provider::create_providers(&next)?;
                Ok::<_, eyre::Report>(Some((next, next_providers)))
            };
            match reloaded.await {
                Ok(Some((next, next_providers))) => {
                    config = next;
                    providers = next_providers;
                    health.expect_pairs(&pair_names(&config));
                }
                Ok(None) => {}
                Err(error) => {
                    warn!(error=%error, "cannot apply the node's feeder sources; keeping the current ones")
                }
            }
        }
        let mut tick_ctx = FeederTickContext {
            config: &config,
            providers: &providers,
            client: &client,
            wallet: &wallet,
            signer,
            validator,
            journal: &mut journal,
            health: &health,
            last_attempt: &mut last_attempt,
        };
        let result = tokio::select! {
            result=feeder_tick(&mut tick_ctx)=>result,
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

fn pair_names(config: &FeederConfig) -> Vec<String> {
    config
        .currency_pairs
        .iter()
        .map(|p| format!("{}/{}", p.base, p.quote))
        .collect()
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
    if !directory.is_dir() {
        return Err(FeederError::StatePathNotDirectory.into());
    }
    Ok(directory.join(format!("pending-{}.json", keccak256(identity.as_bytes()))))
}

pub(crate) struct FeederTickContext<'a> {
    pub config: &'a FeederConfig,
    pub providers: &'a [Box<dyn provider::Provider>],
    pub client: &'a oracle_client::OracleClient,
    pub wallet: &'a alloy_network::EthereumWallet,
    pub signer: alloy_primitives::Address,
    pub validator: alloy_primitives::Address,
    pub journal: &'a mut journal::Journal,
    pub health: &'a FeederHealth,
    pub last_attempt: &'a mut Option<u64>,
}

enum PendingReconciliation {
    Settled,
    Continue { replace_pending: bool },
}

impl FeederTickContext<'_> {
    async fn observe_oracle_freshness(&self, head: &oracle_client::Head) {
        // All reads refer to the same canonical block, including price freshness.
        // A failed price-monitor read must not prevent voting. Freshness expires.
        let monitor = async {
            for pair in &self.config.currency_pairs {
                let (base, quote) = pair.oracle_pair()?;
                match self.client.price_observation(head, base, quote).await {
                    Ok((block, time)) => self.health.record_oracle(
                        &format!("{}/{}", pair.base, pair.quote),
                        block,
                        time,
                    ),
                    Err(error) => warn!(error=%error,"cannot read oracle freshness"),
                }
            }
            Ok::<_, eyre::Report>(())
        };
        if let result @ (Err(_) | Ok(Err(_))) =
            tokio::time::timeout(std::time::Duration::from_secs(2), monitor).await
        {
            warn!(
                ?result,
                "oracle freshness observation incomplete; voting continues"
            );
        }
    }

    fn clear_settled_pending(&mut self) -> Result<()> {
        self.journal.clear().map_err(persistence)?;
        self.health.set_pending(None, 0);
        *self.last_attempt = None;
        Ok(())
    }

    async fn reconcile_pending(
        &mut self,
        head: &oracle_client::Head,
    ) -> Result<PendingReconciliation> {
        let Some(pending) = self.journal.pending().cloned() else {
            return Ok(PendingReconciliation::Continue {
                replace_pending: false,
            });
        };
        let mut replace_pending = false;
        if let Some(receipt) = self.client.receipt(&pending).await? {
            if receipt.success {
                info!(tx_hash=%receipt.hash,block=receipt.height,period=receipt.height/self.config.oracle.vote_period,"oracle vote submitted");
                self.health.record_success(receipt.height);
                self.clear_settled_pending()?;
                return Ok(PendingReconciliation::Settled);
            }
            self.health.record_failure();
            warn!(tx_hash=%receipt.hash,block=receipt.height,"oracle vote reverted; rechecking canonical nonce and period");
            // Outbe can emit a synthetic failure receipt without consuming the
            // nonce. Keep it until a fresh, fee-bumped same-nonce vote is durable.
            replace_pending = true;
        }
        if self.client.nonce(head, self.signer).await? > pending.nonce {
            warn!(tx_hash=%pending.hash,nonce=pending.nonce,"canonical nonce consumed without this receipt; reconciling from chain state");
            self.clear_settled_pending()?;
            return Ok(PendingReconciliation::Settled);
        }
        replace_pending |= head.height.saturating_sub(pending.observed_height)
            > self.config.oracle.vote_period.saturating_mul(2).max(8);
        Ok(PendingReconciliation::Continue { replace_pending })
    }

    async fn check_preflight(&self, head: &oracle_client::Head) -> Result<bool> {
        use oracle_client::PreflightResult;
        match self
            .client
            .preflight(
                head,
                self.config.oracle.vote_period,
                self.validator,
                self.signer,
            )
            .await?
        {
            PreflightResult::AlreadyVoted => {
                self.health.set_reason("already voted in observed period");
                Ok(false)
            }
            PreflightResult::Blocked(reason) => {
                self.health.set_reason(&reason);
                warn!(%reason,"oracle voting blocked; will recheck");
                Ok(false)
            }
            PreflightResult::Eligible => Ok(true),
        }
    }

    async fn prepare_pending_vote(&mut self, period: u64, replace_pending: bool) -> Result<bool> {
        use oracle_client::{PreflightResult, VoteSigningContext};
        let aggregated = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            aggregator::fetch_and_aggregate(self.providers, self.config),
        )
        .await??;
        if aggregated.is_empty() {
            return Err(FeederError::NoPriceDataAvailable.into());
        }
        let fresh = self.client.head().await?;
        if fresh.period(self.config.oracle.vote_period) != period {
            self.health
                .set_reason("period changed during aggregation; fetching fresh prices");
            return Ok(false);
        }
        let preflight = self
            .client
            .preflight(
                &fresh,
                self.config.oracle.vote_period,
                self.validator,
                self.signer,
            )
            .await?;
        if preflight != PreflightResult::Eligible {
            return Ok(false);
        }
        let calldata = vote_builder::encode_vote(&aggregated);
        if let Ok(decoded) = vote_builder::decode_vote_log(&calldata) {
            info!(vote=%decoded,"vote calldata");
        }
        let sign_ctx = VoteSigningContext {
            wallet: self.wallet,
            signer: self.signer,
            chain: &self.config.chain,
        };
        let pending = self
            .client
            .sign_vote(&fresh, &sign_ctx, &calldata, self.journal.pending())
            .await?;
        if replace_pending {
            self.journal.replace(pending).map_err(persistence)?;
        } else {
            self.journal.store(pending).map_err(persistence)?;
        }
        Ok(true)
    }
}

async fn feeder_tick(ctx: &mut FeederTickContext<'_>) -> Result<()> {
    let head = ctx.client.head().await?;
    let period = head.period(ctx.config.oracle.vote_period);
    ctx.health.record_head(head.height);
    ctx.health.set_period(period);
    ctx.health.set_pending(
        ctx.journal.pending().map(|p| p.hash.as_str()),
        ctx.journal.pending().map(|p| p.created_at).unwrap_or(0),
    );
    ctx.observe_oracle_freshness(&head).await;
    let PendingReconciliation::Continue { replace_pending } = ctx.reconcile_pending(&head).await?
    else {
        return Ok(());
    };
    // At most one aggregation/broadcast per observed head. Errors never consume
    // a whole period. Receipt polling continues even on an unchanged head.
    if !ctx.check_preflight(&head).await? || *ctx.last_attempt == Some(head.height) {
        return Ok(());
    }
    *ctx.last_attempt = Some(head.height);
    if (ctx.journal.pending().is_none() || replace_pending)
        && !ctx.prepare_pending_vote(period, replace_pending).await?
    {
        return Ok(());
    }
    let pending = ctx.journal.pending().expect("persisted before broadcast");
    ctx.health
        .set_pending(Some(&pending.hash), pending.created_at);
    ctx.health.set_reason("awaiting canonical receipt");
    // Retain bytes before any send. Retries use the same bytes. A stalled or
    // failed vote can be fee-bumped only at the SAME nonce, never queued behind it.
    ctx.client.broadcast(pending).await?;
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
