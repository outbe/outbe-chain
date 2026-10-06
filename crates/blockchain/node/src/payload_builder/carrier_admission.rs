//! Builder-side admission of OCOMP result-vote carriers.
//!
//! A carrier whose inner vote is invalid fails identically on every node. If the
//! build aborted on such a carrier, every leader would lose its view for as long
//! as the carrier stays in the pool. Before executing such a carrier, the builder
//! asks the shared Metadosis verifier about it against the exact in-progress
//! block state. It skips a carrier the verifier proves invalid, and defers one
//! whose due response window this block has not closed yet.
//!
//! The check reads state and never writes it, so a skipped carrier leaves no
//! trace in the block. Everything else keeps the existing path: the builder
//! executes the carrier, and any failure aborts the build so the leader proposes
//! nothing.

use std::any::Any;

use alloy_consensus::Transaction as _;
use alloy_primitives::{Address, B256};
use outbe_metadosis::api::{verify_result_vote_carrier, ResultVoteCarrierAdmission};
use outbe_ocomp_protocol::{
    profile::poc_schema_limits,
    system_carrier::{
        classify_ocomp_system_carrier, OcompSystemCarrierCandidate, OcompSystemCarrierView,
    },
};
use outbe_primitives::{
    block::BlockContext,
    storage::{direct::DirectStorageProvider, StorageHandle},
    OutbeTxEnvelope,
};
use reth_evm::block::StateDB;
use reth_transaction_pool::error::PoolTransactionError;

/// Block identity used to read state for the admission check.
#[derive(Debug, Clone, Copy)]
pub(super) struct CarrierBlock {
    pub(super) number: u64,
    pub(super) timestamp: u64,
    pub(super) chain_id: u64,
    pub(super) genesis_hash: B256,
    pub(super) beneficiary: Address,
}

/// What the builder does with one pool transaction before executing it.
#[derive(Debug)]
pub(super) enum CarrierDecision {
    /// Not a result-vote carrier, or one the verifier accepts: execute it.
    Execute,
    /// The verifier proved the carrier invalid on this block state.
    Skip,
    /// The carrier fails on this block state but may execute in a later block.
    /// The builder leaves it out of this block and does not classify it as bad.
    Defer,
    /// The verifier could not decide from healthy state. The build must fail.
    Abort(CarrierAdmissionAbort),
}

/// Why a carrier check ends the build instead of deciding about the carrier.
#[derive(Debug, thiserror::Error)]
pub(super) enum CarrierAdmissionAbort {
    #[error("result-vote carrier window opens at height {open_height}")]
    NotYetOpen { open_height: u64 },
    #[error("result-vote carrier state is unavailable: {0}")]
    StateUnavailable(outbe_primitives::error::PrecompileError),
    #[error("result-vote carrier found corrupt committed state: {0}")]
    CorruptCommittedState(outbe_primitives::error::PrecompileError),
}

/// Iterator-local reason recorded for a carrier the verifier proved invalid.
#[derive(Debug, thiserror::Error)]
#[error("OCOMP result-vote carrier is invalid on the block state it would execute on")]
pub(super) struct InvalidResultVoteCarrier;

impl PoolTransactionError for InvalidResultVoteCarrier {
    fn is_bad_transaction(&self) -> bool {
        true
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Iterator-local reason recorded for a carrier whose response window is due
/// but not yet closed on this block state. Execution would reject it here,
/// while the next block closes the window and turns it into a late vote.
#[derive(Debug, thiserror::Error)]
#[error("OCOMP result-vote carrier targets a due response window that this block has not closed")]
pub(super) struct DeferredResultVoteCarrier;

impl PoolTransactionError for DeferredResultVoteCarrier {
    fn is_bad_transaction(&self) -> bool {
        false
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Classifies one transaction and, for a result-vote carrier, runs the shared
/// verifier on `db`, which must hold the block state the carrier would execute on.
pub(super) fn admit<DB: StateDB>(
    db: &mut DB,
    block: CarrierBlock,
    tx: &OutbeTxEnvelope,
    signer: Address,
) -> CarrierDecision {
    let limits = poc_schema_limits();
    let view = OcompSystemCarrierView {
        is_eip1559: tx.is_eip1559(),
        to: tx.to(),
        value: tx.value(),
        input: tx.input().as_ref(),
        gas_limit: tx.gas_limit(),
        max_fee_per_gas: tx.max_fee_per_gas(),
        max_priority_fee_per_gas: tx.max_priority_fee_per_gas(),
    };
    // This function checks only a well-formed result-vote envelope. Malformed
    // envelopes and NOD materialization keep their existing execution path.
    let Ok(Some(OcompSystemCarrierCandidate::ResultVote { .. })) =
        classify_ocomp_system_carrier(view, &limits)
    else {
        return CarrierDecision::Execute;
    };

    let ctx = BlockContext::new_with_genesis_hash(outbe_primitives::block::BlockContextInput {
        block_number: block.number,
        timestamp: block.timestamp,
        chain_id: block.chain_id,
        genesis_hash: block.genesis_hash,
        proposer: block.beneficiary,
        validators: Vec::new(),
    });
    let mut provider = DirectStorageProvider::new(db, ctx);
    let admission = verify_result_vote_carrier(
        StorageHandle::new(&mut provider),
        view.input,
        signer,
        block.number,
        &limits,
    );
    decide(admission)
}

/// Maps the verifier outcome to the builder policy. The builder skips only a
/// proven invalid carrier. State that cannot be read ends the build.
pub(super) fn decide(admission: ResultVoteCarrierAdmission) -> CarrierDecision {
    match admission {
        // A closed window still executes: execution records the soft-failure
        // receipt that the protocol expects for a late vote.
        ResultVoteCarrierAdmission::Valid { .. }
        | ResultVoteCarrierAdmission::DeadlinePassed { .. } => CarrierDecision::Execute,
        ResultVoteCarrierAdmission::InvalidCarrier { .. } => CarrierDecision::Skip,
        // Several windows can be due at one height while the begin zone closes
        // only one of them. Thus this outcome is not a node failure. Aborting on
        // it would stall every leader at this height.
        ResultVoteCarrierAdmission::DeadlineDueUnclosed { .. } => CarrierDecision::Defer,
        ResultVoteCarrierAdmission::NotYetOpen { open_height } => {
            CarrierDecision::Abort(CarrierAdmissionAbort::NotYetOpen { open_height })
        }
        ResultVoteCarrierAdmission::StateUnavailable { source } => {
            CarrierDecision::Abort(CarrierAdmissionAbort::StateUnavailable(source))
        }
        ResultVoteCarrierAdmission::CorruptCommittedState { source } => {
            CarrierDecision::Abort(CarrierAdmissionAbort::CorruptCommittedState(source))
        }
    }
}
