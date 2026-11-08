use std::sync::Arc;

use crate::executor::AccountedParentArtifact;
use alloy_primitives::Address;
use alloy_primitives::B256;
use outbe_primitives::block::BlockContext;
use outbe_primitives::block::BlockRuntimeContext;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

/// Execution context preloaded by the executor before calling the
/// begin-block precompile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreloadedSystemTxContext {
    pub proposer: Address,
    pub finalized_summary: Option<AccountedParentArtifact>,
    /// True only for a block carrying a validator-set-changing BoundaryOutcome
    /// whose target set includes `proposer`. This lets a next-epoch leader produce
    /// the activation block before BoundaryOutcome updates parent-state consensus
    /// membership.
    pub allow_boundary_proposer: bool,
    /// canonical hash of the VRF proof carried in the verified
    /// parent certificate (`keccak256(VrfProof::encode())`). The executor's
    /// Phase 1 preflight derives it from
    /// `outbe_consensus::proof::VerifiedProof::vrf_proof_hash` and feeds it
    /// into the V3 Rewards fingerprint. Thus two parent certificates with
    /// different VRF proofs cannot collide. The value is `B256::ZERO` when the
    /// preflight was skipped (genesis bootstrap / test-only opt-out).
    pub canonical_vrf_proof_hash: B256,
    /// Pre-verified signer indices per `LateFinalizeCredits` batch from
    /// `verify_late_finalize_credits_in_preexec`, eliminating a second BLS
    /// pairing inside `run_late_finalize_credits`.
    pub verified_late_credit_signers: Option<Arc<Vec<Vec<usize>>>>,
    /// Optional explicit gas limit override for system transactions. When `None`,
    /// system transactions execute on the 10B `SYSTEM_TX_ARTIFACT_GAS_LIMIT` lane.
    pub gas_limit_override: Option<u64>,
    /// Aggregate begin-zone gross capacity before this call. Refunds do not
    /// replenish this allowance; end-zone execution uses its separate lane.
    pub remaining_system_gross_gas: u64,
    /// Block gas limit for user transactions.
    pub block_gas_limit: u64,
    /// Cumulative gas consumed by user transactions before `EndBlock`.
    pub user_gas_used: u64,
}

impl PreloadedSystemTxContext {
    pub(crate) const fn for_end_block(
        proposer: Address,
        block_gas_limit: u64,
        user_gas_used: u64,
    ) -> Self {
        Self {
            proposer,
            finalized_summary: None,
            allow_boundary_proposer: false,
            canonical_vrf_proof_hash: B256::ZERO,
            verified_late_credit_signers: None,
            gas_limit_override: None,
            remaining_system_gross_gas: 0,
            block_gas_limit,
            user_gas_used,
        }
    }
}

thread_local! {
    static PRELOADED_SYSTEM_TX_CONTEXT: std::cell::RefCell<Option<PreloadedSystemTxContext>> =
        const { std::cell::RefCell::new(None) };
}

/// Runs `f` with explicit non-calldata context visible to the system
/// precompile on the current thread.
///
/// This keeps CertifiedParentAccounting money fields out of signed calldata. It
/// still gives the precompile an explicit deterministic data path for the
/// parent block's committed execution summary. The executor sets this only
/// around a single `transact_system_call`, and the guard restores the previous
/// value on exit.
pub(crate) fn with_preloaded_system_tx_context<R>(
    context: PreloadedSystemTxContext,
    f: impl FnOnce() -> R,
) -> R {
    struct Reset(Option<PreloadedSystemTxContext>);

    impl Drop for Reset {
        fn drop(&mut self) {
            PRELOADED_SYSTEM_TX_CONTEXT.with(|slot| {
                *slot.borrow_mut() = self.0.take();
            });
        }
    }

    let previous = PRELOADED_SYSTEM_TX_CONTEXT.with(|slot| slot.replace(Some(context)));
    let _reset = Reset(previous);
    f()
}

pub(crate) fn current_preloaded_system_tx_context() -> Option<PreloadedSystemTxContext> {
    PRELOADED_SYSTEM_TX_CONTEXT.with(|slot| slot.borrow().clone())
}

pub(super) fn block_runtime_context_from_storage(
    storage: StorageHandle,
    include_validator_snapshot: bool,
) -> Result<BlockRuntimeContext> {
    let block_number = storage.block_number()?;
    let timestamp = storage.now_secs()?;
    let chain_id = storage.chain_id()?;
    let genesis_hash = storage.genesis_hash()?;
    let preloaded = current_preloaded_system_tx_context();
    let proposer = preloaded
        .as_ref()
        .map(|context| context.proposer)
        .unwrap_or(storage.beneficiary()?);
    let block_gas_limit = preloaded
        .as_ref()
        .map(|context| context.block_gas_limit)
        .unwrap_or(0);
    let user_gas_used = preloaded
        .as_ref()
        .map(|context| context.user_gas_used)
        .unwrap_or(0);

    let validators = if include_validator_snapshot {
        let vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        let mut validators: Vec<Address> = vs
            .get_active_consensus_set()?
            .into_iter()
            .map(|record| record.validator_address)
            .collect();
        validators.sort();
        validators
    } else {
        Vec::new()
    };

    Ok(BlockRuntimeContext::new(
        BlockContext {
            block_gas_limit,
            user_gas_used,
            block_number,
            timestamp,
            chain_id,
            genesis_hash,
            genesis_timestamp: storage.genesis_timestamp()?,
            proposer,
            validators,
        },
        storage,
    ))
}

pub(crate) fn read_preloaded_finalized_summary(
    _storage: &StorageHandle<'_>,
) -> Result<Option<AccountedParentArtifact>> {
    Ok(current_preloaded_system_tx_context().and_then(|context| context.finalized_summary))
}
