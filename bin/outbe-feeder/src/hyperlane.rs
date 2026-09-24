//! Hyperlane liveness: submits this validator's latest signed checkpoints
//! from its own checkpoint bucket to the HyperlaneController precompile.
//!
//! The bucket is the location the validator announced on Outbe's
//! ValidatorAnnounce (`s3+http://host:port/<validator>/<folder>`); inside it
//! every domain has a folder with `checkpoint_latest_index.json` (a bare
//! number) and `checkpoint_<index>_with_id.json` written by the agent.

use std::sync::Arc;
use std::time::Duration;

use alloy_eips::BlockId;
use alloy_network::EthereumWallet;
use alloy_primitives::{Address, Bytes, B256};
use alloy_provider::ProviderBuilder;
use alloy_sol_types::SolCall;
use eyre::{Context, Result};
use serde::Deserialize;
use tracing::{debug, info, warn};

use crate::abi::{IHyperlaneController, IValidatorAnnounce};
use crate::config::HyperlaneConfig;
use crate::health::FeederHealth;
use crate::oracle_client;

/// HyperlaneController precompile address (0xEE14).
pub const HYPERLANE_CONTROLLER_ADDRESS: Address =
    alloy_primitives::address!("0x000000000000000000000000000000000000EE14");

#[derive(Debug, Deserialize)]
struct CheckpointFile {
    value: CheckpointValue,
    serialized_signature: Bytes,
}

#[derive(Debug, Deserialize)]
struct CheckpointValue {
    checkpoint: CheckpointBody,
    message_id: B256,
}

#[derive(Debug, Deserialize)]
struct CheckpointBody {
    root: B256,
    index: u32,
}

pub struct Attester {
    config: HyperlaneConfig,
    rpc_endpoint: String,
    chain_id: u64,
    wallet: EthereumWallet,
    validator: Address,
    client: reqwest::Client,
    health: Arc<FeederHealth>,
}

impl Attester {
    pub fn new(
        config: HyperlaneConfig,
        rpc_endpoint: String,
        chain_id: u64,
        wallet: EthereumWallet,
        validator: Address,
        health: Arc<FeederHealth>,
    ) -> Self {
        Self {
            config,
            rpc_endpoint,
            chain_id,
            wallet,
            validator,
            client: reqwest::Client::new(),
            health,
        }
    }

    /// Polls the bucket forever; every error is logged and retried next tick.
    pub async fn run(self) {
        let interval = Duration::from_secs(self.config.poll_interval_secs);
        info!(
            poll_secs = interval.as_secs(),
            "hyperlane checkpoint attester started"
        );
        loop {
            if let Err(error) = self.tick().await {
                warn!(%error, "hyperlane attester tick failed");
            }
            tokio::time::sleep(interval).await;
        }
    }

    async fn tick(&self) -> Result<()> {
        let provider = ProviderBuilder::new().connect_http(self.rpc_endpoint.parse()?);
        let output = oracle_client::eth_call(
            &provider,
            HYPERLANE_CONTROLLER_ADDRESS,
            IHyperlaneController::domainsCall {}.abi_encode(),
            BlockId::latest(),
        )
        .await
        .with_context(|| "hyperlane domains() failed")?;
        let domains = IHyperlaneController::domainsCall::abi_decode_returns(&output)
            .with_context(|| "hyperlane domains() decode failed")?;
        if domains.is_empty() {
            debug!("hyperlane controller not initialized yet, nothing to submit");
            return Ok(());
        }
        let output = oracle_client::eth_call(
            &provider,
            HYPERLANE_CONTROLLER_ADDRESS,
            IHyperlaneController::missCountCall {
                validator: self.validator,
            }
            .abi_encode(),
            BlockId::latest(),
        )
        .await
        .with_context(|| "hyperlane missCount() failed")?;
        let misses = IHyperlaneController::missCountCall::abi_decode_returns(&output)
            .with_context(|| "hyperlane missCount() decode failed")?;
        self.health.set_hyperlane_miss_count(u64::from(misses));
        if misses > 0 {
            warn!(
                misses,
                "hyperlane liveness misses recorded on-chain; check the validator agent and its bucket"
            );
        }
        let Some(bucket) = self.announced_bucket(&provider).await? else {
            warn!(validator = %self.validator, "validator has not announced a checkpoint location");
            return Ok(());
        };

        for domain in domains {
            let latest = match self.fetch_latest(&bucket, domain).await {
                Ok(Some(latest)) => latest,
                Ok(None) => continue,
                Err(error) => {
                    warn!(domain, %error, "hyperlane checkpoint fetch failed");
                    continue;
                }
            };
            let output = oracle_client::eth_call(
                &provider,
                HYPERLANE_CONTROLLER_ADDRESS,
                IHyperlaneController::submittedIndexCall {
                    validator: self.validator,
                    domain,
                }
                .abi_encode(),
                BlockId::latest(),
            )
            .await
            .with_context(|| "hyperlane submittedIndex() failed")?;
            let submitted = IHyperlaneController::submittedIndexCall::abi_decode_returns(&output)
                .with_context(|| "hyperlane submittedIndex() decode failed")?;
            if latest.value.checkpoint.index <= submitted {
                continue;
            }
            self.submit(domain, &latest).await;
        }
        Ok(())
    }

    /// Public-read base URL of the validator's bucket from its latest
    /// announcement on Outbe, or `None` when it never announced.
    async fn announced_bucket<P: alloy_provider::Provider>(
        &self,
        provider: &P,
    ) -> Result<Option<String>> {
        let output = oracle_client::eth_call(
            provider,
            HYPERLANE_CONTROLLER_ADDRESS,
            IHyperlaneController::validatorAnnounceCall {}.abi_encode(),
            BlockId::latest(),
        )
        .await
        .with_context(|| "hyperlane validatorAnnounce() failed")?;
        let announce = IHyperlaneController::validatorAnnounceCall::abi_decode_returns(&output)
            .with_context(|| "hyperlane validatorAnnounce() decode failed")?;
        let output = oracle_client::eth_call(
            provider,
            announce,
            IValidatorAnnounce::getAnnouncedStorageLocationsCall {
                validators: vec![self.validator],
            }
            .abi_encode(),
            BlockId::latest(),
        )
        .await
        .with_context(|| "getAnnouncedStorageLocations() failed")?;
        let locations =
            IValidatorAnnounce::getAnnouncedStorageLocationsCall::abi_decode_returns(&output)
                .with_context(|| "getAnnouncedStorageLocations() decode failed")?;
        locations
            .first()
            .and_then(|list| list.last())
            .map(|location| bucket_base(location))
            .transpose()
    }

    async fn fetch_latest(&self, bucket: &str, domain: u32) -> Result<Option<CheckpointFile>> {
        let base = format!("{bucket}/{domain}");
        let response = self
            .client
            .get(format!("{base}/checkpoint_latest_index.json"))
            .send()
            .await?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let index: u32 = response
            .error_for_status()?
            .text()
            .await?
            .trim()
            .parse()
            .with_context(|| "checkpoint_latest_index.json is not a number")?;
        let checkpoint: CheckpointFile = self
            .client
            .get(format!("{base}/checkpoint_{index}_with_id.json"))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await
            .with_context(|| format!("checkpoint_{index}_with_id.json decode failed"))?;
        Ok(Some(checkpoint))
    }

    async fn submit(&self, domain: u32, checkpoint: &CheckpointFile) {
        let index = checkpoint.value.checkpoint.index;
        let calldata = IHyperlaneController::submitCheckpointCall {
            domain,
            root: checkpoint.value.checkpoint.root,
            index,
            messageId: checkpoint.value.message_id,
            signature: checkpoint.serialized_signature.clone(),
        }
        .abi_encode();
        match oracle_client::submit_vote(
            &self.rpc_endpoint,
            &self.wallet,
            self.chain_id,
            HYPERLANE_CONTROLLER_ADDRESS,
            &calldata,
            self.config.gasless,
        )
        .await
        {
            Ok(tx_hash) => {
                self.health.record_hyperlane_success();
                info!(domain, index, %tx_hash, "hyperlane checkpoint submitted");
            }
            Err(error) => {
                self.health.record_hyperlane_failure();
                warn!(domain, index, %error, "hyperlane checkpoint submit failed");
            }
        }
    }
}

/// `s3+http://host:port/bucket/folder` (patched agent announcement) ->
/// `http://host:port/bucket`.
fn bucket_base(location: &str) -> Result<String> {
    let rest = location
        .strip_prefix("s3+")
        .ok_or_else(|| eyre::eyre!("unsupported storage location '{location}'"))?;
    let (scheme, path) = rest
        .split_once("://")
        .ok_or_else(|| eyre::eyre!("malformed storage location '{location}'"))?;
    let mut segments = path.split('/').filter(|s| !s.is_empty());
    let (Some(host), Some(bucket)) = (segments.next(), segments.next()) else {
        return Err(eyre::eyre!("storage location '{location}' has no bucket"));
    };
    Ok(format!("{scheme}://{host}/{bucket}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bucket_base_strips_scheme_prefix_and_folder() {
        assert_eq!(
            bucket_base(
                "s3+http://192.168.0.112:9000/0x97cf63acd02be0d6da11fe5c9b834167776a5a50/54322345"
            )
            .unwrap(),
            "http://192.168.0.112:9000/0x97cf63acd02be0d6da11fe5c9b834167776a5a50"
        );
        assert_eq!(
            bucket_base("s3+https://s3.example.org/0xabc").unwrap(),
            "https://s3.example.org/0xabc"
        );
        assert!(bucket_base("s3://0xabc/us-east-1/outbetestnet").is_err());
        assert!(bucket_base("s3+http://host-only").is_err());
    }

    #[test]
    fn checkpoint_file_parses_the_agent_layout() {
        let raw = r#"{
          "value": {
            "checkpoint": {
              "merkle_tree_hook_address": "0x0000000000000000000000006543cef9bbe42d66b5b36ccaf9374de2b55f9cbc",
              "mailbox_domain": 54322345,
              "root": "0x5cd1ceeff6f033960677157f13b472fcfb1617638dbdfb0ab0cde36cd94bf050",
              "index": 2
            },
            "message_id": "0x3eae2775d2a39169fd82693c9883351a72761c9cdf4cddedaf95bf62e454250e"
          },
          "signature": { "r": "0x82079df9c6de4d2ab039216e3275cf803d245819150512e50dde105e2802f989", "s": "0x76b8041d134256ef7df29fa8a8613a5c042af46c3468ab011410e19e6e1cb493", "v": 28 },
          "serialized_signature": "0x82079df9c6de4d2ab039216e3275cf803d245819150512e50dde105e2802f98976b8041d134256ef7df29fa8a8613a5c042af46c3468ab011410e19e6e1cb4931c"
        }"#;
        let file: CheckpointFile = serde_json::from_str(raw).unwrap();
        assert_eq!(file.value.checkpoint.index, 2);
        assert_eq!(file.serialized_signature.len(), 65);
        assert_eq!(
            file.value.message_id,
            alloy_primitives::b256!(
                "0x3eae2775d2a39169fd82693c9883351a72761c9cdf4cddedaf95bf62e454250e"
            )
        );
    }
}
