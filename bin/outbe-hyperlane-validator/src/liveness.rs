//! Hyperlane liveness: submits this validator's latest signed checkpoint of
//! each running domain from the local store to the HyperlaneController
//! precompile.
//!
//! The controller accepts checkpoints only from active validators. While `V`
//! is not in the active set (for example jailed), the daemon keeps signing
//! and serving checkpoints, but it does not submit.

use std::sync::Arc;
use std::time::Duration;

use alloy_eips::BlockId;
use alloy_network::EthereumWallet;
use alloy_primitives::{Address, Bytes};
use alloy_provider::{DynProvider, Provider, ProviderBuilder};
use alloy_sol_types::SolCall;
use eyre::{Context, Result};
use tracing::{info, warn};

use crate::abi::{IHyperlaneController, IValidatorSet};
use crate::client::{self, TxSender};
use crate::config::{ChainConfig, HyperlaneConfig};
use crate::health::ValidatorHealth;
use crate::signer::DomainSigner;

/// HyperlaneController precompile address (0xEE14).
pub const HYPERLANE_CONTROLLER_ADDRESS: Address =
    alloy_primitives::address!("0x000000000000000000000000000000000000EE14");
/// ValidatorSet precompile address (0xEE00).
pub const VALIDATOR_SET_ADDRESS: Address =
    alloy_primitives::address!("0x000000000000000000000000000000000000EE00");

pub struct Liveness {
    config: HyperlaneConfig,
    rpc_endpoint: String,
    chain_id: u64,
    wallet: EthereumWallet,
    validator: Address,
    health: Arc<ValidatorHealth>,
}

impl Liveness {
    pub fn new(
        config: HyperlaneConfig,
        chain: &ChainConfig,
        wallet: EthereumWallet,
        validator: Address,
        health: Arc<ValidatorHealth>,
    ) -> Self {
        Self {
            config,
            rpc_endpoint: chain.rpc_endpoint.clone(),
            chain_id: chain.chain_id,
            wallet,
            validator,
            health,
        }
    }

    /// Polls forever. It logs every error and retries on the next tick.
    pub async fn run(self) {
        let interval = Duration::from_secs(self.config.poll_interval_secs);
        info!(poll_secs = interval.as_secs(), "hyperlane liveness started");
        loop {
            if let Err(error) = self.tick().await {
                warn!(
                    error = format!("{error:#}"),
                    "hyperlane liveness tick failed"
                );
            }
            tokio::time::sleep(interval).await;
        }
    }

    pub async fn tick(&self) -> Result<()> {
        let provider = ProviderBuilder::new()
            .connect_http(self.rpc_endpoint.parse()?)
            .erased();
        let out = self
            .call(
                &provider,
                HYPERLANE_CONTROLLER_ADDRESS,
                IHyperlaneController::missCountCall {
                    validator: self.validator,
                }
                .abi_encode(),
            )
            .await
            .with_context(|| "hyperlane missCount() failed")?;
        let misses = IHyperlaneController::missCountCall::abi_decode_returns(&out)?;
        self.health.set_miss_count(u64::from(misses));
        if misses > 0 {
            warn!(
                misses,
                "hyperlane liveness misses recorded on-chain; check the signing of each domain"
            );
        }

        let out = self
            .call(
                &provider,
                VALIDATOR_SET_ADDRESS,
                IValidatorSet::getActiveValidatorsCall {}.abi_encode(),
            )
            .await
            .with_context(|| "getActiveValidators() failed")?;
        let active = IValidatorSet::getActiveValidatorsCall::abi_decode_returns(&out)?
            .contains(&self.validator);
        self.health.set_active(active);
        if !active {
            info!(validator = %self.validator, "validator is not active; checkpoint submission skipped");
            return Ok(());
        }

        for signer in self.health.running() {
            if let Err(error) = self.submit_latest(&provider, &signer).await {
                warn!(domain = signer.domain(), %error, "hyperlane checkpoint submit failed");
            }
        }
        Ok(())
    }

    async fn call(&self, provider: &DynProvider, to: Address, data: Vec<u8>) -> Result<Bytes> {
        client::eth_call(provider, to, data, BlockId::latest()).await
    }

    /// Submits the latest signed checkpoint when its index is above
    /// `submittedIndex(V, domain)`. Index 0 is never submitted.
    async fn submit_latest(&self, provider: &DynProvider, signer: &DomainSigner) -> Result<()> {
        let domain = signer.domain();
        let Some(index) = signer.latest_index()? else {
            return Ok(());
        };
        let out = self
            .call(
                provider,
                HYPERLANE_CONTROLLER_ADDRESS,
                IHyperlaneController::submittedIndexCall {
                    validator: self.validator,
                    domain,
                }
                .abi_encode(),
            )
            .await
            .with_context(|| "hyperlane submittedIndex() failed")?;
        let submitted = IHyperlaneController::submittedIndexCall::abi_decode_returns(&out)?;
        if index <= submitted {
            return Ok(());
        }
        let Some(checkpoint) = signer.signed_checkpoint(index)? else {
            return Ok(());
        };
        let calldata = IHyperlaneController::submitCheckpointCall {
            domain,
            root: checkpoint.value.checkpoint.root,
            index,
            messageId: checkpoint.value.message_id,
            signature: checkpoint.signature_bytes().to_vec().into(),
        }
        .abi_encode();
        let sender = TxSender {
            rpc_endpoint: &self.rpc_endpoint,
            wallet: &self.wallet,
            chain_id: self.chain_id,
            gasless: self.config.gasless,
        };
        match client::send_tx(&sender, HYPERLANE_CONTROLLER_ADDRESS, &calldata).await {
            Ok(tx_hash) => {
                self.health.record_success();
                info!(domain, index, %tx_hash, "hyperlane checkpoint submitted");
                Ok(())
            }
            Err(error) => {
                self.health.record_failure();
                Err(error)
            }
        }
    }
}
