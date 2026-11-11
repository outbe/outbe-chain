//! Deterministic 4-stage system-transaction primitives (`Consensus -> BeginBlock -> MidBlock -> EndBlock -> Completed`).
//!
//! Outbe represents runtime system transactions as ordinary signed Ethereum
//! legacy transaction artifacts (`gas_limit = 0`, `gas_price = 0`, `value = 0`)
//! so standard `eth_*` RPC methods can expose their receipts and logs. The
//! artifacts are consensus inputs only: execution uses `transact_system_call`
//! with `SYSTEM_ADDRESS` as the EVM caller and contributes `0` visible gas to
//! block headers and receipts.

use crate::error::PrecompileError;

pub use crate::addresses::OUTBE_SYSTEM_TX_ADDRESS;
pub use crate::block::BlockHeightRegime;

pub mod binding;

mod envelope;
mod fsm;
mod gas;
mod input;
mod layout;
mod witness;

pub use input::{assemble_prefix_system_txs, SystemTx, SystemTxStepId};

/// Four-byte kind selector for a system transaction.
pub type SysTxSelector = [u8; 4];

/// Body-zone position of a system transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyZone {
    BeginBlock,
    EndBlock,
}

pub fn selector_from_input(input: &[u8]) -> Result<SysTxSelector, SystemTxError> {
    input
        .get(..4)
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or(SystemTxError::InputTooShort { len: input.len() })
}

pub fn is_reserved_system_tx<T: alloy_consensus::Transaction + ?Sized>(tx: &T) -> bool {
    tx.to() == Some(OUTBE_SYSTEM_TX_ADDRESS)
}

pub use crate::block::BlockSystemFlags;
pub use fsm::{BlockExecutionFsm, BlockExecutionStage, BlockSystemPlan, PlannedSystemStep};
pub use layout::{split_system_layout, validate_system_tx_set_for_publication, SystemTxLayout};

pub use gas::{system_tx_intrinsic_gas, system_tx_visible_gas_limit};

pub use envelope::{
    build_unsigned_system_tx, build_unsigned_system_tx_with_gas_limit, system_tx_nonce,
    SystemTxEnvelopeInput,
};

pub use witness::{recover_phase1_proposer, validate_phase1_witness_against};

/// Stage selector(4), version(1), static step index(1).
pub const SYSTEM_TX_INPUT_VERSION: u8 = 3;
pub const SYSTEM_CALLDATA_FRAMING_BYTES: usize = 6;
pub const CONSENSUS_STAGE_SELECTOR: SysTxSelector = *b"OSCN";
pub const BEGIN_BLOCK_STAGE_SELECTOR: SysTxSelector = *b"OSBB";
pub const END_BLOCK_STAGE_SELECTOR: SysTxSelector = *b"OSEB";
pub const END_BLOCK_BASE_ORDINAL: u8 = 24;
pub const MAX_SYSTEM_TXS_PER_BLOCK: u8 = 32;
pub const MAX_PREFIX_SYSTEM_TXS: usize = 24;
pub const MAX_END_BLOCK_SYSTEM_TXS: usize = 8;
pub const CONSENSUS_PARENT_ACCOUNTING_STEP_INDEX: u8 = 0;
pub const CONSENSUS_LATE_FINALIZE_CREDITS_STEP_INDEX: u8 = 1;
pub const CONSENSUS_BOUNDARY_OUTCOME_STEP_INDEX: u8 = 2;
pub const CONSENSUS_TEE_BOOTSTRAP_STEP_INDEX: u8 = 3;
pub const CONSENSUS_VALIDATOR_LIVENESS_STEP_INDEX: u8 = 4;
pub const CONSENSUS_RADICLE_PUBLICATION_STEP_INDEX: u8 = 5;

pub const fn end_block_nonce_ordinal(step_index: u8) -> Option<u8> {
    let Some(ordinal) = END_BLOCK_BASE_ORDINAL.checked_add(step_index) else {
        return None;
    };
    if ordinal < MAX_SYSTEM_TXS_PER_BLOCK {
        Some(ordinal)
    } else {
        None
    }
}

/// Highest block number that bootstraps the chain without Phase 1
/// (`CertifiedParentAccounting`). Block `n` runs Phase 1 in pre-execution iff
/// `n >= GENESIS_BOOTSTRAP_BLOCK_NUMBER + 1`.
pub const GENESIS_BOOTSTRAP_BLOCK_NUMBER: u64 = 1;

/// Uniform consensus block gas limit across all block heights (`0`, `1`, `>= 2`).
pub const STEADY_BLOCK_GAS_LIMIT: u64 = 32_000_000;

/// Canonical block gas limit committed by `ResourceScheduleV1`.
pub const fn protocol_block_gas_limit(_block_number: u64) -> u64 {
    STEADY_BLOCK_GAS_LIMIT
}

/// Internal execution gas limit used by the Outbe-aware system-call path.
pub const SYSTEM_TX_ARTIFACT_GAS_LIMIT: u64 = 10_000_000_000;

/// Worst-case Ethereum calldata base gas constant retained for `TeeBootstrapV2::preflight`.
pub const SYSTEM_TX_VISIBLE_GAS_FLOOR: u64 = 21_000;
/// Worst-case Ethereum non-zero calldata byte gas constant retained for `TeeBootstrapV2::preflight`.
pub const SYSTEM_TX_NON_ZERO_BYTE_GAS: u64 = 16;

/// Errors returned by deterministic system-tx helpers.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SystemTxError {
    #[error("system tx input too short: {len} bytes")]
    InputTooShort { len: usize },
    #[error("unknown system tx selector: 0x{0:02x?}")]
    UnknownSelector(SysTxSelector),
    #[error("unsupported system tx input version: {0}")]
    UnsupportedVersion(u8),
    #[error("non-empty {stage:?} payload: {len} bytes")]
    NonEmptyStagePayload {
        stage: BlockExecutionStage,
        len: usize,
    },
    #[error("empty block order for block {block_number}")]
    EmptyBlockOrder { block_number: u64 },
    #[error("missing boundary outcome body")]
    MissingBoundaryOutcomeBody,
    #[error("system tx codec error: {0}")]
    Codec(String),
    #[error("calldata kind mismatch: expected {expected}, actual {actual}")]
    CalldataKindMismatch {
        expected: &'static str,
        actual: &'static str,
    },
    #[error("system tx ordinal {ordinal} exceeds max {max}")]
    OrdinalTooLarge { ordinal: u8, max: u8 },
    #[error("system tx nonce overflow for block {block_number}, ordinal {ordinal}")]
    NonceOverflow { block_number: u64, ordinal: u8 },
    #[error("system tx must carry gas_limit == 0, got {actual}")]
    NonZeroGasLimit { actual: u64 },
    #[error("system tx must carry gas_price == 0, got {actual}")]
    NonZeroGasPrice { actual: u128 },
    #[error("system tx step {step:?} is in {actual:?} zone, expected {expected:?}")]
    SystemTxInWrongZone {
        step: SystemTxStepId,
        expected: BodyZone,
        actual: BodyZone,
    },
    #[error(
        "system tx kind order violation in {zone:?}: previous {previous:?}, current {current:?}"
    )]
    OutOfOrder {
        zone: BodyZone,
        previous: SystemTxStepId,
        current: SystemTxStepId,
    },
    #[error("reserved system tx found in user zone at transaction index {index}")]
    MidBlockSystemTx { index: usize },
    #[error("too many system txs in block: {actual} > {max}")]
    TooManySystemTxs { actual: usize, max: u8 },
    #[error(
        "active system tx set mismatch: expected begin {expected_begin:?}, expected end {expected_end:?}, actual begin {actual_begin:?}, actual end {actual_end:?}"
    )]
    ActiveSystemTxSetMismatch {
        expected_begin: Vec<SystemTxStepId>,
        expected_end: Vec<SystemTxStepId>,
        actual_begin: Vec<SystemTxStepId>,
        actual_end: Vec<SystemTxStepId>,
    },
    #[error("unexpected system tx {actual} in stage {stage:?} (expected {expected:?})")]
    UnexpectedSystemTx {
        stage: BlockExecutionStage,
        expected: Option<&'static str>,
        actual: &'static str,
    },
    #[error("unexpected user transaction in stage {stage:?}")]
    UnexpectedUserTx { stage: BlockExecutionStage },
    #[error("block execution finished in stage {stage:?} (expected next {expected_next:?})")]
    IncompleteBlockExecution {
        stage: BlockExecutionStage,
        expected_next: Option<&'static str>,
    },
    #[error(
        "V2 genesis bootstrap: block 1 must carry a BoundaryOutcome system tx (got has_boundary_outcome = false)"
    )]
    V2Block1MissingBoundaryOutcome,
    #[error("V2 genesis bootstrap: block 1 must carry TeeBootstrap")]
    V2Block1MissingTeeBootstrap,
    #[error("TeeBootstrap is only valid at block 1, got block {block_number}")]
    TeeBootstrapWrongHeight { block_number: u64 },
    #[error("V2 genesis bootstrap: block 1 must not carry user transactions (got {actual})")]
    V2Block1ContainsUserTransactions { actual: usize },
    #[error("phase1 tx decode failed: {0}")]
    Phase1TxDecode(String),
    #[error("phase1 tx signature recovery failed: {0}")]
    Phase1SignatureRecovery(String),
    #[error("phase1 tx must call OUTBE_SYSTEM_TX_ADDRESS")]
    Phase1WrongRecipient,
    #[error("phase1 tx must not transfer native value")]
    Phase1NonZeroValue,
    #[error("phase1 tx chain_id mismatch: expected {expected}, actual {actual:?}")]
    Phase1ChainIdMismatch { expected: u64, actual: Option<u64> },
    #[error("phase1 tx nonce mismatch: expected {expected}, actual {actual}")]
    Phase1NonceMismatch { expected: u64, actual: u64 },
    #[error("phase1 tx calldata mismatch")]
    Phase1CalldataMismatch,
    #[error("phase1 tx signature_hash mismatch")]
    Phase1SignatureHashMismatch,
    #[error("phase1 tx signer mismatch: expected {expected}, actual {actual}")]
    Phase1SignerMismatch {
        expected: alloy_primitives::Address,
        actual: alloy_primitives::Address,
    },
}

impl SystemTxError {
    pub(crate) fn from_precompile(error: PrecompileError) -> Self {
        Self::Codec(error.to_string())
    }

    pub(crate) fn phase1_tx_decode(error: impl std::fmt::Display) -> Self {
        Self::Phase1TxDecode(error.to_string())
    }

    pub(crate) fn phase1_signature_recovery(error: impl std::fmt::Display) -> Self {
        Self::Phase1SignatureRecovery(error.to_string())
    }
}

#[cfg(test)]
mod tests;
