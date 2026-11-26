//! Hyperlane liveness: submits this validator's signed checkpoints of each
//! running domain from the local store to the HyperlaneController precompile.
//! Per domain it sends one `submitReport` in each epoch, near the end of
//! the epoch: the checkpoint at the target index of the controller (the
//! liveness vote) with the report of the validator.
//!
//! The controller accepts checkpoints only from active validators. While `V`
//! is not in the active set (for example jailed), the daemon keeps signing
//! and serving checkpoints, but it does not submit.

use std::sync::Arc;
use std::time::Duration;

use alloy_eips::BlockId;
use alloy_network::{Ethereum, EthereumWallet, NetworkWallet};
use alloy_primitives::{Address, Bytes, B256};
use alloy_provider::{DynProvider, Provider};
use alloy_sol_types::SolCall;
use eyre::{Context, Result};
use tracing::{info, warn};

use crate::abi::{IHyperlaneController, IValidatorSet};
use crate::checkpoint::{CheckpointWithMessageId, Signed};
use crate::client::{self, TxSender};
use crate::config::{ChainConfig, HyperlaneConfig};
use crate::errors::LivenessError;
use crate::http::health::ValidatorHealth;
use crate::relayer::report::{fit_report, ActiveValidator, ReportChecks};
use crate::sync::connect;
use crate::validator::signer::DomainSigner;

/// HyperlaneController precompile address (0xEE14).
pub const HYPERLANE_CONTROLLER_ADDRESS: Address =
    alloy_primitives::address!("0x000000000000000000000000000000000000EE14");
/// ValidatorSet precompile address (0xEE00).
pub const VALIDATOR_SET_ADDRESS: Address =
    alloy_primitives::address!("0x000000000000000000000000000000000000EE00");
/// The agent sends its call of the round in the last part of the epoch: the
/// epoch length divided by this value. The poll interval must be much
/// shorter than that part.
const REPORT_EPOCH_PART: u64 = 4;
/// In the last part of the epoch (the epoch length divided by this value),
/// the agent does not wait for a check of the other validators that did not
/// run.
const LAST_CHANCE_EPOCH_PART: u64 = 8;
/// The longest wait for the receipt of `submitReport`.
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
    /// The checks of the other validators for the report.
    checks: Option<ReportChecks>,
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
            checks: None,
        }
    }

    /// Gives the liveness loop the checks of the other validators. Without them the two lists of
    /// the report are empty.
    pub fn with_report_checks(mut self, checks: ReportChecks) -> Self {
        self.checks = Some(checks);
        self
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
        let validators = IValidatorSet::getActiveValidatorsCall::abi_decode_returns(&out)?;
        let active = validators.contains(&self.validator);
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

        // One call for each domain in an epoch, near its end: a validator
        // that is behind has the full epoch to sign the target index.
        let start = self
            .view(
                &provider,
                VALIDATOR_SET_ADDRESS,
                IValidatorSet::getEpochStartBlockCall {},
            )
            .await?;
        let length = self
            .view(
                &provider,
                VALIDATOR_SET_ADDRESS,
                IValidatorSet::getEpochLengthBlocksCall {},
            )
            .await?;
        let length = u64::from(length);
        let block = provider.get_block_number().await?;
        if block < last_part_from(start, length, REPORT_EPOCH_PART) {
            return Ok(());
        }
        // Near the end of the epoch the agent sends the call also when a
        // check of the other validators did not run.
        let last_chance = block >= last_part_from(start, length, LAST_CHANCE_EPOCH_PART);
        let round = self
            .view(
                &provider,
                HYPERLANE_CONTROLLER_ADDRESS,
                IHyperlaneController::livenessRoundCall {},
            )
            .await?;
        // The key that signs the checkpoints of each active validator.
        let mut keys = Vec::with_capacity(validators.len());
        for validator in validators {
            let call = IHyperlaneController::hyperlaneSignerCall { validator };
            let signer = self
                .view(&provider, HYPERLANE_CONTROLLER_ADDRESS, call)
                .await?;
            keys.push(ActiveValidator {
                validator,
                signer: if signer.is_zero() { validator } else { signer },
            });
        }
        for signer in self.health.running() {
            let call = self.submit_round(&provider, &signer, round, &keys, last_chance);
            if let Err(error) = call.await {
                warn!(domain = signer.domain(), %error, "hyperlane checkpoint submit failed");
            }
        }
        Ok(())
    }

    /// Reads one view of `to` at the latest block.
    async fn view<C: SolCall>(
        &self,
        provider: &DynProvider,
        to: Address,
        call: C,
    ) -> Result<C::Return> {
        let out = self
            .call(provider, to, call.abi_encode())
            .await
            .with_context(|| format!("{} failed", C::SIGNATURE))?;
        Ok(C::abi_decode_returns(&out)?)
    }

    /// The ISM and the other validators accept only signatures of
    /// `hyperlaneSigner(V)`, or of `V` while it is zero. Returns an error
    /// text when `H` is another key.
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
                "the checkpoints of {} must have the signature of {expected} (hyperlaneSigner), but private_key is {}",
                self.validator, self.signer
            )
        }))
    }

    async fn call(&self, provider: &DynProvider, to: Address, data: Vec<u8>) -> Result<Bytes> {
        client::eth_call(provider, to, data, BlockId::latest()).await
    }

    /// Sends the call of `round` for the domain of `signer`: the signed
    /// checkpoint at `targetIndex(domain)` (the liveness vote) with the report.
    /// When there is no target, or the store does not have that index yet, it
    /// sends the latest signed checkpoint. It sends nothing when the call
    /// gives the controller no new vote and no newer index: the controller
    /// rejects such a call. When the controller already has the report of
    /// this round, the agent sends only a new vote, with no names: the
    /// controller uses the lists of the first call only. Index 0 is never
    /// submitted.
    async fn submit_round(
        &self,
        provider: &DynProvider,
        signer: &DomainSigner,
        round: u64,
        validators: &[ActiveValidator],
        last_chance: bool,
    ) -> Result<()> {
        let domain = signer.domain();
        let controller = HYPERLANE_CONTROLLER_ADDRESS;
        let validator = self.validator;
        let reported = self
            .view(
                provider,
                controller,
                IHyperlaneController::reportedRoundCall { validator, domain },
            )
            .await?
            == round;
        let Some(latest) = signer.latest_index()? else {
            return Ok(());
        };
        let target = self
            .view(
                provider,
                controller,
                IHyperlaneController::targetIndexCall { domain },
            )
            .await?;
        let voted = match target {
            0 => None,
            target => signer.signed_checkpoint(target)?,
        };
        let is_vote = match &voted {
            None => false,
            Some(checkpoint) => {
                let vote = self
                    .view(
                        provider,
                        controller,
                        IHyperlaneController::targetVoteCall { validator, domain },
                    )
                    .await?;
                vote.index != target || vote.root != checkpoint.value.checkpoint.root
            }
        };
        let submitted = self
            .view(
                provider,
                controller,
                IHyperlaneController::submittedIndexCall { validator, domain },
            )
            .await?;
        if !is_vote && (reported || latest <= submitted) {
            return Ok(());
        }
        // The checks of the other validators are for the target index. Without a vote at the
        // target, the lists are empty.
        let mut not_serving = Vec::new();
        let mut not_delivered = Vec::new();
        let (index, checkpoint) = match voted {
            Some(checkpoint) if reported => (target, checkpoint),
            Some(checkpoint) => {
                let lists = self.report_lists(provider, validators, &checkpoint);
                match lists.await {
                    Ok(lists) => (not_serving, not_delivered) = lists,
                    Err(error) if last_chance => {
                        warn!(
                            domain,
                            error = format!("{error:#}"),
                            "the check of the other validators did not run; the report has no names"
                        );
                    }
                    Err(error) => {
                        return Err(error.wrap_err("the check of the other validators did not run"))
                    }
                }
                (target, checkpoint)
            }
            // The agent did not sign the target yet. A call now has no vote
            // and no names, and the controller uses only the names of the
            // first call of a round: wait for the signature until the last
            // part of the epoch.
            None if target != 0 && !last_chance => return Ok(()),
            None => match signer.signed_checkpoint(latest)? {
                Some(checkpoint) => (latest, checkpoint),
                None => return Ok(()),
            },
        };
        fit_report(&mut not_serving, &mut not_delivered);
        if !not_serving.is_empty() || !not_delivered.is_empty() {
            warn!(
                domain,
                index,
                ?not_serving,
                ?not_delivered,
                "the report names validators"
            );
        }
        let call = IHyperlaneController::submitReportCall {
            domain,
            index,
            root: checkpoint.value.checkpoint.root,
            latestIndex: latest,
            notServing: not_serving.clone(),
            notDelivered: not_delivered.clone(),
        };
        self.submit(provider, call).await?;
        if !reported {
            self.health
                .set_report(domain, round, &not_serving, &not_delivered);
        }
        Ok(())
    }

    /// The two lists of the report for the target checkpoint that this
    /// validator signed as `mine`: the other active validators that do not
    /// serve that checkpoint, and the active validators that did not deliver
    /// a message in their turn. Both are empty without the report checks.
    async fn report_lists(
        &self,
        provider: &DynProvider,
        validators: &[ActiveValidator],
        mine: &Signed<CheckpointWithMessageId>,
    ) -> Result<(Vec<Address>, Vec<Address>)> {
        let Some(checks) = &self.checks else {
            return Ok((Vec::new(), Vec::new()));
        };
        let checkpoint = &mine.value.checkpoint;
        let (domain, target) = (checkpoint.mailbox_domain, checkpoint.index);
        let others: Vec<ActiveValidator> = validators
            .iter()
            .copied()
            .filter(|v| v.validator != self.validator)
            .collect();
        let not_serving = checks
            .not_serving(
                &others,
                domain,
                target,
                mine.value.message_id,
                checkpoint.root,
            )
            .await?;
        // Zero: the target did not move, so there is no delivery check.
        let call = IHyperlaneController::deliveryIndexCall { domain };
        let from = self
            .view(provider, HYPERLANE_CONTROLLER_ADDRESS, call)
            .await?;
        let not_delivered = match from {
            0 => Vec::new(),
            from => {
                checks
                    .not_delivered(validators, self.validator, domain, from, target)
                    .await?
            }
        };
        Ok((not_serving, not_delivered))
    }

    /// Sends `submitReport` and records the result in the health state.
    async fn submit(
        &self,
        provider: &DynProvider,
        call: IHyperlaneController::submitReportCall,
    ) -> Result<()> {
        let (domain, index) = (call.domain, call.index);
        let calldata = call.abi_encode();
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

/// The first block of the last part of an epoch that starts at `start` and
/// has `length` blocks. The part is `length / part` blocks.
fn last_part_from(start: u64, length: u64, part: u64) -> u64 {
    start + length - length / part
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
    use crate::sync::tests::{receipt_json, serve_json_rpc};
    use crate::sync::{Origin, SyncedLeaf, VerifiedBatch};

    const V: Address = Address::repeat_byte(0x77);

    /// The controller and the validator set on Outbe. `hyperlaneSigner(V)`
    /// returns the shared address, `targetIndex` returns the shared target,
    /// `reportedRound` returns the shared round, and `V` has no vote. The
    /// liveness round is 1. Each receipt has status 0 (reverted).
    /// Returns the URL and the count of sent transactions.
    async fn outbe(
        signer_of_v: Arc<Mutex<Address>>,
        target: Arc<Mutex<u32>>,
        reported: Arc<Mutex<u64>>,
    ) -> (String, Arc<Mutex<u32>>) {
        let sent = Arc::new(Mutex::new(0));
        let count = sent.clone();
        let url = serve_json_rpc(move |req: &Value| {
            let id = req["id"].clone();
            let result = match req["method"].as_str().unwrap() {
                "eth_chainId" => json!("0x5"),
                "eth_getTransactionCount" => json!("0x0"),
                "eth_gasPrice" => json!("0x3b9aca00"),
                "eth_estimateGas" => json!("0x10000"),
                "eth_sendRawTransaction" => {
                    *count.lock().unwrap() += 1;
                    json!(B256::repeat_byte(0xab))
                }
                "eth_getTransactionReceipt" => {
                    receipt_json(V, HYPERLANE_CONTROLLER_ADDRESS, 1, false)
                }
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
                        (
                            VALIDATOR_SET_ADDRESS,
                            IValidatorSet::getEpochStartBlockCall::SELECTOR,
                        ) => 0u64.abi_encode(),
                        // An epoch of 0 blocks: the agent sends at once.
                        (
                            VALIDATOR_SET_ADDRESS,
                            IValidatorSet::getEpochLengthBlocksCall::SELECTOR,
                        ) => 0u32.abi_encode(),
                        (VALIDATOR_SET_ADDRESS, _) => vec![V].abi_encode(),
                        (_, IHyperlaneController::livenessRoundCall::SELECTOR) => 1u64.abi_encode(),
                        (_, IHyperlaneController::reportedRoundCall::SELECTOR) => {
                            reported.lock().unwrap().abi_encode()
                        }
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
                "eth_blockNumber" => json!("0x64"),
                other => panic!("unexpected method {other}"),
            };
            json!({"jsonrpc": "2.0", "id": id, "result": result})
        })
        .await;
        (url, sent)
    }

    fn leaf(index: u32) -> SyncedLeaf {
        SyncedLeaf {
            index,
            message_id: B256::repeat_byte(1),
            block: 1,
            root: B256::repeat_byte(2),
        }
    }

    /// The liveness of `V` with the key `key` and the Outbe RPC `url`.
    /// Domain 7 runs and has a signed checkpoint at index 1.
    async fn liveness_of_v(
        url: String,
        key: &PrivateKeySigner,
    ) -> (Liveness, Arc<DomainSigner>, Arc<ValidatorHealth>) {
        let dir = tempfile::tempdir().unwrap().keep();
        let store = Arc::new(Store::open(&dir).unwrap());
        let health = Arc::new(ValidatorHealth::new(store.clone()));
        let origin = Origin {
            domain: 7,
            hook: Address::ZERO,
            validator_announce: Address::ZERO,
            mailbox: Address::ZERO,
            start_block: 1,
        };
        let signer = Arc::new(DomainSigner::new(origin, store, key.clone()).unwrap());
        let batch = VerifiedBatch {
            domain: 7,
            leaves: vec![leaf(0), leaf(1)],
            cursor: 1,
        };
        signer.commit(&batch).await.unwrap();
        health.set_running(signer.clone());
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
        (liveness, signer, health)
    }

    #[tokio::test]
    async fn signer_mismatch_blocks_and_a_reverted_receipt_is_a_failure() {
        let key = PrivateKeySigner::random();
        let signer_of_v = Arc::new(Mutex::new(Address::repeat_byte(0x99)));
        let target = Arc::new(Mutex::new(0));
        let reported = Arc::new(Mutex::new(0));
        let (url, sent) = outbe(signer_of_v.clone(), target.clone(), reported).await;
        let (liveness, _signer, health) = liveness_of_v(url, &key).await;

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
        // sends one call, the target checkpoint with the report.
        *target.lock().unwrap() = 1;
        liveness.tick().await.unwrap();
        assert_eq!(*sent.lock().unwrap(), 2);
    }

    /// The agent is behind when it sends its call of the round: the call has
    /// the report and no vote. When the agent signs the target later in the
    /// same round, it must send the vote.
    #[tokio::test]
    async fn a_vote_goes_also_after_the_report_of_the_round() {
        let key = PrivateKeySigner::random();
        let signer_of_v = Arc::new(Mutex::new(key.address()));
        let target = Arc::new(Mutex::new(2));
        let reported = Arc::new(Mutex::new(0));
        let (url, sent) = outbe(signer_of_v, target, reported.clone()).await;
        let (liveness, signer, _health) = liveness_of_v(url, &key).await;

        // The target is index 2, and the agent signed only index 1: the call
        // has the latest index and no vote. The controller records the
        // report of round 1.
        liveness.tick().await.unwrap();
        assert_eq!(*sent.lock().unwrap(), 1);
        *reported.lock().unwrap() = 1;

        // No new vote: no second call.
        liveness.tick().await.unwrap();
        assert_eq!(*sent.lock().unwrap(), 1);

        // The agent signs the target: the vote goes.
        let batch = VerifiedBatch {
            domain: 7,
            leaves: vec![leaf(2)],
            cursor: 2,
        };
        signer.commit(&batch).await.unwrap();
        liveness.tick().await.unwrap();
        assert_eq!(*sent.lock().unwrap(), 2, "the vote at the target is lost");
    }

    #[test]
    fn the_call_of_the_round_goes_in_the_last_part_of_the_epoch() {
        // An epoch of 1200 blocks from block 900 ends before block 2100.
        assert_eq!(last_part_from(900, 1200, REPORT_EPOCH_PART), 1800);
        assert_eq!(last_part_from(900, 1200, LAST_CHANCE_EPOCH_PART), 1950);
        // A very short epoch: the part has no blocks, so no call goes.
        assert_eq!(last_part_from(900, 3, REPORT_EPOCH_PART), 903);
        assert_eq!(last_part_from(900, 0, REPORT_EPOCH_PART), 900);
    }
}
