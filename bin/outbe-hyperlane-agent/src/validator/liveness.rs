//! Hyperlane liveness: submits this validator's signed checkpoints of each
//! running domain from the local store to the HyperlaneController precompile.
//! Per domain it submits the checkpoint at the target index of the controller
//! (the liveness vote) and the latest checkpoint.
//!
//! The controller accepts checkpoints only from active validators. While `V`
//! is not in the active set (for example jailed), the daemon keeps signing
//! and serving checkpoints, but it does not submit.

use std::sync::Arc;
use std::time::Duration;

use alloy_eips::BlockId;
use alloy_network::{Ethereum, EthereumWallet, NetworkWallet};
use alloy_primitives::{Address, Bytes, B256};
use alloy_provider::DynProvider;
use alloy_sol_types::SolCall;
use eyre::{Context, Result};
use tracing::{info, warn};

use crate::abi::{IHyperlaneController, IValidatorSet};
use crate::checkpoint::{CheckpointWithMessageId, Signed};
use crate::client::{self, TxSender};
use crate::config::{ChainConfig, HyperlaneConfig};
use crate::errors::LivenessError;
use crate::http::health::ValidatorHealth;
use crate::sync::connect;
use crate::validator::signer::DomainSigner;

/// HyperlaneController precompile address (0xEE14).
pub const HYPERLANE_CONTROLLER_ADDRESS: Address =
    alloy_primitives::address!("0x000000000000000000000000000000000000EE14");
/// ValidatorSet precompile address (0xEE00).
pub const VALIDATOR_SET_ADDRESS: Address =
    alloy_primitives::address!("0x000000000000000000000000000000000000EE00");
/// The longest wait for the receipt of `submitCheckpoint`.
const RECEIPT_TIMEOUT: Duration = Duration::from_secs(60);

pub struct Liveness {
    config: HyperlaneConfig,
    rpc_endpoint: String,
    chain_id: u64,
    wallet: EthereumWallet,
    /// The key `H` of `wallet`.
    signer: Address,
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
            signer: NetworkWallet::<Ethereum>::default_signer_address(&wallet),
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
        let provider = connect(&self.rpc_endpoint)?;
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
        let signer_error = self.check_signer(&provider).await?;
        if let Some(error) = &signer_error {
            warn!(error, "checkpoint submission skipped");
        }
        let skip = signer_error.is_some();
        self.health.set_signer_error(signer_error);
        if !active {
            info!(validator = %self.validator, "validator is not active; checkpoint submission skipped");
            return Ok(());
        }
        if skip {
            return Ok(());
        }

        for signer in self.health.running() {
            // The target first: when the target is also the latest index, one
            // transaction does both.
            if let Err(error) = self.submit_target(&provider, &signer).await {
                warn!(domain = signer.domain(), %error, "hyperlane target checkpoint submit failed");
            }
            if let Err(error) = self.submit_latest(&provider, &signer).await {
                warn!(domain = signer.domain(), %error, "hyperlane checkpoint submit failed");
            }
        }
        Ok(())
    }

    /// The controller accepts only signatures of `hyperlaneSigner(V)`, or of
    /// `V` while it is zero. Returns an error text when `H` is another key.
    async fn check_signer(&self, provider: &DynProvider) -> Result<Option<String>> {
        let out = self
            .call(
                provider,
                HYPERLANE_CONTROLLER_ADDRESS,
                IHyperlaneController::hyperlaneSignerCall {
                    validator: self.validator,
                }
                .abi_encode(),
            )
            .await
            .with_context(|| "hyperlane hyperlaneSigner() failed")?;
        let registered = IHyperlaneController::hyperlaneSignerCall::abi_decode_returns(&out)?;
        let expected = if registered == Address::ZERO {
            self.validator
        } else {
            registered
        };
        Ok((expected != self.signer).then(|| {
            format!(
                "the controller accepts checkpoints of {} signed by {expected} (hyperlaneSigner), but private_key is {}",
                self.validator, self.signer
            )
        }))
    }

    async fn call(&self, provider: &DynProvider, to: Address, data: Vec<u8>) -> Result<Bytes> {
        client::eth_call(provider, to, data, BlockId::latest()).await
    }

    /// Submits the signed checkpoint at `targetIndex(domain)`: the liveness
    /// vote of this window. It does nothing when there is no target, when the
    /// store does not have that index yet, or when the controller already
    /// has this root as the vote.
    async fn submit_target(&self, provider: &DynProvider, signer: &DomainSigner) -> Result<()> {
        let domain = signer.domain();
        let out = self
            .call(
                provider,
                HYPERLANE_CONTROLLER_ADDRESS,
                IHyperlaneController::targetIndexCall { domain }.abi_encode(),
            )
            .await
            .with_context(|| "hyperlane targetIndex() failed")?;
        let target = IHyperlaneController::targetIndexCall::abi_decode_returns(&out)?;
        if target == 0 {
            return Ok(());
        }
        let Some(checkpoint) = signer.signed_checkpoint(target)? else {
            return Ok(());
        };
        let out = self
            .call(
                provider,
                HYPERLANE_CONTROLLER_ADDRESS,
                IHyperlaneController::targetVoteCall {
                    validator: self.validator,
                    domain,
                }
                .abi_encode(),
            )
            .await
            .with_context(|| "hyperlane targetVote() failed")?;
        let vote = IHyperlaneController::targetVoteCall::abi_decode_returns(&out)?;
        if vote.index == target && vote.root == checkpoint.value.checkpoint.root {
            return Ok(());
        }
        self.submit(provider, domain, target, &checkpoint).await
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
        self.submit(provider, domain, index, &checkpoint).await
    }

    /// Sends `submitCheckpoint` for the stored `checkpoint` at `index` and
    /// records the result in the health state.
    async fn submit(
        &self,
        provider: &DynProvider,
        domain: u32,
        index: u32,
        checkpoint: &Signed<CheckpointWithMessageId>,
    ) -> Result<()> {
        let calldata = IHyperlaneController::submitCheckpointCall {
            domain,
            root: checkpoint.value.checkpoint.root,
            index,
            messageId: checkpoint.value.message_id,
            signature: checkpoint.signature_bytes().to_vec().into(),
        }
        .abi_encode();
        match self.send(provider, &calldata).await {
            Ok(tx_hash) => {
                self.health.record_success();
                info!(domain, index, %tx_hash, "hyperlane checkpoint included");
                Ok(())
            }
            Err(error) => {
                self.health
                    .record_failure(format!("domain {domain}: {error:#}"));
                Err(error)
            }
        }
    }

    /// Sends the transaction and waits for its receipt. A reverted
    /// transaction is an error.
    async fn send(&self, provider: &DynProvider, calldata: &[u8]) -> Result<B256> {
        let sender = TxSender {
            provider,
            wallet: &self.wallet,
            chain_id: self.chain_id,
            gasless: self.config.gasless,
        };
        let hash = client::send_tx(&sender, HYPERLANE_CONTROLLER_ADDRESS, calldata).await?;
        let receipt = client::wait_receipt(provider, hash, RECEIPT_TIMEOUT).await?;
        if !receipt.status() {
            return Err(LivenessError::Reverted(hash).into());
        }
        Ok(hash)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use alloy_primitives::B256;
    use alloy_signer_local::PrivateKeySigner;
    use alloy_sol_types::SolValue;
    use serde_json::{json, Value};

    use super::*;
    use crate::store::Store;
    use crate::sync::tests::serve_json_rpc;
    use crate::sync::{Origin, SyncedLeaf, VerifiedBatch};

    const V: Address = Address::repeat_byte(0x77);

    /// The controller and the validator set on Outbe. `hyperlaneSigner(V)`
    /// returns the shared address, `targetIndex` returns the shared target,
    /// and `V` has no vote. Each receipt has status 0 (reverted).
    /// Returns the URL and the count of sent transactions.
    async fn outbe(
        signer_of_v: Arc<Mutex<Address>>,
        target: Arc<Mutex<u32>>,
    ) -> (String, Arc<Mutex<u32>>) {
        let sent = Arc::new(Mutex::new(0));
        let count = sent.clone();
        let url = serve_json_rpc(move |req: &Value| {
            let id = req["id"].clone();
            let result = match req["method"].as_str().unwrap() {
                "eth_chainId" => json!("0x5"),
                "eth_getTransactionCount" => json!("0x0"),
                "eth_gasPrice" => json!("0x3b9aca00"),
                "eth_sendRawTransaction" => {
                    *count.lock().unwrap() += 1;
                    json!(B256::repeat_byte(0xab))
                }
                "eth_getTransactionReceipt" => json!({
                    "transactionHash": B256::repeat_byte(0xab),
                    "transactionIndex": "0x0",
                    "blockHash": B256::repeat_byte(3),
                    "blockNumber": "0x1",
                    "from": V,
                    "to": HYPERLANE_CONTROLLER_ADDRESS,
                    "cumulativeGasUsed": "0x5208",
                    "gasUsed": "0x5208",
                    "effectiveGasPrice": "0x1",
                    "contractAddress": null,
                    "logs": [],
                    "logsBloom": format!("0x{}", "00".repeat(256)),
                    "status": "0x0",
                    "type": "0x2",
                }),
                "eth_call" => {
                    let call = &req["params"][0];
                    let to: Address = serde_json::from_value(call["to"].clone()).unwrap();
                    let input = if call["input"].is_null() {
                        &call["data"]
                    } else {
                        &call["input"]
                    };
                    let data: Bytes = serde_json::from_value(input.clone()).unwrap();
                    let selector: [u8; 4] = data[..4].try_into().unwrap();
                    let out = match (to, selector) {
                        (VALIDATOR_SET_ADDRESS, _) => vec![V].abi_encode(),
                        (_, IHyperlaneController::hyperlaneSignerCall::SELECTOR) => {
                            signer_of_v.lock().unwrap().abi_encode()
                        }
                        (_, IHyperlaneController::targetIndexCall::SELECTOR) => {
                            target.lock().unwrap().abi_encode()
                        }
                        (_, IHyperlaneController::targetVoteCall::SELECTOR) => {
                            (0u32, B256::ZERO).abi_encode()
                        }
                        (_, IHyperlaneController::missCountCall::SELECTOR)
                        | (_, IHyperlaneController::submittedIndexCall::SELECTOR) => {
                            0u32.abi_encode()
                        }
                        other => panic!("unexpected call {other:?}"),
                    };
                    json!(Bytes::from(out))
                }
                other => panic!("unexpected method {other}"),
            };
            json!({"jsonrpc": "2.0", "id": id, "result": result})
        })
        .await;
        (url, sent)
    }

    #[tokio::test]
    async fn signer_mismatch_blocks_and_a_reverted_receipt_is_a_failure() {
        let key = PrivateKeySigner::random();
        let signer_of_v = Arc::new(Mutex::new(Address::repeat_byte(0x99)));
        let target = Arc::new(Mutex::new(0));
        let (url, sent) = outbe(signer_of_v.clone(), target.clone()).await;
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path()).unwrap());
        let health = Arc::new(ValidatorHealth::new(store.clone()));
        // Domain 7 has a signed checkpoint at index 1.
        let origin = Origin {
            domain: 7,
            hook: Address::ZERO,
            validator_announce: Address::ZERO,
            mailbox: Address::ZERO,
            start_block: 1,
        };
        let signer = Arc::new(DomainSigner::new(origin, store, key.clone()).unwrap());
        let leaf = |index| SyncedLeaf {
            index,
            message_id: B256::repeat_byte(1),
            block: 1,
            root: B256::repeat_byte(2),
        };
        let batch = VerifiedBatch {
            domain: 7,
            leaves: vec![leaf(0), leaf(1)],
            cursor: 1,
        };
        signer.commit(&batch).await.unwrap();
        health.set_running(signer);
        let config = HyperlaneConfig {
            gasless: true,
            ..Default::default()
        };
        let chain = ChainConfig {
            rpc_endpoint: url,
            chain_id: 5,
        };
        let liveness = Liveness::new(
            config,
            &chain,
            EthereumWallet::from(key.clone()),
            V,
            health.clone(),
        );

        // hyperlaneSigner(V) is another key: no submit, a health error.
        liveness.tick().await.unwrap();
        let status = health.status_json();
        assert_eq!(*sent.lock().unwrap(), 0);
        assert!(status["signer_error"]
            .as_str()
            .unwrap()
            .contains("hyperlaneSigner"));
        assert_eq!(status["healthy"], false);

        // The key matches, but the transaction reverts: a failure.
        *signer_of_v.lock().unwrap() = key.address();
        liveness.tick().await.unwrap();
        let status = health.status_json();
        assert_eq!(*sent.lock().unwrap(), 1);
        assert_eq!(status["signer_error"], Value::Null);
        assert_eq!(
            (status["submitted"].clone(), status["failed"].clone()),
            (json!(0), json!(1))
        );
        assert!(status["last_submit_error"]
            .as_str()
            .unwrap()
            .contains("reverted"));
        assert_eq!(status["last_submit_time"], 0);

        // The controller has a target at index 1 and no vote of V: the tick
        // sends the target checkpoint and then the latest checkpoint.
        *target.lock().unwrap() = 1;
        liveness.tick().await.unwrap();
        assert_eq!(*sent.lock().unwrap(), 3);
    }
}
