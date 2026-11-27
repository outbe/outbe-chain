//! Execution-side submission of verified consensus equivocation evidence.
//!
//! The worker shares `evm-key.hex` with outbe-feeder. A foreign pending transaction
//! yields `sender_busy`; a concurrent insertion can still yield `ReplacementUnderpriced`.
//! Pool failures retry the committed nonce, never allocate a speculative next nonce.
//! Operators should give the feeder a separate `ValidatorDelegateRole::Oracle` key.
//!
//! Simplex uses `simplex::ForwardPolicy::Disabled`: automatic slashing requires at
//! least one honest validator to receive both conflicting votes. A split-brain
//! partition with no overlapping recipients is outside this subsystem's scope.

mod pending;

use std::{sync::Arc, time::Duration};

use alloy_consensus::{BlockHeader, TxEip1559};
use alloy_eips::eip2718::Encodable2718;
use alloy_primitives::{keccak256, Address, Bytes, TxKind, B256, U256};
use alloy_sol_types::SolCall;
use outbe_primitives::{
    equivocation_evidence::{
        EquivocationEvidenceKind, EquivocationEvidenceMailbox, VerifiedEquivocationEvidence,
    },
    error::PrecompileError,
    signer::OutbeEvmSigner,
    storage::{
        readonly::{ReadOnlyStorageProvider, StorageReader},
        StorageHandle,
    },
};
use outbe_slashindicator::{
    errors::SlashIndicatorError, precompile::ISlashIndicator, preflight_equivocation_evidence,
    EquivocationPreflightError,
};
use outbe_validatorset::{contract::ValidatorSet, read_committee_snapshot_for_epoch};
use reth_chainspec::EthChainSpec;
use reth_ethereum::pool::{EthPooledTransaction, TransactionOrigin, TransactionPool};
use reth_primitives_traits::Recovered;
use reth_provider::{BlockNumReader, HeaderProvider, StateProvider, StateProviderFactory};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use pending::{coalesce, fee_for_attempt, rank_delay, PendingSlash, PendingSlashMap};

#[derive(Debug, thiserror::Error)]
#[error("canonical head is unavailable")]
struct MissingHead;

struct StateReader<'a>(&'a dyn StateProvider);

impl StorageReader for StateReader<'_> {
    fn read_storage(&self, address: Address, key: B256) -> Result<U256, PrecompileError> {
        self.0
            .storage(address, key)
            .map(|value| value.unwrap_or_default())
            .map_err(|error| PrecompileError::FatalWithCause(Box::new(error)))
    }
}

/// One block interval. `pending::delay` multiplies it by two blocks for each rank.
const EQUIVOCATION_RETRY_STEP: Duration =
    Duration::from_millis(outbe_consensus::timing::MIN_BLOCK_TIME_MS);

/// Starts the validator's execution-side evidence consumer until cancellation.
pub fn spawn(
    node: crate::OutbeFullNode,
    signer: Arc<OutbeEvmSigner>,
    mailbox: EquivocationEvidenceMailbox,
    shutdown_token: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    spawn_inner(
        node,
        signer,
        mailbox,
        EQUIVOCATION_RETRY_STEP,
        shutdown_token,
    )
}

#[cfg(feature = "test-hooks")]
pub fn spawn_with_retry_step_for_test(
    node: crate::OutbeFullNode,
    signer: Arc<OutbeEvmSigner>,
    mailbox: EquivocationEvidenceMailbox,
    retry_step: Duration,
    shutdown_token: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    spawn_inner(node, signer, mailbox, retry_step, shutdown_token)
}

fn spawn_inner(
    node: crate::OutbeFullNode,
    signer: Arc<OutbeEvmSigner>,
    mailbox: EquivocationEvidenceMailbox,
    retry_step: Duration,
    shutdown_token: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut pending = PendingSlashMap::new();
        let mut drain_retry_at = None;
        loop {
            if shutdown_token.is_cancelled() {
                break;
            }
            drain_when_due(&mut drain_retry_at, retry_step, || {
                drain_mailbox(&node, &mailbox, &mut pending)
            });
            let now = Instant::now();
            let due: Vec<_> = pending
                .iter()
                .filter(|(_, task)| task.next_attempt_at <= now)
                .map(|(key, _)| *key)
                .collect();
            for key in due {
                let Some(mut task) = pending.remove(&key) else {
                    continue;
                };
                let retry = tokio::select! {
                    _ = shutdown_token.cancelled() => return,
                    retry = attempt(&node, &signer, &mut task, retry_step) => retry,
                };
                if retry {
                    pending.insert(key, task);
                }
                if shutdown_token.is_cancelled() {
                    return;
                }
            }
            if drain_retry_at.is_none() && pending.len() < 64 && !mailbox.is_empty() {
                continue;
            }
            let next = pending
                .values()
                .map(|task| task.next_attempt_at)
                .chain(drain_retry_at)
                .chain((!mailbox.is_empty()).then(|| Instant::now() + retry_step))
                .min();
            tokio::select! {
                _ = shutdown_token.cancelled() => break,
                _ = mailbox.notified() => {},
                _ = wait_until(next) => {},
            }
        }
    })
}

fn drain_when_due(
    retry_at: &mut Option<Instant>,
    retry_step: Duration,
    drain: impl FnOnce() -> eyre::Result<()>,
) {
    if retry_at.is_some_and(|deadline| deadline > Instant::now()) {
        return;
    }
    *retry_at = match drain() {
        Ok(()) => None,
        Err(error) => {
            failed("state_read", &error);
            Some(Instant::now() + retry_step)
        }
    };
}

async fn wait_until(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

fn drain_mailbox(
    node: &crate::OutbeFullNode,
    mailbox: &EquivocationEvidenceMailbox,
    pending: &mut PendingSlashMap,
) -> eyre::Result<()> {
    if mailbox.is_empty() {
        return Ok(());
    }
    let state = node.provider.latest()?;
    let mut readonly = ReadOnlyStorageProvider::new_with_chain_identity(
        StateReader(state.as_ref()),
        node.chain_spec().chain().id(),
        node.chain_spec().genesis_hash(),
    );
    StorageHandle::enter(&mut readonly, |storage| {
        let vs = ValidatorSet::new(storage);
        drain_resolved(mailbox, pending, |ev| {
            Ok(vs.lookup_by_pubkey_hash(keccak256(ev.accused_pubkey))?)
        })
    })
}

fn drain_resolved(
    mailbox: &EquivocationEvidenceMailbox,
    pending: &mut PendingSlashMap,
    mut resolve: impl FnMut(&VerifiedEquivocationEvidence) -> eyre::Result<Address>,
) -> eyre::Result<()> {
    let mut read_error = None;
    loop {
        let mut accused = Address::ZERO;
        let evidence = mailbox.pop_if(|ev| match resolve(ev) {
            Ok(address) => {
                accused = address;
                address.is_zero()
                    || pending.contains_key(&(address, ev.epoch))
                    || pending.len() < 64
            }
            Err(error) => {
                read_error = Some(error);
                false
            }
        });
        let Some(evidence) = evidence else { break };
        if accused.is_zero() {
            record_equivocation_skipped("unregistered_signer");
            continue;
        }
        coalesce(pending, accused, evidence);
    }
    match read_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

struct Ready {
    nonce: u64,
    base_fee: u64,
    delay: Duration,
}

enum Tick {
    Ready(Ready),
    Skip {
        reason: &'static str,
        terminal: bool,
    },
}

fn read_tick(
    node: &crate::OutbeFullNode,
    signer: Address,
    task: &PendingSlash,
    retry_step: Duration,
) -> eyre::Result<Tick> {
    let head = node.provider.best_block_number()?;
    let header = node.provider.sealed_header(head)?.ok_or(MissingHead)?;
    let state = node.provider.history_by_block_hash(header.hash())?;
    let mut readonly = ReadOnlyStorageProvider::new_with_chain_identity(
        StateReader(state.as_ref()),
        node.chain_spec().chain().id(),
        node.chain_spec().genesis_hash(),
    );
    StorageHandle::enter(&mut readonly, |storage| -> eyre::Result<Tick> {
        let Some(snapshot) =
            read_committee_snapshot_for_epoch(storage.clone(), task.evidence.epoch)?
        else {
            return Ok(Tick::Skip {
                reason: "expired_epoch",
                terminal: true,
            });
        };
        let ev = &task.evidence;
        let target = match preflight_equivocation_evidence(
            storage.clone(),
            signer,
            ev.kind,
            &ev.first_block,
            &ev.second_block,
        ) {
            Ok(target) => target,
            Err(error) => return classify_preflight(error),
        };
        let vs = ValidatorSet::new(storage);
        let mut active = Vec::new();
        for entry in snapshot.committee {
            if vs.validator_lifecycle(entry.address)?.is_active_status() {
                active.push(entry.address);
            }
        }
        let nonce = state
            .basic_account(&signer)?
            .map(|account| account.nonce)
            .unwrap_or_default();
        Ok(Tick::Ready(Ready {
            nonce,
            base_fee: header.base_fee_per_gas().unwrap_or_default(),
            delay: rank_delay(ev.epoch, target.accused, signer, active, retry_step),
        }))
    })
}

fn classify_preflight(error: EquivocationPreflightError) -> eyre::Result<Tick> {
    use EquivocationPreflightError::*;
    let (reason, terminal) = match error {
        EvidenceAlreadyProcessed(_) => ("already_processed", true),
        ValidatorAlreadyPenalized(_) => ("already_penalized", true),
        Rejected(SlashIndicatorError::SignerNotRegisteredValidator) => {
            ("unregistered_signer", true)
        }
        Rejected(SlashIndicatorError::ValidatorNotSlashable { .. }) => ("not_slashable", false),
        SubmitterNotActive(_) => ("submitter_not_active", false),
        Rejected(_) => ("rejected", true),
        Precompile(error) => return Err(error.into()),
    };
    Ok(Tick::Skip { reason, terminal })
}

async fn attempt(
    node: &crate::OutbeFullNode,
    signer: &OutbeEvmSigner,
    task: &mut PendingSlash,
    retry_step: Duration,
) -> bool {
    task.next_attempt_at = Instant::now() + retry_step;
    let ready = match read_tick(node, signer.address(), task, retry_step) {
        Ok(Tick::Skip { reason, terminal }) => {
            record_equivocation_skipped(reason);
            return !terminal;
        }
        Ok(Tick::Ready(ready)) => ready,
        Err(error) => {
            failed("state_read", &error);
            return true;
        }
    };
    if !task.ranked {
        task.ranked = true;
        if !ready.delay.is_zero() {
            task.next_attempt_at = Instant::now() + ready.delay;
            return true;
        }
    }
    let pool_txs = TransactionPool::get_transactions_by_sender(&node.pool, signer.address());
    let pool: Vec<_> = pool_txs.iter().map(|tx| (*tx.hash(), tx.nonce())).collect();
    let fee = match fee_for_attempt(task.submitted_tx, ready.nonce, ready.base_fee, &pool) {
        Ok(Some(fee)) => fee,
        Ok(None) => return true,
        Err(reason) => {
            record_equivocation_skipped(reason);
            return true;
        }
    };
    if let Err(error) = submit(node, signer, task, ready.nonce, fee).await {
        failed("submit", &error);
    }
    true
}

async fn submit(
    node: &crate::OutbeFullNode,
    signer: &OutbeEvmSigner,
    task: &mut PendingSlash,
    nonce: u64,
    fee: u128,
) -> eyre::Result<()> {
    let tx = TxEip1559 {
        chain_id: node.chain_spec().chain().id(),
        nonce,
        gas_limit: outbe_zerofee::MAX_ZERO_FEE_SLASH_EQUIVOCATION_GAS_LIMIT,
        max_fee_per_gas: fee,
        max_priority_fee_per_gas: 0,
        to: TxKind::Call(outbe_system_common::addresses::SLASH_INDICATOR_ADDRESS),
        value: U256::ZERO,
        input: calldata(&task.evidence),
        access_list: Default::default(),
    };
    let signed = signer.sign_eip1559(tx)?;
    let length = signed.encode_2718_len();
    let tx = EthPooledTransaction::new(Recovered::new_unchecked(signed, signer.address()), length);
    let outcome = node
        .pool
        .add_transaction(TransactionOrigin::Local, tx)
        .await?;
    task.submitted_tx = Some((outcome.hash, nonce, fee));
    record_equivocation_submitted(task.evidence.kind);
    Ok(())
}

fn calldata(ev: &outbe_primitives::equivocation_evidence::VerifiedEquivocationEvidence) -> Bytes {
    let block1 = ev.first_block.clone().into();
    let block2 = ev.second_block.clone().into();
    match ev.kind {
        EquivocationEvidenceKind::ConflictingNotarize => {
            ISlashIndicator::submitConflictingNotarizeEvidenceCall { block1, block2 }.abi_encode()
        }
        EquivocationEvidenceKind::ConflictingFinalize => {
            ISlashIndicator::submitConflictingFinalizeEvidenceCall { block1, block2 }.abi_encode()
        }
        EquivocationEvidenceKind::NullifyFinalize => {
            ISlashIndicator::submitNullifyFinalizeEvidenceCall {
                nullifyBlock: block1,
                finalizeBlock: block2,
            }
            .abi_encode()
        }
    }
    .into()
}

/// Records a locally accepted submission, including a replacement.
pub fn record_equivocation_submitted(kind: EquivocationEvidenceKind) {
    metrics::counter!("outbe_slashing_equivocation_submitted_total", "kind" => kind.label())
        .increment(1);
}

/// Records a deferred or terminal evidence submission decision.
pub fn record_equivocation_skipped(reason: &'static str) {
    metrics::counter!("outbe_slashing_equivocation_skipped_total", "reason" => reason).increment(1);
}

/// Records a submission attempt failure under its stable reason label.
pub fn record_equivocation_submit_failed(reason: &'static str) {
    metrics::counter!("outbe_slashing_equivocation_submit_failed_total", "reason" => reason)
        .increment(1);
}

fn failed(reason: &'static str, error: &dyn std::fmt::Debug) {
    record_equivocation_submit_failed(reason);
    tracing::warn!(?error, reason, "equivocation submission will retry");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preflight_classification_retains_transient_tasks_and_retires_terminal_tasks() {
        use EquivocationPreflightError::*;
        let address = Address::repeat_byte(1);
        for (error, reason, terminal) in [
            (
                EvidenceAlreadyProcessed(SlashIndicatorError::EvidenceAlreadyProcessed),
                "already_processed",
                true,
            ),
            (
                ValidatorAlreadyPenalized(SlashIndicatorError::ValidatorAlreadyPenalized {
                    validator: address,
                    epoch: 1,
                }),
                "already_penalized",
                true,
            ),
            (
                Rejected(SlashIndicatorError::InvalidBlsSignature),
                "rejected",
                true,
            ),
            (
                Rejected(SlashIndicatorError::SignerNotRegisteredValidator),
                "unregistered_signer",
                true,
            ),
            (
                Rejected(SlashIndicatorError::ValidatorNotSlashable {
                    validator: address,
                    status: 1,
                }),
                "not_slashable",
                false,
            ),
            (
                SubmitterNotActive(SlashIndicatorError::SubmitterNotActiveValidator {
                    caller: address,
                    status: 1,
                }),
                "submitter_not_active",
                false,
            ),
        ] {
            let Tick::Skip {
                reason: actual_reason,
                terminal: actual_terminal,
            } = classify_preflight(error).unwrap()
            else {
                panic!("expected skip")
            };
            assert_eq!((actual_reason, actual_terminal), (reason, terminal));
        }
        let storage_error =
            classify_preflight(Precompile(PrecompileError::Storage("retry".into())))
                .err()
                .expect("storage failure is retryable");
        assert!(matches!(
            storage_error.downcast_ref::<PrecompileError>(),
            Some(PrecompileError::Storage(_))
        ));
        let error = classify_preflight(Precompile(PrecompileError::FatalWithCause(Box::new(
            MissingHead,
        ))))
        .err()
        .expect("provider failure remains retryable error");
        let retained = error.downcast_ref::<PrecompileError>().unwrap();
        assert!(matches!(retained, PrecompileError::FatalWithCause(cause)
            if cause.downcast_ref::<MissingHead>().is_some()));
    }
}
