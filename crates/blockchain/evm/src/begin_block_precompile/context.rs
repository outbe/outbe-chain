use crate::executor::AccountedParentArtifact;
use alloy_primitives::Address;
use alloy_primitives::B256;
use alloy_primitives::U256;
use outbe_primitives::block::BlockContext;
use outbe_primitives::block::BlockRuntimeContext;
use outbe_primitives::error::PrecompileError;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

/// Execution context preloaded by the executor before calling the
/// begin-block precompile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PreloadedSystemTxContext {
    pub proposer: Address,
    pub finalized_summary: Option<AccountedParentArtifact>,
    /// True only for a block carrying a validator-set-changing BoundaryOutcome
    /// whose target set includes `proposer`. This lets the activation block be
    /// produced by a next-epoch leader before BoundaryOutcome updates parent-state
    /// consensus membership.
    pub allow_boundary_proposer: bool,
    /// canonical hash of the VRF proof carried in the verified
    /// parent certificate (`keccak256(VrfProof::encode())`). Derived by
    /// the executor's Phase 1 preflight from
    /// `outbe_consensus::proof::VerifiedProof::vrf_proof_hash` and fed
    /// into the V3 Rewards fingerprint so that two parent certificates
    /// with different VRF proofs cannot collide. `B256::ZERO` when the
    /// preflight was skipped (genesis bootstrap / test-only opt-out).
    pub canonical_vrf_proof_hash: B256,
}

thread_local! {
    static PRELOADED_SYSTEM_TX_CONTEXT: std::cell::RefCell<Option<PreloadedSystemTxContext>> =
        const { std::cell::RefCell::new(None) };
}

/// Runs `f` with explicit non-calldata context visible to the system
/// precompile on the current thread.
///
/// This keeps CertifiedParentAccounting money fields out of signed calldata while
/// still giving the precompile an explicit deterministic data path for the
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
                *slot.borrow_mut() = self.0;
            });
        }
    }

    let previous = PRELOADED_SYSTEM_TX_CONTEXT.with(|slot| slot.replace(Some(context)));
    let _reset = Reset(previous);
    f()
}

pub(super) fn current_preloaded_system_tx_context() -> Option<PreloadedSystemTxContext> {
    PRELOADED_SYSTEM_TX_CONTEXT.with(|slot| *slot.borrow())
}

/// Exact finalized state root carried by the executor-owned Phase 1 context.
///
/// This is visible only to the sibling provider-route classifier. Calldata
/// cannot set it, and the guard clears it after the one system transaction.
pub(in super::super) fn preloaded_certified_parent_state_root() -> Option<B256> {
    current_preloaded_system_tx_context()
        .and_then(|context| context.finalized_summary)
        .and_then(|summary| summary.state_root)
}

pub(super) fn block_runtime_context_from_storage(
    storage: StorageHandle,
    include_validator_snapshot: bool,
) -> Result<BlockRuntimeContext> {
    let block_number = storage.block_number()?;
    let timestamp = u256_to_u64("block timestamp", storage.timestamp()?)?;
    let chain_id = storage.chain_id()?;
    let genesis_hash = storage.genesis_hash()?;
    let proposer = current_preloaded_system_tx_context()
        .map(|context| context.proposer)
        .unwrap_or(storage.beneficiary()?);

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
        // OracleSlashWindow only uses block number/timestamp plus storage. Avoid
        // charging a mandatory no-op system tx for an unrelated active-set snapshot.
        Vec::new()
    };

    Ok(BlockRuntimeContext::new(
        BlockContext::new_with_genesis_hash(
            block_number,
            timestamp,
            chain_id,
            genesis_hash,
            proposer,
            validators,
        ),
        storage,
    ))
}

pub(super) fn read_preloaded_finalized_summary(
    _storage: &StorageHandle<'_>,
) -> Result<Option<AccountedParentArtifact>> {
    Ok(current_preloaded_system_tx_context().and_then(|context| context.finalized_summary))
}

fn u256_to_u64(name: &str, value: U256) -> Result<u64> {
    if value > U256::from(u64::MAX) {
        return Err(PrecompileError::Fatal(format!(
            "{name} exceeds u64 range: {value}"
        )));
    }
    Ok(value.to::<u64>())
}
