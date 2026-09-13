//! Deterministic begin/end-block system-transaction primitives.
//!
//! Outbe represents runtime system transactions as ordinary signed Ethereum
//! legacy transaction artifacts so standard `eth_*` RPC methods can expose their
//! receipts and logs. The artifacts are consensus inputs only: execution uses
//! `transact_system_call` with `SYSTEM_ADDRESS` as the EVM caller, while the
//! signed transaction authenticates the proposer and fixes receipt/tx ordering.
//!
//! Begin-zone system transactions run before user transactions in this order:
//!
//! 1. [`SystemTxKind::CertifiedParentAccounting`] for block `>= 2`.
//! 2. [`SystemTxKind::LateFinalizeCredits`] for block `>= 2` (mandatory
//!    inclusion-window phase: records late finalize credits and settles the
//!    matured `N+K` fee escrow).
//! 3. [`SystemTxKind::OcompLifecycleBegin`] once the OCOMP lifecycle is active.
//! 4. [`SystemTxKind::CycleTick`] for block `>= 1`.
//! 5. [`SystemTxKind::RewardsGemDelivery`] for block `>= 1`.
//! 6. [`SystemTxKind::BoundaryOutcome`] iff the header carries a BoundaryOutcome
//!    (mandatory at block `1` under V2 for the genesis bootstrap).
//! 7. [`SystemTxKind::TeeBootstrap`] in the one-time bootstrap block.
//! 8. [`SystemTxKind::OracleSlashWindow`] for block `>= 1`.
//! 9. [`SystemTxKind::HookEvents`] for block `>= 1` (receipt container for
//!    whitelisted pre-exec hook logs; no lifecycle re-execution).
//!
//! Once active, [`SystemTxKind::OcompTerminalRequest`] is the sole end-zone
//! transaction. It follows every user transaction and the compressed-entity
//! seal.
//!
//! ## V2 codec
//!
//! This module ships the V2 wire codec exclusively. V1 system-tx input bytes
//! (selectors `OSF1`/`OSC1`/`OSB1`/`OSO1` with version byte `1`) are rejected
//! at every height. Rewards adds `OSG2`; OCOMP adds `OSE2` and `OSR2`, all
//! without changing the V2 version byte. Greenfield rollout.
//!
//! The split helper below is structural-only: it rejects reserved-address
//! transactions outside the contiguous system zones and rejects wrong-zone or
//! out-of-order system tx kinds. [`validate_active_system_tx_set`] performs the
//! separate membership check for a concrete block number and BoundaryOutcome
//! presence.

use crate::error::PrecompileError;

pub use crate::addresses::OUTBE_SYSTEM_TX_ADDRESS;
pub use outbe_ocomp_protocol::abi::{
    OCOMP_LIFECYCLE_BEGIN_SELECTOR, OCOMP_TERMINAL_REQUEST_SELECTOR,
};

mod envelope;
mod gas;
mod input;
mod kind;
mod layout;
mod phase;
mod witness;

pub use input::SystemTxInputV2;

pub use kind::{
    decode_system_tx_kind, is_reserved_system_tx, selector_from_input,
    system_tx_kind_from_selector, BodyZone, OcompLifecycleActivation, SystemTxKind,
};

pub use phase::SystemTxPhase;

pub use layout::{
    expected_begin_block_kinds, expected_begin_block_kinds_for_activation,
    expected_end_block_kinds, split_system_layout, validate_active_system_tx_set,
    validate_system_tx_set_for_activation, SystemTxLayout,
};

pub use gas::{system_tx_intrinsic_gas, system_tx_visible_gas_limit, SystemTxVisibleGasPlan};

pub use envelope::{
    build_unsigned_system_tx, build_unsigned_system_tx_with_gas_limit, system_tx_nonce,
};

pub use witness::{recover_phase1_proposer, validate_phase1_witness_against};

/// Version byte immediately after the 4-byte kind selector in system-tx input.
///
/// Bumped to `2` for V2 Certified-Parent Accounting. Decoder
/// rejects any other value, so V1 bodies with `1` are rejected at every height.
pub const SYSTEM_TX_INPUT_VERSION: u8 = 2;

/// Selector for [`SystemTxKind::CertifiedParentAccounting`] (V2 OSA3).
pub const CERTIFIED_PARENT_ACCOUNTING_SELECTOR: [u8; 4] = [b'O', b'S', b'A', b'3'];
/// Selector for [`SystemTxKind::CycleTick`] (V2 OSC2).
pub const CYCLE_TICK_SELECTOR: [u8; 4] = [b'O', b'S', b'C', b'2'];
/// Selector for [`SystemTxKind::RewardsGemDelivery`] (V2 OSG2).
pub const REWARDS_GEM_DELIVERY_SELECTOR: [u8; 4] = [b'O', b'S', b'G', b'2'];
/// Selector for [`SystemTxKind::BoundaryOutcome`] (V2 OSB2).
pub const BOUNDARY_OUTCOME_SELECTOR: [u8; 4] = [b'O', b'S', b'B', b'2'];
/// Selector for [`SystemTxKind::OracleSlashWindow`] (V2 OSO2).
pub const ORACLE_SLASH_WINDOW_SELECTOR: [u8; 4] = [b'O', b'S', b'O', b'2'];
/// Selector for the evidence-carrying V1 TEE bootstrap payload.
///
/// `OST2` was never a valid selector for this greenfield chain. Only `OST3`
/// is produced or accepted, in both `DcapRequired` and `GramineDirectDev`
/// networks.
pub const TEE_BOOTSTRAP_SELECTOR: [u8; 4] = [b'O', b'S', b'T', b'3'];
/// Selector for [`SystemTxKind::LateFinalizeCredits`].
pub const LATE_FINALIZE_CREDITS_SELECTOR: [u8; 4] = [b'O', b'S', b'L', b'2'];
/// Selector for [`SystemTxKind::HookEvents`] (V2 OSH2).
pub const HOOK_EVENTS_SELECTOR: [u8; 4] = [b'O', b'S', b'H', b'2'];

/// Hard cap on system transactions emitted in a block.
pub const MAX_SYSTEM_TXS_PER_BLOCK: u8 = 16;

/// Highest block number that bootstraps the chain without Phase 1
/// (`CertifiedParentAccounting`). Block `n` runs Phase 1 in pre-execution iff
/// `n >= GENESIS_BOOTSTRAP_BLOCK_NUMBER + 1`. sets this to `1` so
/// Phase 1 begins at block `2` while block `1` still carries the genesis
/// `BoundaryOutcome` as its first begin-zone system transaction.
pub const GENESIS_BOOTSTRAP_BLOCK_NUMBER: u64 = 1;

/// Consensus gas limit for the evidence-heavy one-time block-1 bootstrap.
pub const BOOTSTRAP_BLOCK_GAS_LIMIT: u64 = 500_000_000;
/// Consensus gas limit before bootstrap and from block 2 onward.
pub const STEADY_BLOCK_GAS_LIMIT: u64 = 30_000_000;

/// Height-selected block gas schedule committed by `ResourceScheduleV1`.
pub const fn protocol_block_gas_limit(block_number: u64) -> u64 {
    if block_number == GENESIS_BOOTSTRAP_BLOCK_NUMBER {
        BOOTSTRAP_BLOCK_GAS_LIMIT
    } else {
        STEADY_BLOCK_GAS_LIMIT
    }
}

/// Internal execution gas limit used by the Outbe-aware system-call path.
/// This value is never used as the visible `gas_limit` of the signed
/// transaction envelope; visible envelopes use their Ethereum intrinsic gas so
/// generic block replay/import tools do not reject them as exceeding the
/// block gas limit.
pub const SYSTEM_TX_ARTIFACT_GAS_LIMIT: u64 = 10_000_000_000;

/// Visible compressed-entity gas reserved for the OCOMP lifecycle phase.
///
/// Terminal expiry can request the first Tribute-partition retirement in the
/// block. Its current cleanup precharge is 15,000 gas; the additional 5,000
/// keeps the mandatory phase from sitting exactly on that boundary.
pub const OCOMP_LIFECYCLE_CE_GAS_RESERVE: u64 = 20_000;

/// Minimum visible gas charged by a system transaction envelope.
pub const SYSTEM_TX_VISIBLE_GAS_FLOOR: u64 = 21_000;

const SYSTEM_TX_ZERO_BYTE_GAS: u64 = 4;
pub const SYSTEM_TX_NON_ZERO_BYTE_GAS: u64 = 16;

/// Errors returned by deterministic system-tx helpers.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SystemTxError {
    #[error("system tx input too short: {len} bytes")]
    InputTooShort { len: usize },
    #[error("unknown system tx selector: 0x{0:02x?}")]
    UnknownSelector([u8; 4]),
    #[error("unsupported system tx input version: {0}")]
    UnsupportedVersion(u8),
    #[error("unexpected body for {kind:?}: {len} bytes")]
    UnexpectedBody { kind: SystemTxKind, len: usize },
    #[error("missing boundary outcome body")]
    MissingBoundaryOutcomeBody,
    #[error("system tx codec error: {0}")]
    Codec(String),
    #[error("calldata kind mismatch: expected {expected:?}, actual {actual:?}")]
    CalldataKindMismatch {
        expected: SystemTxKind,
        actual: SystemTxKind,
    },
    #[error("system tx ordinal {ordinal} exceeds max {max}")]
    OrdinalTooLarge { ordinal: u8, max: u8 },
    #[error("system tx nonce overflow for block {block_number}, ordinal {ordinal}")]
    NonceOverflow { block_number: u64, ordinal: u8 },
    #[error("system tx visible gas overflow for calldata length {len}")]
    VisibleGasOverflow { len: usize },
    #[error(
        "system tx gas limit below intrinsic gas: gas_limit={gas_limit}, intrinsic={intrinsic_gas}"
    )]
    GasLimitBelowIntrinsic { gas_limit: u64, intrinsic_gas: u64 },
    #[error(
        "system tx required gas exceeds block gas limit: required={required_gas}, block_limit={block_gas_limit}"
    )]
    VisibleGasPlanExceedsBlock {
        required_gas: u64,
        block_gas_limit: u64,
    },
    #[error("system tx visible gas plan contains more than one CycleTick")]
    DuplicateCycleTickGasBudget,
    #[error("system tx kind {kind:?} is in {actual:?} zone, expected {expected:?}")]
    SystemTxInWrongZone {
        kind: SystemTxKind,
        expected: BodyZone,
        actual: BodyZone,
    },
    #[error(
        "system tx kind order violation in {zone:?}: previous {previous:?}, current {current:?}"
    )]
    OutOfOrder {
        zone: BodyZone,
        previous: SystemTxKind,
        current: SystemTxKind,
    },
    #[error("reserved system tx found in user zone at transaction index {index}")]
    MidBlockSystemTx { index: usize },
    #[error("too many system txs in block: {actual} > {max}")]
    TooManySystemTxs { actual: usize, max: u8 },
    #[error(
        "active system tx set mismatch: expected begin {expected_begin:?}, expected end {expected_end:?}, actual begin {actual_begin:?}, actual end {actual_end:?}"
    )]
    ActiveSystemTxSetMismatch {
        expected_begin: Vec<SystemTxKind>,
        expected_end: Vec<SystemTxKind>,
        actual_begin: Vec<SystemTxKind>,
        actual_end: Vec<SystemTxKind>,
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
    #[error("phase1 tx gas_limit mismatch: expected {expected}, actual {actual}")]
    Phase1GasLimitMismatch { expected: u64, actual: u64 },
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
    fn from_precompile(error: PrecompileError) -> Self {
        Self::Codec(error.to_string())
    }
}

#[cfg(test)]
mod tests;
